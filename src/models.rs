use serde::{Deserialize, Serialize};

/// A mail folder (JMAP mailbox).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    pub path: String,
    pub unread_count: u32,
    pub total_count: u32,
    /// JMAP mailbox ID (server-assigned string).
    pub mailbox_id: String,
    /// JMAP mailbox role (inbox, drafts, sent, trash, etc.), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Sort order hint from the server.
    #[serde(default)]
    pub sort_order: u32,
}

/// Summary of a message for the list view (no body).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSummary {
    #[serde(default)]
    pub account_id: String,
    /// JMAP email ID (server-assigned stable string).
    pub email_id: String,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub date: String,
    pub is_read: bool,
    pub is_starred: bool,
    pub has_attachments: bool,
    /// JMAP thread ID (server-provided, stable string).
    pub thread_id: Option<String>,
    /// JMAP mailbox ID this message belongs to.
    pub mailbox_id: String,
    pub timestamp: i64,
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub reply_to: Option<String>,
    pub thread_depth: u32,
}

/// Decoded attachment data for display and saving.
#[derive(Debug, Clone)]
pub struct AttachmentData {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

impl AttachmentData {
    pub fn is_image(&self) -> bool {
        self.mime_type.to_ascii_lowercase().starts_with("image/")
    }
}
