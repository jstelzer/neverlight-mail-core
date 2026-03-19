//! Integration tests: identity discovery and email sending.

mod common;

use common::{connect_client, skip_if_no_env};
use neverlight_mail_core::{mailbox, submit};

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
        to: std::slice::from_ref(&user),
        cc: &[],
        subject: "[neverlight-mail-core test] Integration test email",
        text_body: "This is an automated test email from neverlight-mail-core integration tests.\n\nIf you see this, EmailSubmission/set is working.",
        html_body: None,
        drafts_mailbox_id: &drafts_id,
        sent_mailbox_id: &sent_id,
        in_reply_to: None,
        references: None,
    };

    let email_id = submit::send(&client, &req).await.expect("send failed");

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
    let identity =
        submit::find_identity_for_address(&identities, from_addr).expect("no matching identity");
    eprintln!(
        "Using identity: {} <{}> (for {})",
        identity.name, identity.email, from_addr
    );

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
