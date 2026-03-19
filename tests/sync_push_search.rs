//! Integration tests: delta sync, EventSource push, server-side search, capabilities.

mod common;

use common::{connect_client, skip_if_no_env, test_cache, FASTMAIL_SESSION_URL};
use neverlight_mail_core::{email, mailbox, push, sync};

#[tokio::test]
async fn sync_mailboxes_full_then_delta() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;
    let cache = test_cache();

    let folders = sync::sync_mailboxes(&client, &cache, &client.account_id)
        .await
        .expect("first sync failed");

    assert!(!folders.is_empty(), "should have mailboxes");
    let has_inbox = folders.iter().any(|f| f.role.as_deref() == Some("inbox"));
    assert!(has_inbox, "should have inbox");

    let state = cache
        .get_state(client.account_id.clone(), "Mailbox".to_string())
        .await
        .expect("get_state")
        .expect("state should exist after first sync");
    assert!(!state.is_empty(), "state token should be non-empty");
    eprintln!("Mailbox state after full sync: {state}");

    let folders2 = sync::sync_mailboxes(&client, &cache, &client.account_id)
        .await
        .expect("delta sync failed");

    assert_eq!(
        folders.len(),
        folders2.len(),
        "delta sync should return same folder count"
    );

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

    let messages2 = sync::sync_emails(&client, &cache, &client.account_id, &inbox_id, 10)
        .await
        .expect("delta email sync failed");

    assert!(
        !messages2.is_empty(),
        "delta sync should return cached messages"
    );
    eprintln!("Email sync: {} messages on delta sync", messages2.len());
}

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

    let config = push::EventSourceConfig::default();
    let url = push::build_event_source_url(template, &config);

    assert!(!url.is_empty(), "URL should be non-empty");
    assert!(!url.contains("{types}"), "template vars should be replaced");
    assert!(
        !url.contains("{closeafter}"),
        "template vars should be replaced"
    );
    assert!(!url.contains("{ping}"), "template vars should be replaced");
    eprintln!("Built EventSource URL: {url}");
}

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

    let has_websocket = capabilities.contains_key("urn:ietf:params:jmap:websocket");
    let has_event_source = session
        .get("eventSourceUrl")
        .and_then(|v| v.as_str())
        .is_some();
    assert!(
        has_websocket || has_event_source,
        "Fastmail should advertise push via websocket ({has_websocket}) or eventSourceUrl ({has_event_source})"
    );

    assert!(
        capabilities.contains_key("urn:ietf:params:jmap:submission"),
        "Fastmail should advertise urn:ietf:params:jmap:submission"
    );

    let (_session_parsed, client) = connect_client().await;
    assert!(
        client.event_source_url.is_some(),
        "JmapClient should have event_source_url"
    );

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

#[tokio::test]
async fn search_by_text() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

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
}
