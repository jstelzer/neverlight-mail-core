use rusqlite::Connection;

use super::flags::{flags_from_u8, flags_to_u8};
use crate::models::{AttachmentData, Folder, MessageSummary};

/// Shared row-to-struct mapping for both `do_load_messages` and `do_search`.
///
/// Expects columns in this order:
///   0: email_id, 1: subject, 2: sender, 3: date, 4: timestamp,
///   5: is_read, 6: is_starred, 7: has_attachments, 8: thread_id,
///   9: flags_server, 10: flags_local, 11: pending_op, 12: mailbox_id,
///   13: message_id, 14: in_reply_to, 15: thread_depth, 16: reply_to,
///   17: recipient, 18: account_id
fn row_to_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageSummary> {
    let flags_server: i32 = row.get::<_, Option<i32>>(9)?.unwrap_or(0);
    let flags_local: i32 = row.get::<_, Option<i32>>(10)?.unwrap_or(0);
    let pending_op: Option<String> = row.get(11)?;

    // Dual-truth: if pending_op is set, use flags_local; otherwise flags_server
    let effective_flags = if pending_op.is_some() {
        flags_local as u8
    } else {
        flags_server as u8
    };
    let (is_read, is_starred) = flags_from_u8(effective_flags);

    Ok(MessageSummary {
        account_id: row.get(18)?,
        email_id: row.get(0)?,
        subject: row.get(1)?,
        from: row.get(2)?,
        to: row.get::<_, Option<String>>(17)?.unwrap_or_default(),
        date: row.get(3)?,
        timestamp: row.get(4)?,
        is_read,
        is_starred,
        has_attachments: row.get::<_, i32>(7)? != 0,
        thread_id: row.get(8)?,
        mailbox_id: row.get(12)?,
        message_id: row.get::<_, Option<String>>(13)?.unwrap_or_default(),
        in_reply_to: row.get(14)?,
        thread_depth: row.get::<_, Option<u32>>(15)?.unwrap_or(0),
        reply_to: row.get(16)?,
    })
}

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

