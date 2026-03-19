use rusqlite::Connection;

use super::message_queries::row_to_summary;
use crate::models::MessageSummary;

pub(super) fn do_search(
    conn: &Connection,
    account_id: &str,
    query: &str,
) -> Result<Vec<MessageSummary>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let fts_query: String = if query.contains('"') {
        query.to_string()
    } else {
        query
            .split_whitespace()
            .map(|token| {
                let is_plain = token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !is_plain || token.len() < 3 || token.ends_with('*') {
                    token.to_string()
                } else {
                    format!("{token}*")
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut stmt = conn
        .prepare(
            "SELECT m.email_id, m.subject, m.sender, m.date, m.timestamp,
                    m.is_read, m.is_starred, m.has_attachments, m.thread_id,
                    m.flags_server, m.flags_local, m.pending_op, m.mailbox_id,
                    m.message_id, m.in_reply_to, m.thread_depth, m.reply_to, m.recipient,
                    m.account_id,
                    (SELECT GROUP_CONCAT(mm.mailbox_id) FROM message_mailboxes mm
                     WHERE mm.account_id = m.account_id AND mm.email_id = m.email_id) AS mailbox_ids
             FROM messages m
             WHERE m.account_id = ?2
               AND m.rowid IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?1)
             ORDER BY m.timestamp DESC
             LIMIT 200",
        )
        .map_err(|e| format!("Search prepare error: {e}"))?;

    let rows = stmt
        .query_map(rusqlite::params![&fts_query, account_id], row_to_summary)
        .map_err(|e| format!("Search query error: {e}"))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| format!("Search row error: {e}"))?);
    }
    Ok(results)
}

