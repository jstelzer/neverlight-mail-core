//! JMAP Email methods (RFC 8621 §4).
//!
//! Email/query, Email/get, Email/set, Email/changes.

use serde_json::Value;

use crate::client::{JmapClient, JmapError};
use crate::mime;
use crate::models::{AttachmentData, MessageSummary};
use crate::parse;
use crate::types::{FlagOp, Flags, State};

/// Default page size for Email/query.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Properties requested for list view (Email/get).
const SUMMARY_PROPERTIES: &[&str] = &[
    "id",
    "threadId",
    "mailboxIds",
    "keywords",
    "from",
    "to",
    "subject",
    "receivedAt",
    "size",
    "hasAttachment",
    "preview",
    "messageId",
    "inReplyTo",
];

/// Properties for body fetch (Email/get with bodyValues).
const BODY_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
];

/// Result of an Email/query call.
pub struct QueryResult {
    pub ids: Vec<String>,
    pub state: State,
    pub total: u32,
    pub can_calculate_changes: bool,
}

/// Query emails in a mailbox, sorted by receivedAt descending.
///
/// Uses result references to batch query+get in a single HTTP request.
pub async fn query(
    client: &JmapClient,
    mailbox_id: &str,
    limit: u32,
    position: u32,
) -> Result<QueryResult, JmapError> {
    let call = client.method(
        "Email/query",
        serde_json::json!({
            "filter": { "inMailbox": mailbox_id },
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit,
            "position": position,
            "calculateTotal": true,
        }),
        "q0",
    );

    let resp = client.call(vec![call]).await?;

    let result = resp
        .method_responses
        .first()
        .ok_or_else(|| JmapError::RequestError("Empty response from Email/query".into()))?;

    parse_query_result(&result.1)
}

/// Query + get in a single batched request using result references.
///
/// Returns email summaries for display in the message list.
pub async fn query_and_get(
    client: &JmapClient,
    mailbox_id: &str,
    limit: u32,
    position: u32,
) -> Result<(Vec<MessageSummary>, QueryResult), JmapError> {
    let query_call = client.method(
        "Email/query",
        serde_json::json!({
            "filter": { "inMailbox": mailbox_id },
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit,
            "position": position,
            "calculateTotal": true,
        }),
        "q0",
    );

    let get_call = client.method(
        "Email/get",
        serde_json::json!({
            "#ids": JmapClient::result_ref("q0", "Email/query", "/ids"),
            "properties": SUMMARY_PROPERTIES,
        }),
        "g0",
    );

    let resp = client.call(vec![query_call, get_call]).await?;

    // Parse query result (first response)
    let query_resp = resp
        .method_responses
        .iter()
        .find(|mc| mc.2 == "q0")
        .ok_or_else(|| JmapError::RequestError("Missing Email/query response".into()))?;
    let query_result = parse_query_result(&query_resp.1)?;

    // Parse get result (second response)
    let get_resp = resp
        .method_responses
        .iter()
        .find(|mc| mc.2 == "g0")
        .ok_or_else(|| JmapError::RequestError("Missing Email/get response".into()))?;
    let messages = parse_email_list(&get_resp.1, mailbox_id)?;

    Ok((messages, query_result))
}

/// Fetch summaries for specific email IDs.
pub async fn get_summaries(
    client: &JmapClient,
    ids: &[String],
    mailbox_id: &str,
) -> Result<Vec<MessageSummary>, JmapError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let call = client.method(
        "Email/get",
        serde_json::json!({
            "ids": ids,
            "properties": SUMMARY_PROPERTIES,
        }),
        "g0",
    );

    let resp = client.call(vec![call]).await?;

    let result = resp
        .method_responses
        .first()
        .ok_or_else(|| JmapError::RequestError("Empty response from Email/get".into()))?;

    parse_email_list(&result.1, mailbox_id)
}

