//! JMAP EmailSubmission (RFC 8621 §7).
//!
//! Replaces SMTP entirely — sending is a JSON POST.
//! Creates a draft email, then submits it for delivery in a single batched request.
//!
//! Creation references (RFC 8620 §5.3): When `Email/set` creates an object with
//! creation ID `"draft"`, subsequent methods in the same request can reference
//! it as `"#draft"`. The server resolves this to the actual server-assigned ID.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{JmapClient, JmapError};

/// A sender identity from Identity/get.
///
/// Field mapping: JMAP uses camelCase (`mayDelete`), we use snake_case.
/// Parsing is manual rather than serde rename to keep the JSON handling
/// consistent with the rest of the codebase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub email: String,
    /// JMAP field: `mayDelete` (camelCase in JSON).
    pub may_delete: bool,
}

/// Fetch all identities for the account.
///
/// Identities represent sender addresses the user can send from.
pub async fn get_identities(client: &JmapClient) -> Result<Vec<Identity>, JmapError> {
    let call = client.method(
        "Identity/get",
        serde_json::json!({
            "properties": ["id", "name", "email", "mayDelete"],
        }),
        "i0",
    );

    let resp = client.call(vec![call]).await?;

    let list = resp
        .method_responses
        .iter()
        .find(|mc| mc.2 == "i0")
        .and_then(|mc| mc.1.get("list"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| JmapError::RequestError("Missing list in Identity/get response".into()))?;

    Ok(parse_identities(list))
}

/// Find the best identity for a given sender address.
///
/// Matching order:
/// 1. Exact match (`mental@neverlight.com` == `mental@neverlight.com`)
/// 2. Wildcard domain match (`mental@neverlight.com` matches `*@neverlight.com`)
/// 3. Falls back to the first identity if nothing matches
///
/// Returns `None` only when the identity list is empty.
pub fn find_identity_for_address<'a>(
    identities: &'a [Identity],
    from_address: &str,
) -> Option<&'a Identity> {
    let from_lower = from_address.to_lowercase();
    let from_domain = from_lower.rsplit_once('@').map(|(_, d)| d);

    // Exact match
    if let Some(id) = identities.iter().find(|id| id.email.to_lowercase() == from_lower) {
        return Some(id);
    }

    // Wildcard domain match: identity email is `*@domain`
    if let Some(domain) = from_domain {
        let wildcard = format!("*@{domain}");
        if let Some(id) = identities.iter().find(|id| id.email.to_lowercase() == wildcard) {
            return Some(id);
        }
    }

    // Fallback
    identities.first()
}

/// Parameters for sending an email.
pub struct SendRequest<'a> {
    pub identity_id: &'a str,
    pub from: &'a str,
    pub to: &'a [String],
    pub cc: &'a [String],
    pub subject: &'a str,
    pub text_body: &'a str,
    pub html_body: Option<&'a str>,
    pub drafts_mailbox_id: &'a str,
    pub sent_mailbox_id: &'a str,
    /// RFC 5322 Message-ID of the parent message (for replies).
    pub in_reply_to: Option<&'a str>,
    /// Space-separated RFC 5322 Message-ID chain (for threading).
    pub references: Option<&'a str>,
}

/// Build the Email/set create object for a draft.
///
/// Separated from `send()` so the exact JSON payload is unit-testable
/// without a live server.
fn build_draft_create(req: &SendRequest<'_>) -> Value {
    let to_addrs: Vec<Value> = req.to
        .iter()
        .map(|addr| serde_json::json!({ "email": addr }))
        .collect();
    let cc_addrs: Vec<Value> = req.cc
        .iter()
        .map(|addr| serde_json::json!({ "email": addr }))
        .collect();

    let mut email_create = serde_json::json!({
        "mailboxIds": { (req.drafts_mailbox_id): true },
        "from": [{ "email": req.from }],
        "to": to_addrs,
        "cc": cc_addrs,
        "subject": req.subject,
        "keywords": { "$draft": true },
        "bodyValues": {
            "text": { "value": req.text_body },
        },
        "textBody": [{ "partId": "text", "type": "text/plain" }],
    });

    // Only include htmlBody when HTML content is provided.
    // Omitting it entirely tells the server this is a plain-text-only message.
    if let Some(html) = req.html_body {
        email_create["bodyValues"]["html"] = serde_json::json!({ "value": html });
        email_create["htmlBody"] = serde_json::json!([{ "partId": "html", "type": "text/html" }]);
    }

    // Threading headers (RFC 8621 §4.1.4: inReplyTo and references are String[] arrays).
    if let Some(irt) = req.in_reply_to {
        email_create["inReplyTo"] = serde_json::json!([irt]);
    }
    if let Some(refs) = req.references {
        let ids: Vec<&str> = refs.split_whitespace().collect();
        email_create["references"] = serde_json::json!(ids);
    }

    email_create
}

