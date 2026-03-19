//! Integration tests: session discovery, mailbox listing, email query, body fetch.

mod common;

use common::{connect_client, skip_if_no_env};
use neverlight_mail_core::{email, mailbox};

#[tokio::test]
async fn session_connects_and_discovers_capabilities() {
    skip_if_no_env!();
    let (session, _client) = connect_client().await;

    assert!(
        !session.account_id.is_empty(),
        "account_id should be non-empty"
    );
    assert!(!session.api_url.is_empty(), "api_url should be non-empty");
    assert!(
        session.max_objects_in_get > 0,
        "max_objects_in_get should be > 0"
    );
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

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all failed");

    assert!(!folders.is_empty(), "should have at least one mailbox");

    let inbox = mailbox::find_by_role(&folders, "inbox");
    assert!(inbox.is_some(), "should have an inbox");

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
