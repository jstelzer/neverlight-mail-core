//! Folder, account, and sync state SQL operations.

use rusqlite::Connection;

use crate::models::Folder;

/// Inner save-folders logic that operates on an existing transaction.
fn save_folders_in_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    folders: &[Folder],
) -> Result<(), String> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO folders (account_id, path, name, mailbox_id, role, sort_order, unread_count, total_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(account_id, path) DO UPDATE SET
                 name = excluded.name,
                 mailbox_id = excluded.mailbox_id,
                 role = excluded.role,
                 sort_order = excluded.sort_order,
                 unread_count = excluded.unread_count,
                 total_count = excluded.total_count",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    for f in folders {
        stmt.execute(rusqlite::params![
            account_id,
            f.path,
            f.name,
            f.mailbox_id,
            f.role,
            f.sort_order,
            f.unread_count,
            f.total_count,
        ])
        .map_err(|e| format!("Cache insert error: {e}"))?;
    }
    drop(stmt);

    // Remove folders that no longer exist on the server
    let server_ids: Vec<&str> = folders.iter().map(|f| f.mailbox_id.as_str()).collect();
    if server_ids.is_empty() {
        tx.execute(
            "DELETE FROM message_mailboxes WHERE account_id = ?1",
            [account_id],
        )
        .map_err(|e| format!("Cache junction cascade error: {e}"))?;
        tx.execute(
            "DELETE FROM attachments WHERE account_id = ?1 AND email_id IN (
                SELECT email_id FROM messages WHERE account_id = ?1
            )",
            [account_id],
        )
        .map_err(|e| format!("Cache cascade error: {e}"))?;
        tx.execute("DELETE FROM messages WHERE account_id = ?1", [account_id])
            .map_err(|e| format!("Cache cascade error: {e}"))?;
        tx.execute("DELETE FROM folders WHERE account_id = ?1", [account_id])
            .map_err(|e| format!("Cache delete error: {e}"))?;
    } else {
        let placeholders: String = (0..server_ids.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");

        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        params.push(Box::new(account_id.to_string()));
        for id in &server_ids {
            params.push(Box::new(id.to_string()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        // Remove junction rows for stale mailboxes
        let sql = format!(
            "DELETE FROM message_mailboxes WHERE account_id = ?1 AND mailbox_id NOT IN ({placeholders})"
        );
        tx.execute(&sql, param_refs.as_slice())
            .map_err(|e| format!("Cache junction cascade error: {e}"))?;

        // Remove orphaned messages (no remaining mailbox associations)
        tx.execute(
            "DELETE FROM attachments WHERE account_id = ?1 AND email_id NOT IN (
                SELECT email_id FROM message_mailboxes WHERE account_id = ?1
            )",
            [account_id],
        )
        .map_err(|e| format!("Cache cascade error: {e}"))?;
        tx.execute(
            "DELETE FROM messages WHERE account_id = ?1 AND email_id NOT IN (
                SELECT email_id FROM message_mailboxes WHERE account_id = ?1
            )",
            [account_id],
        )
        .map_err(|e| format!("Cache cascade error: {e}"))?;

        let sql = format!(
            "DELETE FROM folders WHERE account_id = ?1 AND mailbox_id NOT IN ({placeholders})"
        );
        tx.execute(&sql, param_refs.as_slice())
            .map_err(|e| format!("Cache delete error: {e}"))?;
    }
    Ok(())
}

pub(super) fn do_save_folders(
    conn: &Connection,
    account_id: &str,
    folders: &[Folder],
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;
    save_folders_in_tx(&tx, account_id, folders)?;
    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

/// Atomic: save folders + set sync state in one transaction.
pub(super) fn do_save_folders_and_set_state(
    conn: &Connection,
    account_id: &str,
    folders: &[Folder],
    resource: &str,
    state: &str,
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;
    save_folders_in_tx(&tx, account_id, folders)?;
    set_state_in_tx(&tx, account_id, resource, state)?;
    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

/// Atomic: upsert + remove folders + set sync state in one transaction.
pub(super) fn do_delta_folders_and_set_state(
    conn: &Connection,
    account_id: &str,
    upsert: &[Folder],
    remove_ids: &[String],
    resource: &str,
    state: &str,
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    // Upsert folders
    upsert_folders_in_tx(&tx, account_id, upsert)?;

    // Remove destroyed folders (cascade to messages + attachments)
    if !remove_ids.is_empty() {
        remove_folders_in_tx(&tx, account_id, remove_ids)?;
    }

    // Set state
    set_state_in_tx(&tx, account_id, resource, state)?;

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

/// Set sync state within an existing transaction.
fn set_state_in_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    resource: &str,
    state: &str,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO sync_state (account_id, resource, state) VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id, resource) DO UPDATE SET state = excluded.state",
        rusqlite::params![account_id, resource, state],
    )
    .map_err(|e| format!("set_state error: {e}"))?;
    Ok(())
}

/// Upsert folders within an existing transaction (no prune).
fn upsert_folders_in_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    folders: &[Folder],
) -> Result<(), String> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO folders (account_id, path, name, mailbox_id, role, sort_order, unread_count, total_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(account_id, path) DO UPDATE SET
                 name = excluded.name,
                 mailbox_id = excluded.mailbox_id,
                 role = excluded.role,
                 sort_order = excluded.sort_order,
                 unread_count = excluded.unread_count,
                 total_count = excluded.total_count",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    for f in folders {
        stmt.execute(rusqlite::params![
            account_id,
            f.path,
            f.name,
            f.mailbox_id,
            f.role,
            f.sort_order,
            f.unread_count,
            f.total_count,
        ])
        .map_err(|e| format!("Cache upsert error: {e}"))?;
    }
    drop(stmt);
    Ok(())
}

