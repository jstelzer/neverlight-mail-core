//! Integration tests against a live JMAP server (Fastmail).
//!
//! Requires environment variables:
//!   NEVERLIGHT_MAIL_JMAP_TOKEN — Fastmail API token (fmu1-...) or app password (mu1-...)
//!   NEVERLIGHT_MAIL_USER       — Account email address
//!
//! Source from `.envrc` in the crate root.
//! Skipped automatically when env vars are missing.

use neverlight_mail_core::session::JmapSession;
use neverlight_mail_core::types::FlagOp;
use neverlight_mail_core::{client::JmapClient, email, mailbox, submit};

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
    };

    let email_id = submit::send(&client, &req)
        .await
        .expect("send failed");

    assert!(!email_id.is_empty(), "should return created email ID");
    eprintln!("Sent test email: id={email_id}");
}
