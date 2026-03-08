//! Flag operations, moves, deletes, and search.

use serde_json::Value;

use crate::client::{JmapClient, JmapError};
use crate::models::MessageSummary;
use crate::types::FlagOp;

use super::query::{parse_email_list, parse_query_result, QueryResult, SUMMARY_PROPERTIES};

// ---------------------------------------------------------------------------
// Flag operations (Email/set keyword patches)
// ---------------------------------------------------------------------------

/// Apply a flag operation to an email.
///
/// Uses JMAP keyword patches: `keywords/$seen: true` or `keywords/$seen: null`.
/// Setting to `null` removes the keyword (JMAP convention for patch-delete).
pub async fn set_flag(
    client: &JmapClient,
    email_id: &str,
    op: &FlagOp,
) -> Result<(), JmapError> {
    let patch = flag_op_to_patch(op);
    let call = client.method(
        "Email/set",
        serde_json::json!({
            "update": {
                email_id: patch,
            },
        }),
        "f0",
    );

    let resp = client.call(vec![call]).await?;
    check_set_errors(&resp, email_id, "f0")
}

/// Apply flag operations to multiple emails in a single request.
pub async fn set_flags_batch(
    client: &JmapClient,
    ops: &[(String, FlagOp)],
) -> Result<(), JmapError> {
    if ops.is_empty() {
        return Ok(());
    }

    let mut update = serde_json::Map::new();
    for (email_id, op) in ops {
        update.insert(email_id.clone(), flag_op_to_patch(op));
    }

    let call = client.method(
        "Email/set",
        serde_json::json!({ "update": update }),
        "fb0",
    );

    let resp = client.call(vec![call]).await?;

    // Check for any update errors
    let set_resp = resp.method_responses.iter().find(|mc| mc.2 == "fb0")
        .ok_or_else(|| JmapError::RequestError("Missing Email/set response".into()))?;
    if let Some(errors) = set_resp.1.get("notUpdated").and_then(|v| v.as_object()) {
        if let Some((id, err)) = errors.iter().next() {
            let err_type = err.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            return Err(JmapError::MethodError {
                method: "Email/set".into(),
                error_type: err_type.into(),
                description: format!("Failed to update {id}"),
            });
        }
    }
    Ok(())
}

/// Convert a FlagOp to a JMAP keyword patch object.
fn flag_op_to_patch(op: &FlagOp) -> Value {
    match op {
        FlagOp::SetSeen(true) => serde_json::json!({ "keywords/$seen": true }),
        FlagOp::SetSeen(false) => serde_json::json!({ "keywords/$seen": null }),
        FlagOp::SetFlagged(true) => serde_json::json!({ "keywords/$flagged": true }),
        FlagOp::SetFlagged(false) => serde_json::json!({ "keywords/$flagged": null }),
    }
}

// ---------------------------------------------------------------------------
// Move + Delete (Email/set mailboxIds + destroy)
// ---------------------------------------------------------------------------

/// Move an email from one mailbox to another.
///
/// Patches `mailboxIds` to remove the source and add the destination.
pub async fn move_to(
    client: &JmapClient,
    email_id: &str,
    from_mailbox: &str,
    to_mailbox: &str,
) -> Result<(), JmapError> {
    let patch = serde_json::json!({
        format!("mailboxIds/{from_mailbox}"): null,
        format!("mailboxIds/{to_mailbox}"): true,
    });

    let call = client.method(
        "Email/set",
        serde_json::json!({
            "update": { email_id: patch },
        }),
        "mv0",
    );

    let resp = client.call(vec![call]).await?;
    check_set_errors(&resp, email_id, "mv0")
}

/// Move an email to the trash mailbox.
pub async fn trash(
    client: &JmapClient,
    email_id: &str,
    current_mailbox: &str,
    trash_mailbox: &str,
) -> Result<(), JmapError> {
    move_to(client, email_id, current_mailbox, trash_mailbox).await
}

