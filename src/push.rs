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
pub fn build_event_source_url(
    template: &str,
    config: &EventSourceConfig,
) -> String {
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
        format!(
            "{template}{sep}types={types_str}&closeafter={close_after_str}&ping={ping_str}"
        )
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
    let http = reqwest::Client::new();
    let mut resp = http
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

    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("EventSource read error: {e}"))? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // SSE events are terminated by a blank line (\n\n)
        while let Some(pos) = buffer.find("\n\n") {
            let event_block = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            for line in event_block.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }

                if let Some(change) = parse_state_change(data, &client.account_id) {
                    log::debug!("EventSource: state change received");
                    on_change(change);
                    events_received += 1;
                    if config.close_after > 0 && events_received >= config.close_after {
                        return Ok(());
                    }
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
        let template = "https://example.com/event/?types={types}&closeafter={closeafter}&ping={ping}";
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
}