/// Build the EmailSubmission/set create object.
///
/// Uses `"#draft"` as a creation reference (RFC 8620 §5.3) to reference the
/// email created by `Email/set { create: { "draft": ... } }` in the same request.
///
/// The `envelope` is intentionally omitted. Per RFC 8621 §7, the server derives
/// it from the email headers and the identity when not provided. This avoids
/// SMTP relay rejections for custom-domain addresses (e.g. Fastmail validating
/// `mailFrom` independently of the identity's allowed addresses).
fn build_submission_create(req: &SendRequest<'_>) -> Value {
    serde_json::json!({
        "identityId": req.identity_id,
        "emailId": "#draft",
    })
}

/// Build the onSuccessUpdateEmail patch for post-send cleanup.
///
/// Moves the email from Drafts to Sent, removes the $draft keyword,
/// and marks as $seen (app behavior choice — sent mail shows as read).
fn build_on_success_patch(req: &SendRequest<'_>) -> Value {
    let drafts_key = format!("mailboxIds/{}", req.drafts_mailbox_id);
    let sent_key = format!("mailboxIds/{}", req.sent_mailbox_id);

    serde_json::json!({
        (drafts_key): null,
        (sent_key): true,
        "keywords/$draft": null,
        "keywords/$seen": true,
    })
}

