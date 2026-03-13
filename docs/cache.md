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

Email summaries (no body). Keyed by `(account_id, email_id)`. Each message has a single `mailbox_id` — we track the *primary* mailbox, not every mailbox the message appears in. This is a deliberate simplification: JMAP allows a message in multiple mailboxes, but for the list view we only need to know where the user is looking at it.

**Body columns**: `body_rendered` (plain text) and `body_markdown` are nullable. They start as NULL and are populated lazily when the user opens a message. This means the cache-first read path (`load_body`) returns `None` for messages that haven't been viewed yet — the caller then fetches from the server and calls `save_body`.

### `attachments`

Binary attachment data. Keyed by `(account_id, email_id, idx)`. Foreign key to `messages` with `ON DELETE CASCADE`. Populated alongside `body_rendered`/`body_markdown` when a message body is fetched.

### `sync_state`

JMAP state tokens per `(account_id, resource)`. Resources are `"Mailbox"` for mailbox state or `"Email:{mailbox_id}"` for per-mailbox email state. These tokens drive delta sync — without them, the sync loop falls back to a full fetch.

### `message_fts`

FTS5 virtual table over `(subject, sender, body_rendered)`. Kept in sync via triggers on `messages` (insert/update/delete). Queried by `do_search` for cross-account full-text search.

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
3. Created + updated → `Email/get` by ID, then partition:
   - Still in this mailbox → `save_messages`
   - Moved to another mailbox → `remove_message` from this mailbox's cache
4. State token is updated after each batch
5. If `hasMoreChanges`, loop (capped at 50 iterations)
6. Falls back to full sync on `cannotCalculateChanges`

### Mailbox sync

Always does a full `Mailbox/get` fetch when changes are detected (mailbox list is small). `save_folders` handles cascading: folders removed from the server get their messages and attachments deleted.

## Invariants

These must hold at all times. Tests should prove each one.

1. **Account isolation**: Operations on account A never affect account B. Every query and mutation includes `account_id` in its WHERE clause.

2. **Pending-op preservation**: A background sync never overwrites `flags_local` or `is_read`/`is_starred` on a message with a pending_op.

3. **Prune correctness**: After `prune_mailbox(account, mailbox, live_ids)`:
   - Every cached message for that (account, mailbox) has an email_id in `live_ids`
   - Messages in other mailboxes are untouched
   - Messages in other accounts are untouched

4. **Body cache independence**: `save_body` and `load_body` are keyed by `(account_id, email_id)`. Saving a body for account A's copy of email_id X does not populate account B's copy.

5. **FTS consistency**: The FTS index stays in sync with the messages table via triggers. After insert, update, or delete on messages, the FTS index reflects the current state.

6. **State token durability**: After a successful sync, the state token is persisted. A crash after sync but before state save means the next sync re-fetches the same delta (idempotent, not data-losing).

7. **Folder cascade**: When `save_folders` removes a folder that no longer exists on the server, all messages and attachments in that folder are deleted.

8. **Flag round-trip**: `flags_to_u8(is_read, is_starred)` → `flags_from_u8(u8)` is lossless. `flags_to_u8(false, false) == 0`, `flags_to_u8(true, false) == 1`, `flags_to_u8(false, true) == 2`, `flags_to_u8(true, true) == 3`.

## Known Simplifications

1. **Single mailbox per message**: JMAP allows a message in multiple mailboxes. We store one `mailbox_id`. If a message appears in both Inbox and a label, we track whichever mailbox we synced first. This can cause a message to "disappear" from a folder view if delta sync sees it move.

2. **Per-mailbox email state**: State tokens are keyed by `Email:{mailbox_id}`, but `Email/changes` is account-global in JMAP. This means the same email change may be processed multiple times if multiple mailboxes are synced. The upsert is idempotent, so this is correct but redundant work.

3. **Search is cross-account**: `do_search` queries the FTS index without an account filter. For multi-account users, results blend across accounts. Each result has `account_id` set from the messages table so the caller can partition if needed.

4. **No attachment-only cache eviction**: Body and attachment data are stored forever once fetched. There's no LRU or size-based eviction. The database grows monotonically until messages are pruned by sync.

5. **No WAL mode**: The database doesn't explicitly enable WAL mode. Since all writes happen on one thread, this is fine, but WAL would allow concurrent reads from the async side if we ever need that.

## Test Coverage Required

Each invariant above maps to tests. Here's what exists and what's missing.

### Existing tests

| Invariant | Test | File |
|---|---|---|
| Account isolation | `messages_bodies_flags_and_removal_are_isolated_per_account` | `queries.rs` |
| Account isolation (folders) | `folders_are_isolated_per_account` | `folder_queries.rs` |
| Prune correctness | `prune_removes_stale_messages` | `queries.rs` |
| Prune correctness | `prune_with_empty_live_set_clears_mailbox` | `queries.rs` |
| Prune correctness | `prune_does_not_affect_other_mailboxes` | `queries.rs` |
| Prune correctness | `prune_does_not_affect_other_accounts` | `queries.rs` |
| Prune no-op | `prune_noop_when_all_messages_are_live` | `queries.rs` |
| Schema + FTS | `schema_creates_cleanly`, `fts_triggers_work` | `schema.rs` |
| Flag round-trip | (implicit in `messages_bodies_flags_and_removal_are_isolated_per_account`) | `queries.rs` |

### Tests added to prove invariants

| Invariant | Test | File |
|---|---|---|
| Pending-op preservation | `pending_op_preserved_during_sync_upsert` | `queries.rs` |
| Pending-op → clear | `clear_pending_op_applies_server_flags` | `queries.rs` |
| Pending-op → revert | `revert_pending_op_restores_server_flags` | `queries.rs` |
| Flag round-trip | `flag_encoding_round_trips_all_combinations` | `queries.rs` |
| Folder cascade | `folder_removal_cascades_to_messages_and_attachments` | `queries.rs` |
| FTS after update | `fts_finds_updated_subject` | `queries.rs` |
| FTS after delete | `fts_removes_deleted_message` | `queries.rs` |
| Upsert preserves body | `upsert_preserves_cached_body` | `queries.rs` |
| Thread loading (sort) | `load_thread_returns_sorted_by_timestamp` | `queries.rs` |
| Thread loading (filter) | `load_thread_filters_by_mailbox_ids` | `queries.rs` |
| Search prefix matching | `search_prefix_matching` | `queries.rs` |
| State get/set round-trip | `state_get_set_round_trip` | `folder_queries.rs` |
| Folder sort order | `load_folders_sorts_inbox_first_then_by_sort_order_then_alpha` | `folder_queries.rs` |
