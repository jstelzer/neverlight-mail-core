use std::path::PathBuf;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use super::commands::CacheCmd;
use super::folder_queries;
use super::queries;
use super::schema::{run_migrations, SCHEMA};
use crate::models::{AttachmentData, BackfillProgress, Folder, MessageSummary};

// ---------------------------------------------------------------------------
// CacheHandle — Clone + Send + Sync async facade
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CacheHandle {
    tx: mpsc::UnboundedSender<CacheCmd>,
}

impl CacheHandle {
    /// Open (or create) the cache database and spawn the background thread.
    pub fn open(app_id: &str) -> Result<Self, String> {
        let db_path = Self::resolve_path()?;

        std::fs::create_dir_all(&db_path)
            .map_err(|e| format!("Failed to create cache dir: {e}"))?;

        let db_file = db_path.join(format!("cache-{app_id}.db"));
        let conn =
            Connection::open(&db_file).map_err(|e| format!("Failed to open cache db: {e}"))?;

        // Enable foreign key enforcement so ON DELETE CASCADE works
        // (e.g., pruning messages cascades to attachments).
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| format!("Failed to enable foreign keys: {e}"))?;

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
        mailbox_id: String,
        messages: Vec<MessageSummary>,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SaveMessages {
                account_id,
                mailbox_id,
                messages,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn load_messages(
        &self,
        account_id: String,
        mailbox_id: String,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<MessageSummary>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::LoadMessages {
                account_id,
                mailbox_id,
                limit,
                offset,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Load all messages in a thread across the given mailbox IDs, sorted by timestamp ASC.
    pub async fn load_thread(
        &self,
        account_id: String,
        thread_id: String,
        mailbox_ids: Vec<String>,
    ) -> Result<Vec<MessageSummary>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::LoadThread {
                account_id,
                thread_id,
                mailbox_ids,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn load_body(
        &self,
        account_id: String,
        email_id: String,
    ) -> Result<Option<(String, String, Vec<AttachmentData>)>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::LoadBody {
                account_id,
                email_id,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn save_body(
        &self,
        account_id: String,
        email_id: String,
        body_markdown: String,
        body_plain: String,
        attachments: Vec<AttachmentData>,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SaveBody {
                account_id,
                email_id,
                body_markdown,
                body_plain,
                attachments,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn update_flags(
        &self,
        account_id: String,
        email_id: String,
        flags_local: u8,
        pending_op: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::UpdateFlags {
                account_id,
                email_id,
                flags_local,
                pending_op,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn clear_pending_op(
        &self,
        account_id: String,
        email_id: String,
        flags_server: u8,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::ClearPendingOp {
                account_id,
                email_id,
                flags_server,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn revert_pending_op(
        &self,
        account_id: String,
        email_id: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::RevertPendingOp {
                account_id,
                email_id,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn prune_mailbox(
        &self,
        account_id: String,
        mailbox_id: String,
        live_email_ids: Vec<String>,
    ) -> Result<u64, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::PruneMailbox {
                account_id,
                mailbox_id,
                live_email_ids,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn remove_message(
        &self,
        account_id: String,
        email_id: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::RemoveMessage {
                account_id,
                email_id,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Upsert folders without pruning absent ones (delta sync).
    pub async fn upsert_folders(
        &self,
        account_id: String,
        folders: Vec<Folder>,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::UpsertFolders {
                account_id,
                folders,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Remove specific folders by mailbox ID, cascading to messages and attachments.
    pub async fn remove_folders(
        &self,
        account_id: String,
        mailbox_ids: Vec<String>,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::RemoveFolders {
                account_id,
                mailbox_ids,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn remove_account(&self, account_id: String) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::RemoveAccount { account_id, reply })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn get_state(
        &self,
        account_id: String,
        resource: String,
    ) -> Result<Option<String>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::GetState {
                account_id,
                resource,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn set_state(
        &self,
        account_id: String,
        resource: String,
        state: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SetState {
                account_id,
                resource,
                state,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn get_backfill_progress(
        &self,
        account_id: String,
        mailbox_id: String,
    ) -> Result<Option<BackfillProgress>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::GetBackfillProgress {
                account_id,
                mailbox_id,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn set_backfill_progress(
        &self,
        account_id: String,
        mailbox_id: String,
        position: u32,
        total: u32,
        completed: bool,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SetBackfillProgress {
                account_id,
                mailbox_id,
                position,
                total,
                completed,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn list_backfill_progress(
        &self,
        account_id: String,
    ) -> Result<Vec<BackfillProgress>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::ListBackfillProgress {
                account_id,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn reset_backfill_progress(
        &self,
        account_id: String,
        mailbox_id: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::ResetBackfillProgress {
                account_id,
                mailbox_id,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Atomic: save folders + set sync state in one transaction.
    pub async fn save_folders_and_set_state(
        &self,
        account_id: String,
        folders: Vec<Folder>,
        resource: String,
        state: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SaveFoldersAndSetState {
                account_id,
                folders,
                resource,
                state,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Atomic: upsert + remove folders + set sync state.
    pub async fn delta_folders_and_set_state(
        &self,
        account_id: String,
        upsert: Vec<Folder>,
        remove_ids: Vec<String>,
        resource: String,
        state: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::DeltaFoldersAndSetState {
                account_id,
                upsert,
                remove_ids,
                resource,
                state,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Atomic: save messages + set sync state + mark mailbox as populated.
    pub async fn save_messages_and_set_state(
        &self,
        account_id: String,
        mailbox_id: String,
        messages: Vec<MessageSummary>,
        resource: String,
        state: String,
        populated_mailbox_id: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::SaveMessagesAndSetState {
                account_id,
                mailbox_id,
                messages,
                resource,
                state,
                populated_mailbox_id,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Atomic: remove destroyed + save created/updated + set state.
    pub async fn delta_email_batch(
        &self,
        account_id: String,
        remove_ids: Vec<String>,
        save_groups: Vec<(String, Vec<MessageSummary>)>,
        resource: String,
        state: String,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::DeltaEmailBatch {
                account_id,
                remove_ids,
                save_groups,
                resource,
                state,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    /// Expire pending ops older than `max_age_secs`.
    pub async fn expire_pending_ops(
        &self,
        account_id: String,
        max_age_secs: i64,
    ) -> Result<u64, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::ExpirePendingOps {
                account_id,
                max_age_secs,
                reply,
            })
            .map_err(|_| "Cache unavailable".to_string())?;
        rx.await.map_err(|_| "Cache unavailable".to_string())?
    }

    pub async fn search(
        &self,
        account_id: String,
        query: String,
    ) -> Result<Vec<MessageSummary>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(CacheCmd::Search {
                account_id,
                query,
                reply,
            })
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
                let _ = reply.send(folder_queries::do_save_folders(&conn, &account_id, &folders));
            }
            CacheCmd::LoadFolders { account_id, reply } => {
                let _ = reply.send(folder_queries::do_load_folders(&conn, &account_id));
            }
            CacheCmd::SaveMessages {
                account_id,
                mailbox_id,
                messages,
                reply,
            } => {
                let _ = reply.send(queries::do_save_messages(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    &messages,
                ));
            }
            CacheCmd::LoadMessages {
                account_id,
                mailbox_id,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(queries::do_load_messages(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    limit,
                    offset,
                ));
            }
            CacheCmd::LoadBody {
                account_id,
                email_id,
                reply,
            } => {
                let _ = reply.send(queries::do_load_body(&conn, &account_id, &email_id));
            }
            CacheCmd::SaveBody {
                account_id,
                email_id,
                body_markdown,
                body_plain,
                attachments,
                reply,
            } => {
                let _ = reply.send(queries::do_save_body(
                    &conn,
                    &account_id,
                    &email_id,
                    &body_markdown,
                    &body_plain,
                    &attachments,
                ));
            }
            CacheCmd::UpdateFlags {
                account_id,
                email_id,
                flags_local,
                pending_op,
                reply,
            } => {
                let _ = reply.send(queries::do_update_flags(
                    &conn,
                    &account_id,
                    &email_id,
                    flags_local,
                    &pending_op,
                ));
            }
            CacheCmd::ClearPendingOp {
                account_id,
                email_id,
                flags_server,
                reply,
            } => {
                let _ = reply.send(queries::do_clear_pending_op(
                    &conn,
                    &account_id,
                    &email_id,
                    flags_server,
                ));
            }
            CacheCmd::RevertPendingOp {
                account_id,
                email_id,
                reply,
            } => {
                let _ = reply.send(queries::do_revert_pending_op(
                    &conn,
                    &account_id,
                    &email_id,
                ));
            }
            CacheCmd::RemoveMessage {
                account_id,
                email_id,
                reply,
            } => {
                let _ = reply.send(queries::do_remove_message(&conn, &account_id, &email_id));
            }
            CacheCmd::PruneMailbox {
                account_id,
                mailbox_id,
                live_email_ids,
                reply,
            } => {
                let _ = reply.send(queries::do_prune_mailbox(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    &live_email_ids,
                ));
            }
            CacheCmd::Search {
                account_id,
                query,
                reply,
            } => {
                let _ = reply.send(queries::do_search(&conn, &account_id, &query));
            }
            CacheCmd::LoadThread {
                account_id,
                thread_id,
                mailbox_ids,
                reply,
            } => {
                let _ = reply.send(queries::do_load_thread(
                    &conn,
                    &account_id,
                    &thread_id,
                    &mailbox_ids,
                ));
            }
            CacheCmd::UpsertFolders {
                account_id,
                folders,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_upsert_folders(&conn, &account_id, &folders));
            }
            CacheCmd::RemoveFolders {
                account_id,
                mailbox_ids,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_remove_folders_by_id(
                    &conn,
                    &account_id,
                    &mailbox_ids,
                ));
            }
            CacheCmd::RemoveAccount { account_id, reply } => {
                let _ = reply.send(folder_queries::do_remove_account(&conn, &account_id));
            }
            CacheCmd::GetState {
                account_id,
                resource,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_get_state(&conn, &account_id, &resource));
            }
            CacheCmd::SetState {
                account_id,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_set_state(&conn, &account_id, &resource, &state));
            }
            CacheCmd::GetBackfillProgress {
                account_id,
                mailbox_id,
                reply,
            } => {
                let _ = reply.send(queries::do_get_backfill_progress(&conn, &account_id, &mailbox_id));
            }
            CacheCmd::SetBackfillProgress {
                account_id,
                mailbox_id,
                position,
                total,
                completed,
                reply,
            } => {
                let _ = reply.send(queries::do_set_backfill_progress(
                    &conn, &account_id, &mailbox_id, position, total, completed,
                ));
            }
            CacheCmd::ListBackfillProgress {
                account_id,
                reply,
            } => {
                let _ = reply.send(queries::do_list_backfill_progress(&conn, &account_id));
            }
            CacheCmd::ResetBackfillProgress {
                account_id,
                mailbox_id,
                reply,
            } => {
                let _ = reply.send(queries::do_reset_backfill_progress(&conn, &account_id, &mailbox_id));
            }
            CacheCmd::SaveFoldersAndSetState {
                account_id,
                folders,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_save_folders_and_set_state(
                    &conn, &account_id, &folders, &resource, &state,
                ));
            }
            CacheCmd::DeltaFoldersAndSetState {
                account_id,
                upsert,
                remove_ids,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_delta_folders_and_set_state(
                    &conn, &account_id, &upsert, &remove_ids, &resource, &state,
                ));
            }
            CacheCmd::SaveMessagesAndSetState {
                account_id,
                mailbox_id,
                messages,
                resource,
                state,
                populated_mailbox_id,
                reply,
            } => {
                let _ = reply.send(queries::do_save_messages_and_set_state(
                    &conn, &account_id, &mailbox_id, &messages, &resource, &state, &populated_mailbox_id,
                ));
            }
            CacheCmd::DeltaEmailBatch {
                account_id,
                remove_ids,
                save_groups,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(queries::do_delta_email_batch(
                    &conn, &account_id, &remove_ids, &save_groups, &resource, &state,
                ));
            }
            CacheCmd::ExpirePendingOps {
                account_id,
                max_age_secs,
                reply,
            } => {
                let _ = reply.send(queries::do_expire_pending_ops(
                    &conn, &account_id, max_age_secs,
                ));
            }
        }
    }
    log::debug!("Cache thread exiting");
}
