use std::path::PathBuf;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use super::commands::CacheCmd;
use super::queries;
use super::schema::{run_migrations, SCHEMA};
use crate::models::{AttachmentData, Folder, MessageSummary};

// ---------------------------------------------------------------------------
// CacheHandle — Clone + Send + Sync async facade
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CacheHandle {
    tx: mpsc::UnboundedSender<CacheCmd>,
}

impl CacheHandle {
    /// Open (or create) the cache database and spawn the background thread.
    /// Each app should pass a unique `app_id` (e.g. "tui", "cosmic") so that
    /// concurrent apps get separate DB files (`cache-tui.db`, `cache-cosmic.db`).
    pub fn open(app_id: &str) -> Result<Self, String> {
        let db_path = Self::resolve_path()?;

        std::fs::create_dir_all(&db_path)
            .map_err(|e| format!("Failed to create cache dir: {e}"))?;

        let db_file = db_path.join(format!("cache-{app_id}.db"));
        let conn =
            Connection::open(&db_file).map_err(|e| format!("Failed to open cache db: {e}"))?;

        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("Failed to init cache schema: {e}"))?;

        run_migrations(&conn);

        let (tx, rx) = mpsc::unbounded_channel();

        std::thread::Builder::new()
            .name("neverlight-mail-cache".into())
            .spawn(move || run_loop(conn, rx))
            .map_err(|e| format!("Failed to spawn cache thread: {e}"))?;

        Ok(CacheHandle { tx })
    }

    fn resolve_path() -> Result<PathBuf, String> {
        let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        Ok(base.join("neverlight-mail"))
    }

    // -- async methods -------------------------------------------------------

    pub async fn save_folders(
        &self,
        account_id: String,
        folders: Vec<Folder>,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SaveFolders {
                account_id,
                folders,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn load_folders(&self, account_id: String) -> Result<Vec<Folder>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::LoadFolders { account_id, reply })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn save_messages(
        &self,
        account_id: String,
        mailbox_hash: u64,
        messages: Vec<MessageSummary>,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SaveMessages {
                account_id,
                mailbox_hash,
                messages,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn load_messages(
        &self,
        account_id: String,
        mailbox_hash: u64,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<MessageSummary>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::LoadMessages {
                account_id,
                mailbox_hash,
                limit,
                offset,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn load_body(
        &self,
        account_id: String,
        envelope_hash: u64,
    ) -> Result<Option<(String, String, Vec<AttachmentData>)>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::LoadBody {
                account_id,
                envelope_hash,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn save_body(
        &self,
        account_id: String,
        envelope_hash: u64,
        body_markdown: String,
        body_plain: String,
        attachments: Vec<AttachmentData>,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SaveBody {
                account_id,
                envelope_hash,
                body_markdown,
                body_plain,
                attachments,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Set local flags and mark a pending operation.
    pub async fn update_flags(
        &self,
        account_id: String,
        envelope_hash: u64,
        flags_local: u8,
        pending_op: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::UpdateFlags {
                account_id,
                envelope_hash,
                flags_local,
                pending_op,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// IMAP op succeeded — update server flags and clear pending.
    pub async fn clear_pending_op(
        &self,
        account_id: String,
        envelope_hash: u64,
        flags_server: u8,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::ClearPendingOp {
                account_id,
                envelope_hash,
                flags_server,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// IMAP op failed — revert local flags to server flags, clear pending.
    pub async fn revert_pending_op(
        &self,
        account_id: String,
        envelope_hash: u64,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::RevertPendingOp {
                account_id,
                envelope_hash,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Remove a message from the cache (after successful move).
    pub async fn remove_message(
        &self,
        account_id: String,
        envelope_hash: u64,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::RemoveMessage {
                account_id,
                envelope_hash,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Remove all cached data for an account (folders, messages, attachments).
    pub async fn remove_account(&self, account_id: String) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::RemoveAccount { account_id, reply })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Full-text search across all folders.
    pub async fn search(&self, query: String) -> Result<Vec<MessageSummary>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::Search { query, reply })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }
}

// -- background thread ---------------------------------------------------

fn run_loop(conn: Connection, mut rx: mpsc::UnboundedReceiver<CacheCmd>) {
    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            CacheCmd::SaveFolders {
                account_id,
                folders,
                reply,
            } => {
                let _ = reply.send(queries::do_save_folders(&conn, &account_id, &folders));
            }
            CacheCmd::LoadFolders { account_id, reply } => {
                let _ = reply.send(queries::do_load_folders(&conn, &account_id));
            }
            CacheCmd::SaveMessages {
                account_id,
                mailbox_hash,
                messages,
                reply,
            } => {
                let _ = reply.send(queries::do_save_messages(
                    &conn,
                    &account_id,
                    mailbox_hash,
                    &messages,
                ));
            }
            CacheCmd::LoadMessages {
                account_id,
                mailbox_hash,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(queries::do_load_messages(
                    &conn,
                    &account_id,
                    mailbox_hash,
                    limit,
                    offset,
                ));
            }
            CacheCmd::LoadBody {
                account_id,
                envelope_hash,
                reply,
            } => {
                let _ = reply.send(queries::do_load_body(&conn, &account_id, envelope_hash));
            }
            CacheCmd::SaveBody {
                account_id,
                envelope_hash,
                body_markdown,
                body_plain,
                attachments,
                reply,
            } => {
                let _ = reply.send(queries::do_save_body(
                    &conn,
                    &account_id,
                    envelope_hash,
                    &body_markdown,
                    &body_plain,
                    &attachments,
                ));
            }
            CacheCmd::UpdateFlags {
                account_id,
                envelope_hash,
                flags_local,
                pending_op,
                reply,
            } => {
                let _ = reply.send(queries::do_update_flags(
                    &conn,
                    &account_id,
                    envelope_hash,
                    flags_local,
                    &pending_op,
                ));
            }
            CacheCmd::ClearPendingOp {
                account_id,
                envelope_hash,
                flags_server,
                reply,
            } => {
                let _ = reply.send(queries::do_clear_pending_op(
                    &conn,
                    &account_id,
                    envelope_hash,
                    flags_server,
                ));
            }
            CacheCmd::RevertPendingOp {
                account_id,
                envelope_hash,
                reply,
            } => {
                let _ = reply.send(queries::do_revert_pending_op(
                    &conn,
                    &account_id,
                    envelope_hash,
                ));
            }
            CacheCmd::RemoveMessage {
                account_id,
                envelope_hash,
                reply,
            } => {
                let _ = reply.send(queries::do_remove_message(
                    &conn,
                    &account_id,
                    envelope_hash,
                ));
            }
            CacheCmd::Search { query, reply } => {
                let _ = reply.send(queries::do_search(&conn, &query));
            }
            CacheCmd::RemoveAccount { account_id, reply } => {
                let _ = reply.send(queries::do_remove_account(&conn, &account_id));
            }
        }
    }
    log::debug!("Cache thread exiting");
}
