//! Integration tests against a live JMAP server (Fastmail).
//!
//! Requires environment variables:
//!   NEVERLIGHT_MAIL_JMAP_TOKEN — Fastmail API token (fmu1-...) or app password (mu1-...)
//!   NEVERLIGHT_MAIL_USER       — Account email address
//!
//! Source from `.envrc` in the crate root.
//! Skipped automatically when env vars are missing.

use neverlight_mail_core::session::JmapSession;
use neverlight_mail_core::store::CacheHandle;
use neverlight_mail_core::types::FlagOp;
use neverlight_mail_core::{client::JmapClient, email, mailbox, push, submit, sync};

const FASTMAIL_SESSION_URL: &str = "https://api.fastmail.com/jmap/session";

macro_rules! skip_if_no_env {
    () => {
        if std::env::var("NEVERLIGHT_MAIL_JMAP_TOKEN").is_err()
            || std::env::var("NEVERLIGHT_MAIL_USER").is_err()
        {
            eprintln!("SKIP: NEVERLIGHT_MAIL_JMAP_TOKEN or NEVERLIGHT_MAIL_USER not set");
            return;
        }
    };
}

/// Connect using the appropriate auth method based on token prefix.
/// - `fmu1-` → Bearer token (API token)
/// - `mu1-`  → Basic auth (app password, needs username)
async fn connect_client() -> (JmapSession, JmapClient) {
    let token = std::env::var("NEVERLIGHT_MAIL_JMAP_TOKEN").expect("NEVERLIGHT_MAIL_JMAP_TOKEN");
    let user = std::env::var("NEVERLIGHT_MAIL_USER").expect("NEVERLIGHT_MAIL_USER");

    if token.starts_with("fmu1-") {
        // API token — use Bearer auth
        JmapSession::connect_with_token(FASTMAIL_SESSION_URL, &token)
            .await
            .expect("Bearer auth failed")
    } else {
        // App password — use Basic auth with username:password
        JmapSession::connect_with_basic(FASTMAIL_SESSION_URL, &user, &token)
            .await
            .expect("Basic auth failed")
    }
}

// ---------------------------------------------------------------------------
// Phase 1a: Session + Mailbox discovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_connects_and_discovers_capabilities() {
    skip_if_no_env!();
    let (session, _client) = connect_client().await;

    assert!(!session.account_id.is_empty(), "account_id should be non-empty");
    assert!(!session.api_url.is_empty(), "api_url should be non-empty");
    assert!(session.max_objects_in_get > 0, "max_objects_in_get should be > 0");
    eprintln!(
        "Connected: account_id={}, max_get={}, max_set={}, max_calls={}",
        session.account_id,
        session.max_objects_in_get,
        session.max_objects_in_set,
        session.max_calls_in_request
    );
}

