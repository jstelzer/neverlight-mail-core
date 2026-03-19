use tokio::sync::oneshot;

use crate::models::{AttachmentData, BackfillProgress, Folder, MessageSummary};

#[allow(clippy::type_complexity)]
pub(super) enum CacheCmd {
    SaveFolders {
        account_id: String,
        folders: Vec<Folder>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    LoadFolders {
        account_id: String,
        reply: oneshot::Sender<Result<Vec<Folder>, String>>,
    },
    SaveMessages {
        account_id: String,
        mailbox_id: String,
        messages: Vec<MessageSummary>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    LoadMessages {
        account_id: String,
        mailbox_id: String,
        limit: u32,
        offset: u32,
        reply: oneshot::Sender<Result<Vec<MessageSummary>, String>>,
    },
    LoadBody {
        account_id: String,
        email_id: String,
        reply: oneshot::Sender<Result<Option<(String, String, Vec<AttachmentData>)>, String>>,
    },
    SaveBody {
        account_id: String,
        email_id: String,
        body_markdown: String,
        body_plain: String,
        attachments: Vec<AttachmentData>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    UpdateFlags {
        account_id: String,
        email_id: String,
        flags_local: u8,
        pending_op: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ClearPendingOp {
        account_id: String,
        email_id: String,
        flags_server: u8,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RevertPendingOp {
        account_id: String,
        email_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveMessage {
        account_id: String,
        email_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    PruneMailbox {
        account_id: String,
        mailbox_id: String,
        live_email_ids: Vec<String>,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Search {
        account_id: String,
        query: String,
        reply: oneshot::Sender<Result<Vec<MessageSummary>, String>>,
    },
    LoadThread {
        account_id: String,
        thread_id: String,
        mailbox_ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<MessageSummary>, String>>,
    },
    UpsertFolders {
        account_id: String,
        folders: Vec<Folder>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveFolders {
        account_id: String,
        mailbox_ids: Vec<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveAccount {
        account_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GetState {
        account_id: String,
        resource: String,
        reply: oneshot::Sender<Result<Option<String>, String>>,
    },
    SetState {
        account_id: String,
        resource: String,
        state: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GetBackfillProgress {
        account_id: String,
        mailbox_id: String,
        reply: oneshot::Sender<Result<Option<BackfillProgress>, String>>,
    },
    SetBackfillProgress {
        account_id: String,
        mailbox_id: String,
        position: u32,
        total: u32,
        completed: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ListBackfillProgress {
        account_id: String,
        reply: oneshot::Sender<Result<Vec<BackfillProgress>, String>>,
    },
    ResetBackfillProgress {
        account_id: String,
        mailbox_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Atomic: save folders + set sync state in one transaction.
    SaveFoldersAndSetState {
        account_id: String,
        folders: Vec<Folder>,
        resource: String,
        state: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Atomic: upsert + remove folders + set sync state in one transaction.
    DeltaFoldersAndSetState {
        account_id: String,
        upsert: Vec<Folder>,
        remove_ids: Vec<String>,
        resource: String,
        state: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Atomic: save messages + set sync state + mark mailbox populated.
    SaveMessagesAndSetState {
        account_id: String,
        mailbox_id: String,
        messages: Vec<MessageSummary>,
        resource: String,
        state: String,
        populated_mailbox_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Atomic: remove destroyed + save created/updated + set state in one tx.
    DeltaEmailBatch {
        account_id: String,
        remove_ids: Vec<String>,
        save_groups: Vec<(String, Vec<MessageSummary>)>,
        resource: String,
        state: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Expire pending ops older than max_age_secs by reverting to server flags.
    ExpirePendingOps {
        account_id: String,
        max_age_secs: i64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
}
