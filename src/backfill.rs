//! Background history walking: fetch older messages in batches.
//!
//! Backfill is purely additive — only `save_messages`, never `prune_mailbox`.
//! Resumable across restarts via the `backfill_progress` table.

use crate::client::{JmapClient, JmapError};
use crate::email;
use crate::store::CacheHandle;

/// Result of a single backfill batch for one mailbox.
#[derive(Debug, Clone)]
pub struct BackfillBatchResult {
    pub mailbox_id: String,
    pub fetched: u32,
    pub position: u32,
    pub total: u32,
    pub completed: bool,
}

/// Fetch one batch of older messages for a mailbox.
///
/// Reads the current backfill position from cache, queries the server for the
/// next page of messages, saves them (additive only), and updates progress.
///
/// `page_size` is also used as the starting offset (skip the head page).
pub async fn backfill_batch(
    client: &JmapClient,
    cache: &CacheHandle,
    account_id: &str,
    mailbox_id: &str,
    page_size: u32,
    max_messages: Option<u32>,
) -> Result<BackfillBatchResult, JmapError> {
    // Read current progress — default to page_size (skip head page)
    let progress = cache
        .get_backfill_progress(account_id.to_string(), mailbox_id.to_string())
        .await
        .map_err(JmapError::RequestError)?;

    let position = progress.as_ref().map(|p| p.position).unwrap_or(page_size);

    // Check max_messages limit
    if let Some(max) = max_messages {
        if position >= max {
            let total = progress.map(|p| p.total).unwrap_or(0);
            cache
                .set_backfill_progress(
                    account_id.to_string(),
                    mailbox_id.to_string(),
                    position,
                    total,
                    true,
                )
                .await
                .map_err(JmapError::RequestError)?;
            return Ok(BackfillBatchResult {
                mailbox_id: mailbox_id.to_string(),
                fetched: 0,
                position,
                total,
                completed: true,
            });
        }
    }

    // Query + get the next batch
    let (mut messages, query_result) =
        email::query_and_get(client, mailbox_id, page_size, position).await?;
    let fetched = messages.len() as u32;
    let total = query_result.total;

    log::debug!(
        "backfill_batch: mailbox={} position={} fetched={} total={}",
        mailbox_id,
        position,
        fetched,
        total
    );

    // Set account_id on all messages before caching
    for m in &mut messages {
        m.account_id = account_id.to_string();
    }

    // Additive save — never prune
    if !messages.is_empty() {
        cache
            .save_messages(account_id.to_string(), mailbox_id.to_string(), messages)
            .await
            .map_err(JmapError::RequestError)?;
    }

    let new_position = position + fetched;
    let completed = fetched == 0
        || new_position >= total
        || max_messages.is_some_and(|max| new_position >= max);

    cache
        .set_backfill_progress(
            account_id.to_string(),
            mailbox_id.to_string(),
            new_position,
            total,
            completed,
        )
        .await
        .map_err(JmapError::RequestError)?;

    Ok(BackfillBatchResult {
        mailbox_id: mailbox_id.to_string(),
        fetched,
        position: new_position,
        total,
        completed,
    })
}