pub(super) fn do_save_messages(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
    messages: &[MessageSummary],
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    // Collect email IDs that have pending ops
    let mut pending_set = std::collections::HashSet::new();
    {
        let mut stmt = tx
            .prepare(
                "SELECT email_id FROM messages
                 WHERE account_id = ?1 AND mailbox_id = ?2 AND pending_op IS NOT NULL",
            )
            .map_err(|e| format!("Cache prepare error: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![account_id, mailbox_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| format!("Cache query error: {e}"))?;
        for id in rows.flatten() {
            pending_set.insert(id);
        }
    }

    // Cascade: delete attachments for non-pending messages
    tx.execute(
        "DELETE FROM attachments WHERE account_id = ?1 AND email_id IN (
            SELECT email_id FROM messages
            WHERE account_id = ?1 AND mailbox_id = ?2 AND pending_op IS NULL
        )",
        rusqlite::params![account_id, mailbox_id],
    )
    .map_err(|e| format!("Cache attachment cascade error: {e}"))?;

    // Delete non-pending messages for this mailbox
    tx.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND mailbox_id = ?2 AND pending_op IS NULL",
        rusqlite::params![account_id, mailbox_id],
    )
    .map_err(|e| format!("Cache delete error: {e}"))?;

    let mut stmt = tx
        .prepare(
            "INSERT OR IGNORE INTO messages
             (account_id, email_id, mailbox_id, subject, sender, date, timestamp,
              is_read, is_starred, has_attachments, thread_id, flags_server, flags_local,
              message_id, in_reply_to, thread_depth, reply_to, recipient)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    let mut update_server_stmt = tx
        .prepare(
            "UPDATE messages SET flags_server = ?1, subject = ?2, sender = ?3,
             date = ?4, timestamp = ?5, has_attachments = ?6, thread_id = ?7,
             message_id = ?8, in_reply_to = ?9, thread_depth = ?10, reply_to = ?11,
             recipient = ?12
             WHERE account_id = ?13 AND email_id = ?14 AND pending_op IS NOT NULL",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    for m in messages {
        let server_flags = flags_to_u8(m.is_read, m.is_starred);

        if pending_set.contains(&m.email_id) {
            update_server_stmt
                .execute(rusqlite::params![
                    server_flags as i32,
                    m.subject,
                    m.from,
                    m.date,
                    m.timestamp,
                    m.has_attachments as i32,
                    m.thread_id,
                    m.message_id,
                    m.in_reply_to,
                    m.thread_depth,
                    m.reply_to,
                    m.to,
                    account_id,
                    m.email_id,
                ])
                .map_err(|e| format!("Cache update error: {e}"))?;
        } else {
            stmt.execute(rusqlite::params![
                account_id,
                m.email_id,
                mailbox_id,
                m.subject,
                m.from,
                m.date,
                m.timestamp,
                m.is_read as i32,
                m.is_starred as i32,
                m.has_attachments as i32,
                m.thread_id,
                server_flags as i32,
                server_flags as i32,
                m.message_id,
                m.in_reply_to,
                m.thread_depth,
                m.reply_to,
                m.to,
            ])
            .map_err(|e| format!("Cache insert error: {e}"))?;
        }
    }
    drop(stmt);
    drop(update_server_stmt);

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

pub(super) fn do_load_messages(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
    limit: u32,
    offset: u32,
) -> Result<Vec<MessageSummary>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT email_id, subject, sender, date, timestamp,
                    is_read, is_starred, has_attachments, thread_id,
                    flags_server, flags_local, pending_op, mailbox_id,
                    message_id, in_reply_to, thread_depth, reply_to, recipient,
                    account_id
             FROM messages
             WHERE mailbox_id = ?1 AND account_id = ?4
             ORDER BY
                 MAX(timestamp) OVER (
                     PARTITION BY COALESCE(thread_id, email_id)
                 ) DESC,
                 COALESCE(thread_id, email_id),
                 timestamp ASC
             LIMIT ?2 OFFSET ?3",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    let rows = stmt
        .query_map(
            rusqlite::params![mailbox_id, limit, offset, account_id],
            row_to_summary,
        )
        .map_err(|e| format!("Cache query error: {e}"))?;

    let mut messages = Vec::new();
    for row in rows {
        messages.push(row.map_err(|e| format!("Cache row error: {e}"))?);
    }
    Ok(messages)
}

pub(super) fn do_load_body(
    conn: &Connection,
    account_id: &str,
    email_id: &str,
) -> Result<Option<(String, String, Vec<AttachmentData>)>, String> {
    let row_result = conn.query_row(
        "SELECT body_rendered, body_markdown FROM messages WHERE account_id = ?1 AND email_id = ?2",
        rusqlite::params![account_id, email_id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        },
    );

    let (body_plain, body_markdown) = match row_result {
        Ok((Some(plain), md)) => (plain, md.unwrap_or_default()),
        Ok((None, _)) => return Ok(None),
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(format!("Cache body load error: {e}")),
    };

    let mut stmt = conn
        .prepare(
            "SELECT idx, filename, mime_type, data FROM attachments
             WHERE account_id = ?1 AND email_id = ?2 ORDER BY idx",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    let rows = stmt
        .query_map(rusqlite::params![account_id, email_id], |row| {
            Ok(AttachmentData {
                filename: row.get(1)?,
                mime_type: row.get(2)?,
                data: row.get(3)?,
            })
        })
        .map_err(|e| format!("Cache query error: {e}"))?;

    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row.map_err(|e| format!("Cache row error: {e}"))?);
    }

    Ok(Some((body_markdown, body_plain, attachments)))
}

pub(super) fn do_save_body(
    conn: &Connection,
    account_id: &str,
    email_id: &str,
    body_markdown: &str,
    body_plain: &str,
    attachments: &[AttachmentData],
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    tx.execute(
        "UPDATE messages SET body_rendered = ?1, body_markdown = ?2
         WHERE account_id = ?3 AND email_id = ?4",
        rusqlite::params![body_plain, body_markdown, account_id, email_id],
    )
    .map_err(|e| format!("Cache body save error: {e}"))?;

    tx.execute(
        "DELETE FROM attachments WHERE account_id = ?1 AND email_id = ?2",
        rusqlite::params![account_id, email_id],
    )
    .map_err(|e| format!("Cache attachment delete error: {e}"))?;

    let mut stmt = tx
        .prepare(
            "INSERT INTO attachments (account_id, email_id, idx, filename, mime_type, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    for (i, att) in attachments.iter().enumerate() {
        stmt.execute(rusqlite::params![
            account_id,
            email_id,
            i as i32,
            att.filename,
            att.mime_type,
            att.data,
        ])
        .map_err(|e| format!("Cache attachment insert error: {e}"))?;
    }
    drop(stmt);

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

// -- Dual-truth flag operations --------------------------------

pub(super) fn do_update_flags(
    conn: &Connection,
    account_id: &str,
    email_id: &str,
    flags_local: u8,
    pending_op: &str,
) -> Result<(), String> {
    let (is_read, is_starred) = flags_from_u8(flags_local);
    conn.execute(
        "UPDATE messages SET flags_local = ?1, pending_op = ?2, is_read = ?3, is_starred = ?4
         WHERE account_id = ?5 AND email_id = ?6",
        rusqlite::params![
            flags_local as i32,
            pending_op,
            is_read as i32,
            is_starred as i32,
            account_id,
            email_id,
        ],
    )
    .map_err(|e| format!("Cache update_flags error: {e}"))?;
    Ok(())
}

pub(super) fn do_clear_pending_op(
    conn: &Connection,
    account_id: &str,
    email_id: &str,
    flags_server: u8,
) -> Result<(), String> {
    let (is_read, is_starred) = flags_from_u8(flags_server);
    conn.execute(
        "UPDATE messages SET flags_server = ?1, flags_local = ?1, pending_op = NULL,
         is_read = ?2, is_starred = ?3
         WHERE account_id = ?4 AND email_id = ?5",
        rusqlite::params![
            flags_server as i32,
            is_read as i32,
            is_starred as i32,
            account_id,
            email_id,
        ],
    )
    .map_err(|e| format!("Cache clear_pending error: {e}"))?;
    Ok(())
}

pub(super) fn do_revert_pending_op(
    conn: &Connection,
    account_id: &str,
    email_id: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE messages SET flags_local = flags_server, pending_op = NULL,
         is_read = CASE WHEN (flags_server & 1) != 0 THEN 1 ELSE 0 END,
         is_starred = CASE WHEN (flags_server & 2) != 0 THEN 1 ELSE 0 END
         WHERE account_id = ?1 AND email_id = ?2",
        rusqlite::params![account_id, email_id],
    )
    .map_err(|e| format!("Cache revert_pending error: {e}"))?;
    Ok(())
}

