//! Delta sync loop: Email/changes + Mailbox/changes (RFC 8620 §5.2).
//!
//! Replaces IMAP polling with efficient state-based delta sync.
//! Only fetches what changed since the last known state token.

use serde_json::Value;

use crate::client::{JmapClient, JmapError};
use crate::email;
use crate::mailbox;
use crate::models::{Folder, MessageSummary};
use crate::store::CacheHandle;
use crate::types::State;

/// Result of a delta sync operation.
#[derive(Debug)]
pub struct SyncResult {
    pub new_state: State,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    pub has_more_changes: bool,
}

/// Sync mailboxes: fetch changes since `since_state`.
///
/// If `since_state` is None, performs a full fetch.
/// Returns the new state and lists of changed/destroyed mailbox IDs.
pub async fn sync_mailboxes(
    client: &JmapClient,
    cache: &CacheHandle,
    account_id: &str,
) -> Result<Vec<Folder>, JmapError> {
    log::debug!("sync_mailboxes: start for account {}", account_id);
    let since_state = cache
        .get_state(account_id.to_string(), "Mailbox".to_string())
        .await
        .unwrap_or(None);

    log::debug!("sync_mailboxes: state={:?}", since_state.as_deref().unwrap_or("(none)"));
    let folders = match since_state {
        Some(state) => sync_mailboxes_delta(client, cache, account_id, &state).await?,
        None => sync_mailboxes_full(client, cache, account_id).await?,
    };

    log::debug!("sync_mailboxes: done, {} folders", folders.len());
    Ok(folders)
}

async fn sync_mailboxes_full(
    client: &JmapClient,
    cache: &CacheHandle,
    account_id: &str,
) -> Result<Vec<Folder>, JmapError> {
    log::info!("sync_mailboxes_full: fetching all mailboxes for {}", account_id);
    let call = client.method(
        "Mailbox/get",
        serde_json::json!({
            "properties": ["id", "name", "parentId", "role", "sortOrder", "totalEmails", "unreadEmails"],
        }),
        "m0",
    );

    let resp = client.call(vec![call]).await?;
    let mc = resp.method_responses.iter().find(|mc| mc.2 == "m0")
        .ok_or_else(|| JmapError::RequestError("Missing Mailbox/get response".into()))?;

    let state = mc.1.get("state")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let list = mc.1.get("list")
        .and_then(|v| v.as_array())
        .ok_or_else(|| JmapError::RequestError("Missing list in Mailbox/get".into()))?;

    let folders = mailbox::parse_mailboxes_from_list(list)?;

    // Persist
    let _ = cache.save_folders(account_id.to_string(), folders.clone()).await;
    let _ = cache.set_state(account_id.to_string(), "Mailbox".to_string(), state.to_string()).await;

    Ok(folders)
}

async fn sync_mailboxes_delta(
    client: &JmapClient,
    cache: &CacheHandle,
    account_id: &str,
    since_state: &str,
) -> Result<Vec<Folder>, JmapError> {
    log::debug!("sync_mailboxes_delta: since_state={} for {}", since_state, account_id);
    let call = client.method(
        "Mailbox/changes",
        serde_json::json!({ "sinceState": since_state }),
        "mc0",
    );

    let resp = match client.call(vec![call]).await {
        Err(JmapError::CannotCalculateChanges) => {
            log::info!("Mailbox cannotCalculateChanges — full resync");
            return sync_mailboxes_full(client, cache, account_id).await;
        }
        other => other?,
    };
    let mc = resp.method_responses.iter().find(|mc| mc.2 == "mc0")
        .ok_or_else(|| JmapError::RequestError("Missing Mailbox/changes response".into()))?;

    let changes = parse_changes_response(&mc.1)?;

    if changes.created.is_empty() && changes.updated.is_empty() && changes.destroyed.is_empty() {
        // No changes — just update state and return cached folders
        let _ = cache.set_state(
            account_id.to_string(),
            "Mailbox".to_string(),
            changes.new_state.0.clone(),
        ).await;
        let folders = cache.load_folders(account_id.to_string()).await
            .unwrap_or_default();
        return Ok(folders);
    }

    // Fetch only created + updated mailboxes by ID
    let fetch_ids: Vec<String> = changes.created.iter()
        .chain(changes.updated.iter())
        .cloned()
        .collect();

    if !fetch_ids.is_empty() {
        let fetched = mailbox::fetch_by_ids(client, &fetch_ids).await?;
        if !fetched.is_empty() {
            let _ = cache.upsert_folders(account_id.to_string(), fetched).await;
        }
    }

    // Remove destroyed mailboxes (cascades to messages + attachments)
    if !changes.destroyed.is_empty() {
        log::info!("Mailbox delta: removing {} destroyed folders", changes.destroyed.len());
        let _ = cache.remove_folders(account_id.to_string(), changes.destroyed).await;
    }

    // Update state
    let _ = cache.set_state(
        account_id.to_string(),
        "Mailbox".to_string(),
        changes.new_state.0.clone(),
    ).await;

    // Return full folder list from cache
    let folders = cache.load_folders(account_id.to_string()).await
        .unwrap_or_default();
    Ok(folders)
}

