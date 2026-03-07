//! Shared envelope/message processing for IMAP and JMAP backends.
//!
//! Pure functions that operate on melib types — no backend-specific logic.

use melib::email::address::MessageID;
use melib::email::attachment_types::{ContentType, Text};
use melib::email::attachments::Attachment;
use melib::{Envelope, MailboxHash};

use crate::models::{AttachmentData, MessageSummary};

/// Build a `MessageSummary` from a melib `Envelope`.
pub fn envelope_to_summary(envelope: &Envelope, mailbox_hash: MailboxHash) -> MessageSummary {
    let from_str = envelope
        .from()
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let to_str = envelope
        .to()
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let msg_id = envelope.message_id().to_string();
    let refs = envelope.references();
    let thread_id = Some(compute_thread_id(&msg_id, refs));
    let thread_depth = refs.len() as u32;
    let in_reply_to = envelope
        .in_reply_to()
        .and_then(|r| r.refs().last().map(|id| id.to_string()));

    let reply_to = envelope
        .other_headers()
        .get("Reply-To")
        .map(|s| s.to_string());

    MessageSummary {
        account_id: String::new(),
        uid: envelope.hash().0,
        subject: envelope.subject().to_string(),
        from: from_str,
        to: to_str,
        date: envelope.date_as_str().to_string(),
        is_read: envelope.is_seen(),
        is_starred: envelope.flags().is_flagged(),
        has_attachments: envelope.has_attachments,
        thread_id,
        envelope_hash: envelope.hash().0,
        timestamp: envelope.timestamp as i64,
        mailbox_hash: mailbox_hash.0,
        message_id: msg_id,
        in_reply_to,
        reply_to,
        thread_depth,
    }
}

/// Compute a deterministic thread ID from the root message-ID in the References chain.
/// If references exist, the root is references[0] (the original message).
/// Otherwise, this message IS the root and we hash its own message-ID.
pub fn compute_thread_id(message_id: &str, references: &[MessageID]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if let Some(root) = references.first() {
        root.to_string().hash(&mut hasher);
    } else {
        message_id.hash(&mut hasher);
    }
    hasher.finish()
}

/// Walk the MIME tree and extract text/plain, text/html, and attachments.
pub fn extract_body(att: &Attachment) -> (Option<String>, Option<String>, Vec<AttachmentData>) {
    let mut text_plain = None;
    let mut text_html = None;
    let mut attachments = Vec::new();
    extract_parts(att, &mut text_plain, &mut text_html, &mut attachments);
    (text_plain, text_html, attachments)
}

fn extract_parts(
    att: &Attachment,
    plain: &mut Option<String>,
    html: &mut Option<String>,
    attachments: &mut Vec<AttachmentData>,
) {
    match &att.content_type {
        ContentType::Text {
            kind: Text::Plain, ..
        } if !att.content_disposition.kind.is_attachment() => {
            let bytes = att.decode(Default::default());
            let text = String::from_utf8_lossy(&bytes);
            if !text.trim().is_empty() {
                let combined = plain.take().unwrap_or_default() + &text;
                *plain = Some(combined);
            }
        }
        ContentType::Text {
            kind: Text::Html, ..
        } if !att.content_disposition.kind.is_attachment() => {
            let bytes = att.decode(Default::default());
            let text = String::from_utf8_lossy(&bytes);
            if !text.trim().is_empty() {
                let combined = html.take().unwrap_or_default() + &text;
                *html = Some(combined);
            }
        }
        ContentType::Multipart { parts, .. } => {
            for part in parts {
                extract_parts(part, plain, html, attachments);
            }
        }
        _ => {
            let filename = att
                .filename()
                .or_else(|| att.content_disposition.filename.clone())
                .unwrap_or_else(|| "unnamed".into());
            attachments.push(AttachmentData {
                filename,
                mime_type: att.content_type.to_string(),
                data: att.decode(Default::default()),
            });
        }
    }
}