/// Send an email using a single batched request:
/// 1. `Email/set` create — creates the draft with body parts
/// 2. `EmailSubmission/set` — submits for delivery via creation reference `#draft`
/// 3. `onSuccessUpdateEmail` — moves from Drafts→Sent on success
pub async fn send(
    client: &JmapClient,
    req: &SendRequest<'_>,
) -> Result<String, JmapError> {
    log::info!(
        "submit::send: from={}, to={:?}, cc={:?}, identity={}, subject={}",
        req.from, req.to, req.cc, req.identity_id, req.subject,
    );
    log::debug!(
        "submit::send: drafts_mailbox={}, sent_mailbox={}",
        req.drafts_mailbox_id, req.sent_mailbox_id,
    );

    let email_create = build_draft_create(req);
    let submission_create = build_submission_create(req);
    let on_success_patch = build_on_success_patch(req);

    log::debug!("submit::send: draft payload: {}", email_create);
    log::debug!("submit::send: submission payload: {}", submission_create);

    let create_call = client.method(
        "Email/set",
        serde_json::json!({
            "create": { "draft": email_create },
        }),
        "c0",
    );

    let submit_call = client.method(
        "EmailSubmission/set",
        serde_json::json!({
            "create": { "send": submission_create },
            "onSuccessUpdateEmail": {
                "#send": on_success_patch,
            },
        }),
        "s0",
    );

    log::info!("submit::send: calling JMAP (Email/set + EmailSubmission/set)");
    let resp = client.call(vec![create_call, submit_call]).await?;
    log::debug!(
        "submit::send: got {} method responses",
        resp.method_responses.len(),
    );

    // Check for Email/set create errors
    let create_resp = resp.method_responses.iter().find(|mc| mc.2 == "c0");
    if let Some(cr) = create_resp {
        log::debug!("submit::send: Email/set response: {}", cr.1);
        if let Some(errors) = cr.1.get("notCreated").and_then(|v| v.as_object()) {
            if let Some(err) = errors.get("draft") {
                let err_type = err.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
                let desc = err.get("description").and_then(|v| v.as_str()).unwrap_or("");
                log::error!(
                    "submit::send: Email/set create failed: type={}, desc={}",
                    err_type, desc,
                );
                return Err(JmapError::MethodError {
                    method: "Email/set create".into(),
                    error_type: err_type.into(),
                    description: desc.into(),
                });
            }
        }
    }

    // Check for EmailSubmission/set errors
    let submit_resp = resp.method_responses.iter().find(|mc| mc.2 == "s0");
    if let Some(sr) = submit_resp {
        log::debug!("submit::send: EmailSubmission/set response: {}", sr.1);
        if let Some(errors) = sr.1.get("notCreated").and_then(|v| v.as_object()) {
            if let Some(err) = errors.get("send") {
                let err_type = err.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
                let desc = err.get("description").and_then(|v| v.as_str()).unwrap_or("");
                log::error!(
                    "submit::send: EmailSubmission/set failed: type={}, desc={}",
                    err_type, desc,
                );
                return Err(JmapError::MethodError {
                    method: "EmailSubmission/set".into(),
                    error_type: err_type.into(),
                    description: desc.into(),
                });
            }
        }
    }

    // Extract the created email ID
    let email_id = create_resp
        .and_then(|cr| cr.1.get("created"))
        .and_then(|v| v.get("draft"))
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    log::info!("submit::send: success, email_id={}", email_id);
    Ok(email_id)
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_identities(list: &[Value]) -> Vec<Identity> {
    list.iter()
        .map(|item| Identity {
            id: item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            name: item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            email: item.get("email").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            may_delete: item.get("mayDelete").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> SendRequest<'static> {
        SendRequest {
            identity_id: "id-1",
            from: "alice@example.com",
            to: &[],
            cc: &[],
            subject: "Test",
            text_body: "Hello",
            html_body: None,
            drafts_mailbox_id: "mb-drafts",
            sent_mailbox_id: "mb-sent",
            in_reply_to: None,
            references: None,
        }
    }

    // -- Identity parsing --

    #[test]
    fn parses_identity_list() {
        let list: Vec<Value> = serde_json::from_value(serde_json::json!([
            {
                "id": "id-1",
                "name": "Jason Stelzer",
                "email": "jason@example.com",
                "mayDelete": false
            },
            {
                "id": "id-2",
                "name": "Jay",
                "email": "jay@neverlight.com",
                "mayDelete": true
            }
        ]))
        .unwrap();

        let identities = parse_identities(&list);
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0].id, "id-1");
        assert_eq!(identities[0].email, "jason@example.com");
        assert!(!identities[0].may_delete);
        assert_eq!(identities[1].name, "Jay");
        assert!(identities[1].may_delete);
    }

    #[test]
    fn identity_handles_missing_fields() {
        let list: Vec<Value> = serde_json::from_value(serde_json::json!([
            { "id": "id-1" }
        ]))
        .unwrap();

        let identities = parse_identities(&list);
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].id, "id-1");
        assert_eq!(identities[0].name, "");
        assert_eq!(identities[0].email, "");
    }

    // -- Draft payload: plain text only --

    #[test]
    fn draft_plain_text_only_omits_html_body() {
        let to = vec!["bob@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            ..sample_request()
        };

        let draft = build_draft_create(&req);

        // textBody should reference "text" part
        assert_eq!(draft["textBody"][0]["partId"], "text");
        assert_eq!(draft["textBody"][0]["type"], "text/plain");

        // htmlBody should NOT be present
        assert!(draft.get("htmlBody").is_none(), "htmlBody should be omitted for plain-text-only");

        // bodyValues should only have "text"
        assert!(draft["bodyValues"].get("text").is_some());
        assert!(draft["bodyValues"].get("html").is_none());
        assert_eq!(draft["bodyValues"]["text"]["value"], "Hello");
    }

    // -- Draft payload: text + html --

    #[test]
    fn draft_with_html_includes_html_body() {
        let to = vec!["bob@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            html_body: Some("<p>Hello</p>"),
            ..sample_request()
        };

        let draft = build_draft_create(&req);

        // textBody present
        assert_eq!(draft["textBody"][0]["partId"], "text");

        // htmlBody present and references html part
        assert_eq!(draft["htmlBody"][0]["partId"], "html");
        assert_eq!(draft["htmlBody"][0]["type"], "text/html");

        // bodyValues has both
        assert_eq!(draft["bodyValues"]["text"]["value"], "Hello");
        assert_eq!(draft["bodyValues"]["html"]["value"], "<p>Hello</p>");
    }

    // -- Draft payload: multiple recipients --

    #[test]
    fn draft_multiple_recipients() {
        let to = vec!["bob@example.com".to_string(), "carol@example.com".to_string()];
        let cc = vec!["dave@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            cc: &cc,
            ..sample_request()
        };

        let draft = build_draft_create(&req);

        let to_arr = draft["to"].as_array().unwrap();
        assert_eq!(to_arr.len(), 2);
        assert_eq!(to_arr[0]["email"], "bob@example.com");
        assert_eq!(to_arr[1]["email"], "carol@example.com");

        let cc_arr = draft["cc"].as_array().unwrap();
        assert_eq!(cc_arr.len(), 1);
        assert_eq!(cc_arr[0]["email"], "dave@example.com");
    }

    // -- Submission payload --

    #[test]
    fn submission_references_draft_creation_id() {
        let to = vec!["bob@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            ..sample_request()
        };

        let sub = build_submission_create(&req);

        // emailId uses creation reference (RFC 8620 §5.3)
        assert_eq!(sub["emailId"], "#draft");
        assert_eq!(sub["identityId"], "id-1");
        // envelope is intentionally omitted — server derives it from headers + identity
        assert!(sub.get("envelope").is_none());
    }

    // -- On-success patch --

    #[test]
    fn on_success_patch_moves_drafts_to_sent() {
        let req = sample_request();
        let patch = build_on_success_patch(&req);

        // Removes from drafts
        assert!(patch["mailboxIds/mb-drafts"].is_null());
        // Adds to sent
        assert_eq!(patch["mailboxIds/mb-sent"], true);
        // Removes draft keyword
        assert!(patch["keywords/$draft"].is_null());
        // Marks as seen (app behavior)
        assert_eq!(patch["keywords/$seen"], true);
    }

    // -- Draft payload: mailbox and metadata --

    #[test]
    fn draft_targets_correct_mailbox() {
        let to = vec!["bob@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            ..sample_request()
        };

        let draft = build_draft_create(&req);

        assert_eq!(draft["mailboxIds"]["mb-drafts"], true);
        assert_eq!(draft["subject"], "Test");
        assert_eq!(draft["from"][0]["email"], "alice@example.com");
        assert_eq!(draft["keywords"]["$draft"], true);
    }

    // -- Draft payload: threading headers --

    #[test]
    fn draft_omits_threading_headers_when_none() {
        let to = vec!["bob@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            ..sample_request()
        };
        let draft = build_draft_create(&req);
        assert!(draft.get("inReplyTo").is_none());
        assert!(draft.get("references").is_none());
    }

    #[test]
    fn draft_includes_in_reply_to() {
        let to = vec!["bob@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            in_reply_to: Some("<parent@example.com>"),
            ..sample_request()
        };
        let draft = build_draft_create(&req);
        let irt = draft["inReplyTo"].as_array().unwrap();
        assert_eq!(irt.len(), 1);
        assert_eq!(irt[0], "<parent@example.com>");
    }

    #[test]
    fn draft_includes_references_chain() {
        let to = vec!["bob@example.com".to_string()];
        let req = SendRequest {
            to: &to,
            references: Some("<root@example.com> <parent@example.com>"),
            ..sample_request()
        };
        let draft = build_draft_create(&req);
        let refs = draft["references"].as_array().unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0], "<root@example.com>");
        assert_eq!(refs[1], "<parent@example.com>");
    }

    // -- Identity matching --

    fn test_identities() -> Vec<Identity> {
        vec![
            Identity {
                id: "id-fm".into(),
                name: "Jason Stelzer".into(),
                email: "jstelzer@fastmail.com".into(),
                may_delete: false,
            },
            Identity {
                id: "id-nl".into(),
                name: "Jason Stelzer".into(),
                email: "*@neverlight.com".into(),
                may_delete: false,
            },
            Identity {
                id: "id-eb".into(),
                name: "Jason Stelzer".into(),
                email: "cynic@elitistbastard.com".into(),
                may_delete: true,
            },
        ]
    }

    #[test]
    fn identity_exact_match() {
        let ids = test_identities();
        let found = find_identity_for_address(&ids, "jstelzer@fastmail.com").unwrap();
        assert_eq!(found.id, "id-fm");
    }

    #[test]
    fn identity_exact_match_case_insensitive() {
        let ids = test_identities();
        let found = find_identity_for_address(&ids, "Jstelzer@Fastmail.COM").unwrap();
        assert_eq!(found.id, "id-fm");
    }

    #[test]
    fn identity_wildcard_domain_match() {
        let ids = test_identities();
        let found = find_identity_for_address(&ids, "mental@neverlight.com").unwrap();
        assert_eq!(found.id, "id-nl");
    }

    #[test]
    fn identity_wildcard_any_localpart() {
        let ids = test_identities();
        let found = find_identity_for_address(&ids, "anything@neverlight.com").unwrap();
        assert_eq!(found.id, "id-nl");
    }

    #[test]
    fn identity_wildcard_case_insensitive() {
        let ids = test_identities();
        let found = find_identity_for_address(&ids, "Mental@Neverlight.COM").unwrap();
        assert_eq!(found.id, "id-nl");
    }

    #[test]
    fn identity_fallback_to_first() {
        let ids = test_identities();
        let found = find_identity_for_address(&ids, "unknown@other.com").unwrap();
        assert_eq!(found.id, "id-fm");
    }

    #[test]
    fn identity_empty_list_returns_none() {
        assert!(find_identity_for_address(&[], "foo@bar.com").is_none());
    }
}
