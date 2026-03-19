use rusqlite::Connection;

use super::flags::{flags_from_u8, flags_to_u8};
use crate::models::MessageSummary;

/// Shared row-to-struct mapping for both `do_load_messages` and `do_search`.
///
/// Expects columns in this order:
///   0: email_id, 1: subject, 2: sender, 3: date, 4: timestamp,
///   5: is_read, 6: is_starred, 7: has_attachments, 8: thread_id,
///   9: flags_server, 10: flags_local, 11: pending_op, 12: context_mailbox_id,
///   13: message_id, 14: in_reply_to, 15: thread_depth, 16: reply_to,
///   17: recipient, 18: account_id, 19: mailbox_ids (GROUP_CONCAT)
pub(super) fn row_to_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageSummary> {
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

    let context_mailbox_id: String = row.get::<_, Option<String>>(12)?.unwrap_or_default();
    let mailbox_ids_csv: Option<String> = row.get(19)?;
    let mailbox_ids: Vec<String> = mailbox_ids_csv
        .map(|csv| csv.split(',').map(|s| s.to_string()).collect())
        .unwrap_or_else(|| {
            // Fallback: if no junction data, use context_mailbox_id
            if context_mailbox_id.is_empty() {
                Vec::new()
            } else {
                vec![context_mailbox_id.clone()]
            }
        });

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
        mailbox_ids,
        context_mailbox_id,
        message_id: row.get::<_, Option<String>>(13)?.unwrap_or_default(),
        in_reply_to: row.get(14)?,
        thread_depth: row.get::<_, Option<u32>>(15)?.unwrap_or(0),
        reply_to: row.get(16)?,
    })
}