/// Remove folders by ID within an existing transaction (cascade).
fn remove_folders_in_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    mailbox_ids: &[String],
) -> Result<(), String> {
    if mailbox_ids.is_empty() {
        return Ok(());
    }

    let placeholders: String = (0..mailbox_ids.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",");

    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    params.push(Box::new(account_id.to_string()));
    for id in mailbox_ids {
        params.push(Box::new(id.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|p| p.as_ref()).collect();

    // Cascade: junction rows → orphaned messages/attachments → folders
    let sql = format!(
        "DELETE FROM message_mailboxes WHERE account_id = ?1 AND mailbox_id IN ({placeholders})"
    );
    tx.execute(&sql, param_refs.as_slice())
        .map_err(|e| format!("Cache junction cascade error: {e}"))?;

    tx.execute(
        "DELETE FROM attachments WHERE account_id = ?1 AND email_id NOT IN (
            SELECT email_id FROM message_mailboxes WHERE account_id = ?1
        )",
        [account_id],
    )
    .map_err(|e| format!("Cache cascade error: {e}"))?;
    tx.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND email_id NOT IN (
            SELECT email_id FROM message_mailboxes WHERE account_id = ?1
        )",
        [account_id],
    )
    .map_err(|e| format!("Cache cascade error: {e}"))?;

    let sql = format!(
        "DELETE FROM folders WHERE account_id = ?1 AND mailbox_id IN ({placeholders})"
    );
    tx.execute(&sql, param_refs.as_slice())
        .map_err(|e| format!("Cache delete error: {e}"))?;

    Ok(())
}