/// Permanently destroy emails (cannot be undone).
pub async fn destroy(
    client: &JmapClient,
    email_ids: &[String],
) -> Result<(), JmapError> {
    if email_ids.is_empty() {
        return Ok(());
    }

    let call = client.method(
        "Email/set",
        serde_json::json!({
            "destroy": email_ids,
        }),
        "d0",
    );

    let resp = client.call(vec![call]).await?;

    let set_resp = resp.method_responses.iter().find(|mc| mc.2 == "d0")
        .ok_or_else(|| JmapError::RequestError("Missing Email/set response".into()))?;
    if let Some(errors) = set_resp.1.get("notDestroyed").and_then(|v| v.as_object()) {
        if let Some((id, err)) = errors.iter().next() {
            let err_type = err.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            return Err(JmapError::MethodError {
                method: "Email/set".into(),
                error_type: err_type.into(),
                description: format!("Failed to destroy {id}"),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Search filter for Email/query (RFC 8621 §4.4.1).
///
/// Fields are ANDed together. Only non-None fields are included in the filter.
#[derive(Debug, Default)]
pub struct SearchFilter {
    /// Full-text search across subject, body, from, to, etc.
    pub text: Option<String>,
    /// Search in subject only.
    pub subject: Option<String>,
    /// Search in from addresses.
    pub from: Option<String>,
    /// Search in to addresses.
    pub to: Option<String>,
    /// Restrict to a specific mailbox.
    pub in_mailbox: Option<String>,
    /// Only emails with attachments.
    pub has_attachment: Option<bool>,
    /// Emails received after this date (RFC 3339).
    pub after: Option<String>,
    /// Emails received before this date (RFC 3339).
    pub before: Option<String>,
}

impl SearchFilter {
    fn to_json(&self) -> Value {
        let mut filter = serde_json::Map::new();
        if let Some(ref text) = self.text {
            filter.insert("text".into(), Value::String(text.clone()));
        }
        if let Some(ref subject) = self.subject {
            filter.insert("subject".into(), Value::String(subject.clone()));
        }
        if let Some(ref from) = self.from {
            filter.insert("from".into(), Value::String(from.clone()));
        }
        if let Some(ref to) = self.to {
            filter.insert("to".into(), Value::String(to.clone()));
        }
        if let Some(ref mailbox_id) = self.in_mailbox {
            filter.insert("inMailbox".into(), Value::String(mailbox_id.clone()));
        }
        if let Some(has) = self.has_attachment {
            filter.insert("hasAttachment".into(), Value::Bool(has));
        }
        if let Some(ref after) = self.after {
            filter.insert("after".into(), Value::String(after.clone()));
        }
        if let Some(ref before) = self.before {
            filter.insert("before".into(), Value::String(before.clone()));
        }
        Value::Object(filter)
    }
}

/// Server-side search: Email/query with filters, then Email/get for results.
///
/// Returns matched messages with full summary data. Uses result references
/// to batch query+get in a single HTTP request.
pub async fn search(
    client: &JmapClient,
    filter: &SearchFilter,
    limit: u32,
) -> Result<(Vec<MessageSummary>, QueryResult), JmapError> {
    let query_call = client.method(
        "Email/query",
        serde_json::json!({
            "filter": filter.to_json(),
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit,
            "calculateTotal": true,
        }),
        "sq0",
    );

    let get_call = client.method(
        "Email/get",
        serde_json::json!({
            "#ids": JmapClient::result_ref("sq0", "Email/query", "/ids"),
            "properties": SUMMARY_PROPERTIES,
        }),
        "sg0",
    );

    let resp = client.call(vec![query_call, get_call]).await?;

    let query_resp = resp.method_responses.iter().find(|mc| mc.2 == "sq0")
        .ok_or_else(|| JmapError::RequestError("Missing search query response".into()))?;
    let query_result = parse_query_result(&query_resp.1)?;

    let get_resp = resp.method_responses.iter().find(|mc| mc.2 == "sg0")
        .ok_or_else(|| JmapError::RequestError("Missing search get response".into()))?;
    // Use empty string as fallback mailbox ID since search spans mailboxes
    let messages = parse_email_list(&get_resp.1, "")?;

    Ok((messages, query_result))
}

/// Check Email/set response for update errors on a single email.
pub fn check_set_errors(
    resp: &crate::client::JmapResponse,
    email_id: &str,
    call_id: &str,
) -> Result<(), JmapError> {
    let set_resp = resp.method_responses.iter().find(|mc| mc.2 == call_id)
        .ok_or_else(|| JmapError::RequestError("Missing Email/set response".into()))?;

    if let Some(errors) = set_resp.1.get("notUpdated").and_then(|v| v.as_object()) {
        if let Some(err) = errors.get(email_id) {
            let err_type = err.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            let desc = err.get("description").and_then(|v| v.as_str()).unwrap_or("");
            return Err(JmapError::MethodError {
                method: "Email/set".into(),
                error_type: err_type.into(),
                description: desc.into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_op_seen_true_patch() {
        let patch = flag_op_to_patch(&FlagOp::SetSeen(true));
        assert_eq!(patch["keywords/$seen"], true);
    }

    #[test]
    fn flag_op_seen_false_patch() {
        let patch = flag_op_to_patch(&FlagOp::SetSeen(false));
        assert!(patch["keywords/$seen"].is_null());
    }

    #[test]
    fn flag_op_flagged_true_patch() {
        let patch = flag_op_to_patch(&FlagOp::SetFlagged(true));
        assert_eq!(patch["keywords/$flagged"], true);
    }

    #[test]
    fn flag_op_flagged_false_patch() {
        let patch = flag_op_to_patch(&FlagOp::SetFlagged(false));
        assert!(patch["keywords/$flagged"].is_null());
    }

    #[test]
    fn check_set_errors_passes_on_success() {
        let resp = crate::client::JmapResponse {
            method_responses: vec![crate::client::MethodCall(
                "Email/set".into(),
                serde_json::json!({
                    "updated": { "M001": null },
                    "notUpdated": {}
                }),
                "f0".into(),
            )],
            session_state: None,
        };
        assert!(check_set_errors(&resp, "M001", "f0").is_ok());
    }

    #[test]
    fn check_set_errors_catches_failure() {
        let resp = crate::client::JmapResponse {
            method_responses: vec![crate::client::MethodCall(
                "Email/set".into(),
                serde_json::json!({
                    "updated": {},
                    "notUpdated": {
                        "M001": {
                            "type": "notFound",
                            "description": "Email not found"
                        }
                    }
                }),
                "f0".into(),
            )],
            session_state: None,
        };
        let err = check_set_errors(&resp, "M001", "f0").unwrap_err();
        let err_str = format!("{err}");
        assert!(err_str.contains("notFound"));
    }

    #[test]
    fn search_filter_text_only() {
        let filter = SearchFilter {
            text: Some("invoice".into()),
            ..Default::default()
        };
        let json = filter.to_json();
        assert_eq!(json["text"], "invoice");
        assert!(json.get("from").is_none());
        assert!(json.get("inMailbox").is_none());
    }

    #[test]
    fn search_filter_combined() {
        let filter = SearchFilter {
            from: Some("alice@example.com".into()),
            subject: Some("quarterly report".into()),
            has_attachment: Some(true),
            in_mailbox: Some("mb-inbox".into()),
            ..Default::default()
        };
        let json = filter.to_json();
        assert_eq!(json["from"], "alice@example.com");
        assert_eq!(json["subject"], "quarterly report");
        assert_eq!(json["hasAttachment"], true);
        assert_eq!(json["inMailbox"], "mb-inbox");
        assert!(json.get("text").is_none());
    }

    #[test]
    fn search_filter_date_range() {
        let filter = SearchFilter {
            after: Some("2026-01-01T00:00:00Z".into()),
            before: Some("2026-02-01T00:00:00Z".into()),
            ..Default::default()
        };
        let json = filter.to_json();
        assert_eq!(json["after"], "2026-01-01T00:00:00Z");
        assert_eq!(json["before"], "2026-02-01T00:00:00Z");
    }

    #[test]
    fn search_filter_empty() {
        let filter = SearchFilter::default();
        let json = filter.to_json();
        let obj = json.as_object().unwrap();
        assert!(obj.is_empty(), "default filter should produce empty object");
    }
}