/// Sync emails in a mailbox: fetch changes since last known state.
///
/// If no state is stored, performs a full query+get.
pub async fn sync_emails(
    client: &JmapClient,
    cache: &CacheHandle,
    account_id: &str,
    mailbox_id: &str,
    page_size: u32,
) -> Result<Vec<MessageSummary>, JmapError> {
    log::debug!("sync_emails: start for {} mailbox={}", account_id, mailbox_id);
    let resource = "Email".to_string();
    let since_state = cache
        .get_state(account_id.to_string(), resource.clone())
        .await
        .unwrap_or(None);

    log::debug!("sync_emails: email state={:?}", since_state.as_deref().unwrap_or("(none)"));
    match since_state {
        Some(state) => {
            sync_emails_delta(client, cache, account_id, mailbox_id, &state, &resource, page_size).await
        }
        None => {
            sync_emails_head(client, cache, account_id, mailbox_id, &resource, page_size).await
        }
    }
}

async fn sync_emails_head(
    client: &JmapClient,
    cache: &CacheHandle,
    account_id: &str,
    mailbox_id: &str,
    resource: &str,
    page_size: u32,
) -> Result<Vec<MessageSummary>, JmapError> {
    log::info!("sync_emails_head: fetching up to {} emails for mailbox={}", page_size, mailbox_id);
    let (mut messages, query_result) =
        email::query_and_get(client, mailbox_id, page_size, 0).await?;
    log::debug!("sync_emails_head: got {} messages from server", messages.len());

    // Set account_id on all messages before caching
    for m in &mut messages {
        m.account_id = account_id.to_string();
    }

    let _ = cache.save_messages(
        account_id.to_string(),
        mailbox_id.to_string(),
        messages.clone(),
    ).await;

    // Only prune when backfill hasn't populated older messages.
    // Backfill is additive — pruning would wipe backfilled history.
    // Delta sync handles server-side deletions via the destroyed list.
    let has_backfill = cache
        .get_backfill_progress(account_id.to_string(), mailbox_id.to_string())
        .await
        .ok()
        .flatten()
        .is_some_and(|p| p.position > page_size);

    if !has_backfill {
        let live_ids: Vec<String> = messages.iter().map(|m| m.email_id.clone()).collect();
        let pruned = cache.prune_mailbox(
            account_id.to_string(),
            mailbox_id.to_string(),
            live_ids,
        ).await.unwrap_or(0);
        if pruned > 0 {
            log::info!("Pruned {pruned} stale messages from {mailbox_id}");
        }
    }

    // Store the Email/get state (for Email/changes), not the queryState
    let state = query_result.get_state
        .as_ref()
        .unwrap_or(&query_result.state);
    let _ = cache.set_state(
        account_id.to_string(),
        resource.to_string(),
        state.0.clone(),
    ).await;

    Ok(messages)
}

