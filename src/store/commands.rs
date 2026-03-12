use tokio::sync::oneshot;

use crate::models::{AttachmentData, Folder, MessageSummary};

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
        query: String,
        reply: oneshot::Sender<Result<Vec<MessageSummary>, String>>,
    },
    LoadThread {
        account_id: String,
        thread_id: String,
        mailbox_ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<MessageSummary>, String>>,
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
}
