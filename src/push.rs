//! JMAP EventSource push notifications (RFC 8620 §7.3).
//!
//! SSE stream that replaces IMAP IDLE. The server sends state change events
//! when data changes, allowing the client to trigger delta sync only when needed.

use std::collections::HashMap;

use serde_json::Value;

use crate::client::JmapClient;

/// A state change event from the EventSource stream.
#[derive(Debug, Clone)]
pub struct StateChange {
    /// Map of type name → new state token.
    /// e.g. { "Email" → "s42", "Mailbox" → "s15" }
    pub changed: HashMap<String, String>,
}

/// Configuration for EventSource connection.
pub struct EventSourceConfig {
    /// JMAP types to listen for (e.g. ["Email", "Mailbox"]).
    pub types: Vec<String>,
    /// Close after receiving this many events (0 = no limit).
    pub close_after: u32,
    /// Ping interval in seconds (from session capabilities).
    pub ping: u32,
}

impl Default for EventSourceConfig {
    fn default() -> Self {
        Self {
            types: vec!["Email".into(), "Mailbox".into()],
            close_after: 0,
            ping: 0,
        }
    }
}

/// Build the EventSource URL with query parameters.
///
/// If the template contains `{types}`, `{closeafter}`, `{ping}` placeholders
/// (RFC 8620 §7.3), they are substituted. Otherwise query params are appended.
pub fn build_event_source_url(template: &str, config: &EventSourceConfig) -> String {
    let types_str = config.types.join(",");
    let close_after_str = config.close_after.to_string();
    let ping_str = config.ping.to_string();

    if template.contains("{types}") {
        // Template-style URL — substitute placeholders
        template
            .replace("{types}", &types_str)
            .replace("{closeafter}", &close_after_str)
            .replace("{ping}", &ping_str)
    } else {
        // Bare URL — append query parameters
        let sep = if template.contains('?') { "&" } else { "?" };
        format!("{template}{sep}types={types_str}&closeafter={close_after_str}&ping={ping_str}")
    }
}

/// Parse an SSE `state` event data payload into a StateChange.
///
/// The event data is JSON:
/// ```json
/// {
///   "changed": {
///     "u1234": {
///       "Email": "s42",
///       "Mailbox": "s15"
///     }
///   }
/// }
/// ```
pub fn parse_state_change(data: &str, account_id: &str) -> Option<StateChange> {
    let json: Value = serde_json::from_str(data).ok()?;
    let changed = json
        .get("changed")
        .and_then(|v| v.as_object())?
        .get(account_id)
        .and_then(|v| v.as_object())?;

    let mut map = HashMap::new();
    for (type_name, state) in changed {
        let Some(state_str) = state.as_str() else {
            continue;
        };
        map.insert(type_name.clone(), state_str.to_string());
    }

    if map.is_empty() {
        None
    } else {
        Some(StateChange { changed: map })
    }
}

/// A parsed SSE event block.
struct SseEvent {
    event_type: Option<String>,
    data: String,
}

/// Split the buffer at the first SSE event boundary (blank line).
/// Returns (event_block, remaining_buffer) or None if no complete event yet.
fn split_sse_event(buf: &str) -> Option<(String, String)> {
    // Try \r\n\r\n first (CRLF), then \n\n (LF)
    buf.find("\r\n\r\n")
        .map(|pos| (buf[..pos].to_string(), buf[pos + 4..].to_string()))
        .or_else(|| {
            buf.find("\n\n")
                .map(|pos| (buf[..pos].to_string(), buf[pos + 2..].to_string()))
        })
}

/// Parse an SSE event block into its `event:` type and concatenated `data:` payload.
fn parse_sse_block(block: &str) -> SseEvent {
    let mut event_type = None;
    let mut data_parts: Vec<&str> = Vec::new();

    for line in block.lines() {
        // Strip trailing CR if present (mixed line endings)
        let line = line.strip_suffix('\r').unwrap_or(line);

        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_parts.push(value.trim());
        }
        // Ignore id:, retry:, comments (:), and unknown fields
    }

    SseEvent {
        event_type,
        // Per SSE spec, multiple data: lines are joined with newlines
        data: data_parts.join("\n"),
    }
}

