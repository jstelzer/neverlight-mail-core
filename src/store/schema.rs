use rusqlite::Connection;

/// Schema DDL run on open.
///
/// JMAP-native schema: uses TEXT IDs (email_id, mailbox_id, thread_id)
/// instead of INTEGER hashes.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (
    account_id TEXT NOT NULL,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    mailbox_id TEXT NOT NULL,
    role TEXT,
    sort_order INTEGER DEFAULT 0,
    unread_count INTEGER DEFAULT 0,
    total_count INTEGER DEFAULT 0,
    PRIMARY KEY (account_id, path),
    UNIQUE (account_id, mailbox_id)
);

CREATE TABLE IF NOT EXISTS messages (
    account_id TEXT NOT NULL,
    email_id TEXT NOT NULL,
    mailbox_id TEXT NOT NULL,
    subject TEXT,
    sender TEXT,
    date TEXT,
    timestamp INTEGER NOT NULL DEFAULT 0,
    is_read INTEGER DEFAULT 0,
    is_starred INTEGER DEFAULT 0,
    has_attachments INTEGER DEFAULT 0,
    thread_id TEXT,
    body_rendered TEXT,
    flags_server INTEGER DEFAULT 0,
    flags_local INTEGER DEFAULT 0,
    pending_op TEXT,
    message_id TEXT,
    in_reply_to TEXT,
    thread_depth INTEGER DEFAULT 0,
    body_markdown TEXT,
    reply_to TEXT,
    recipient TEXT,
    PRIMARY KEY (account_id, email_id),
    FOREIGN KEY (account_id, mailbox_id) REFERENCES folders(account_id, mailbox_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_mailbox
    ON messages(account_id, mailbox_id, timestamp DESC);

CREATE TABLE IF NOT EXISTS attachments (
    account_id TEXT NOT NULL,
    email_id TEXT NOT NULL,
    idx INTEGER NOT NULL,
    filename TEXT NOT NULL DEFAULT 'unnamed',
    mime_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    data BLOB NOT NULL,
    PRIMARY KEY (account_id, email_id, idx),
    FOREIGN KEY (account_id, email_id) REFERENCES messages(account_id, email_id) ON DELETE CASCADE
);
";

/// Sync state table — added in Phase 3.
/// Stores JMAP state tokens per (account, resource) for delta sync.
const SYNC_STATE_DDL: &str = "
CREATE TABLE IF NOT EXISTS sync_state (
    account_id TEXT NOT NULL,
    resource TEXT NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY (account_id, resource)
);
";

/// Run forward-only migrations.
pub(super) fn run_migrations(conn: &Connection) {
    // Sync state table
    if let Err(e) = conn.execute_batch(SYNC_STATE_DDL) {
        log::warn!("sync_state migration failed: {}", e);
    }
    // Index on message_id for threading lookups
    let indexes = [
        "CREATE INDEX IF NOT EXISTS idx_messages_message_id ON messages(message_id)",
        "CREATE INDEX IF NOT EXISTS idx_folders_account ON folders(account_id)",
        "CREATE INDEX IF NOT EXISTS idx_messages_account_mailbox ON messages(account_id, mailbox_id, timestamp DESC)",
    ];
    for sql in &indexes {
        if let Err(e) = conn.execute(sql, []) {
            log::warn!("Index creation failed: {}", e);
        }
    }

    // FTS5 full-text search index
    let fts_ddl = [
        "CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
            subject,
            sender,
            body_rendered,
            content='messages',
            content_rowid='rowid'
        )",
        "CREATE TRIGGER IF NOT EXISTS messages_fts_ai AFTER INSERT ON messages BEGIN
          INSERT INTO message_fts(rowid, subject, sender, body_rendered)
          VALUES (new.rowid, new.subject, new.sender, new.body_rendered);
        END",
        "CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
          INSERT INTO message_fts(message_fts, rowid, subject, sender, body_rendered)
          VALUES('delete', old.rowid, old.subject, old.sender, old.body_rendered);
        END",
        "CREATE TRIGGER IF NOT EXISTS messages_fts_au AFTER UPDATE ON messages BEGIN
          INSERT INTO message_fts(message_fts, rowid, subject, sender, body_rendered)
          VALUES('delete', old.rowid, old.subject, old.sender, old.body_rendered);
          INSERT INTO message_fts(rowid, subject, sender, body_rendered)
          VALUES (new.rowid, new.subject, new.sender, new.body_rendered);
        END",
    ];
    for ddl in &fts_ddl {
        if let Err(e) = conn.execute_batch(ddl) {
            log::warn!(
                "FTS5 migration failed ({}): {}",
                ddl.chars().take(60).collect::<String>(),
                e
            );
        }
    }

    // Rebuild FTS index from existing content
    if let Err(e) = conn.execute("INSERT INTO message_fts(message_fts) VALUES('rebuild')", []) {
        log::warn!("FTS5 rebuild failed: {}", e);
    }

    // Invalidate cached rendered bodies so they re-render through the updated
    // html-safe-md pipeline. Only runs once: skips if bodies are already NULL.
    migrate_invalidate_body_cache(conn);
}

/// One-shot migration: NULL out cached body columns to force re-rendering.
///
/// The rendering pipeline (html-safe-md) changed — cached bodies from older
/// runs contain layout table decoration, leaked CSS, and raw tracking URLs.
/// Setting them to NULL causes the cache-first path to miss, triggering a
/// fresh render on next view.
fn migrate_invalidate_body_cache(conn: &Connection) {
    // Check if there's anything to invalidate
    let has_stale: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE body_rendered IS NOT NULL OR body_markdown IS NOT NULL)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !has_stale {
        return;
    }

    // Use a marker table to ensure this only runs once
    let already_done: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='_body_cache_v2')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if already_done {
        return;
    }

    log::info!("Invalidating cached email bodies for re-rendering (pipeline update)");
    if let Err(e) = conn.execute_batch(
        "UPDATE messages SET body_rendered = NULL, body_markdown = NULL;
         CREATE TABLE IF NOT EXISTS _body_cache_v2 (migrated INTEGER DEFAULT 1);
         INSERT INTO _body_cache_v2 VALUES (1);"
    ) {
        log::warn!("Body cache invalidation failed: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{run_migrations, SCHEMA};

    #[test]
    fn schema_creates_cleanly() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(SCHEMA).expect("create schema");
        run_migrations(&conn);

        // Verify tables exist
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('folders', 'messages', 'attachments')",
                [],
                |row| row.get(0),
            )
            .expect("count tables");
        assert_eq!(count, 3);
    }

    #[test]
    fn fts_triggers_work() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(SCHEMA).expect("create schema");
        run_migrations(&conn);

        // Insert a folder first (FK constraint)
        conn.execute(
            "INSERT INTO folders (account_id, path, name, mailbox_id, unread_count, total_count) VALUES ('a', 'INBOX', 'INBOX', 'mb1', 0, 1)",
            [],
        ).expect("insert folder");

        // Insert a message
        conn.execute(
            "INSERT INTO messages (account_id, email_id, mailbox_id, subject, sender, date, timestamp, body_rendered)
             VALUES ('a', 'e1', 'mb1', 'searchneedle', 'sender@example.com', '2026-01-01', 1000, 'body text')",
            [],
        ).expect("insert message");

        // FTS should find it
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message_fts WHERE message_fts MATCH 'searchneedle'",
                [],
                |row| row.get(0),
            )
            .expect("query fts");
        assert_eq!(hits, 1);
    }
}
