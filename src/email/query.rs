//! Email/query and Email/get for message list display.

use serde_json::Value;

use crate::client::{JmapClient, JmapError};
use crate::models::MessageSummary;
use crate::types::{Flags, State};

/// Default page size for Email/query.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Properties requested for list view (Email/get).
pub(super) const SUMMARY_PROPERTIES: &[&str] = &[
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

/// Result of an Email/query call.
pub struct QueryResult {
    pub ids: Vec<String>,
    /// Query state (from Email/query `queryState`). Used for Email/queryChanges.
    pub state: State,
    /// Object state (from Email/get `state`). Used for Email/changes.
    pub get_state: Option<State>,
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
        .iter()
        .find(|mc| mc.2 == "q0")
        .ok_or_else(|| JmapError::RequestError("Missing Email/query response".into()))?;

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

    // Capture the Email/get state (for Email/changes, distinct from queryState)
    let get_state = get_resp
        .1
        .get("state")
        .and_then(|v| v.as_str())
        .map(|s| State(s.to_string()));
    let mut query_result = query_result;
    query_result.get_state = get_state;

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

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

pub(super) fn parse_query_result(data: &Value) -> Result<QueryResult, JmapError> {
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

    let total = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let can_calculate_changes = data
        .get("canCalculateChanges")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(QueryResult {
        ids,
        state: State(state),
        get_state: None, // populated by query_and_get from Email/get response
        total,
        can_calculate_changes,
    })
}

pub(super) fn parse_email_list(
    data: &Value,
    fallback_mailbox_id: &str,
) -> Result<Vec<MessageSummary>, JmapError> {
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

    // mailboxIds is { "mb-id": true, ... } — extract ALL keys
    let mailbox_ids: Vec<String> = item
        .get("mailboxIds")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();

    // context_mailbox_id: prefer the fallback (the mailbox the UI is viewing),
    // otherwise pick the first key. If the message moved out of the context
    // mailbox, context_mailbox_id will differ, signaling delta sync to act.
    let context_mailbox_id = if mailbox_ids.contains(&fallback_mailbox_id.to_string()) {
        fallback_mailbox_id.to_string()
    } else {
        mailbox_ids
            .first()
            .cloned()
            .unwrap_or_else(|| fallback_mailbox_id.to_string())
    };

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

    let message_id_arr = item.get("messageId").and_then(|v| v.as_array());
    let message_id = message_id_arr
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let in_reply_to_arr = item.get("inReplyTo").and_then(|v| v.as_array());
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
        mailbox_ids,
        context_mailbox_id,
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

/// Parse RFC 3339 timestamp to unix epoch seconds.
/// Falls back to 0 on parse failure.
fn parse_rfc3339_timestamp(s: &str) -> i64 {
    // Simple parse: "2026-01-15T10:30:00Z" or with offset
    // We only need seconds precision for sorting.
    parse_rfc3339_simple(s).unwrap_or(0)
}

fn parse_rfc3339_simple(s: &str) -> Option<i64> {
    // RFC 3339 is always ASCII ("2026-01-15T10:30:00Z"). Reject non-ASCII
    // early to avoid panics from byte slicing on multi-byte strings.
    if !s.is_ascii() || s.len() < 19 {
        return None;
    }

    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;

    // Validate month range before indexing
    if !(1..=12).contains(&month) {
        return None;
    }

    // Approximate days from epoch (good enough for sorting)
    let days = (year - 1970) * 365 + (year - 1969) / 4 - (year - 1901) / 100 + (year - 1601) / 400;
    let month_days: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = if month > 2 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)) {
        1
    } else {
        0
    };
    let total_days = days + month_days[(month - 1) as usize] + day - 1 + leap;
    let utc_secs = total_days * 86400 + hour * 3600 + min * 60 + sec;

    // Parse timezone offset: Z, +HH:MM, or -HH:MM
    let tz_part = &s[19..];
    let offset_secs = parse_tz_offset(tz_part);

    Some(utc_secs - offset_secs)
}

/// Parse a timezone offset like "+02:00", "-05:30", or "Z" into seconds.
fn parse_tz_offset(s: &str) -> i64 {
    let s = s.trim();
    if s.is_empty() || s == "Z" || s == "z" {
        return 0;
    }
    if !s.is_ascii() || s.len() < 6 {
        return 0;
    }
    let sign: i64 = if s.starts_with('-') { -1 } else { 1 };
    let s = &s[1..]; // skip +/- (safe: verified ASCII above)
    let oh: i64 = s[0..2].parse().unwrap_or(0);
    let om: i64 = s[3..5].parse().unwrap_or(0);
    sign * (oh * 3600 + om * 60)
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
        assert_eq!(m1.context_mailbox_id, "mb-inbox");
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
        assert!(ts > 1_700_000_000); // after 2023
        assert!(ts < 1_900_000_000); // before 2030
    }

    #[test]
    fn rfc3339_with_timezone_offset() {
        let utc = parse_rfc3339_timestamp("2026-01-15T10:30:00Z");
        let plus2 = parse_rfc3339_timestamp("2026-01-15T12:30:00+02:00");
        let minus5 = parse_rfc3339_timestamp("2026-01-15T05:30:00-05:00");
        assert_eq!(utc, plus2);
        assert_eq!(utc, minus5);
    }

    #[test]
    fn rfc3339_invalid_returns_zero() {
        assert_eq!(parse_rfc3339_timestamp(""), 0);
        assert_eq!(parse_rfc3339_timestamp("not-a-date"), 0);
    }

    #[test]
    fn rfc3339_multibyte_utf8_does_not_panic() {
        // 19+ bytes but not ASCII — must not panic on byte slicing
        assert_eq!(parse_rfc3339_timestamp("日本語テスト文字列ですよ"), 0);
        assert_eq!(parse_rfc3339_timestamp("2026-01-15T10:30:00Ü"), 0);
        assert_eq!(parse_rfc3339_timestamp("2026—01—15T10:30:00Z"), 0); // em-dashes
    }

    #[test]
    fn rfc3339_invalid_month_returns_zero() {
        assert_eq!(parse_rfc3339_timestamp("2026-00-15T10:30:00Z"), 0);
        assert_eq!(parse_rfc3339_timestamp("2026-13-15T10:30:00Z"), 0);
        assert_eq!(parse_rfc3339_timestamp("2026-99-15T10:30:00Z"), 0);
    }

    #[test]
    fn tz_offset_multibyte_does_not_panic() {
        assert_eq!(parse_tz_offset("+日本:00"), 0);
        assert_eq!(parse_tz_offset("Ü"), 0);
    }

    #[test]
    fn result_ref_format_for_batching() {
        let r = JmapClient::result_ref("q0", "Email/query", "/ids");
        assert_eq!(r["resultOf"], "q0");
        assert_eq!(r["name"], "Email/query");
        assert_eq!(r["path"], "/ids");
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
        assert_eq!(messages[0].context_mailbox_id, "mb-fallback");
        assert!(!messages[0].is_read);
    }

    #[test]
    fn mailbox_id_prefers_context_mailbox_when_present() {
        // Message is in both inbox and sent — context mailbox should win
        let data = serde_json::json!({
            "list": [{
                "id": "M100",
                "mailboxIds": { "mb-sent": true, "mb-inbox": true }
            }]
        });
        let messages = parse_email_list(&data, "mb-inbox").unwrap();
        assert_eq!(messages[0].context_mailbox_id, "mb-inbox");
    }

    #[test]
    fn mailbox_id_reflects_move_when_context_mailbox_absent() {
        // Message moved to trash — no longer in inbox
        let data = serde_json::json!({
            "list": [{
                "id": "M100",
                "mailboxIds": { "mb-trash": true }
            }]
        });
        let messages = parse_email_list(&data, "mb-inbox").unwrap();
        assert_eq!(messages[0].context_mailbox_id, "mb-trash");
    }
}
