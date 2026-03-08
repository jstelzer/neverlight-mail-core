//! Owned types for the JMAP-only engine.
//!
//! These replace the melib re-exports (`EnvelopeHash`, `MailboxHash`, `Flag`, `FlagOp`,
//! `BackendEvent`, `RefreshEventKind`). All consumers import these from
//! `neverlight_mail_core::types`.

use serde::{Deserialize, Serialize};

/// Stable JMAP email identifier (server-assigned, never changes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmailId(pub String);

/// JMAP mailbox identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MailboxId(pub String);

/// JMAP state token for delta sync (opaque string from the server).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State(pub String);

/// JMAP blob identifier (for upload/download).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobId(pub String);

/// JMAP identity identifier (for sending).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdentityId(pub String);

/// JMAP thread identifier (server-provided, stable).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(pub String);

/// Email flags as JMAP keywords.
///
/// JMAP uses string keywords (`$seen`, `$flagged`, `$draft`, `$answered`).
/// We map them to a struct for ergonomics. Custom keywords are not tracked
/// in this struct — they flow through as raw strings where needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub draft: bool,
    pub answered: bool,
}

impl Flags {
    /// Convert to JMAP keywords map for `Email/set`.
    pub fn to_keywords(&self) -> Vec<(&'static str, bool)> {
        vec![
            ("$seen", self.seen),
            ("$flagged", self.flagged),
            ("$draft", self.draft),
            ("$answered", self.answered),
        ]
    }

    /// Parse from JMAP keywords object.
    pub fn from_keywords(keywords: &serde_json::Value) -> Self {
        let obj = keywords.as_object();
        Flags {
            seen: obj
                .and_then(|o| o.get("$seen"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            flagged: obj
                .and_then(|o| o.get("$flagged"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            draft: obj
                .and_then(|o| o.get("$draft"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            answered: obj
                .and_then(|o| o.get("$answered"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }
}

/// Flag mutation operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagOp {
    SetSeen(bool),
    SetFlagged(bool),
}

/// Delta sync event emitted by the sync loop.
#[derive(Debug, Clone)]
pub enum SyncEvent {
    /// New email appeared.
    Created(EmailId),
    /// Email properties changed (flags, mailbox membership).
    Updated(EmailId),
    /// Email was permanently destroyed.
    Destroyed(EmailId),
    /// Flags changed on an email.
    FlagsChanged(EmailId, Flags),
    /// New mailbox appeared.
    MailboxCreated(MailboxId),
    /// Mailbox properties changed (name, counts).
    MailboxUpdated(MailboxId),
    /// Mailbox was destroyed.
    MailboxDestroyed(MailboxId),
}

/// JMAP mailbox role (RFC 8621 §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailboxRole {
    Inbox,
    Drafts,
    Sent,
    Trash,
    Junk,
    Archive,
    /// A role we don't have a variant for.
    Other(String),
}

impl MailboxRole {
    pub fn from_str_opt(s: Option<&str>) -> Option<Self> {
        match s {
            Some("inbox") => Some(Self::Inbox),
            Some("drafts") => Some(Self::Drafts),
            Some("sent") => Some(Self::Sent),
            Some("trash") => Some(Self::Trash),
            Some("junk") => Some(Self::Junk),
            Some("archive") => Some(Self::Archive),
            Some(other) => Some(Self::Other(other.to_string())),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_keywords() {
        let flags = Flags {
            seen: true,
            flagged: false,
            draft: false,
            answered: true,
        };
        let kw = flags.to_keywords();
        assert!(kw.contains(&("$seen", true)));
        assert!(kw.contains(&("$flagged", false)));
        assert!(kw.contains(&("$answered", true)));
    }

    #[test]
    fn flags_from_jmap_keywords_json() {
        let json = serde_json::json!({
            "$seen": true,
            "$flagged": true,
            "$draft": false,
        });
        let flags = Flags::from_keywords(&json);
        assert!(flags.seen);
        assert!(flags.flagged);
        assert!(!flags.draft);
        assert!(!flags.answered);
    }

    #[test]
    fn flags_from_empty_keywords() {
        let json = serde_json::json!({});
        let flags = Flags::from_keywords(&json);
        assert!(!flags.seen);
        assert!(!flags.flagged);
    }

    #[test]
    fn mailbox_role_parsing() {
        assert_eq!(MailboxRole::from_str_opt(Some("inbox")), Some(MailboxRole::Inbox));
        assert_eq!(MailboxRole::from_str_opt(Some("trash")), Some(MailboxRole::Trash));
        assert_eq!(MailboxRole::from_str_opt(None), None);
    }

    #[test]
    fn email_id_equality() {
        let a = EmailId("M1234".into());
        let b = EmailId("M1234".into());
        let c = EmailId("M5678".into());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn email_id_serde_round_trip() {
        let id = EmailId("Mdeadbeef".into());
        let json = serde_json::to_string(&id).unwrap();
        let back: EmailId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