#[tokio::test]
async fn fetches_mailboxes_with_inbox() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let folders = mailbox::fetch_all(&client)
        .await
        .expect("fetch_all failed");

    assert!(!folders.is_empty(), "should have at least one mailbox");

    let inbox = mailbox::find_by_role(&folders, "inbox");
    assert!(inbox.is_some(), "should have an inbox");

    // Verify inbox is sorted first
    assert_eq!(
        folders[0].role.as_deref(),
        Some("inbox"),
        "inbox should be first after sorting"
    );

    eprintln!("Mailboxes ({}):", folders.len());
    for f in &folders {
        eprintln!(
            "  {} [{}] role={:?} unread={} total={}",
            f.path, f.mailbox_id, f.role, f.unread_count, f.total_count
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 1b: Email query + get
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queries_inbox_messages() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let inbox_id = mailbox::find_by_role(&folders, "inbox").expect("no inbox found");

    let (messages, query_result) = email::query_and_get(&client, &inbox_id, 10, 0)
        .await
        .expect("query_and_get failed");

    assert!(query_result.total > 0, "inbox should have messages");
    assert!(!messages.is_empty(), "should return at least one message");

    let first = &messages[0];
    assert!(!first.email_id.is_empty(), "email_id should be non-empty");

    eprintln!(
        "Inbox: {} total, {} returned",
        query_result.total,
        messages.len()
    );
    for m in &messages {
        let read = if m.is_read { " " } else { "●" };
        let star = if m.is_starred { "★" } else { " " };
        eprintln!("  {read}{star} {} — {} ({})", m.from, m.subject, m.date);
    }
}

// ---------------------------------------------------------------------------
// Phase 1c: Body fetch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetches_message_body() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let inbox_id = mailbox::find_by_role(&folders, "inbox").expect("no inbox found");

    let (messages, _) = email::query_and_get(&client, &inbox_id, 1, 0)
        .await
        .expect("query_and_get");

    let Some(msg) = messages.first() else {
        eprintln!("SKIP: inbox is empty");
        return;
    };

    let (markdown, plain, attachments) = email::get_body(&client, &msg.email_id)
        .await
        .expect("get_body failed");

    assert!(
        !markdown.is_empty() || !plain.is_empty(),
        "body should have some content"
    );

    eprintln!("Body for '{}' ({}):", msg.subject, msg.email_id);
    eprintln!("  Markdown: {} chars", markdown.len());
    eprintln!("  Plain: {} chars", plain.len());
    eprintln!("  Attachments: {}", attachments.len());
    if !markdown.is_empty() {
        let preview = &markdown[..markdown.len().min(200)];
        eprintln!("  Preview: {preview}");
    }
}

// ---------------------------------------------------------------------------
// Phase 2a: Flag operations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn toggle_read_flag() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let inbox_id = mailbox::find_by_role(&folders, "inbox").expect("no inbox");

    let (messages, _) = email::query_and_get(&client, &inbox_id, 5, 0)
        .await
        .expect("query_and_get");

    let Some(msg) = messages.first() else {
        eprintln!("SKIP: inbox is empty");
        return;
    };

    let original_read = msg.is_read;
    let email_id = &msg.email_id;

    // Toggle: if read, mark unread; if unread, mark read
    email::set_flag(&client, email_id, &FlagOp::SetSeen(!original_read))
        .await
        .expect("set_flag failed");

    // Re-fetch and verify
    let updated = email::get_summaries(&client, &[email_id.clone()], &inbox_id)
        .await
        .expect("get_summaries");
    assert_eq!(updated[0].is_read, !original_read, "flag should have toggled");

    // Restore original state
    email::set_flag(&client, email_id, &FlagOp::SetSeen(original_read))
        .await
        .expect("restore flag failed");

    eprintln!(
        "Toggled read flag on '{}': {} → {} → {} (restored)",
        msg.subject, original_read, !original_read, original_read
    );
}

// ---------------------------------------------------------------------------
// Phase 2b: Move + Delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn move_message_to_trash_and_back() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let inbox_id = mailbox::find_by_role(&folders, "inbox").expect("no inbox");
    let trash_id = mailbox::find_by_role(&folders, "trash").expect("no trash");

    let (messages, _) = email::query_and_get(&client, &inbox_id, 5, 0)
        .await
        .expect("query_and_get");

    let Some(msg) = messages.first() else {
        eprintln!("SKIP: inbox is empty");
        return;
    };
    let email_id = &msg.email_id;

    // Move to trash
    email::trash(&client, email_id, &inbox_id, &trash_id)
        .await
        .expect("trash failed");

    // Verify it's in trash
    let in_trash = email::get_summaries(&client, &[email_id.clone()], &trash_id)
        .await
        .expect("get from trash");
    assert_eq!(in_trash.len(), 1, "message should be in trash");

    // Move back to inbox
    email::move_to(&client, email_id, &trash_id, &inbox_id)
        .await
        .expect("move back failed");

    // Verify it's back in inbox
    let in_inbox = email::get_summaries(&client, &[email_id.clone()], &inbox_id)
        .await
        .expect("get from inbox");
    assert_eq!(in_inbox.len(), 1, "message should be back in inbox");

    eprintln!(
        "Moved '{}' to trash and back: {}",
        msg.subject, email_id
    );
}