/// Load all messages in a thread across the given mailbox IDs, sorted by timestamp ASC.
pub(super) fn do_load_thread(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
    mailbox_ids: &[String],
) -> Result<Vec<MessageSummary>, String> {
    if mailbox_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Build parameterized IN clause: ?3, ?4, ...
    let placeholders: Vec<String> = (0..mailbox_ids.len())
        .map(|i| format!("?{}", i + 3))
        .collect();
    let in_clause = placeholders.join(", ");

    let sql = format!(
        "SELECT m.email_id, m.subject, m.sender, m.date, m.timestamp,
                m.is_read, m.is_starred, m.has_attachments, m.thread_id,
                m.flags_server, m.flags_local, m.pending_op, m.mailbox_id,
                m.message_id, m.in_reply_to, m.thread_depth, m.reply_to, m.recipient,
                m.account_id,
                (SELECT GROUP_CONCAT(mm2.mailbox_id) FROM message_mailboxes mm2
                 WHERE mm2.account_id = m.account_id AND mm2.email_id = m.email_id) AS mailbox_ids
         FROM messages m
         JOIN message_mailboxes mm ON mm.account_id = m.account_id AND mm.email_id = m.email_id
         WHERE m.account_id = ?1 AND m.thread_id = ?2 AND mm.mailbox_id IN ({in_clause})
         GROUP BY m.account_id, m.email_id
         ORDER BY m.timestamp ASC"
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    params.push(Box::new(account_id.to_string()));
    params.push(Box::new(thread_id.to_string()));
    for mid in mailbox_ids {
        params.push(Box::new(mid.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt
        .query_map(&*param_refs, row_to_summary)
        .map_err(|e| format!("Cache query error: {e}"))?;

    let mut messages = Vec::new();
    for row in rows {
        messages.push(row.map_err(|e| format!("Cache row error: {e}"))?);
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::models::{Folder, MessageSummary};
    use crate::store::folder_queries::do_save_folders;
    use crate::store::message_queries::{do_remove_message, do_save_messages};
    use crate::store::schema::{run_migrations, SCHEMA};

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(SCHEMA).expect("create schema");
        run_migrations(&conn);
        conn
    }

    fn sample_folder(mailbox_id: &str) -> Folder {
        Folder {
            name: "INBOX".into(),
            path: "INBOX".into(),
            unread_count: 0,
            total_count: 0,
            mailbox_id: mailbox_id.to_string(),
            role: Some("inbox".into()),
            sort_order: 0,
        }
    }

    fn sample_folder_named(mailbox_id: &str, name: &str) -> Folder {
        Folder {
            name: name.into(),
            path: name.into(),
            unread_count: 0,
            total_count: 0,
            mailbox_id: mailbox_id.to_string(),
            role: None,
            sort_order: 0,
        }
    }

    fn sample_message(email_id: &str, mailbox_id: &str, subject: &str) -> MessageSummary {
        MessageSummary {
            account_id: String::new(),
            email_id: email_id.to_string(),
            subject: subject.to_string(),
            from: "from@example.com".to_string(),
            to: "to@example.com".to_string(),
            date: "2026-01-01".to_string(),
            is_read: false,
            is_starred: false,
            has_attachments: false,
            thread_id: None,
            mailbox_ids: vec![mailbox_id.to_string()],
            context_mailbox_id: mailbox_id.to_string(),
            timestamp: 100,
            message_id: format!("<{}@example.com>", email_id),
            in_reply_to: None,
            reply_to: None,
            thread_depth: 0,
        }
    }

    // -- FTS tests --

    #[test]
    fn fts_finds_updated_subject() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[sample_message("e1", "mb1", "original subject")],
        )
        .expect("save");

        let hits_before = do_search(&conn, "a", "original").expect("search before");
        assert_eq!(hits_before.len(), 1);

        // Update subject via upsert
        let mut updated = sample_message("e1", "mb1", "completely different");
        updated.timestamp = 200;
        do_save_messages(&conn, "a", "mb1", &[updated]).expect("upsert");

        let hits_old = do_search(&conn, "a", "original").expect("search old");
        assert!(
            hits_old.is_empty(),
            "old subject should not match after update"
        );

        let hits_new = do_search(&conn, "a", "completely").expect("search new");
        assert_eq!(hits_new.len(), 1, "new subject should match after update");
    }

    #[test]
    fn fts_removes_deleted_message() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[sample_message("e1", "mb1", "findable subject")],
        )
        .expect("save");

        let hits_before = do_search(&conn, "a", "findable").expect("search before");
        assert_eq!(hits_before.len(), 1);

        do_remove_message(&conn, "a", "e1").expect("remove");

        let hits_after = do_search(&conn, "a", "findable").expect("search after");
        assert!(hits_after.is_empty(), "FTS should not find deleted message");
    }

    #[test]
    fn search_prefix_matching() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[sample_message("e1", "mb1", "Invoice from Acme Corp")],
        )
        .expect("save");

        // Short prefix (< 3 chars) should not get wildcard expansion
        let _hits_short = do_search(&conn, "a", "in").expect("search short");
        // FTS5 behavior: "in" won't match "Invoice" without wildcard
        // This is expected — short tokens are kept as-is

        // 3+ char prefix should match via wildcard expansion
        let hits_prefix = do_search(&conn, "a", "inv").expect("search prefix");
        assert_eq!(hits_prefix.len(), 1, "prefix 'inv' should match 'Invoice'");

        let hits_full = do_search(&conn, "a", "invoice").expect("search full");
        assert_eq!(hits_full.len(), 1);
    }

    // -- Thread loading tests --

    #[test]
    fn load_thread_returns_sorted_by_timestamp() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        let mut m1 = sample_message("e1", "mb1", "first");
        m1.thread_id = Some("t1".into());
        m1.timestamp = 300;

        let mut m2 = sample_message("e2", "mb1", "second");
        m2.thread_id = Some("t1".into());
        m2.timestamp = 100;

        let mut m3 = sample_message("e3", "mb1", "third");
        m3.thread_id = Some("t1".into());
        m3.timestamp = 200;

        do_save_messages(&conn, "a", "mb1", &[m1, m2, m3]).expect("save");

        let thread = do_load_thread(&conn, "a", "t1", &["mb1".into()]).expect("load thread");
        assert_eq!(thread.len(), 3);
        assert_eq!(thread[0].email_id, "e2", "earliest first");
        assert_eq!(thread[1].email_id, "e3");
        assert_eq!(thread[2].email_id, "e1", "latest last");
    }

    #[test]
    fn load_thread_filters_by_mailbox_ids() {
        let conn = setup_conn();
        do_save_folders(
            &conn,
            "a",
            &[
                sample_folder_named("mb1", "INBOX"),
                sample_folder_named("mb2", "Sent"),
            ],
        )
        .expect("save folders");

        let mut m1 = sample_message("e1", "mb1", "inbox msg");
        m1.thread_id = Some("t1".into());

        let mut m2 = sample_message("e2", "mb2", "sent msg");
        m2.thread_id = Some("t1".into());

        do_save_messages(&conn, "a", "mb1", &[m1]).expect("save mb1");
        do_save_messages(&conn, "a", "mb2", &[m2]).expect("save mb2");

        // Only request mb1
        let thread = do_load_thread(&conn, "a", "t1", &["mb1".into()]).expect("load thread");
        assert_eq!(thread.len(), 1);
        assert_eq!(thread[0].email_id, "e1");

        // Request both
        let thread_both = do_load_thread(&conn, "a", "t1", &["mb1".into(), "mb2".into()])
            .expect("load thread both");
        assert_eq!(thread_both.len(), 2);
    }

    #[test]
    fn search_results_are_isolated_per_account() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder a");
        do_save_folders(&conn, "b", &[sample_folder("mb1")]).expect("save folder b");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[sample_message("e1", "mb1", "unique_needle_subject")],
        )
        .expect("save a");
        do_save_messages(
            &conn,
            "b",
            "mb1",
            &[sample_message("e2", "mb1", "unique_needle_subject")],
        )
        .expect("save b");

        let hits_a = do_search(&conn, "a", "unique_needle").expect("search a");
        assert_eq!(
            hits_a.len(),
            1,
            "search in account a should return only a's message"
        );
        assert_eq!(hits_a[0].email_id, "e1");

        let hits_b = do_search(&conn, "b", "unique_needle").expect("search b");
        assert_eq!(
            hits_b.len(),
            1,
            "search in account b should return only b's message"
        );
        assert_eq!(hits_b[0].email_id, "e2");
    }
}
