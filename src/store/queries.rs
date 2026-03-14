use rusqlite::Connection;

use super::flags::{flags_from_u8, flags_to_u8};
use crate::models::{AttachmentData, BackfillProgress, MessageSummary};

/// Shared row-to-struct mapping for both `do_load_messages` and `do_search`.
///
/// Expects columns in this order:
///   0: email_id, 1: subject, 2: sender, 3: date, 4: timestamp,
///   5: is_read, 6: is_starred, 7: has_attachments, 8: thread_id,
///   9: flags_server, 10: flags_local, 11: pending_op, 12: context_mailbox_id,
///   13: message_id, 14: in_reply_to, 15: thread_depth, 16: reply_to,
///   17: recipient, 18: account_id, 19: mailbox_ids (GROUP_CONCAT)
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

pub(super) fn do_save_messages(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
    messages: &[MessageSummary],
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Cache tx error: {e}"))?;

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

// ---------------------------------------------------------------------------
// Backfill progress
// ---------------------------------------------------------------------------

pub(super) fn do_get_backfill_progress(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
) -> Result<Option<BackfillProgress>, String> {
    match conn.query_row(
        "SELECT position, total, completed FROM backfill_progress
         WHERE account_id = ?1 AND mailbox_id = ?2",
        rusqlite::params![account_id, mailbox_id],
        |row| {
            let completed_int: i32 = row.get(2)?;
            Ok(BackfillProgress {
                account_id: account_id.to_string(),
                mailbox_id: mailbox_id.to_string(),
                position: row.get(0)?,
                total: row.get(1)?,
                completed: completed_int != 0,
            })
        },
    ) {
        Ok(p) => Ok(Some(p)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("get_backfill_progress error: {e}")),
    }
}

pub(super) fn do_set_backfill_progress(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
    position: u32,
    total: u32,
    completed: bool,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO backfill_progress (account_id, mailbox_id, position, total, completed, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(account_id, mailbox_id) DO UPDATE SET
             position = excluded.position,
             total = excluded.total,
             completed = excluded.completed,
             updated_at = excluded.updated_at",
        rusqlite::params![account_id, mailbox_id, position, total, completed as i32],
    )
    .map_err(|e| format!("set_backfill_progress error: {e}"))?;
    Ok(())
}

pub(super) fn do_list_backfill_progress(
    conn: &Connection,
    account_id: &str,
) -> Result<Vec<BackfillProgress>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT mailbox_id, position, total FROM backfill_progress
             WHERE account_id = ?1 AND completed = 0",
        )
        .map_err(|e| format!("list_backfill_progress prepare error: {e}"))?;

    let rows = stmt
        .query_map([account_id], |row| {
            Ok(BackfillProgress {
                account_id: account_id.to_string(),
                mailbox_id: row.get(0)?,
                position: row.get(1)?,
                total: row.get(2)?,
                completed: false,
            })
        })
        .map_err(|e| format!("list_backfill_progress query error: {e}"))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| format!("list_backfill_progress row error: {e}"))?);
    }
    Ok(results)
}