/// Upsert folders without pruning absent ones.
///
/// Unlike `do_save_folders`, this inserts/updates the given folders but does
/// NOT delete folders missing from the list. Used by delta sync to apply
/// created/updated mailboxes without affecting the rest.
pub(super) fn do_upsert_folders(
    conn: &Connection,
    account_id: &str,
    folders: &[Folder],
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    let mut stmt = tx
        .prepare(
            "INSERT INTO folders (account_id, path, name, mailbox_id, role, sort_order, unread_count, total_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(account_id, path) DO UPDATE SET
                 name = excluded.name,
                 mailbox_id = excluded.mailbox_id,
                 role = excluded.role,
                 sort_order = excluded.sort_order,
                 unread_count = excluded.unread_count,
                 total_count = excluded.total_count",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    for f in folders {
        stmt.execute(rusqlite::params![
            account_id,
            f.path,
            f.name,
            f.mailbox_id,
            f.role,
            f.sort_order,
            f.unread_count,
            f.total_count,
        ])
        .map_err(|e| format!("Cache upsert error: {e}"))?;
    }
    drop(stmt);

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

/// Remove specific folders by mailbox ID, cascading to their messages and attachments.
pub(super) fn do_remove_folders_by_id(
    conn: &Connection,
    account_id: &str,
    mailbox_ids: &[String],
) -> Result<(), String> {
    if mailbox_ids.is_empty() {
        return Ok(());
    }

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    let placeholders: String = (0..mailbox_ids.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",");

    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    params.push(Box::new(account_id.to_string()));
    for id in mailbox_ids {
        params.push(Box::new(id.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|p| p.as_ref()).collect();

    // Cascade: junction rows → orphaned messages/attachments → folders

    // Remove junction rows for these mailboxes
    let sql = format!(
        "DELETE FROM message_mailboxes WHERE account_id = ?1 AND mailbox_id IN ({placeholders})"
    );
    tx.execute(&sql, param_refs.as_slice())
        .map_err(|e| format!("Cache junction cascade error: {e}"))?;

    // Remove orphaned messages (no remaining mailbox associations)
    tx.execute(
        "DELETE FROM attachments WHERE account_id = ?1 AND email_id NOT IN (
            SELECT email_id FROM message_mailboxes WHERE account_id = ?1
        )",
        [account_id],
    )
    .map_err(|e| format!("Cache cascade error: {e}"))?;
    tx.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND email_id NOT IN (
            SELECT email_id FROM message_mailboxes WHERE account_id = ?1
        )",
        [account_id],
    )
    .map_err(|e| format!("Cache cascade error: {e}"))?;

    let sql = format!(
        "DELETE FROM folders WHERE account_id = ?1 AND mailbox_id IN ({placeholders})"
    );
    tx.execute(&sql, param_refs.as_slice())
        .map_err(|e| format!("Cache delete error: {e}"))?;

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

pub(super) fn do_load_folders(conn: &Connection, account_id: &str) -> Result<Vec<Folder>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT path, name, mailbox_id, role, sort_order, unread_count, total_count FROM folders
             WHERE account_id = ?1",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    let rows = stmt
        .query_map([account_id], |row| {
            Ok(Folder {
                path: row.get(0)?,
                name: row.get(1)?,
                mailbox_id: row.get(2)?,
                role: row.get(3)?,
                sort_order: row.get(4)?,
                unread_count: row.get(5)?,
                total_count: row.get(6)?,
            })
        })
        .map_err(|e| format!("Cache query error: {e}"))?;

    let mut folders = Vec::new();
    for row in rows {
        folders.push(row.map_err(|e| format!("Cache row error: {e}"))?);
    }

    // Sort: INBOX first, then by sort_order, then alphabetical
    folders.sort_by(|a, b| {
        let a_inbox = a.role.as_deref() == Some("inbox");
        let b_inbox = b.role.as_deref() == Some("inbox");
        if a_inbox && !b_inbox {
            std::cmp::Ordering::Less
        } else if !a_inbox && b_inbox {
            std::cmp::Ordering::Greater
        } else {
            a.sort_order.cmp(&b.sort_order).then(a.path.cmp(&b.path))
        }
    });

    Ok(folders)
}

pub(super) fn do_remove_account(conn: &Connection, account_id: &str) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    tx.execute("DELETE FROM message_mailboxes WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache junction cleanup error: {e}"))?;
    tx.execute("DELETE FROM attachments WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache attachment cleanup error: {e}"))?;
    tx.execute("DELETE FROM messages WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache message cleanup error: {e}"))?;
    tx.execute("DELETE FROM folders WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache folder cleanup error: {e}"))?;
    tx.execute("DELETE FROM sync_state WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache sync_state cleanup error: {e}"))?;
    tx.execute("DELETE FROM backfill_progress WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache backfill_progress cleanup error: {e}"))?;

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