pub(super) fn do_remove_message(
    conn: &Connection,
    account_id: &str,
    email_id: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM attachments WHERE account_id = ?1 AND email_id = ?2",
        rusqlite::params![account_id, email_id],
    )
    .map_err(|e| format!("Cache attachment cascade error: {e}"))?;

    conn.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND email_id = ?2",
        rusqlite::params![account_id, email_id],
    )
    .map_err(|e| format!("Cache remove_message error: {e}"))?;
    Ok(())
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

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

pub(super) fn do_search(conn: &Connection, query: &str) -> Result<Vec<MessageSummary>, String> {
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
                    m.account_id
             FROM messages m
             WHERE m.rowid IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?1)
             ORDER BY m.timestamp DESC
             LIMIT 200",
        )
        .map_err(|e| format!("Search prepare error: {e}"))?;

    let rows = stmt
        .query_map([&fts_query], row_to_summary)
        .map_err(|e| format!("Search query error: {e}"))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| format!("Search row error: {e}"))?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{
        do_load_body, do_load_folders, do_load_messages, do_remove_message, do_save_body,
        do_save_folders, do_save_messages, do_update_flags,
    };
    use crate::models::{AttachmentData, Folder, MessageSummary};
    use crate::store::flags::flags_to_u8;
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
            mailbox_id: mailbox_id.to_string(),
            timestamp: 100,
            message_id: format!("<{}@example.com>", email_id),
            in_reply_to: None,
            reply_to: None,
            thread_depth: 0,
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
    fn messages_bodies_flags_and_removal_are_isolated_per_account() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder a");
        do_save_folders(&conn, "b", &[sample_folder("mb1")]).expect("save folder b");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e42", "mb1", "subject-a")])
            .expect("save messages a");
        do_save_messages(&conn, "b", "mb1", &[sample_message("e42", "mb1", "subject-b")])
            .expect("save messages b");

        let a_before = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load a before");
        let b_before = do_load_messages(&conn, "b", "mb1", 50, 0).expect("load b before");
        assert_eq!(a_before[0].subject, "subject-a");
        assert_eq!(b_before[0].subject, "subject-b");

        do_save_body(
            &conn,
            "a",
            "e42",
            "md body",
            "plain body",
            &[AttachmentData {
                filename: "a.txt".to_string(),
                mime_type: "text/plain".to_string(),
                data: b"hello".to_vec(),
            }],
        )
        .expect("save body a");

        let a_body = do_load_body(&conn, "a", "e42").expect("load body a");
        let b_body = do_load_body(&conn, "b", "e42").expect("load body b");
        assert!(a_body.is_some());
        assert!(b_body.is_none());

        do_update_flags(&conn, "a", "e42", flags_to_u8(true, true), "pending")
            .expect("update flags a");
        let a_after_flags = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load a flags");
        let b_after_flags = do_load_messages(&conn, "b", "mb1", 50, 0).expect("load b flags");
        assert!(a_after_flags[0].is_read);
        assert!(a_after_flags[0].is_starred);
        assert!(!b_after_flags[0].is_read);
        assert!(!b_after_flags[0].is_starred);

        do_remove_message(&conn, "a", "e42").expect("remove message a");
        let a_after_remove = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load a removed");
        let b_after_remove = do_load_messages(&conn, "b", "mb1", 50, 0).expect("load b removed");
        assert!(a_after_remove.is_empty());
        assert_eq!(b_after_remove.len(), 1);
    }
}
