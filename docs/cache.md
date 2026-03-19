# Cache Layer Design

## Purpose

The cache is a SQLite database that stores a local replica of server state. It exists so the UI can render instantly from disk while sync happens in the background. It is **not** an offline mail store — it's a read-through cache with optimistic write-ahead for flag changes.

## Architecture

```
┌──────────────┐    CacheCmd (mpsc)    ┌─────────────────┐
│  async code  │ ──────────────────►   │  background     │
│  (tokio)     │                       │  thread          │
│              │ ◄──────────────────   │  (rusqlite)      │
│              │  Result (oneshot)     │                  │
└──────────────┘                       └─────────────────┘
```

`CacheHandle` is the only public type. It is `Clone + Send + Sync`. All SQLite access happens on a single dedicated thread — this avoids `rusqlite::Connection` Send/Sync issues entirely. Each command carries a `oneshot::Sender` for the reply.

The channel is unbounded. This is fine because write volume is bounded by sync frequency and user interaction rate.

## Tables

### `folders`

Server-side mailbox metadata. Keyed by `(account_id, path)` with a unique constraint on `(account_id, mailbox_id)`.

Upserted wholesale on every sync. Stale folders (no longer on the server) are deleted along with their messages and attachments.

### `messages`

Email summaries (no body). Keyed by `(account_id, email_id)`. Mailbox membership is tracked in the `message_mailboxes` junction table (see below), not as a column on this table. The legacy `mailbox_id` column is retained but unused by queries.

### `message_mailboxes`

Junction table for the many-to-many relationship between messages and mailboxes. Keyed by `(account_id, email_id, mailbox_id)`. Foreign key to `messages` with `ON DELETE CASCADE`. When loading messages for a folder view, the query JOINs through this table and sets `context_mailbox_id` to the folder being viewed. `GROUP_CONCAT` collects the full `mailbox_ids` list.

**Body columns**: `body_rendered` (plain text) and `body_markdown` are nullable. They start as NULL and are populated lazily when the user opens a message. This means the cache-first read path (`load_body`) returns `None` for messages that haven't been viewed yet — the caller then fetches from the server and calls `save_body`.

### `attachments`

Binary attachment data. Keyed by `(account_id, email_id, idx)`. Foreign key to `messages` with `ON DELETE CASCADE`. Populated alongside `body_rendered`/`body_markdown` when a message body is fetched.

### `sync_state`

JMAP state tokens per `(account_id, resource)`. Resources are `"Mailbox"` for mailbox state or `"Email"` for account-global email state (RFC 8620 §5.2). These tokens drive delta sync — without them, the sync loop falls back to a full fetch.

### `backfill_progress`

Per-mailbox backfill tracking for background history walking. Keyed by `(account_id, mailbox_id)`. Stores `position` (next offset to fetch), `total` (server-reported count), `completed` flag, and `updated_at` timestamp. UPSERTed after each batch. Deleted on account removal or manual reset ("Sync full history").

### `message_fts`

FTS5 virtual table over `(subject, sender, body_rendered)`. Kept in sync via triggers on `messages` (insert/update/delete). Queried by `do_search` with an `account_id` filter for per-account full-text search.

## Dual-Truth Flag Model

Flag changes need optimistic UI updates before the server confirms. The cache tracks this with three columns per message:

| Column | What it holds |
|---|---|
| `flags_server` | Last known server state (2-bit encoding: bit 0 = seen, bit 1 = flagged) |
| `flags_local` | What the UI is currently showing |
| `pending_op` | Non-NULL when a flag change is in flight (describes the operation) |

### State machine

```
┌─────────────┐
│ Idle        │  flags_server == flags_local, pending_op IS NULL
│             │  UI reads from flags_server
└─────┬───────┘
      │ User toggles a flag
      ▼
┌─────────────┐
│ Pending     │  flags_local != flags_server, pending_op IS NOT NULL
│             │  UI reads from flags_local (optimistic)
└─────┬───┬───┘
      │   │
      │   │ Server rejects → revert_pending_op()
      │   │   flags_local := flags_server, pending_op := NULL
      │   └──────────────────────────────────────────────────► back to Idle
      │
      │ Server confirms → clear_pending_op(new_server_flags)
      │   flags_server := new_flags, flags_local := new_flags, pending_op := NULL
      └──────────────────────────────────────────────────────► back to Idle
```

### Sync-during-pending interaction

When `do_save_messages` upserts a message that has a pending_op:
- Server-side metadata (subject, date, thread_id, etc.) is updated normally
- `flags_server` is updated to the incoming server value
- `flags_local`, `is_read`, `is_starred` are **preserved** (not overwritten)

This prevents a background sync from clobbering the user's optimistic flag toggle.

### The `is_read`/`is_starred` redundancy

The `is_read` and `is_starred` columns duplicate information derivable from `flags_server`/`flags_local`. They exist so the ORDER BY and WHERE clauses in queries don't need bit arithmetic. They are always kept in sync with the effective flags by `do_update_flags`, `do_clear_pending_op`, and `do_revert_pending_op`.

