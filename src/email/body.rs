//! Email body fetching and attachment extraction.

use serde_json::Value;

use crate::client::{JmapClient, JmapError};
use crate::mime;
use crate::models::AttachmentData;
use crate::parse;

/// Properties for body fetch (Email/get with bodyValues).
const BODY_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
];

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

    // Try bodyValues path first (text is inline — no blob downloads needed)
    let body_values = email.get("bodyValues").and_then(|v| v.as_object());
    let text_body = email.get("textBody").and_then(|v| v.as_array());
    let html_body = email.get("htmlBody").and_then(|v| v.as_array());

    let text_plain = extract_body_value(body_values, text_body);
    let text_html = extract_body_value(body_values, html_body);

    if text_plain.is_some() || text_html.is_some() {
        let markdown = mime::render_body_markdown(text_plain.as_deref(), text_html.as_deref());
        let plain = mime::render_body(text_plain.as_deref(), text_html.as_deref());
        let attachments = extract_attachments(client, email).await;
        return Ok((markdown, plain, attachments));
    }

    // Fallback: download raw blob and parse via RFC 5322
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

    // Merge parsed attachments with any blob-based ones we got
    let mut all_attachments = extract_attachments(client, email).await;
    all_attachments.extend(parsed.attachments);

    Ok((markdown, plain, all_attachments))
}

/// Extract body text from bodyValues using the part IDs in textBody/htmlBody.
fn extract_body_value(
    body_values: Option<&serde_json::Map<String, Value>>,
    body_parts: Option<&Vec<Value>>,
) -> Option<String> {
    let values = body_values?;
    let parts = body_parts?;
    let mut combined = String::new();
    for part in parts {
        let Some(part_id) = part.get("partId").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(value) = values.get(part_id).and_then(|v| v.get("value")).and_then(|v| v.as_str()) else {
            continue;
        };
        if !value.is_empty() {
            combined.push_str(value);
        }
    }
    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

/// Extract all downloadable parts: explicit attachments + inline images from body parts.
///
/// JMAP's `attachments` property excludes inline images (disposition != "attachment").
/// For emails with inline photos, the images appear in `htmlBody`/`textBody` arrays
/// as parts with image/* types and blobIds but no bodyValue. We download those too.
///
/// Individual blob download failures are logged and skipped rather than
/// aborting the entire body fetch.
async fn extract_attachments(
    client: &JmapClient,
    email: &Value,
) -> Vec<AttachmentData> {
    let body_values = email.get("bodyValues").and_then(|v| v.as_object());
    let mut result = Vec::new();
    let mut seen_blobs = std::collections::HashSet::new();

    // 1. Explicit attachments (JMAP `attachments` property)
    if let Some(atts) = email.get("attachments").and_then(|v| v.as_array()) {
        for att in atts {
            let Some(blob_id) = att.get("blobId").and_then(|v| v.as_str()) else {
                continue;
            };
            seen_blobs.insert(blob_id.to_string());
            download_part(client, att, blob_id, &mut result).await;
        }
    }

    // 2. Inline image parts from htmlBody/textBody that aren't in bodyValues
    for key in ["htmlBody", "textBody"] {
        let Some(parts) = email.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for part in parts {
            let Some(blob_id) = part.get("blobId").and_then(|v| v.as_str()) else {
                continue;
            };
            // Skip if already downloaded as explicit attachment
            if seen_blobs.contains(blob_id) {
                continue;
            }
            // Skip text parts that have bodyValues (already handled as body text)
            let part_id = part.get("partId").and_then(|v| v.as_str()).unwrap_or("");
            if body_values.is_some_and(|bv| bv.contains_key(part_id)) {
                continue;
            }
            // Only download image parts
            let mime = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if !mime.starts_with("image/") {
                continue;
            }
            seen_blobs.insert(blob_id.to_string());
            download_part(client, part, blob_id, &mut result).await;
        }
    }

    result
}

async fn download_part(
    client: &JmapClient,
    part: &Value,
    blob_id: &str,
    result: &mut Vec<AttachmentData>,
) {
    let filename = part
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed")
        .to_string();
    let mime_type = part
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("application/octet-stream")
        .to_string();

    match client.download_blob(blob_id).await {
        Ok(data) => {
            result.push(AttachmentData {
                filename,
                mime_type,
                data,
            });
        }
        Err(e) => {
            log::warn!(
                "Skipping attachment '{}' (blob {}): {}",
                filename,
                blob_id,
                e,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