/// Fetch full body content for an email.
///
/// Strategy: request bodyValues inline first. If the server returns body text,
/// use it directly. Otherwise fall back to blob download + RFC 5322 parsing.
pub async fn get_body(
    client: &JmapClient,
    email_id: &str,
) -> Result<(String, String, Vec<AttachmentData>), JmapError> {
    let call = client.method(
        "Email/get",
        serde_json::json!({
            "ids": [email_id],
            "properties": BODY_PROPERTIES,
            "fetchAllBodyValues": true,
            "bodyProperties": ["partId", "blobId", "type", "name", "size", "disposition"],
        }),
        "b0",
    );

    let resp = client.call(vec![call]).await?;

    let result = resp
        .method_responses
        .first()
        .ok_or_else(|| JmapError::RequestError("Empty response from Email/get body".into()))?;

    let email = result
        .1
        .get("list")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| JmapError::RequestError("Email not found".into()))?;

    // Try bodyValues path first
    let body_values = email.get("bodyValues").and_then(|v| v.as_object());
    let text_body = email.get("textBody").and_then(|v| v.as_array());
    let html_body = email.get("htmlBody").and_then(|v| v.as_array());

    let text_plain = extract_body_value(body_values, text_body);
    let text_html = extract_body_value(body_values, html_body);

    // Extract attachments metadata
    let attachments = extract_attachments(client, email).await?;

    if text_plain.is_some() || text_html.is_some() {
        let markdown = mime::render_body_markdown(text_plain.as_deref(), text_html.as_deref());
        let plain = mime::render_body(text_plain.as_deref(), text_html.as_deref());
        return Ok((markdown, plain, attachments));
    }

    // Fallback: download raw blob and parse
    let blob_id = email
        .get("blobId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JmapError::RequestError("No blobId for body fallback".into()))?;

    let raw = client.download_blob(blob_id).await?;
    let parsed = parse::parse_body(&raw);

    let markdown = mime::render_body_markdown(
        parsed.text_plain.as_deref(),
        parsed.text_html.as_deref(),
    );
    let plain = mime::render_body(
        parsed.text_plain.as_deref(),
        parsed.text_html.as_deref(),
    );

    // Merge parsed attachments with any we already got
    let mut all_attachments = attachments;
    all_attachments.extend(parsed.attachments);

    Ok((markdown, plain, all_attachments))
}

// ---------------------------------------------------------------------------
// Phase 2a: Flag operations (Email/set keyword patches)
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
    check_set_errors(&resp, email_id)
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
    let set_resp = resp.method_responses.first()
        .ok_or_else(|| JmapError::RequestError("Empty Email/set response".into()))?;
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
// Phase 2b: Move + Delete (Email/set mailboxIds + destroy)
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
    check_set_errors(&resp, email_id)
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

    let set_resp = resp.method_responses.first()
        .ok_or_else(|| JmapError::RequestError("Empty Email/set response".into()))?;
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