pub(super) fn do_get_state(
    conn: &Connection,
    account_id: &str,
    resource: &str,
) -> Result<Option<String>, String> {
    match conn.query_row(
        "SELECT state FROM sync_state WHERE account_id = ?1 AND resource = ?2",
        rusqlite::params![account_id, resource],
        |row| row.get(0),
    ) {
        Ok(state) => Ok(Some(state)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("get_state error: {e}")),
    }
}

pub(super) fn do_set_state(
    conn: &Connection,
    account_id: &str,
    resource: &str,
    state: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO sync_state (account_id, resource, state) VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id, resource) DO UPDATE SET state = excluded.state",
        rusqlite::params![account_id, resource, state],
    )
    .map_err(|e| format!("set_state error: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{run_migrations, SCHEMA};

    fn setup_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
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

    #[test]
    fn folders_are_isolated_per_account() {
        let conn = setup_conn();

        do_save_folders(&conn, "a", &[Folder {
            mailbox_id: "mb1".into(),
            unread_count: 1,
            total_count: 2,
            ..sample_folder("mb1")
        }])
        .expect("save folders a");

        do_save_folders(&conn, "b", &[Folder {
            mailbox_id: "mb1".into(),
            unread_count: 9,
            total_count: 10,
            ..sample_folder("mb1")
        }])
        .expect("save folders b");

        let a_folders = do_load_folders(&conn, "a").expect("load folders a");
        let b_folders = do_load_folders(&conn, "b").expect("load folders b");

        assert_eq!(a_folders.len(), 1);
        assert_eq!(b_folders.len(), 1);
        assert_eq!(a_folders[0].unread_count, 1);
        assert_eq!(b_folders[0].unread_count, 9);
    }

    #[test]
    fn state_get_set_round_trip() {
        let conn = setup_conn();

        // No prior state → None
        let none = do_get_state(&conn, "a", "Mailbox").expect("get empty");
        assert!(none.is_none());

        // Set and get
        do_set_state(&conn, "a", "Mailbox", "s42").expect("set");
        let some = do_get_state(&conn, "a", "Mailbox").expect("get");
        assert_eq!(some.as_deref(), Some("s42"));

        // Overwrite
        do_set_state(&conn, "a", "Mailbox", "s99").expect("set again");
        let updated = do_get_state(&conn, "a", "Mailbox").expect("get updated");
        assert_eq!(updated.as_deref(), Some("s99"));

        // Different resource is independent
        let other = do_get_state(&conn, "a", "Email:mb1").expect("get other");
        assert!(other.is_none());

        // Different account is independent
        let other_acct = do_get_state(&conn, "b", "Mailbox").expect("get other account");
        assert!(other_acct.is_none());
    }

    #[test]
    fn load_folders_sorts_inbox_first_then_by_sort_order_then_alpha() {
        let conn = setup_conn();

        do_save_folders(&conn, "a", &[
            Folder {
                name: "Zeta".into(),
                path: "Zeta".into(),
                mailbox_id: "mb3".into(),
                role: None,
                sort_order: 5,
                unread_count: 0,
                total_count: 0,
            },
            Folder {
                name: "Alpha".into(),
                path: "Alpha".into(),
                mailbox_id: "mb2".into(),
                role: None,
                sort_order: 5,
                unread_count: 0,
                total_count: 0,
            },
            Folder {
                name: "Drafts".into(),
                path: "Drafts".into(),
                mailbox_id: "mb4".into(),
                role: Some("drafts".into()),
                sort_order: 3,
                unread_count: 0,
                total_count: 0,
            },
            Folder {
                name: "Inbox".into(),
                path: "Inbox".into(),
                mailbox_id: "mb1".into(),
                role: Some("inbox".into()),
                sort_order: 10,
                unread_count: 0,
                total_count: 0,
            },
        ]).expect("save folders");

        let folders = do_load_folders(&conn, "a").expect("load");
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();

        // Inbox always first regardless of sort_order
        assert_eq!(names[0], "Inbox");
        // Then by sort_order: Drafts (3) before Alpha/Zeta (5)
        assert_eq!(names[1], "Drafts");
        // Same sort_order: alphabetical by path
        assert_eq!(names[2], "Alpha");
        assert_eq!(names[3], "Zeta");
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

    #[test]
    fn upsert_folders_does_not_delete_unmentioned() {
        let conn = setup_conn();

        // Save folder A via full save
        do_save_folders(&conn, "a", &[sample_folder_named("mb1", "Alpha")])
            .expect("save A");

        // Upsert folder B — A should survive
        do_upsert_folders(&conn, "a", &[sample_folder_named("mb2", "Beta")])
            .expect("upsert B");

        let folders = do_load_folders(&conn, "a").expect("load");
        assert_eq!(folders.len(), 2);
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"Alpha"));
        assert!(names.contains(&"Beta"));
    }

    #[test]
    fn remove_folders_by_id_cascades() {
        let conn = setup_conn();
        use crate::models::MessageSummary;
        use crate::store::queries::{do_save_messages, do_load_messages, do_save_body, do_load_body};
        use crate::models::AttachmentData;

        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Trash"),
        ]).expect("save folders");

        let msg = MessageSummary {
            account_id: String::new(),
            email_id: "e1".into(),
            subject: "trash msg".into(),
            from: "from@example.com".into(),
            to: "to@example.com".into(),
            date: "2026-01-01".into(),
            is_read: false,
            is_starred: false,
            has_attachments: false,
            thread_id: None,
            mailbox_ids: vec!["mb2".into()],
            context_mailbox_id: "mb2".into(),
            timestamp: 100,
            message_id: "<e1@example.com>".into(),
            in_reply_to: None,
            reply_to: None,
            thread_depth: 0,
        };
        do_save_messages(&conn, "a", "mb2", &[msg]).expect("save msg");
        do_save_body(&conn, "a", "e1", "# body", "body", &[AttachmentData {
            filename: "file.txt".into(),
            mime_type: "text/plain".into(),
            data: b"data".to_vec(),
        }]).expect("save body");

        // Remove mb2 specifically
        do_remove_folders_by_id(&conn, "a", &["mb2".to_string()]).expect("remove mb2");

        // mb2's messages and body should be gone
        let msgs = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2");
        assert!(msgs.is_empty());
        let body = do_load_body(&conn, "a", "e1").expect("load body");
        assert!(body.is_none());

        // mb1 folder should still exist
        let folders = do_load_folders(&conn, "a").expect("load folders");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].mailbox_id, "mb1");
    }

    #[test]
    fn remove_folders_by_id_does_not_affect_others() {
        let conn = setup_conn();

        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Sent"),
            sample_folder_named("mb3", "Trash"),
        ]).expect("save folders");

        do_remove_folders_by_id(&conn, "a", &["mb2".to_string()]).expect("remove mb2");

        let folders = do_load_folders(&conn, "a").expect("load");
        assert_eq!(folders.len(), 2);
        let ids: Vec<&str> = folders.iter().map(|f| f.mailbox_id.as_str()).collect();
        assert!(ids.contains(&"mb1"));
        assert!(ids.contains(&"mb3"));
        assert!(!ids.contains(&"mb2"));
    }
}