/// Listen to the EventSource stream and call `on_change` for each state change.
///
/// This is a long-lived connection. Call from a spawned task.
/// Returns when the connection closes or an error occurs.
pub async fn listen(
    client: &JmapClient,
    config: &EventSourceConfig,
    mut on_change: impl FnMut(StateChange),
) -> Result<(), String> {
    let Some(template) = &client.event_source_url else {
        return Err("Server does not support EventSource push".into());
    };

    let url = build_event_source_url(template, config);
    log::info!("Connecting EventSource: {}", url);

    let auth = client.auth_header().await;
    let mut resp = client
        .http
        .get(&url)
        .header("Accept", "text/event-stream")
        .header("Authorization", &auth)
        .send()
        .await
        .map_err(|e| format!("EventSource connect failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("EventSource HTTP {}", resp.status()));
    }

    log::debug!("EventSource connected, streaming events");

    // Stream SSE events as they arrive (long-lived connection).
    // reqwest::Response::chunk() reads the next chunk from the response body
    // without buffering the entire response.
    let mut buffer = String::new();
    let mut events_received: u32 = 0;

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("EventSource read error: {e}"))?
    {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Split on blank lines (SSE event boundary). Accept both \n\n and \r\n\r\n.
        while let Some((block, rest)) = split_sse_event(&buffer) {
            buffer = rest;

            let parsed = parse_sse_block(&block);

            // Only process "state" events (or events with no explicit type, per SSE spec default)
            if let Some(ref et) = parsed.event_type {
                if et != "state" {
                    continue;
                }
            }

            if parsed.data.is_empty() {
                continue;
            }

            if let Some(change) = parse_state_change(&parsed.data, &client.account_id) {
                log::debug!("EventSource: state change received");
                on_change(change);
                events_received += 1;
                if config.close_after > 0 && events_received >= config.close_after {
                    return Ok(());
                }
            }
        }
    }

    log::debug!("EventSource stream ended");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_event_source_url() {
        let template = "https://api.fastmail.com/jmap/event/?types={types}&closeafter={closeafter}&ping={ping}";
        let config = EventSourceConfig {
            types: vec!["Email".into(), "Mailbox".into()],
            close_after: 300,
            ping: 60,
        };

        let url = build_event_source_url(template, &config);
        assert_eq!(
            url,
            "https://api.fastmail.com/jmap/event/?types=Email,Mailbox&closeafter=300&ping=60"
        );
    }

    #[test]
    fn builds_url_with_defaults() {
        let template =
            "https://example.com/event/?types={types}&closeafter={closeafter}&ping={ping}";
        let config = EventSourceConfig::default();
        let url = build_event_source_url(template, &config);
        assert!(url.contains("types=Email,Mailbox"));
        assert!(url.contains("closeafter=0"));
    }

    #[test]
    fn parses_state_change_event() {
        let data = r#"{"changed":{"u1234":{"Email":"s42","Mailbox":"s15"}}}"#;
        let change = parse_state_change(data, "u1234").unwrap();

        assert_eq!(change.changed.get("Email").unwrap(), "s42");
        assert_eq!(change.changed.get("Mailbox").unwrap(), "s15");
    }

    #[test]
    fn parses_single_type_change() {
        let data = r#"{"changed":{"u1234":{"Email":"s99"}}}"#;
        let change = parse_state_change(data, "u1234").unwrap();

        assert_eq!(change.changed.len(), 1);
        assert_eq!(change.changed.get("Email").unwrap(), "s99");
    }

    #[test]
    fn ignores_other_accounts() {
        let data = r#"{"changed":{"u9999":{"Email":"s42"}}}"#;
        let result = parse_state_change(data, "u1234");
        assert!(result.is_none());
    }

    #[test]
    fn handles_invalid_json() {
        assert!(parse_state_change("not json", "u1234").is_none());
        assert!(parse_state_change("{}", "u1234").is_none());
        assert!(parse_state_change(r#"{"changed":{}}"#, "u1234").is_none());
    }

    // -- SSE block parsing ---------------------------------------------------

    #[test]
    fn splits_lf_events() {
        let buf = "data: hello\n\ndata: world\n\n";
        let (block, rest) = split_sse_event(buf).unwrap();
        assert_eq!(block, "data: hello");
        assert_eq!(rest, "data: world\n\n");
    }

    #[test]
    fn splits_crlf_events() {
        let buf = "data: hello\r\n\r\ndata: world\r\n\r\n";
        let (block, rest) = split_sse_event(buf).unwrap();
        assert_eq!(block, "data: hello");
        assert_eq!(rest, "data: world\r\n\r\n");
    }

    #[test]
    fn no_split_on_incomplete() {
        assert!(split_sse_event("data: partial\n").is_none());
        assert!(split_sse_event("data: partial").is_none());
    }

    #[test]
    fn parses_sse_block_with_event_type() {
        let block = "event: state\ndata: {\"foo\":1}";
        let parsed = parse_sse_block(block);
        assert_eq!(parsed.event_type.as_deref(), Some("state"));
        assert_eq!(parsed.data, r#"{"foo":1}"#);
    }

    #[test]
    fn parses_sse_block_no_event_type() {
        let block = "data: {\"foo\":1}";
        let parsed = parse_sse_block(block);
        assert!(parsed.event_type.is_none());
        assert_eq!(parsed.data, r#"{"foo":1}"#);
    }

    #[test]
    fn parses_multiline_data() {
        let block = "data: line1\ndata: line2\ndata: line3";
        let parsed = parse_sse_block(block);
        assert_eq!(parsed.data, "line1\nline2\nline3");
    }

    #[test]
    fn handles_crlf_lines_in_block() {
        let block = "event: state\r\ndata: payload";
        let parsed = parse_sse_block(block);
        assert_eq!(parsed.event_type.as_deref(), Some("state"));
        assert_eq!(parsed.data, "payload");
    }

    #[test]
    fn ignores_non_state_event_types() {
        // Verifies the parse_sse_block captures the event type correctly
        // so the caller can filter on it
        let block = "event: ping\ndata: {}";
        let parsed = parse_sse_block(block);
        assert_eq!(parsed.event_type.as_deref(), Some("ping"));
    }
}