/// Check Email/set response for update errors on a single email.
fn check_set_errors(
    resp: &crate::client::JmapResponse,
    email_id: &str,
) -> Result<(), JmapError> {
    let set_resp = resp.method_responses.first()
        .ok_or_else(|| JmapError::RequestError("Empty Email/set response".into()))?;

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

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_query_result(data: &Value) -> Result<QueryResult, JmapError> {
    let ids = data
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let state = data
        .get("queryState")
        .or_else(|| data.get("state"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let total = data
        .get("total")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let can_calculate_changes = data
        .get("canCalculateChanges")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(QueryResult {
        ids,
        state: State(state),
        total,
        can_calculate_changes,
    })
}

fn parse_email_list(data: &Value, fallback_mailbox_id: &str) -> Result<Vec<MessageSummary>, JmapError> {
    let list = data
        .get("list")
        .and_then(|v| v.as_array())
        .ok_or_else(|| JmapError::RequestError("Missing list in Email/get response".into()))?;

    let mut messages = Vec::with_capacity(list.len());
    for item in list {
        messages.push(parse_email_summary(item, fallback_mailbox_id));
    }
    Ok(messages)
}

fn parse_email_summary(item: &Value, fallback_mailbox_id: &str) -> MessageSummary {
    let email_id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let thread_id = item
        .get("threadId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // mailboxIds is { "mb-id": true, ... } — pick the first, or use fallback
    let mailbox_id = item
        .get("mailboxIds")
        .and_then(|v| v.as_object())
        .and_then(|obj| obj.keys().next())
        .map(|s| s.to_string())
        .unwrap_or_else(|| fallback_mailbox_id.to_string());

    let keywords = item
        .get("keywords")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let flags = Flags::from_keywords(&keywords);

    let from = item
        .get("from")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .map(format_address)
        .unwrap_or_default();

    let to = item
        .get("to")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .map(format_address)
        .unwrap_or_default();

    let subject = item
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("(no subject)")
        .to_string();

    let received_at = item
        .get("receivedAt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let timestamp = parse_rfc3339_timestamp(&received_at);

    let has_attachment = item
        .get("hasAttachment")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let message_id_arr = item
        .get("messageId")
        .and_then(|v| v.as_array());
    let message_id = message_id_arr
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let in_reply_to_arr = item
        .get("inReplyTo")
        .and_then(|v| v.as_array());
    let in_reply_to = in_reply_to_arr
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    MessageSummary {
        account_id: String::new(), // filled by caller when caching
        email_id,
        subject,
        from,
        to,
        date: received_at,
        timestamp,
        is_read: flags.seen,
        is_starred: flags.flagged,
        has_attachments: has_attachment,
        thread_id,
        mailbox_id,
        message_id,
        in_reply_to,
        reply_to: None,
        thread_depth: 0,
    }
}

/// Format a JMAP address object { name, email } to display string.
fn format_address(addr: &Value) -> String {
    let name = addr.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let email = addr.get("email").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        email.to_string()
    } else {
        format!("{name} <{email}>")
    }
}

/// Extract body text from bodyValues using the part IDs in textBody/htmlBody.
fn extract_body_value(
    body_values: Option<&serde_json::Map<String, Value>>,
    body_parts: Option<&Vec<Value>>,
) -> Option<String> {
    let values = body_values?;
    let parts = body_parts?;
    let part_id = parts.first()?.get("partId")?.as_str()?;
    let value = values.get(part_id)?.get("value")?.as_str()?;
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Extract attachment metadata + download data for non-inline attachments.
async fn extract_attachments(
    client: &JmapClient,
    email: &Value,
) -> Result<Vec<AttachmentData>, JmapError> {
    let Some(atts) = email.get("attachments").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };

    let mut result = Vec::new();
    for att in atts {
        let blob_id = att.get("blobId").and_then(|v| v.as_str());
        let Some(blob_id) = blob_id else { continue };

        let filename = att
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let mime_type = att
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream")
            .to_string();

        let data = client.download_blob(blob_id).await?;
        result.push(AttachmentData {
            filename,
            mime_type,
            data,
        });
    }
    Ok(result)
}

/// Parse RFC 3339 timestamp to unix epoch seconds.
/// Falls back to 0 on parse failure.
fn parse_rfc3339_timestamp(s: &str) -> i64 {
    // Simple parse: "2026-01-15T10:30:00Z" or with offset
    // We only need seconds precision for sorting.
    parse_rfc3339_simple(s).unwrap_or(0)
}

fn parse_rfc3339_simple(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }

    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;

    // Approximate days from epoch (good enough for sorting)
    let days = (year - 1970) * 365 + (year - 1969) / 4 - (year - 1901) / 100 + (year - 1601) / 400;
    let month_days: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = if month > 2 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)) {
        1
    } else {
        0
    };
    let total_days = days + month_days[(month - 1) as usize] + day - 1 + leap;

    Some(total_days * 86400 + hour * 3600 + min * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_query_response() -> Value {
        serde_json::json!({
            "accountId": "u1234",
            "ids": ["M001", "M002", "M003"],
            "queryState": "qs-abc",
            "total": 42,
            "position": 0,
            "canCalculateChanges": true
        })
    }

    fn sample_email_get_response() -> Value {
        serde_json::json!({
            "accountId": "u1234",
            "state": "s-xyz",
            "list": [
                {
                    "id": "M001",
                    "threadId": "T001",
                    "mailboxIds": { "mb-inbox": true },
                    "keywords": { "$seen": true, "$flagged": true },
                    "from": [{ "name": "Alice", "email": "alice@example.com" }],
                    "to": [{ "name": "Bob", "email": "bob@example.com" }],
                    "subject": "Hello from Alice",
                    "receivedAt": "2026-01-15T10:30:00Z",
                    "size": 4096,
                    "hasAttachment": false,
                    "preview": "Hey Bob, just wanted to...",
                    "messageId": ["msg001@example.com"],
                    "inReplyTo": null
                },
                {
                    "id": "M002",
                    "threadId": "T002",
                    "mailboxIds": { "mb-inbox": true },
                    "keywords": {},
                    "from": [{ "name": "", "email": "noreply@service.com" }],
                    "to": [{ "email": "bob@example.com" }],
                    "subject": "Your receipt",
                    "receivedAt": "2026-01-14T08:00:00Z",
                    "size": 2048,
                    "hasAttachment": true,
                    "preview": "Thank you for your purchase...",
                    "messageId": ["msg002@service.com"],
                    "inReplyTo": ["msg001@example.com"]
                }
            ],
            "notFound": []
        })
    }

    fn sample_body_response() -> Value {
        serde_json::json!({
            "accountId": "u1234",
            "list": [{
                "id": "M001",
                "blobId": "B001",
                "bodyValues": {
                    "1": { "value": "Hello, this is the plain text body.", "isEncodingProblem": false },
                    "2": { "value": "<p>Hello, this is the <strong>HTML</strong> body.</p>", "isEncodingProblem": false }
                },
                "textBody": [{ "partId": "1", "type": "text/plain" }],
                "htmlBody": [{ "partId": "2", "type": "text/html" }],
                "attachments": []
            }]
        })
    }

    #[test]
    fn parses_query_result() {
        let data = sample_query_response();
        let result = parse_query_result(&data).unwrap();

        assert_eq!(result.ids, vec!["M001", "M002", "M003"]);
        assert_eq!(result.state.0, "qs-abc");
        assert_eq!(result.total, 42);
        assert!(result.can_calculate_changes);
    }

    #[test]
    fn parses_email_summaries() {
        let data = sample_email_get_response();
        let messages = parse_email_list(&data, "mb-inbox").unwrap();

        assert_eq!(messages.len(), 2);

        let m1 = &messages[0];
        assert_eq!(m1.email_id, "M001");
        assert_eq!(m1.thread_id.as_deref(), Some("T001"));
        assert_eq!(m1.subject, "Hello from Alice");
        assert_eq!(m1.from, "Alice <alice@example.com>");
        assert_eq!(m1.to, "Bob <bob@example.com>");
        assert!(m1.is_read);
        assert!(m1.is_starred);
        assert!(!m1.has_attachments);
        assert_eq!(m1.mailbox_id, "mb-inbox");
        assert_eq!(m1.message_id, "msg001@example.com");
        assert!(m1.in_reply_to.is_none());

        let m2 = &messages[1];
        assert_eq!(m2.email_id, "M002");
        assert!(!m2.is_read);
        assert!(!m2.is_starred);
        assert!(m2.has_attachments);
        assert_eq!(m2.from, "noreply@service.com");
        assert_eq!(m2.in_reply_to.as_deref(), Some("msg001@example.com"));
    }

    #[test]
    fn parses_body_values() {
        let data = sample_body_response();
        let email = data["list"][0].clone();

        let body_values = email.get("bodyValues").and_then(|v| v.as_object());
        let text_body = email.get("textBody").and_then(|v| v.as_array());
        let html_body = email.get("htmlBody").and_then(|v| v.as_array());

        let plain = extract_body_value(body_values, text_body);
        let html = extract_body_value(body_values, html_body);

        assert_eq!(plain.as_deref(), Some("Hello, this is the plain text body."));
        assert!(html.unwrap().contains("<strong>HTML</strong>"));
    }

    #[test]
    fn format_address_with_name() {
        let addr = serde_json::json!({"name": "Alice", "email": "alice@example.com"});
        assert_eq!(format_address(&addr), "Alice <alice@example.com>");
    }

    #[test]
    fn format_address_without_name() {
        let addr = serde_json::json!({"name": "", "email": "noreply@example.com"});
        assert_eq!(format_address(&addr), "noreply@example.com");
    }

    #[test]
    fn format_address_name_only() {
        let addr = serde_json::json!({"name": "Alice"});
        assert_eq!(format_address(&addr), "Alice <>");
    }

    #[test]
    fn rfc3339_timestamp_parsing() {
        let ts = parse_rfc3339_timestamp("2026-01-15T10:30:00Z");
        assert!(ts > 0);
        // 2026-01-15 should be roughly 56 years * 365.25 days * 86400
        assert!(ts > 1_700_000_000); // after 2023
        assert!(ts < 1_900_000_000); // before 2030
    }

    #[test]
    fn rfc3339_invalid_returns_zero() {
        assert_eq!(parse_rfc3339_timestamp(""), 0);
        assert_eq!(parse_rfc3339_timestamp("not-a-date"), 0);
    }

    #[test]
    fn result_ref_format_for_batching() {
        let r = JmapClient::result_ref("q0", "Email/query", "/ids");
        assert_eq!(r["resultOf"], "q0");
        assert_eq!(r["name"], "Email/query");
        assert_eq!(r["path"], "/ids");
    }

    // -- Phase 2a: Flag operation tests --

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

    // -- Phase 2b: Move/delete tests --

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
        assert!(check_set_errors(&resp, "M001").is_ok());
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
        let err = check_set_errors(&resp, "M001").unwrap_err();
        let err_str = format!("{err}");
        assert!(err_str.contains("notFound"));
    }

    #[test]
    fn handles_missing_fields_gracefully() {
        let data = serde_json::json!({
            "list": [{
                "id": "M999"
            }]
        });
        let messages = parse_email_list(&data, "mb-fallback").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].email_id, "M999");
        assert_eq!(messages[0].subject, "(no subject)");
        assert_eq!(messages[0].mailbox_id, "mb-fallback");
        assert!(!messages[0].is_read);
    }
}
