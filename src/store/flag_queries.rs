use rusqlite::Connection;

use super::flags::flags_from_u8;

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
        "UPDATE messages SET flags_local = ?1, pending_op = ?2, is_read = ?3, is_starred = ?4,
         pending_op_at = strftime('%s', 'now')
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
         pending_op_at = NULL, is_read = ?2, is_starred = ?3
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
         pending_op_at = NULL,
         is_read = CASE WHEN (flags_server & 1) != 0 THEN 1 ELSE 0 END,
         is_starred = CASE WHEN (flags_server & 2) != 0 THEN 1 ELSE 0 END
         WHERE account_id = ?1 AND email_id = ?2",
        rusqlite::params![account_id, email_id],
    )
    .map_err(|e| format!("Cache revert_pending error: {e}"))?;
    Ok(())
}

/// Expire pending ops older than `max_age_secs` by reverting to server flags.
pub(super) fn do_expire_pending_ops(
    conn: &Connection,
    account_id: &str,
    max_age_secs: i64,
) -> Result<u64, String> {
    let affected = conn
        .execute(
            "UPDATE messages SET
            flags_local = flags_server,
            pending_op = NULL,
            pending_op_at = NULL,
            is_read = CASE WHEN (flags_server & 1) != 0 THEN 1 ELSE 0 END,
            is_starred = CASE WHEN (flags_server & 2) != 0 THEN 1 ELSE 0 END
         WHERE account_id = ?1
           AND pending_op IS NOT NULL
           AND pending_op_at IS NOT NULL
           AND pending_op_at < (strftime('%s', 'now') - ?2)",
            rusqlite::params![account_id, max_age_secs],
        )
        .map_err(|e| format!("Cache expire_pending error: {e}"))?;

    if affected > 0 {
        log::info!("Expired {} stuck pending ops for {}", affected, account_id);
    }
    Ok(affected as u64)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::store::flags::flags_to_u8;
    use crate::store::folder_queries::do_save_folders;
    use crate::store::message_queries::{do_load_messages, do_save_messages};
    use crate::store::schema::{run_migrations, SCHEMA};

    use crate::models::{Folder, MessageSummary};

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
    fn clear_pending_op_applies_server_flags() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")]).expect("save");

        // User marks read (optimistic)
        do_update_flags(&conn, "a", "e1", flags_to_u8(true, false), "mark_read")
            .expect("update flags");

        // Server confirms with read=true, starred=false
        do_clear_pending_op(&conn, "a", "e1", flags_to_u8(true, false)).expect("clear pending");

        let after = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load after");
        assert!(after[0].is_read);
        assert!(!after[0].is_starred);

        // Verify pending_op is cleared by checking that a subsequent sync
        // upsert *does* overwrite flags (no pending_op to protect them)
        let mut server_msg = sample_message("e1", "mb1", "test");
        server_msg.is_read = false; // server says unread now
        do_save_messages(&conn, "a", "mb1", &[server_msg]).expect("sync upsert");

        let final_state = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load final");
        assert!(
            !final_state[0].is_read,
            "without pending_op, sync should overwrite flags"
        );
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

    // -- Pending-op expiry tests --

    #[test]
    fn expire_pending_ops_reverts_old_ops() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        // Insert a message (unread)
        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")]).expect("save");

        // Simulate a pending op set 10 minutes ago
        do_update_flags(&conn, "a", "e1", flags_to_u8(true, false), "mark_read")
            .expect("update flags");
        // Backdate the pending_op_at to 600 seconds ago
        conn.execute(
            "UPDATE messages SET pending_op_at = strftime('%s', 'now') - 600
             WHERE account_id = 'a' AND email_id = 'e1'",
            [],
        )
        .expect("backdate");

        // Expire ops older than 300 seconds
        let expired = do_expire_pending_ops(&conn, "a", 300).expect("expire");
        assert_eq!(expired, 1, "should expire 1 op");

        // Message should be reverted to server flags (unread)
        let msgs = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load");
        assert!(!msgs[0].is_read, "expired op should revert to server flags");
    }

    #[test]
    fn expire_pending_ops_preserves_fresh_ops() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")]).expect("save");

        // Set a pending op (timestamp will be "now")
        do_update_flags(&conn, "a", "e1", flags_to_u8(true, false), "mark_read")
            .expect("update flags");

        // Try to expire with 300s threshold — should not expire fresh ops
        let expired = do_expire_pending_ops(&conn, "a", 300).expect("expire");
        assert_eq!(expired, 0, "fresh ops should not be expired");

        // Message should still show optimistic flags
        let msgs = do_load_messages(&conn, "a", "mb1", 50, 0).expect("load");
        assert!(msgs[0].is_read, "fresh pending op should still be active");
    }

    #[test]
    fn update_flags_sets_pending_op_at() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")]).expect("save");

        do_update_flags(&conn, "a", "e1", flags_to_u8(true, false), "mark_read")
            .expect("update flags");

        // pending_op_at should be set
        let op_at: Option<i64> = conn
            .query_row(
                "SELECT pending_op_at FROM messages WHERE account_id = 'a' AND email_id = 'e1'",
                [],
                |row| row.get(0),
            )
            .expect("query pending_op_at");
        assert!(
            op_at.is_some(),
            "pending_op_at should be set after update_flags"
        );
    }

    #[test]
    fn clear_pending_op_clears_pending_op_at() {
        let conn = setup_conn();
        do_save_folders(&conn, "a", &[sample_folder("mb1")]).expect("save folder");

        do_save_messages(&conn, "a", "mb1", &[sample_message("e1", "mb1", "test")]).expect("save");

        do_update_flags(&conn, "a", "e1", flags_to_u8(true, false), "mark_read")
            .expect("update flags");
        do_clear_pending_op(&conn, "a", "e1", flags_to_u8(true, false)).expect("clear pending");

        let op_at: Option<i64> = conn
            .query_row(
                "SELECT pending_op_at FROM messages WHERE account_id = 'a' AND email_id = 'e1'",
                [],
                |row| row.get(0),
            )
            .expect("query pending_op_at");
        assert!(
            op_at.is_none(),
            "pending_op_at should be cleared after clear_pending_op"
        );
    }
}
