//! Folder, account, and sync state SQL operations.

use rusqlite::Connection;

use crate::models::Folder;

pub(super) fn do_save_folders(
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
        .map_err(|e| format!("Cache insert error: {e}"))?;
    }
    drop(stmt);

    // Remove folders that no longer exist on the server
    let server_ids: Vec<&str> = folders.iter().map(|f| f.mailbox_id.as_str()).collect();
    if server_ids.is_empty() {
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

        let sql = format!(
            "DELETE FROM attachments WHERE account_id = ?1 AND email_id IN (
                SELECT email_id FROM messages WHERE account_id = ?1 AND mailbox_id NOT IN ({placeholders})
            )"
        );
        tx.execute(&sql, param_refs.as_slice())
            .map_err(|e| format!("Cache cascade error: {e}"))?;

        let sql = format!(
            "DELETE FROM messages WHERE account_id = ?1 AND mailbox_id NOT IN ({placeholders})"
        );
        tx.execute(&sql, param_refs.as_slice())
            .map_err(|e| format!("Cache cascade error: {e}"))?;

        let sql = format!(
            "DELETE FROM folders WHERE account_id = ?1 AND mailbox_id NOT IN ({placeholders})"
        );
        tx.execute(&sql, param_refs.as_slice())
            .map_err(|e| format!("Cache delete error: {e}"))?;
    }

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

    tx.execute("DELETE FROM attachments WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache attachment cleanup error: {e}"))?;
    tx.execute("DELETE FROM messages WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache message cleanup error: {e}"))?;
    tx.execute("DELETE FROM folders WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache folder cleanup error: {e}"))?;
    tx.execute("DELETE FROM sync_state WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("Cache sync_state cleanup error: {e}"))?;

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
}
