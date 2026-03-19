use rusqlite::Connection;

use crate::models::BackfillProgress;

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

    use super::*;
    use crate::store::schema::{run_migrations, SCHEMA};

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(SCHEMA).expect("create schema");
        run_migrations(&conn);
        conn
    }

    #[test]
    fn backfill_progress_upsert_and_read() {
        let conn = setup_conn();

        // No prior progress -> None
        let none = do_get_backfill_progress(&conn, "a", "mb1").expect("get empty");
        assert!(none.is_none());

        // Set progress
        do_set_backfill_progress(&conn, "a", "mb1", 100, 5000, false).expect("set");
        let p = do_get_backfill_progress(&conn, "a", "mb1")
            .expect("get")
            .expect("should exist");
        assert_eq!(p.position, 100);
        assert_eq!(p.total, 5000);
        assert!(!p.completed);

        // Upsert updates existing row
        do_set_backfill_progress(&conn, "a", "mb1", 200, 5000, false).expect("upsert");
        let p = do_get_backfill_progress(&conn, "a", "mb1")
            .expect("get")
            .expect("should exist");
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

        let a = do_get_backfill_progress(&conn, "a", "mb1")
            .expect("get a")
            .expect("a exists");
        let b = do_get_backfill_progress(&conn, "b", "mb1")
            .expect("get b")
            .expect("b exists");
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