// ---------------------------------------------------------------------------
// Phase 2c: Identity + Send
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetches_identities() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let identities = submit::get_identities(&client)
        .await
        .expect("get_identities failed");

    assert!(!identities.is_empty(), "should have at least one identity");

    eprintln!("Identities ({}):", identities.len());
    for id in &identities {
        eprintln!("  {} — {} <{}>", id.id, id.name, id.email);
    }
}

#[tokio::test]
async fn send_test_email() {
    skip_if_no_env!();
    if std::env::var("NEVERLIGHT_MAIL_TEST_SEND").as_deref() != Ok("true") {
        eprintln!("SKIP: NEVERLIGHT_MAIL_TEST_SEND not set to 'true'");
        return;
    }

    let (_session, client) = connect_client().await;
    let user = std::env::var("NEVERLIGHT_MAIL_USER").unwrap();

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let drafts_id = mailbox::find_by_role(&folders, "drafts").expect("no drafts");
    let sent_id = mailbox::find_by_role(&folders, "sent").expect("no sent");

    let identities = submit::get_identities(&client).await.expect("identities");
    let identity = &identities[0];

    let req = submit::SendRequest {
        identity_id: &identity.id,
        from: &identity.email,
        to: &[user.clone()],
        cc: &[],
        subject: "[neverlight-mail-core test] Integration test email",
        text_body: "This is an automated test email from neverlight-mail-core integration tests.\n\nIf you see this, EmailSubmission/set is working.",
        html_body: None,
        drafts_mailbox_id: &drafts_id,
        sent_mailbox_id: &sent_id,
        in_reply_to: None,
        references: None,
    };

    let email_id = submit::send(&client, &req)
        .await
        .expect("send failed");

    assert!(!email_id.is_empty(), "should return created email ID");
    eprintln!("Sent test email: id={email_id}");
}