**Invariant**: `is_read == (effective_flags & 1 != 0)` and `is_starred == (effective_flags & 2 != 0)`, where effective_flags is `flags_local` if pending_op is set, else `flags_server`.

## Sync ↔ Cache Interaction

### Full sync (no state token)

1. `Email/query` + `Email/get` fetches summaries for the mailbox
2. `save_messages` upserts them (preserving pending ops)
3. `prune_mailbox` deletes cached messages not in the server's response
4. State token is saved for future delta syncs

### Delta sync (has state token)

1. `Email/changes` returns created/updated/destroyed IDs
2. Destroyed → `remove_message` from cache
3. Created + updated → `Email/get` by ID, then partition by `mailbox_ids`:
   - Still in this mailbox → `save_messages` (updates junction table with full mailbox set)
   - Moved to another mailbox → `save_messages` with new mailbox context (junction table updated, old association removed)
4. State token is updated after each batch
5. If `hasMoreChanges`, loop (capped at 50 iterations)
6. Falls back to full sync on `cannotCalculateChanges`

### Mailbox sync

Uses delta sync: `Mailbox/changes` returns created/updated/destroyed IDs.
- Created + updated → `Mailbox/get` with specific IDs, then `upsert_folders` (no pruning)
- Destroyed → `remove_folders` cascades to messages and attachments
- Falls back to full `Mailbox/get` on `cannotCalculateChanges`
- No-op (just state update) when no changes detected

### Backfill sync

Background history walking fetches older messages in batches. Runs as a separate job from head sync.

1. Read `backfill_progress` for the mailbox — default starting position is `page_size` (skip head page)
2. `Email/query` + `Email/get` at `position` offset
3. `save_messages` (additive only — **never prune**)
4. Update `backfill_progress` with new position, total, and completed flag
5. Mark completed when: fetched 0 messages, reached end, or hit `max_messages_per_mailbox` limit

**Prune-skip rule**: Head sync skips `prune_mailbox` for any mailbox where backfill has started (`position > page_size`). This prevents head sync from nuking backfilled older messages. Delta sync handles deletions via the `destroyed` list, so correctness is preserved.

## Invariants

These must hold at all times. Tests should prove each one.

1. **Account isolation**: Operations on account A never affect account B. Every query and mutation includes `account_id` in its WHERE clause.

2. **Pending-op preservation**: A background sync never overwrites `flags_local` or `is_read`/`is_starred` on a message with a pending_op.

3. **Prune correctness**: After `prune_mailbox(account, mailbox, live_ids)`:
   - Only junction rows for `(account, mailbox)` with email_ids not in `live_ids` are removed
   - Messages that still have other mailbox associations are preserved
   - Orphaned messages (no remaining associations) are cleaned up
   - Messages in other mailboxes and accounts are untouched

4. **Body cache independence**: `save_body` and `load_body` are keyed by `(account_id, email_id)`. Saving a body for account A's copy of email_id X does not populate account B's copy.

5. **FTS consistency**: The FTS index stays in sync with the messages table via triggers. After insert, update, or delete on messages, the FTS index reflects the current state.

6. **State token durability**: After a successful sync, the state token is persisted. A crash after sync but before state save means the next sync re-fetches the same delta (idempotent, not data-losing).

7. **Folder cascade**: When `save_folders` removes a folder that no longer exists on the server, all messages and attachments in that folder are deleted.

8. **Flag round-trip**: `flags_to_u8(is_read, is_starred)` → `flags_from_u8(u8)` is lossless. `flags_to_u8(false, false) == 0`, `flags_to_u8(true, false) == 1`, `flags_to_u8(false, true) == 2`, `flags_to_u8(true, true) == 3`.

9. **Backfill additivity**: Backfill only calls `save_messages`, never `prune_mailbox`. It cannot delete data.

10. **Backfill resumability**: Reads `position` from `backfill_progress` on restart and continues from that offset. No work is lost across process restarts.

11. **Prune-skip**: Head sync skips `prune_mailbox` when backfill has started for that mailbox (`position > page_size`). Prevents head sync from wiping backfilled history.

## Known Simplifications

1. **No attachment-only cache eviction**: Body and attachment data are stored forever once fetched. There's no LRU or size-based eviction. The database grows monotonically until messages are pruned by sync.

## Resolved Simplifications

These were previously known limitations, now fixed:

- **~~Per-mailbox email state~~**: Email state is now account-global (`"Email"` key), matching RFC 8620 §5.2. A marker-table migration (`_email_state_v2`) cleans stale `Email:*` keys on upgrade.

- **~~Search is cross-account~~**: `do_search` now takes `account_id` and filters results to the active account.

- **~~Full mailbox refetch on delta~~**: `sync_mailboxes_delta` now fetches only created/updated mailboxes by ID and removes only destroyed ones, instead of re-fetching the entire list.

- **~~Single mailbox per message~~**: Messages now track all mailbox memberships via the `message_mailboxes` junction table. A message in both Inbox and Archive appears correctly in both folder views. Pruning removes only the junction row, not the message itself, so it remains visible in other folders. Delta sync partitions by `mailbox_ids.contains()` instead of single-field comparison.