/// Inner save-messages logic that operates on an existing transaction.
/// Used by both `do_save_messages` (standalone) and `do_save_messages_and_set_state` (combined).
pub(super) fn save_messages_in_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    mailbox_id: &str,
    messages: &[MessageSummary],
) -> Result<(), String> {
    // Upsert: insert new messages or update existing ones.
    // Messages with a pending_op get server-side fields updated but keep their
    // local flags and pending_op intact. All other messages get a full upsert
    // that preserves cached body data (body_rendered, body_markdown).
    let mut upsert_stmt = tx
        .prepare(
            "INSERT INTO messages
             (account_id, email_id, mailbox_id, subject, sender, date, timestamp,
              is_read, is_starred, has_attachments, thread_id, flags_server, flags_local,
              message_id, in_reply_to, thread_depth, reply_to, recipient)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(account_id, email_id) DO UPDATE SET
                 mailbox_id = excluded.mailbox_id,
                 subject = excluded.subject,
                 sender = excluded.sender,
                 date = excluded.date,
                 timestamp = excluded.timestamp,
                 has_attachments = excluded.has_attachments,
                 thread_id = excluded.thread_id,
                 message_id = excluded.message_id,
                 in_reply_to = excluded.in_reply_to,
                 thread_depth = excluded.thread_depth,
                 reply_to = excluded.reply_to,
                 recipient = excluded.recipient,
                 -- Only update flags when no pending op is in flight
                 flags_server = CASE WHEN messages.pending_op IS NOT NULL
                     THEN messages.flags_server ELSE excluded.flags_server END,
                 flags_local = CASE WHEN messages.pending_op IS NOT NULL
                     THEN messages.flags_local ELSE excluded.flags_local END,
                 is_read = CASE WHEN messages.pending_op IS NOT NULL
                     THEN messages.is_read ELSE excluded.is_read END,
                 is_starred = CASE WHEN messages.pending_op IS NOT NULL
                     THEN messages.is_starred ELSE excluded.is_starred END",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    // Prepared statements for junction table sync
    let mut delete_mbox_stmt = tx
        .prepare("DELETE FROM message_mailboxes WHERE account_id = ?1 AND email_id = ?2")
        .map_err(|e| format!("Cache prepare error: {e}"))?;
    let mut insert_mbox_stmt = tx
        .prepare(
            "INSERT OR IGNORE INTO message_mailboxes (account_id, email_id, mailbox_id)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(|e| format!("Cache prepare error: {e}"))?;

    for m in messages {
        let server_flags = flags_to_u8(m.is_read, m.is_starred);

        upsert_stmt
            .execute(rusqlite::params![
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
            .map_err(|e| format!("Cache upsert error: {e}"))?;

        // Sync junction table: replace all mailbox associations
        delete_mbox_stmt
            .execute(rusqlite::params![account_id, m.email_id])
            .map_err(|e| format!("Cache mbox delete error: {e}"))?;

        if m.mailbox_ids.is_empty() {
            // Fallback: use the mailbox_id parameter (legacy callers)
            insert_mbox_stmt
                .execute(rusqlite::params![account_id, m.email_id, mailbox_id])
                .map_err(|e| format!("Cache mbox insert error: {e}"))?;
        } else {
            for mid in &m.mailbox_ids {
                insert_mbox_stmt
                    .execute(rusqlite::params![account_id, m.email_id, mid])
                    .map_err(|e| format!("Cache mbox insert error: {e}"))?;
            }
        }
    }
    drop(upsert_stmt);
    drop(delete_mbox_stmt);
    drop(insert_mbox_stmt);
    Ok(())
}

/// Set sync state within an existing transaction.
pub(super) fn set_state_in_tx(
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

pub(super) fn do_save_messages(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
    messages: &[MessageSummary],
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;
    save_messages_in_tx(&tx, account_id, mailbox_id, messages)?;
    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

/// Atomic: save messages + set sync state + mark mailbox as populated.
pub(super) fn do_save_messages_and_set_state(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
    messages: &[MessageSummary],
    resource: &str,
    state: &str,
    populated_mailbox_id: &str,
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;
    save_messages_in_tx(&tx, account_id, mailbox_id, messages)?;
    set_state_in_tx(&tx, account_id, resource, state)?;
    // Mark this mailbox as populated
    let populated_key = format!("Populated:{populated_mailbox_id}");
    set_state_in_tx(&tx, account_id, &populated_key, "1")?;
    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(())
}

/// Atomic: remove destroyed emails + save created/updated + set state.
pub(super) fn do_delta_email_batch(
    conn: &Connection,
    account_id: &str,
    remove_ids: &[String],
    save_groups: &[(String, Vec<MessageSummary>)],
    resource: &str,
    state: &str,
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    // Remove destroyed emails
    for id in remove_ids {
        tx.execute(
            "DELETE FROM attachments WHERE account_id = ?1 AND email_id = ?2",
            rusqlite::params![account_id, id],
        )
        .map_err(|e| format!("Cache attachment cascade error: {e}"))?;
        tx.execute(
            "DELETE FROM message_mailboxes WHERE account_id = ?1 AND email_id = ?2",
            rusqlite::params![account_id, id],
        )
        .map_err(|e| format!("Cache junction cascade error: {e}"))?;
        tx.execute(
            "DELETE FROM messages WHERE account_id = ?1 AND email_id = ?2",
            rusqlite::params![account_id, id],
        )
        .map_err(|e| format!("Cache remove_message error: {e}"))?;
    }

    // Save each group of messages
    for (mailbox_id, messages) in save_groups {
        save_messages_in_tx(&tx, account_id, mailbox_id, messages)?;
    }

    // Set state
    set_state_in_tx(&tx, account_id, resource, state)?;

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
            "SELECT m.email_id, m.subject, m.sender, m.date, m.timestamp,
                    m.is_read, m.is_starred, m.has_attachments, m.thread_id,
                    m.flags_server, m.flags_local, m.pending_op, ?1 AS context_mailbox_id,
                    m.message_id, m.in_reply_to, m.thread_depth, m.reply_to, m.recipient,
                    m.account_id,
                    (SELECT GROUP_CONCAT(mm2.mailbox_id) FROM message_mailboxes mm2
                     WHERE mm2.account_id = m.account_id AND mm2.email_id = m.email_id) AS mailbox_ids
             FROM messages m
             JOIN message_mailboxes mm ON mm.account_id = m.account_id AND mm.email_id = m.email_id
             WHERE mm.mailbox_id = ?1 AND m.account_id = ?4
             GROUP BY m.account_id, m.email_id
             ORDER BY
                 MAX(m.timestamp) OVER (
                     PARTITION BY COALESCE(m.thread_id, m.email_id)
                 ) DESC,
                 COALESCE(m.thread_id, m.email_id),
                 m.timestamp ASC
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

/// Remove junction rows for a mailbox that are not in the given set of live email IDs.
/// Then clean up orphaned messages (no remaining mailbox associations).
/// Used after a full sync to prune messages that were deleted/moved server-side.
pub(super) fn do_prune_mailbox(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
    live_email_ids: &[String],
) -> Result<u64, String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

    // Step 1: Remove junction rows for stale messages in this mailbox
    let deleted = if live_email_ids.is_empty() {
        tx.execute(
            "DELETE FROM message_mailboxes WHERE account_id = ?1 AND mailbox_id = ?2",
            rusqlite::params![account_id, mailbox_id],
        )
        .map_err(|e| format!("Cache prune error: {e}"))? as u64
    } else {
        let placeholders: Vec<String> = (0..live_email_ids.len())
            .map(|i| format!("?{}", i + 3))
            .collect();
        let sql = format!(
            "DELETE FROM message_mailboxes WHERE account_id = ?1 AND mailbox_id = ?2 AND email_id NOT IN ({})",
            placeholders.join(", ")
        );

        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        params.push(Box::new(account_id.to_string()));
        params.push(Box::new(mailbox_id.to_string()));
        for id in live_email_ids {
            params.push(Box::new(id.clone()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        tx.execute(&sql, param_refs.as_slice())
            .map_err(|e| format!("Cache prune error: {e}"))? as u64
    };

    // Step 2: Clean up orphaned messages — those with no remaining mailbox associations
    tx.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND email_id NOT IN (
            SELECT email_id FROM message_mailboxes WHERE account_id = ?1
        )",
        rusqlite::params![account_id],
    )
    .map_err(|e| format!("Cache orphan cleanup error: {e}"))?;

    tx.commit()
        .map_err(|e| format!("Cache commit error: {e}"))?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::models::{AttachmentData, Folder, MessageSummary};
    use crate::store::body_queries::{do_load_body, do_save_body};
    use crate::store::flag_queries::do_update_flags;
    use crate::store::flags::flags_to_u8;
    use crate::store::folder_queries::{do_get_state, do_save_folders};
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

    #[test]
    fn messages_bodies_flags_and_removal_are_isolated_per_account() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder a");
        do_save_folders(&conn, "b", &[sample_folder("mb1")]).expect("save folder b");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[sample_message("e42", "mb1", "subject-a")],
        )
        .expect("save messages a");
        do_save_messages(
            &conn,
            "b",
            "mb1",
            &[sample_message("e42", "mb1", "subject-b")],
        )
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

    #[test]
    fn prune_removes_stale_messages() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        let mut m1 = sample_message("e1", "mb1", "msg 1");
        m1.timestamp = 100;
        let mut m2 = sample_message("e2", "mb1", "msg 2");
        m2.timestamp = 200;
        let mut m3 = sample_message("e3", "mb1", "msg 3");
        m3.timestamp = 300;
        do_save_messages(&conn, "a", "mb1", &[m1, m2, m3]).expect("save");

        let before = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load before");
        assert_eq!(before.len(), 3);

        // Server now only has e1 and e3 (e2 was deleted)
        let pruned =
            do_prune_mailbox(&conn, "a", "mb1", &["e1".into(), "e3".into()]).expect("prune");
        assert_eq!(pruned, 1);

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert_eq!(after.len(), 2);
        let ids: Vec<&str> = after.iter().map(|m| m.email_id.as_str()).collect();
        assert!(ids.contains(&"e1"));
        assert!(ids.contains(&"e3"));
        assert!(!ids.contains(&"e2"));
    }

    #[test]
    fn prune_with_empty_live_set_clears_mailbox() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[
                sample_message("e1", "mb1", "msg 1"),
                sample_message("e2", "mb1", "msg 2"),
            ],
        )
        .expect("save");

        let pruned = do_prune_mailbox(&conn, "a", "mb1", &[]).expect("prune");
        assert_eq!(pruned, 2);

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert!(after.is_empty());
    }

    #[test]
    fn prune_does_not_affect_other_mailboxes() {
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

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[sample_message("e1", "mb1", "inbox msg")],
        )
        .expect("save mb1");
        do_save_messages(
            &conn,
            "a",
            "mb2",
            &[sample_message("e2", "mb2", "sent msg")],
        )
        .expect("save mb2");

        // Prune mb1 with empty live set — should not touch mb2
        let pruned = do_prune_mailbox(&conn, "a", "mb1", &[]).expect("prune");
        assert_eq!(pruned, 1);

        let mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1");
        let mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2");
        assert!(mb1.is_empty());
        assert_eq!(mb2.len(), 1);
    }

    #[test]
    fn prune_does_not_affect_other_accounts() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder a");
        do_save_folders(&conn, "b", &[sample_folder("mb1")]).expect("save folder b");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "acct a")])
            .expect("save a");
        do_save_messages(&conn, "b", "mb1", &[sample_message("e1", "mb1", "acct b")])
            .expect("save b");

        // Prune account a — should not touch account b
        let pruned = do_prune_mailbox(&conn, "a", "mb1", &[]).expect("prune");
        assert_eq!(pruned, 1);

        let a = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load a");
        let b = do_load_messages(&conn, "b", "mb1", 50, 0).expect("load b");
        assert!(a.is_empty());
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn prune_noop_when_all_messages_are_live() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[
                sample_message("e1", "mb1", "msg 1"),
                sample_message("e2", "mb1", "msg 2"),
            ],
        )
        .expect("save");

        let pruned =
            do_prune_mailbox(&conn, "a", "mb1", &["e1".into(), "e2".into()]).expect("prune");
        assert_eq!(pruned, 0);

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert_eq!(after.len(), 2);
    }

    #[test]
    fn pending_op_preserved_during_sync_upsert() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        // Insert a message (unread, unstarred)
        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")]).expect("save");

        // User marks it read+starred (optimistic)
        do_update_flags(
            &conn,
            "a",
            "e1",
            flags_to_u8(true, true),
            "mark_read_starred",
        )
        .expect("update flags");

        let mid = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mid");
        assert!(mid[0].is_read, "optimistic read flag should show");
        assert!(mid[0].is_starred, "optimistic star flag should show");

        // Background sync upserts the same message (server still shows unread)
        let server_msg = sample_message("e1", "mb1", "test — updated subject");
        do_save_messages(&conn, "a", "mb1", &[server_msg]).expect("sync upsert");

        // Optimistic flags must survive the sync upsert
        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert_eq!(
            after[0].subject, "test — updated subject",
            "subject should update"
        );
        assert!(after[0].is_read, "optimistic read flag must survive sync");
        assert!(
            after[0].is_starred,
            "optimistic star flag must survive sync"
        );
    }

    // -- Body cache tests --

    #[test]
    fn upsert_preserves_cached_body() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")]).expect("save");

        // Cache a body
        do_save_body(&conn, "a", "e1", "# Hello", "Hello", &[]).expect("save body");

        let body_before = do_load_body(&conn, "a", "e1").expect("load body before");
        assert!(body_before.is_some());

        // Sync upserts the same message (new subject from server)
        let mut updated = sample_message("e1", "mb1", "updated subject");
        updated.timestamp = 200;
        do_save_messages(&conn, "a", "mb1", &[updated]).expect("sync upsert");

        // Body should still be cached
        let body_after = do_load_body(&conn, "a", "e1").expect("load body after");
        assert!(body_after.is_some(), "upsert must not NULL out cached body");
        let (md, _, _) = body_after.unwrap();
        assert_eq!(md, "# Hello");
    }

    // -- Folder cascade tests --

    #[test]
    fn folder_removal_cascades_to_messages_and_attachments() {
        let conn = setup_conn();
        do_save_folders(
            &conn,
            "a",
            &[
                sample_folder_named("mb1", "INBOX"),
                sample_folder_named("mb2", "Trash"),
            ],
        )
        .expect("save folders");

        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[sample_message("e1", "mb1", "inbox msg")],
        )
        .expect("save mb1 msg");
        do_save_messages(
            &conn,
            "a",
            "mb2",
            &[sample_message("e2", "mb2", "trash msg")],
        )
        .expect("save mb2 msg");

        // Save a body+attachment for the trash message
        do_save_body(
            &conn,
            "a",
            "e2",
            "# body",
            "body",
            &[AttachmentData {
                filename: "file.txt".into(),
                mime_type: "text/plain".into(),
                data: b"data".to_vec(),
            }],
        )
        .expect("save body");

        // Now sync folders with only INBOX — Trash is gone
        do_save_folders(&conn, "a", &[sample_folder_named("mb1", "INBOX")])
            .expect("save folders update");

        // Trash message and its attachments should be gone
        let mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2");
        assert!(
            mb2.is_empty(),
            "messages in removed folder should be deleted"
        );

        let body = do_load_body(&conn, "a", "e2").expect("load body");
        assert!(body.is_none(), "body of removed message should be gone");

        // Inbox message should be untouched
        let mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1");
        assert_eq!(mb1.len(), 1);
    }

    // -- Multi-mailbox junction tests --

    #[test]
    fn message_in_multiple_mailboxes_loads_from_both() {
        let conn = setup_conn();
        do_save_folders(
            &conn,
            "a",
            &[
                sample_folder_named("mb1", "INBOX"),
                sample_folder_named("mb2", "Archive"),
            ],
        )
        .expect("save folders");

        let mut msg = sample_message("e1", "mb1", "multi-mailbox msg");
        msg.mailbox_ids = vec!["mb1".into(), "mb2".into()];
        do_save_messages(&conn, "a", "mb1", &[msg]).expect("save");

        let from_mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1");
        assert_eq!(from_mb1.len(), 1, "message should appear in mb1");
        assert_eq!(from_mb1[0].email_id, "e1");
        assert_eq!(from_mb1[0].context_mailbox_id, "mb1");
        assert!(from_mb1[0].mailbox_ids.contains(&"mb1".to_string()));
        assert!(from_mb1[0].mailbox_ids.contains(&"mb2".to_string()));

        let from_mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2");
        assert_eq!(from_mb2.len(), 1, "message should appear in mb2");
        assert_eq!(from_mb2[0].email_id, "e1");
        assert_eq!(from_mb2[0].context_mailbox_id, "mb2");
    }

    #[test]
    fn prune_removes_junction_row_not_message() {
        let conn = setup_conn();
        do_save_folders(
            &conn,
            "a",
            &[
                sample_folder_named("mb1", "INBOX"),
                sample_folder_named("mb2", "Archive"),
            ],
        )
        .expect("save folders");

        let mut msg = sample_message("e1", "mb1", "multi-mailbox msg");
        msg.mailbox_ids = vec!["mb1".into(), "mb2".into()];
        do_save_messages(&conn, "a", "mb1", &[msg]).expect("save");

        // Prune mb1 with empty live set — should remove junction row for mb1
        let pruned = do_prune_mailbox(&conn, "a", "mb1", &[]).expect("prune");
        assert_eq!(pruned, 1, "should remove one junction row");

        // Message should be gone from mb1
        let from_mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1");
        assert!(
            from_mb1.is_empty(),
            "message should no longer appear in mb1"
        );

        // Message should still be loadable from mb2
        let from_mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2");
        assert_eq!(from_mb2.len(), 1, "message should still appear in mb2");
        assert_eq!(from_mb2[0].email_id, "e1");
    }

    #[test]
    fn delta_sync_move_removes_junction_row() {
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

        // Message starts in both mb1 and mb2
        let mut msg = sample_message("e1", "mb1", "will be moved");
        msg.mailbox_ids = vec!["mb1".into(), "mb2".into()];
        do_save_messages(&conn, "a", "mb1", &[msg]).expect("save initial");

        let from_mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1 before");
        assert_eq!(from_mb1.len(), 1);

        // Simulate delta sync: message updated to only be in mb2
        let mut moved = sample_message("e1", "mb2", "will be moved");
        moved.mailbox_ids = vec!["mb2".into()];
        do_save_messages(&conn, "a", "mb2", &[moved]).expect("save after move");

        // Message should be gone from mb1
        let from_mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1 after");
        assert!(
            from_mb1.is_empty(),
            "message should no longer appear in mb1 after move"
        );

        // Message should still be in mb2
        let from_mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2 after");
        assert_eq!(from_mb2.len(), 1);
        assert_eq!(from_mb2[0].email_id, "e1");
        assert_eq!(from_mb2[0].mailbox_ids, vec!["mb2".to_string()]);
    }

    // -- Atomic write + state tests --

    #[test]
    fn save_messages_and_set_state_is_atomic() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        let msg = sample_message("e1", "mb1", "atomic test");
        do_save_messages_and_set_state(&conn, "a", "mb1", &[msg], "Email", "s42", "mb1")
            .expect("atomic save");

        // Both message and state should be present
        let msgs = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].subject, "atomic test");

        let state = do_get_state(&conn, "a", "Email").expect("get state");
        assert_eq!(state.as_deref(), Some("s42"));

        // Populated flag should be set
        let populated = do_get_state(&conn, "a", "Populated:mb1").expect("get populated");
        assert!(
            populated.is_some(),
            "populated flag should be set after head sync"
        );
    }

    #[test]
    fn delta_email_batch_removes_and_saves_atomically() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        // Insert two messages
        do_save_messages(
            &conn,
            "a",
            "mb1",
            &[
                sample_message("e1", "mb1", "msg 1"),
                sample_message("e2", "mb1", "msg 2"),
            ],
        )
        .expect("save initial");

        // Delta: remove e1, add e3, set state
        let new_msg = sample_message("e3", "mb1", "msg 3");
        do_delta_email_batch(
            &conn,
            "a",
            &["e1".to_string()],
            &[("mb1".to_string(), vec![new_msg])],
            "Email",
            "s99",
        )
        .expect("delta batch");

        let msgs = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load");
        let ids: Vec<&str> = msgs.iter().map(|m| m.email_id.as_str()).collect();
        assert!(!ids.contains(&"e1"), "e1 should be removed");
        assert!(ids.contains(&"e2"), "e2 should survive");
        assert!(ids.contains(&"e3"), "e3 should be added");

        let state = do_get_state(&conn, "a", "Email").expect("get state");
        assert_eq!(state.as_deref(), Some("s99"));
    }

    #[test]
    fn delta_batch_saves_cross_mailbox_messages() {
        // Simulates sync_emails_delta for Inbox discovering new mail in Sent.
        // Email/changes is account-global, so the delta batch must fan out
        // created/updated messages to ALL their mailboxes, not just the active one.
        let conn = setup_conn();
        do_save_folders(
            &conn,
            "a",
            &[
                sample_folder_named("inbox", "INBOX"),
                sample_folder_named("sent", "Sent"),
            ],
        )
        .expect("save folders");

        // Seed Inbox with one message + set Email state
        do_save_messages_and_set_state(
            &conn,
            "a",
            "inbox",
            &[sample_message("e1", "inbox", "existing inbox msg")],
            "Email",
            "s5",
            "inbox",
        )
        .expect("seed inbox");

        // Mark Sent as populated (simulates a prior head sync)
        conn.execute(
            "INSERT INTO sync_state (account_id, resource, state) VALUES ('a', 'Populated:sent', '1')",
            [],
        ).expect("mark sent populated");

        // Delta batch from Inbox sync: new message arrived in Sent, not Inbox.
        // This is the save_groups pattern from sync_emails_delta.
        let mut sent_msg = sample_message("e2", "sent", "new mail in sent");
        sent_msg.mailbox_ids = vec!["sent".into()];
        do_delta_email_batch(
            &conn,
            "a",
            &[],
            &[("sent".to_string(), vec![sent_msg])],
            "Email",
            "s6",
        )
        .expect("delta batch");

        // New message should be loadable from Sent
        let from_sent = do_load_messages(&conn, "a", "sent", 50, 0).expect("load sent");
        assert_eq!(from_sent.len(), 1);
        assert_eq!(from_sent[0].email_id, "e2");
        assert_eq!(from_sent[0].subject, "new mail in sent");

        // Inbox should still have its original message
        let from_inbox = do_load_messages(&conn, "a", "inbox", 50, 0).expect("load inbox");
        assert_eq!(from_inbox.len(), 1);
        assert_eq!(from_inbox[0].email_id, "e1");

        // State should have advanced
        let state = do_get_state(&conn, "a", "Email").expect("get state");
        assert_eq!(state.as_deref(), Some("s6"));
    }
}