#[tokio::test]
async fn send_to_external_address() {
    skip_if_no_env!();
    if std::env::var("NEVERLIGHT_MAIL_TEST_SEND").as_deref() != Ok("true") {
        eprintln!("SKIP: NEVERLIGHT_MAIL_TEST_SEND not set to 'true'");
        return;
    }

    let (_session, client) = connect_client().await;

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let drafts_id = mailbox::find_by_role(&folders, "drafts").expect("no drafts");
    let sent_id = mailbox::find_by_role(&folders, "sent").expect("no sent");

    let identities = submit::get_identities(&client).await.expect("identities");
    eprintln!("Available identities:");
    for id in &identities {
        eprintln!("  {} — {} <{}>", id.id, id.name, id.email);
    }

    let from_addr = "mental@neverlight.com";
    let identity = submit::find_identity_for_address(&identities, from_addr)
        .expect("no matching identity");
    eprintln!("Using identity: {} <{}> (for {})", identity.name, identity.email, from_addr);

    let to = vec!["jason.stelzer@gmail.com".to_string()];
    let req = submit::SendRequest {
        identity_id: &identity.id,
        from: from_addr,
        to: &to,
        cc: &[],
        subject: "[neverlight-mail-core] External send test",
        text_body: "Testing JMAP EmailSubmission to an external (gmail) address.\n\nIf you got this, it works.",
        html_body: None,
        drafts_mailbox_id: &drafts_id,
        sent_mailbox_id: &sent_id,
        in_reply_to: None,
        references: None,
    };

    eprintln!("Sending: from={}, to={:?}", from_addr, to);
    match submit::send(&client, &req).await {
        Ok(email_id) => {
            eprintln!("SUCCESS: email_id={email_id}");
            assert!(!email_id.is_empty());
        }
        Err(e) => {
            panic!("Send to external address failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 3a: Delta sync
// ---------------------------------------------------------------------------

/// Open a throwaway cache for integration tests.
fn test_cache() -> CacheHandle {
    CacheHandle::open("integration-test").expect("open cache")
}

#[tokio::test]
async fn sync_mailboxes_full_then_delta() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;
    let cache = test_cache();

    // First sync — should do a full fetch (no prior state)
    let folders = sync::sync_mailboxes(&client, &cache, &client.account_id)
        .await
        .expect("first sync failed");

    assert!(!folders.is_empty(), "should have mailboxes");
    let has_inbox = folders.iter().any(|f| f.role.as_deref() == Some("inbox"));
    assert!(has_inbox, "should have inbox");

    // State should now be persisted
    let state = cache
        .get_state(client.account_id.clone(), "Mailbox".to_string())
        .await
        .expect("get_state")
        .expect("state should exist after first sync");
    assert!(!state.is_empty(), "state token should be non-empty");
    eprintln!("Mailbox state after full sync: {state}");

    // Second sync — should take the delta path (state exists)
    let folders2 = sync::sync_mailboxes(&client, &cache, &client.account_id)
        .await
        .expect("delta sync failed");

    assert_eq!(
        folders.len(),
        folders2.len(),
        "delta sync should return same folder count"
    );

    // State should still be present (possibly same, possibly updated)
    let state2 = cache
        .get_state(client.account_id.clone(), "Mailbox".to_string())
        .await
        .expect("get_state")
        .expect("state should still exist");
    eprintln!("Mailbox state after delta sync: {state2}");
}

#[tokio::test]
async fn sync_emails_head_then_delta() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;
    let cache = test_cache();

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let inbox_id = mailbox::find_by_role(&folders, "inbox").expect("no inbox");

    // First sync — may be full or delta with cannotCalculateChanges fallback
    let messages = sync::sync_emails(&client, &cache, &client.account_id, &inbox_id, 10)
        .await
        .expect("first email sync failed");

    assert!(!messages.is_empty(), "inbox should have messages");
    eprintln!("Email sync: {} messages on first sync", messages.len());

    let resource = "Email".to_string();
    let state = cache
        .get_state(client.account_id.clone(), resource.clone())
        .await
        .expect("get_state")
        .expect("email state should exist");
    eprintln!("Email state after full sync: {state}");

    // Second sync — delta path
    let messages2 = sync::sync_emails(&client, &cache, &client.account_id, &inbox_id, 10)
        .await
        .expect("delta email sync failed");

    // Should return same messages from cache (no changes expected in this window)
    assert!(!messages2.is_empty(), "delta sync should return cached messages");
    eprintln!("Email sync: {} messages on delta sync", messages2.len());
}

// ---------------------------------------------------------------------------
// Phase 3b: EventSource push
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_provides_event_source_url() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    assert!(
        client.event_source_url.is_some(),
        "Fastmail should provide an eventSourceUrl"
    );

    let template = client.event_source_url.as_ref().unwrap();
    eprintln!("EventSource template: {template}");

    // Fastmail's eventSourceUrl may or may not contain template variables.
    // If it has {types}, build_event_source_url substitutes them.
    // If not (e.g. bare URL), substitution is a no-op — query params must be appended.
    let config = push::EventSourceConfig::default();
    let url = push::build_event_source_url(template, &config);

    assert!(!url.is_empty(), "URL should be non-empty");
    assert!(!url.contains("{types}"), "template vars should be replaced");
    assert!(!url.contains("{closeafter}"), "template vars should be replaced");
    assert!(!url.contains("{ping}"), "template vars should be replaced");
    eprintln!("Built EventSource URL: {url}");
}

// ---------------------------------------------------------------------------
// Capability discovery
// ---------------------------------------------------------------------------

/// Prove that Fastmail advertises push and submission capabilities.
///
/// Fetches the raw JMAP session JSON and checks for the URNs that
/// `discovery::parse_session_object` maps to `supports_push` and
/// `supports_submission`.  If both are present, any config showing
/// `false` was written without running discovery.
#[tokio::test]
async fn fastmail_advertises_push_and_submission() {
    skip_if_no_env!();
    let token = std::env::var("NEVERLIGHT_MAIL_JMAP_TOKEN").unwrap();
    let user = std::env::var("NEVERLIGHT_MAIL_USER").unwrap();

    let http = reqwest::Client::new();
    let req = http.get(FASTMAIL_SESSION_URL);
    let req = if token.starts_with("fmu1-") {
        req.bearer_auth(&token)
    } else {
        req.basic_auth(&user, Some(&token))
    };

    let resp = req.send().await.expect("session fetch failed");

    assert!(resp.status().is_success(), "HTTP {}", resp.status());

    let session: serde_json::Value = resp.json().await.expect("invalid JSON");

    let capabilities = session
        .get("capabilities")
        .and_then(|v| v.as_object())
        .expect("missing capabilities");

    // Push: websocket URN or eventSourceUrl
    let has_websocket = capabilities.contains_key("urn:ietf:params:jmap:websocket");
    let has_event_source = session
        .get("eventSourceUrl")
        .and_then(|v| v.as_str())
        .is_some();
    assert!(
        has_websocket || has_event_source,
        "Fastmail should advertise push via websocket ({has_websocket}) or eventSourceUrl ({has_event_source})"
    );

    // Submission
    assert!(
        capabilities.contains_key("urn:ietf:params:jmap:submission"),
        "Fastmail should advertise urn:ietf:params:jmap:submission"
    );

    // Also verify our parsed session agrees
    let (_session_parsed, client) = connect_client().await;
    assert!(
        client.event_source_url.is_some(),
        "JmapClient should have event_source_url"
    );

    // Print all capabilities for visibility
    eprintln!("Fastmail session capabilities:");
    for key in capabilities.keys() {
        eprintln!("  {key}");
    }
    if has_event_source {
        eprintln!(
            "eventSourceUrl: {}",
            session["eventSourceUrl"].as_str().unwrap()
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 4a: Server-side search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_by_text() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    // Search for the test email we sent in Phase 2
    let filter = email::SearchFilter {
        text: Some("neverlight-mail-core".into()),
        ..Default::default()
    };

    let (messages, query_result) = email::search(&client, &filter, 10)
        .await
        .expect("search failed");

    assert!(query_result.total > 0, "should find at least one result");
    assert!(!messages.is_empty(), "should return messages");

    eprintln!(
        "Search 'neverlight-mail-core': {} total, {} returned",
        query_result.total,
        messages.len()
    );
    for m in &messages {
        eprintln!("  {} — {}", m.from, m.subject);
    }
}

#[tokio::test]
async fn search_with_mailbox_filter() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let inbox_id = mailbox::find_by_role(&folders, "inbox").expect("no inbox");

    let filter = email::SearchFilter {
        in_mailbox: Some(inbox_id.clone()),
        text: Some("test".into()),
        ..Default::default()
    };

    let (messages, query_result) = email::search(&client, &filter, 5)
        .await
        .expect("search failed");

    eprintln!(
        "Search 'test' in inbox: {} total, {} returned",
        query_result.total,
        messages.len()
    );
    // Don't assert count — may or may not have "test" in inbox
}

// ---------------------------------------------------------------------------
// Phase 4b: Mailbox management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_rename_delete_mailbox() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    // Create
    let mailbox_id = mailbox::create(&client, "neverlight-test-folder", None)
        .await
        .expect("create mailbox failed");
    assert!(!mailbox_id.is_empty());
    eprintln!("Created mailbox: {mailbox_id}");

    // Verify it appears in the list
    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let found = folders.iter().find(|f| f.mailbox_id == mailbox_id);
    assert!(found.is_some(), "new mailbox should appear in list");
    assert_eq!(found.unwrap().name, "neverlight-test-folder");

    // Rename
    mailbox::rename(&client, &mailbox_id, "neverlight-test-renamed")
        .await
        .expect("rename failed");

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all after rename");
    let found = folders.iter().find(|f| f.mailbox_id == mailbox_id).unwrap();
    assert_eq!(found.name, "neverlight-test-renamed");
    eprintln!("Renamed to: {}", found.name);

    // Delete
    mailbox::destroy(&client, &mailbox_id, false)
        .await
        .expect("destroy failed");

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all after destroy");
    let found = folders.iter().find(|f| f.mailbox_id == mailbox_id);
    assert!(found.is_none(), "deleted mailbox should not appear in list");
    eprintln!("Deleted mailbox {mailbox_id}");
}