pub(super) fn do_reset_backfill_progress(
    conn: &Connection,
    account_id: &str,
    mailbox_id: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM backfill_progress WHERE account_id = ?1 AND mailbox_id = ?2",
        rusqlite::params![account_id, mailbox_id],
    )
    .map_err(|e| format!("reset_backfill_progress error: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{
        do_clear_pending_op, do_load_body, do_load_messages, do_load_thread,
        do_prune_mailbox, do_remove_message, do_revert_pending_op,
        do_save_body, do_save_messages, do_search, do_update_flags,
    };
    use crate::models::{AttachmentData, Folder, MessageSummary};
    use crate::store::folder_queries::do_save_folders;
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
        let pruned = do_prune_mailbox(&conn, "a", "mb1", &["e1".into(), "e3".into()])
            .expect("prune");
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

        do_save_messages(&conn, "a", "mb1", &[
            sample_message("e1", "mb1", "msg 1"),
            sample_message("e2", "mb1", "msg 2"),
        ]).expect("save");

        let pruned = do_prune_mailbox(&conn, "a", "mb1", &[]).expect("prune");
        assert_eq!(pruned, 2);

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert!(after.is_empty());
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
    fn prune_does_not_affect_other_mailboxes() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Sent"),
        ]).expect("save folders");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "inbox msg")])
            .expect("save mb1");
        do_save_messages(&conn, "a", "mb2", &[sample_message("e2", "mb2", "sent msg")])
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

        do_save_messages(&conn, "a", "mb1", &[
            sample_message("e1", "mb1", "msg 1"),
            sample_message("e2", "mb1", "msg 2"),
        ]).expect("save");

        let pruned = do_prune_mailbox(&conn, "a", "mb1", &["e1".into(), "e2".into()])
            .expect("prune");
        assert_eq!(pruned, 0);

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert_eq!(after.len(), 2);
    }

    // -- Dual-truth flag invariant tests --

    #[test]
    fn pending_op_preserved_during_sync_upsert() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        // Insert a message (unread, unstarred)
        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")])
            .expect("save");

        // User marks it read+starred (optimistic)
        do_update_flags(&conn, "a", "e1", flags_to_u8(true, true), "mark_read_starred")
            .expect("update flags");

        let mid = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mid");
        assert!(mid[0].is_read, "optimistic read flag should show");
        assert!(mid[0].is_starred, "optimistic star flag should show");

        // Background sync upserts the same message (server still shows unread)
        let server_msg = sample_message("e1", "mb1", "test — updated subject");
        do_save_messages(&conn, "a", "mb1", &[server_msg]).expect("sync upsert");

        // Optimistic flags must survive the sync upsert
        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert_eq!(after[0].subject, "test — updated subject", "subject should update");
        assert!(after[0].is_read, "optimistic read flag must survive sync");
        assert!(after[0].is_starred, "optimistic star flag must survive sync");
    }

    #[test]
    fn clear_pending_op_applies_server_flags() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")])
            .expect("save");

        // User marks read (optimistic)
        do_update_flags(&conn, "a", "e1", flags_to_u8(true, false), "mark_read")
            .expect("update flags");

        // Server confirms with read=true, starred=false
        do_clear_pending_op(&conn, "a", "e1", flags_to_u8(true, false))
            .expect("clear pending");

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert!(after[0].is_read);
        assert!(!after[0].is_starred);

        // Verify pending_op is cleared by checking that a subsequent sync
        // upsert *does* overwrite flags (no pending_op to protect them)
        let mut server_msg = sample_message("e1", "mb1", "test");
        server_msg.is_read = false; // server says unread now
        do_save_messages(&conn, "a", "mb1", &[server_msg]).expect("sync upsert");

        let final_state = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load final");
        assert!(!final_state[0].is_read, "without pending_op, sync should overwrite flags");
    }

    #[test]
    fn revert_pending_op_restores_server_flags() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        // Insert as read
        let mut msg = sample_message("e1", "mb1", "test");
        msg.is_read = true;
        do_save_messages(&conn, "a", "mb1", &[msg]).expect("save");

        // User marks unread (optimistic)
        do_update_flags(&conn, "a", "e1", flags_to_u8(false, false), "mark_unread")
            .expect("update flags");

        let mid = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mid");
        assert!(!mid[0].is_read, "optimistic: should show unread");

        // Server rejects — revert
        do_revert_pending_op(&conn, "a", "e1").expect("revert");

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert!(after[0].is_read, "revert should restore server read flag");
    }

    #[test]
    fn flag_encoding_round_trips_all_combinations() {
        use crate::store::flags::flags_from_u8;

        assert_eq!(flags_to_u8(false, false), 0);
        assert_eq!(flags_to_u8(true, false), 1);
        assert_eq!(flags_to_u8(false, true), 2);
        assert_eq!(flags_to_u8(true, true), 3);

        assert_eq!(flags_from_u8(0), (false, false));
        assert_eq!(flags_from_u8(1), (true, false));
        assert_eq!(flags_from_u8(2), (false, true));
        assert_eq!(flags_from_u8(3), (true, true));

        // Round-trip
        for is_read in [false, true] {
            for is_starred in [false, true] {
                let encoded = flags_to_u8(is_read, is_starred);
                let (r, s) = flags_from_u8(encoded);
                assert_eq!(r, is_read);
                assert_eq!(s, is_starred);
            }
        }
    }

    // -- Body cache tests --

    #[test]
    fn upsert_preserves_cached_body() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")])
            .expect("save");

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
        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Trash"),
        ]).expect("save folders");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "inbox msg")])
            .expect("save mb1 msg");
        do_save_messages(&conn, "a", "mb2", &[sample_message("e2", "mb2", "trash msg")])
            .expect("save mb2 msg");

        // Save a body+attachment for the trash message
        do_save_body(&conn, "a", "e2", "# body", "body", &[AttachmentData {
            filename: "file.txt".into(),
            mime_type: "text/plain".into(),
            data: b"data".to_vec(),
        }]).expect("save body");

        // Now sync folders with only INBOX — Trash is gone
        do_save_folders(&conn, "a", &[sample_folder_named("mb1", "INBOX")])
            .expect("save folders update");

        // Trash message and its attachments should be gone
        let mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2");
        assert!(mb2.is_empty(), "messages in removed folder should be deleted");

        let body = do_load_body(&conn, "a", "e2").expect("load body");
        assert!(body.is_none(), "body of removed message should be gone");

        // Inbox message should be untouched
        let mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1");
        assert_eq!(mb1.len(), 1);
    }

    // -- FTS tests --

    #[test]
    fn fts_finds_updated_subject() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "original subject")])
            .expect("save");

        let hits_before = do_search(&conn, "a","original").expect("search before");
        assert_eq!(hits_before.len(), 1);

        // Update subject via upsert
        let mut updated = sample_message("e1", "mb1", "completely different");
        updated.timestamp = 200;
        do_save_messages(&conn, "a", "mb1", &[updated]).expect("upsert");

        let hits_old = do_search(&conn, "a","original").expect("search old");
        assert!(hits_old.is_empty(), "old subject should not match after update");

        let hits_new = do_search(&conn, "a","completely").expect("search new");
        assert_eq!(hits_new.len(), 1, "new subject should match after update");
    }

    #[test]
    fn fts_removes_deleted_message() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "findable subject")])
            .expect("save");

        let hits_before = do_search(&conn, "a","findable").expect("search before");
        assert_eq!(hits_before.len(), 1);

        do_remove_message(&conn, "a", "e1").expect("remove");

        let hits_after = do_search(&conn, "a","findable").expect("search after");
        assert!(hits_after.is_empty(), "FTS should not find deleted message");
    }

    #[test]
    fn search_prefix_matching() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[
            sample_message("e1", "mb1", "Invoice from Acme Corp"),
        ]).expect("save");

        // Short prefix (< 3 chars) should not get wildcard expansion
        let _hits_short = do_search(&conn, "a","in").expect("search short");
        // FTS5 behavior: "in" won't match "Invoice" without wildcard
        // This is expected — short tokens are kept as-is

        // 3+ char prefix should match via wildcard expansion
        let hits_prefix = do_search(&conn, "a","inv").expect("search prefix");
        assert_eq!(hits_prefix.len(), 1, "prefix 'inv' should match 'Invoice'");

        let hits_full = do_search(&conn, "a","invoice").expect("search full");
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
        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Sent"),
        ]).expect("save folders");

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

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "unique_needle_subject")])
            .expect("save a");
        do_save_messages(&conn, "b", "mb1", &[sample_message("e2", "mb1", "unique_needle_subject")])
            .expect("save b");

        let hits_a = do_search(&conn, "a", "unique_needle").expect("search a");
        assert_eq!(hits_a.len(), 1, "search in account a should return only a's message");
        assert_eq!(hits_a[0].email_id, "e1");

        let hits_b = do_search(&conn, "b", "unique_needle").expect("search b");
        assert_eq!(hits_b.len(), 1, "search in account b should return only b's message");
        assert_eq!(hits_b[0].email_id, "e2");
    }

    // -- Multi-mailbox junction tests --

    #[test]
    fn message_in_multiple_mailboxes_loads_from_both() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Archive"),
        ]).expect("save folders");

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
        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Archive"),
        ]).expect("save folders");

        let mut msg = sample_message("e1", "mb1", "multi-mailbox msg");
        msg.mailbox_ids = vec!["mb1".into(), "mb2".into()];
        do_save_messages(&conn, "a", "mb1", &[msg]).expect("save");

        // Prune mb1 with empty live set — should remove junction row for mb1
        let pruned = do_prune_mailbox(&conn, "a", "mb1", &[]).expect("prune");
        assert_eq!(pruned, 1, "should remove one junction row");

        // Message should be gone from mb1
        let from_mb1 = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load mb1");
        assert!(from_mb1.is_empty(), "message should no longer appear in mb1");

        // Message should still be loadable from mb2
        let from_mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2");
        assert_eq!(from_mb2.len(), 1, "message should still appear in mb2");
        assert_eq!(from_mb2[0].email_id, "e1");
    }

    #[test]
    fn delta_sync_move_removes_junction_row() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[
            sample_folder_named("mb1", "INBOX"),
            sample_folder_named("mb2", "Archive"),
        ]).expect("save folders");

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
        assert!(from_mb1.is_empty(), "message should no longer appear in mb1 after move");

        // Message should still be in mb2
        let from_mb2 = do_load_messages(&conn, "a", "mb2", 50, 0).expect("load mb2 after");
        assert_eq!(from_mb2.len(), 1);
        assert_eq!(from_mb2[0].email_id, "e1");
        assert_eq!(from_mb2[0].mailbox_ids, vec!["mb2".to_string()]);
    }

    // -----------------------------------------------------------------------
    // Backfill progress tests
    // -----------------------------------------------------------------------

    use super::{
        do_get_backfill_progress, do_list_backfill_progress,
        do_reset_backfill_progress, do_set_backfill_progress,
    };

    #[test]
    fn backfill_progress_upsert_and_read() {
        let conn = setup_conn();

        // No prior progress → None
        let none = do_get_backfill_progress(&conn, "a", "mb1").expect("get empty");
        assert!(none.is_none());

        // Set progress
        do_set_backfill_progress(&conn, "a", "mb1", 100, 5000, false).expect("set");
        let p = do_get_backfill_progress(&conn, "a", "mb1").expect("get").expect("should exist");
        assert_eq!(p.position, 100);
        assert_eq!(p.total, 5000);
        assert!(!p.completed);

        // Upsert updates existing row
        do_set_backfill_progress(&conn, "a", "mb1", 200, 5000, false).expect("upsert");
        let p = do_get_backfill_progress(&conn, "a", "mb1").expect("get").expect("should exist");
        assert_eq!(p.position, 200);
    }

    #[test]
    fn list_backfill_returns_incomplete_only() {
        let conn = setup_conn();

        do_set_backfill_progress(&conn, "a", "mb1", 100, 5000, false).expect("set mb1");
        do_set_backfill_progress(&conn, "a", "mb2", 3000, 3000, true).expect("set mb2 completed");
        do_set_backfill_progress(&conn, "a", "mb3", 50, 1000, false).expect("set mb3");

        let incomplete = do_list_backfill_progress(&conn, "a").expect("list");
        assert_eq!(incomplete.len(), 2);
        let ids: Vec<&str> = incomplete.iter().map(|p| p.mailbox_id.as_str()).collect();
        assert!(ids.contains(&"mb1"));
        assert!(ids.contains(&"mb3"));
        assert!(!ids.contains(&"mb2"));
    }

    #[test]
    fn reset_backfill_deletes_row() {
        let conn = setup_conn();

        do_set_backfill_progress(&conn, "a", "mb1", 500, 5000, false).expect("set");
        do_reset_backfill_progress(&conn, "a", "mb1").expect("reset");
        let none = do_get_backfill_progress(&conn, "a", "mb1").expect("get after reset");
        assert!(none.is_none());
    }

    #[test]
    fn backfill_progress_isolated_per_account() {
        let conn = setup_conn();

        do_set_backfill_progress(&conn, "a", "mb1", 100, 5000, false).expect("set a");
        do_set_backfill_progress(&conn, "b", "mb1", 200, 3000, false).expect("set b");

        let a = do_get_backfill_progress(&conn, "a", "mb1").expect("get a").expect("a exists");
        let b = do_get_backfill_progress(&conn, "b", "mb1").expect("get b").expect("b exists");
        assert_eq!(a.position, 100);
        assert_eq!(a.total, 5000);
        assert_eq!(b.position, 200);
        assert_eq!(b.total, 3000);

        // List only returns for the specified account
        let a_list = do_list_backfill_progress(&conn, "a").expect("list a");
        assert_eq!(a_list.len(), 1);
        assert_eq!(a_list[0].account_id, "a");
    }
}