- **~~No WAL mode~~**: Cache now opens with `journal_mode=WAL` and `busy_timeout=5000` for better concurrent read/write behavior.

- **~~Ghost messages after cross-client delete~~**: `JmapError::NotFound` is now detected when flag/move/destroy operations target a server-deleted message. The FFI layer evicts the stale cached copy instead of leaving a ghost.

## Test Coverage Required

Each invariant above maps to tests. Here's what exists and what's missing.

### Test coverage

| Invariant                    | Test                                                            | File                  |
|------------------------------|-----------------------------------------------------------------|-----------------------|
| Account isolation            | `messages_bodies_flags_and_removal_are_isolated_per_account`    | `message_queries.rs`  |
| Account isolation (folders)  | `folders_are_isolated_per_account`                              | `folder_queries.rs`   |
| Prune correctness            | `prune_removes_stale_messages`                                  | `message_queries.rs`  |
| Prune correctness            | `prune_with_empty_live_set_clears_mailbox`                      | `message_queries.rs`  |
| Prune correctness            | `prune_does_not_affect_other_mailboxes`                         | `message_queries.rs`  |
| Prune correctness            | `prune_does_not_affect_other_accounts`                          | `message_queries.rs`  |
| Prune no-op                  | `prune_noop_when_all_messages_are_live`                         | `message_queries.rs`  |
| Schema + FTS                 | `schema_creates_cleanly`, `fts_triggers_work`                   | `schema.rs`           |
| Pending-op preservation      | `pending_op_preserved_during_sync_upsert`                       | `message_queries.rs`  |
| Pending-op → clear           | `clear_pending_op_applies_server_flags`                         | `flag_queries.rs`     |
| Pending-op → revert          | `revert_pending_op_restores_server_flags`                       | `flag_queries.rs`     |
| Pending-op expiry            | `expire_pending_ops_reverts_old_ops`                            | `flag_queries.rs`     |
| Pending-op expiry (fresh)    | `expire_pending_ops_preserves_fresh_ops`                        | `flag_queries.rs`     |
| Flag round-trip              | `flag_encoding_round_trips_all_combinations`                    | `message_queries.rs`  |
| Folder cascade               | `folder_removal_cascades_to_messages_and_attachments`           | `message_queries.rs`  |
| FTS after update             | `fts_finds_updated_subject`                                     | `search_queries.rs`   |
| FTS after delete             | `fts_removes_deleted_message`                                   | `search_queries.rs`   |
| Upsert preserves body        | `upsert_preserves_cached_body`                                  | `message_queries.rs`  |
| Thread loading (sort)        | `load_thread_returns_sorted_by_timestamp`                       | `search_queries.rs`   |
| Thread loading (filter)      | `load_thread_filters_by_mailbox_ids`                            | `search_queries.rs`   |
| Search prefix matching       | `search_prefix_matching`                                        | `search_queries.rs`   |
| Search account isolation     | `search_results_are_isolated_per_account`                       | `search_queries.rs`   |
| State get/set round-trip     | `state_get_set_round_trip`                                      | `folder_queries.rs`   |
| Folder sort order            | `load_folders_sorts_inbox_first_then_by_sort_order_then_alpha`  | `folder_queries.rs`   |
| Email state migration        | `email_state_migration_cleans_stale_keys`                       | `schema.rs`           |
| Upsert no-prune              | `upsert_folders_does_not_delete_unmentioned`                    | `folder_queries.rs`   |
| Folder remove cascade        | `remove_folders_by_id_cascades`                                 | `folder_queries.rs`   |
| Folder remove isolation      | `remove_folders_by_id_does_not_affect_others`                   | `folder_queries.rs`   |
| Multi-mailbox loading        | `message_in_multiple_mailboxes_loads_from_both`                 | `message_queries.rs`  |
| Junction-aware prune         | `prune_removes_junction_row_not_message`                        | `message_queries.rs`  |
| Delta move via junction      | `delta_sync_move_removes_junction_row`                          | `message_queries.rs`  |
| Cross-mailbox delta          | `delta_batch_saves_cross_mailbox_messages`                      | `message_queries.rs`  |
| Atomic save+state            | `save_messages_and_set_state_is_atomic`                         | `message_queries.rs`  |
| Atomic delta batch           | `delta_email_batch_removes_and_saves_atomically`                | `message_queries.rs`  |
| Backfill CRUD                | `backfill_progress_upsert_and_read`                             | `backfill_queries.rs` |
| Backfill list filtering      | `list_backfill_returns_incomplete_only`                         | `backfill_queries.rs` |
| Backfill reset               | `reset_backfill_deletes_row`                                    | `backfill_queries.rs` |
| Backfill account isolation   | `backfill_progress_isolated_per_account`                        | `backfill_queries.rs` |
| NotFound detection           | `check_set_errors_returns_not_found`                            | `email/flags.rs`      |
| Config round-trip            | `config_round_trips_max_messages`                               | `config.rs`           |
