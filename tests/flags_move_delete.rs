//! Integration tests: flag operations, move/trash, mailbox CRUD.

mod common;

use common::{connect_client, skip_if_no_env};
use neverlight_mail_core::types::FlagOp;
use neverlight_mail_core::{email, mailbox};

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

    email::set_flag(&client, email_id, &FlagOp::SetSeen(!original_read))
        .await
        .expect("set_flag failed");

    let updated = email::get_summaries(&client, std::slice::from_ref(email_id), &inbox_id)
        .await
        .expect("get_summaries");
    assert_eq!(
        updated[0].is_read, !original_read,
        "flag should have toggled"
    );

    email::set_flag(&client, email_id, &FlagOp::SetSeen(original_read))
        .await
        .expect("restore flag failed");

    eprintln!(
        "Toggled read flag on '{}': {} → {} → {} (restored)",
        msg.subject, original_read, !original_read, original_read
    );
}

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

    email::trash(&client, email_id, &inbox_id, &trash_id)
        .await
        .expect("trash failed");

    let in_trash = email::get_summaries(&client, std::slice::from_ref(email_id), &trash_id)
        .await
        .expect("get from trash");
    assert_eq!(in_trash.len(), 1, "message should be in trash");

    email::move_to(&client, email_id, &trash_id, &inbox_id)
        .await
        .expect("move back failed");

    let in_inbox = email::get_summaries(&client, std::slice::from_ref(email_id), &inbox_id)
        .await
        .expect("get from inbox");
    assert_eq!(in_inbox.len(), 1, "message should be back in inbox");

    eprintln!("Moved '{}' to trash and back: {}", msg.subject, email_id);
}

#[tokio::test]
async fn create_rename_delete_mailbox() {
    skip_if_no_env!();
    let (_session, client) = connect_client().await;

    let mailbox_id = mailbox::create(&client, "neverlight-test-folder", None)
        .await
        .expect("create mailbox failed");
    assert!(!mailbox_id.is_empty());
    eprintln!("Created mailbox: {mailbox_id}");

    let folders = mailbox::fetch_all(&client).await.expect("fetch_all");
    let found = folders.iter().find(|f| f.mailbox_id == mailbox_id);
    assert!(found.is_some(), "new mailbox should appear in list");
    assert_eq!(found.unwrap().name, "neverlight-test-folder");

    mailbox::rename(&client, &mailbox_id, "neverlight-test-renamed")
        .await
        .expect("rename failed");

    let folders = mailbox::fetch_all(&client)
        .await
        .expect("fetch_all after rename");
    let found = folders.iter().find(|f| f.mailbox_id == mailbox_id).unwrap();
    assert_eq!(found.name, "neverlight-test-renamed");
    eprintln!("Renamed to: {}", found.name);

    mailbox::destroy(&client, &mailbox_id, false)
        .await
        .expect("destroy failed");

    let folders = mailbox::fetch_all(&client)
        .await
        .expect("fetch_all after destroy");
    let found = folders.iter().find(|f| f.mailbox_id == mailbox_id);
    assert!(found.is_none(), "deleted mailbox should not appear in list");
    eprintln!("Deleted mailbox {mailbox_id}");
}