async fn sync_emails_delta(
    client: &JmapClient,
    cache: &CacheHandle,
    account_id: &str,
    mailbox_id: &str,
    since_state: &str,
    resource: &str,
    page_size: u32,
) -> Result<Vec<MessageSummary>, JmapError> {
    let mut current_state = since_state.to_string();
    log::debug!("sync_emails_delta: since_state={} mailbox={}", since_state, mailbox_id);

    // Loop to drain all pending changes (server may paginate via hasMoreChanges).
    // Cap iterations to avoid runaway loops if the server misbehaves.
    for iteration in 0..50 {
        let call = client.method(
            "Email/changes",
            serde_json::json!({ "sinceState": current_state }),
            "ec0",
        );

        let resp = match client.call(vec![call]).await {
            Err(JmapError::CannotCalculateChanges) => {
                log::info!("Email cannotCalculateChanges for {} — head resync", mailbox_id);
                return sync_emails_head(client, cache, account_id, mailbox_id, resource, page_size).await;
            }
            other => other?,
        };
        let mc = resp.method_responses.iter().find(|mc| mc.2 == "ec0")
            .ok_or_else(|| JmapError::RequestError("Missing Email/changes response".into()))?;

        let changes = parse_changes_response(&mc.1)?;
        log::debug!(
            "sync_emails_delta: iteration={} created={} updated={} destroyed={} has_more={}",
            iteration, changes.created.len(), changes.updated.len(),
            changes.destroyed.len(), changes.has_more_changes,
        );

        // Remove destroyed emails from cache
        for id in &changes.destroyed {
            let _ = cache.remove_message(account_id.to_string(), id.clone()).await;
        }

        // Fetch created + updated emails
        let fetch_ids: Vec<String> = changes.created.iter()
            .chain(changes.updated.iter())
            .cloned()
            .collect();

        if !fetch_ids.is_empty() {
            let mut summaries = email::get_summaries(client, &fetch_ids, mailbox_id).await?;
            for m in &mut summaries {
                m.account_id = account_id.to_string();
            }

            // Partition: messages still in this mailbox vs. moved elsewhere.
            // Email/changes is account-global, so updated messages may have moved
            // to a different mailbox. Check mailbox_ids (full truth from JMAP).
            let mut still_here = Vec::new();
            let mut moved_away = Vec::new();
            for m in summaries {
                if m.mailbox_ids.contains(&mailbox_id.to_string()) {
                    still_here.push(m);
                } else {
                    moved_away.push(m);
                }
            }

            // Save messages that are (still) in this mailbox — save_messages
            // syncs the junction table with all their mailbox_ids
            if !still_here.is_empty() {
                let _ = cache.save_messages(
                    account_id.to_string(),
                    mailbox_id.to_string(),
                    still_here,
                ).await;
            }

            // For messages that moved out of this mailbox: save with new mailbox_ids
            // (this updates the junction table, removing this mailbox's association
            // without deleting the message entirely)
            for m in moved_away {
                log::debug!(
                    "Delta sync: email {} no longer in {} (now in {:?})",
                    m.email_id, mailbox_id, m.mailbox_ids,
                );
                let context = m.mailbox_ids.first().cloned().unwrap_or_default();
                let _ = cache.save_messages(
                    account_id.to_string(),
                    context,
                    vec![m],
                ).await;
            }
        }

        // Update state after each batch so progress is durable
        let _ = cache.set_state(
            account_id.to_string(),
            resource.to_string(),
            changes.new_state.0.clone(),
        ).await;

        if !changes.has_more_changes {
            break;
        }

        log::info!(
            "Email/changes hasMoreChanges for {} (iteration {}) — fetching next batch",
            mailbox_id, iteration
        );
        current_state = changes.new_state.0;
    }

    // Return full list from cache
    let messages = cache.load_messages(
        account_id.to_string(),
        mailbox_id.to_string(),
        page_size,
        0,
    ).await.unwrap_or_default();

    // Account-global Email state can be up-to-date while a specific mailbox
    // has never been fully synced (no cached messages). Fall back to a full
    // query+get for this mailbox so it gets populated.
    if messages.is_empty() {
        log::info!(
            "Delta sync returned no cached messages for {} — falling back to head sync",
            mailbox_id,
        );
        return sync_emails_head(client, cache, account_id, mailbox_id, resource, page_size).await;
    }

    Ok(messages)
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_changes_response(data: &Value) -> Result<SyncResult, JmapError> {
    let new_state = data
        .get("newState")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let created = parse_id_list(data.get("created"));
    let updated = parse_id_list(data.get("updated"));
    let destroyed = parse_id_list(data.get("destroyed"));

    let has_more_changes = data
        .get("hasMoreChanges")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(SyncResult {
        new_state: State(new_state),
        created,
        updated,
        destroyed,
        has_more_changes,
    })
}

fn parse_id_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_changes_response() {
        let data = serde_json::json!({
            "accountId": "u1234",
            "oldState": "s1",
            "newState": "s2",
            "created": ["M100", "M101"],
            "updated": ["M050"],
            "destroyed": ["M001"],
            "hasMoreChanges": false
        });

        let result = parse_changes_response(&data).unwrap();
        assert_eq!(result.new_state.0, "s2");
        assert_eq!(result.created, vec!["M100", "M101"]);
        assert_eq!(result.updated, vec!["M050"]);
        assert_eq!(result.destroyed, vec!["M001"]);
        assert!(!result.has_more_changes);
    }

    #[test]
    fn parses_empty_changes() {
        let data = serde_json::json!({
            "oldState": "s1",
            "newState": "s1",
            "created": [],
            "updated": [],
            "destroyed": [],
            "hasMoreChanges": false
        });

        let result = parse_changes_response(&data).unwrap();
        assert!(result.created.is_empty());
        assert!(result.updated.is_empty());
        assert!(result.destroyed.is_empty());
    }

    #[test]
    fn parses_changes_with_more() {
        let data = serde_json::json!({
            "newState": "s3",
            "created": ["M200"],
            "updated": [],
            "destroyed": [],
            "hasMoreChanges": true
        });

        let result = parse_changes_response(&data).unwrap();
        assert!(result.has_more_changes);
        assert_eq!(result.created.len(), 1);
    }

    #[test]
    fn handles_missing_fields() {
        let data = serde_json::json!({ "newState": "s1" });
        let result = parse_changes_response(&data).unwrap();
        assert!(result.created.is_empty());
        assert!(result.updated.is_empty());
        assert!(result.destroyed.is_empty());
    }
}
