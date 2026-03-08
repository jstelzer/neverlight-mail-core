# JMAP-Only Pivot for Neverlight Mail

**Status:** Architecture finalized, implementation not started
**Date:** 2026-03-07
**Target provider:** Fastmail
**License target:** MIT/Apache-2.0 (dual-license)

## The Pivot

Drop melib, drop IMAP, drop lettre/SMTP. Implement JMAP (RFC 8620 + 8621) natively in Rust as the sole mail transport for neverlight-mail-core. Sending goes through `EmailSubmission/set`, not SMTP.

### Why

1. **melib is GPL.** Every downstream crate inherits GPL. A permissive-licensed JMAP client crate fills a real gap in the Rust ecosystem.

2. **melib's JMAP backend is broken.** `Email/changes.updated` silently dropped. `EmailQueryChanges.added` silently dropped. No `Mailbox/changes`. `Flag::TRASHED` maps to `$junk`. We'd spend more time patching around melib than writing a clean implementation.

3. **IMAP is dead weight.** This is a personal mail client on Fastmail. IMAP adds: long-lived TCP state machines, IDLE death cascades, UID tracking, folder-by-folder polling, `imap-codec`/`imap-types` version pinning nightmares, and the entire reconnect/backoff machinery in the GUI layer. All of that evaporates with JMAP.

4. **SMTP dies too.** Fastmail supports `urn:ietf:params:jmap:submission`. Sending is a JSON POST, not a separate protocol with STARTTLS negotiation, port guessing, and relay config. lettre goes away.

5. **The codebase gets radically simpler.** One protocol, one transport (HTTP), one auth flow, stateless requests. The entire `watch.rs` IDLE death cascade, reconnect backoff, and connection health machinery in the GUI becomes a polling loop or SSE stream over HTTP.

### What dies

| Module                     | Reason                                                |
|----------------------------|-------------------------------------------------------|
| `imap.rs`                  | No more IMAP                                          |
| `smtp.rs`                  | `EmailSubmission/set` replaces SMTP                   |
| `envelope.rs`              | melib `Envelope` type gone; replaced by `mail-parser` |
| melib dependency           | GPL, buggy JMAP, IMAP baggage                         |
| lettre dependency          | No more SMTP                                          |
| isahc dependency           | Replaced by reqwest (single HTTP client)              |
| indexmap dependency        | Only used for melib `AccountSettings`                 |
| imap-codec/imap-types pins | No more IMAP means no more pinning nightmares         |

### What survives (unchanged)

| Module         | Why                                                                         |
|----------------|-----------------------------------------------------------------------------|
| `models.rs`    | `Folder`, `MessageSummary`, `AttachmentData` are transport-agnostic         |
| `store/*`      | SQLite cache is protocol-neutral (column renames only)                      |
| `mime.rs`      | Pure functions over `html-safe-md`, no melib deps                           |
| `keyring.rs`   | OS credential storage, protocol-independent                                 |
| `config.rs`    | Config resolution, multi-account — minor cleanup to remove SMTP/IMAP fields |
| `discovery.rs` | Already clean (swap isahc for reqwest)                                      |
| All UI code    | Sidebar, message list, message view, compose — protocol-invisible           |

### What gets rewritten

| Module        | Old                                 | New                                                |
|---------------|-------------------------------------|----------------------------------------------------|
| `envelope.rs` | melib `Envelope` → `MessageSummary` | `mail-parser` `Message` → `MessageSummary`         |
| `jmap.rs`     | melib `JmapType` wrapper            | Native JMAP client (RFC 8620/8621)                 |
| `lib.rs`      | melib re-exports                    | Owned types (`EmailId`, `MailboxId`, `Flag`, etc.) |

---

## New Architecture

### Module layout

```
neverlight-mail-core/
├── Cargo.toml
├── src/
│   ├── lib.rs              — pub mod + owned type exports
│   ├── types.rs            — EmailId, MailboxId, Flag, FlagOp, ChangeState, etc.
│   ├── client.rs           — JmapClient: HTTP transport, request batching, error handling
│   ├── session.rs          — Session discovery, capability negotiation, account state
│   ├── mailbox.rs          — Mailbox/get, Mailbox/changes, Mailbox/set
│   ├── email.rs            — Email/query, Email/get, Email/set, Email/changes
│   ├── submit.rs           — EmailSubmission/set (replaces SMTP entirely)
│   ├── sync.rs             — Delta sync loop: Email/changes + Mailbox/changes
│   ├── push.rs             — EventSource SSE stream (RFC 8620 §7.3)
│   ├── parse.rs            — RFC 5322 body/attachment extraction via mail-parser
│   ├── mime.rs             — render_body, render_body_markdown (unchanged)
│   ├── config.rs           — Config resolution (simplified: no SMTP fields)
│   ├── discovery.rs        — .well-known/jmap probe (swap isahc → reqwest)
│   ├── keyring.rs          — OS keyring (unchanged)
│   ├── models.rs           — Folder, MessageSummary, AttachmentData (unchanged)
│   └── store/
│       ├── mod.rs           — Re-exports (unchanged)
│       ├── schema.rs        — DDL + migration (rename envelope_hash → email_id, etc.)
│       ├── flags.rs         — Flag encode/decode (unchanged)
│       ├── commands.rs      — CacheCmd enum (unchanged)
│       ├── queries.rs       — SQL functions (column name updates)
│       └── handle.rs        — CacheHandle async facade (unchanged)
└── tests/
    ├── fixtures/            — Email fixtures (unchanged)
    └── jmap_types_test.rs   — Serde round-trip tests for JMAP request/response types
```

### Dependencies (post-pivot)

| Crate | Purpose | License |
|-------|---------|---------|
| reqwest | HTTP client (JMAP API, SSE, blob upload/download) | MIT/Apache-2.0 |
| mail-parser | RFC 5322 parsing (replaces melib envelope/attachment) | MIT/Apache-2.0 |
| rusqlite | SQLite cache (bundled) | MIT |
| html-safe-md | HTML → markdown/plaintext rendering | (ours) |
| keyring | OS credential storage | MIT/Apache-2.0 |
| tokio | Async runtime | MIT |
| serde / serde_json | JMAP request/response serialization | MIT/Apache-2.0 |
| dirs | XDG directory resolution | MIT/Apache-2.0 |
| open | System browser | MIT |
| uuid | Account ID generation | MIT/Apache-2.0 |
| log | Logging | MIT/Apache-2.0 |

**Removed:** melib, lettre, isahc, indexmap, futures, imap-codec, imap-types

### Owned types (types.rs)

Replace melib re-exports with our own types. These propagate to all consumers.

```rust
/// Stable JMAP email identifier (server-assigned, never changes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmailId(pub String);

/// JMAP mailbox identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MailboxId(pub String);

/// JMAP state token for delta sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State(pub String);

/// Email flags (JMAP keywords).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    pub seen: bool,     // $seen
    pub flagged: bool,  // $flagged
    pub draft: bool,    // $draft
    pub answered: bool, // $answered
}

/// Flag mutation operation.
#[derive(Debug, Clone)]
pub enum FlagOp {
    SetSeen(bool),
    SetFlagged(bool),
}

/// Delta sync event (replaces melib BackendEvent/RefreshEventKind).
#[derive(Debug, Clone)]
pub enum SyncEvent {
    Created(EmailId),
    Updated(EmailId),
    Destroyed(EmailId),
    FlagsChanged(EmailId, Flags),
    MailboxCreated(MailboxId),
    MailboxUpdated(MailboxId),
    MailboxDestroyed(MailboxId),
}
```

**Migration path for GUI:** The GUI currently imports `EnvelopeHash`, `MailboxHash`, `Flag`, `FlagOp`, `BackendEvent`, `RefreshEventKind` from `neverlight_mail_core`. It stores raw `u64` values internally. The new types use `String` IDs (JMAP's native format). The GUI's `app/types.rs` switches from `u64` hashes to `String` IDs. Since the GUI already accesses these through `neverlight_mail_core::` re-exports, no import paths change — only the types behind them.

---

## JMAP Client Design

### Transport layer (client.rs)

```rust
pub struct JmapClient {
    http: reqwest::Client,
    api_url: String,
    upload_url: String,
    download_url: String,
    event_source_url: Option<String>,
    account_id: String,
}
```

All JMAP operations are `POST {api_url}` with a JSON body containing method calls. The client handles:

- **Request batching** — multiple method calls in a single HTTP request with back-references
- **Error classification** — `cannotCalculateChanges` triggers full resync, `stateMismatch` retries
- **Blob upload** — `POST {upload_url}` for email submission
- **Blob download** — `GET {download_url}` for raw RFC 5322 content

### Request batching pattern

```json
{
  "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
  "methodCalls": [
    ["Mailbox/get", { "accountId": "..." }, "0"],
    ["Email/query", { "accountId": "...", "filter": { "inMailbox": "..." } }, "1"],
    ["Email/get", { "accountId": "...", "#ids": { "resultOf": "1", "name": "Email/query", "path": "/ids" } }, "2"]
  ]
}
```

One round trip: fetch mailboxes + query inbox + get email objects.

### Session management (session.rs)

On connect:
1. `GET /.well-known/jmap` (existing `discovery.rs`)
2. Parse Session object → extract `apiUrl`, `uploadUrl`, `downloadUrl`, `eventSourceUrl`, `accounts`
3. Validate required capabilities: `urn:ietf:params:jmap:core`, `urn:ietf:params:jmap:mail`, `urn:ietf:params:jmap:submission`
4. Store session state (capability limits: `maxObjectsInGet`, `maxObjectsInSet`, `maxCallsInRequest`)

Auth: Fastmail app-specific passwords via HTTP Basic. Bearer token support for future OAuth.

### Delta sync (sync.rs)

The core sync loop — this is the key win over IMAP:

```
loop:
    Email/changes { sinceState } → created, updated, destroyed, newState
    if created:  Email/get { ids: created } → insert into cache
    if updated:  Email/get { ids: updated, properties: [keywords, mailboxIds] } → update cache
    if destroyed: remove from cache
    Mailbox/changes { sinceState } → folder count/name changes
    persist newState
    sleep(poll_interval) or await SSE ping
```

**Unlike melib, we handle ALL of `created`, `updated`, AND `destroyed`.** No silent drops.

If `Email/changes` returns `cannotCalculateChanges`, fall back to full `Email/query` for the affected mailbox.

### Push notifications (push.rs)

JMAP EventSource (RFC 8620 §7.3):

```
GET {eventSourceUrl}?types=Email,Mailbox&closeafter=state&ping=30
Accept: text/event-stream
```

Server sends SSE events when state changes. Client receives ping, calls `Email/changes` with the new state. No polling needed.

This replaces the entire IDLE death cascade. HTTP SSE is:
- Stateless reconnect (just re-GET with last state)
- No TCP keepalive issues
- Works through proxies and load balancers
- Built into reqwest via `reqwest-eventsource` or manual chunked response reading

**Fallback:** If SSE isn't available or disconnects, fall back to polling (configurable interval, default 30s).

### Email submission (submit.rs)

Replaces lettre/SMTP entirely:

```
1. Upload RFC 5322 blob:
   POST {uploadUrl} → blobId

2. Create draft + submit in one batch:
   Email/set { create: { "draft1": { from, to, subject, textBody, ... } } }
   EmailSubmission/set { create: { "sub1": { emailId: "#draft1", identityId: "..." } } }

3. Server sends via SMTP on our behalf
```

Or for simple text emails, construct the Email object directly with `bodyValues` — no blob upload needed.

Identity selection: `Identity/get` returns the account's sending identities. Match on `from` address or use default.

---

## Schema Migration

### Cache column renames

The SQLite cache stores protocol-neutral data, but column names reference melib concepts:

| Old column | New column | Type change |
|-----------|-----------|-------------|
| `envelope_hash` (INTEGER) | `email_id` (TEXT) | u64 → JMAP string ID |
| `mailbox_hash` (INTEGER) | `mailbox_id` (TEXT) | u64 → JMAP string ID |

Migration: forward-only, add new columns + index, backfill from old, drop old. Existing IMAP-era cache data is invalidated (full resync on first JMAP connect).

### models.rs field renames

```rust
pub struct MessageSummary {
    pub account_id: String,
    pub email_id: String,          // was: envelope_hash (u64)
    pub subject: String,
    pub from: String,
    pub to: String,
    pub date: String,
    pub is_read: bool,
    pub is_starred: bool,
    pub has_attachments: bool,
    pub thread_id: Option<String>, // was: Option<u64> — JMAP provides native threadId
    pub mailbox_id: String,        // was: mailbox_hash (u64)
    pub timestamp: i64,
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub reply_to: Option<String>,
    pub thread_depth: u32,
}
```

Note: JMAP provides `threadId` natively — no need for our `compute_thread_id()` hash trick. Server-provided thread IDs are canonical.

---

## Body Parsing (parse.rs)

Replaces `envelope.rs`. Uses `mail-parser` (MIT) instead of melib for RFC 5322 parsing:

```rust
use mail_parser::Message;

pub fn parse_email(raw: &[u8]) -> ParsedEmail { ... }
pub fn extract_body(msg: &Message) -> (Option<String>, Option<String>, Vec<AttachmentData>) { ... }
```

`mail-parser` handles:
- MIME tree walking
- Content-Transfer-Encoding decoding
- Character set conversion
- Attachment extraction
- Header parsing (From, To, Subject, Date, Message-ID, References, In-Reply-To)

This replaces melib's `Envelope`, `Attachment`, `ContentType`, `Mail::new()`, and `att.decode()`.

For list views, JMAP returns structured data (`from`, `subject`, `receivedAt`, `keywords`) directly — no RFC 5322 parsing needed. `mail-parser` is only needed when fetching full message bodies via blob download.

---

## Implementation Phases

### Phase 0: Scaffold (branch: `jmap` on both repos)

1. Create `types.rs` with owned types
2. Swap `isahc` → `reqwest` in `discovery.rs`
3. Stub out `client.rs`, `session.rs`, `email.rs`, `mailbox.rs`, `submit.rs`, `sync.rs`
4. Add `mail-parser` dep, create `parse.rs` with body extraction
5. Remove `melib`, `lettre`, `isahc`, `indexmap` from Cargo.toml
6. Delete `imap.rs`, `smtp.rs`, old `jmap.rs`, `envelope.rs`
7. Update `lib.rs` exports
8. Schema migration for `email_id`/`mailbox_id` columns

**Gate:** `cargo check` passes with stubs. No melib or lettre in the dep tree.

### Phase 1: Read path

1. `session.rs` — connect, discover capabilities, store session
2. `client.rs` — batched method calls, error handling
3. `mailbox.rs` — `Mailbox/get` → `Vec<Folder>`
4. `email.rs` — `Email/query` + `Email/get` → `Vec<MessageSummary>` (header-only)
5. `email.rs` — blob download + `parse.rs` → full body + attachments
6. Wire into GUI: `connect_account()` → `fetch_folders()` → `fetch_messages()` → `fetch_body()`

**Gate:** Launch app, connect to Fastmail, see folder list, see message list, read a message body.

### Phase 2: Write path

1. `email.rs` — `Email/set` for flag changes (`$seen`, `$flagged`)
2. `email.rs` — `Email/set` for mailbox moves (patch `mailboxIds`)
3. `email.rs` — `Email/set { destroy }` for permanent delete
4. `submit.rs` — `EmailSubmission/set` for sending
5. Wire into GUI: flag toggles, trash/archive, compose+send

**Gate:** Toggle read/star, move to trash, send a reply — all via JMAP, no SMTP.

### Phase 3: Sync + push

1. `sync.rs` — `Email/changes` delta loop (created + updated + destroyed — all handled)
2. `sync.rs` — `Mailbox/changes` for folder mutations
3. `push.rs` — EventSource SSE stream
4. Wire into GUI: replace IDLE watch + periodic refresh with SSE + delta sync
5. Fallback: polling at configurable interval when SSE unavailable

**Gate:** Change a flag in Fastmail web UI, see it reflected in the app within seconds (SSE) or within one poll cycle.

### Phase 4: Polish

1. Search — `Email/query` with JMAP filter operators
2. Mailbox management — `Mailbox/set` for create/rename/delete
3. Identity selection — `Identity/get`, match on From address
4. Error handling — submission errors, rate limits, quota warnings
5. Pagination — `Email/query` with `position`/`limit` for large mailboxes

---

## GUI Impact

### What changes in neverlight-mail

| Area | Change |
|------|--------|
| `app/types.rs` | `MailSession` becomes single variant (no IMAP), ID fields switch from `u64` to `String` |
| `app/watch.rs` | IDLE stream → SSE stream or poll timer. `BackendEvent`/`RefreshEventKind` → `SyncEvent` |
| `app/actions.rs` | `Flag`/`FlagOp` → owned types. `EnvelopeHash`/`MailboxHash` → `EmailId`/`MailboxId` |
| `app/body.rs` | `EnvelopeHash` → `EmailId` |
| `app/sync.rs` | `MailboxHash` → `MailboxId` |
| `app/compose.rs` | `smtp::send_email()` → `submit::send_email()` |
| `app/setup.rs` | Remove SMTP config fields. Simplify to: server, username, app password |
| `app/mod.rs` | Remove IMAP session variant, simplify connect flow |

### What doesn't change

- `ui/sidebar.rs` — renders `Folder`, no protocol types
- `ui/message_list.rs` — renders `MessageSummary`, no protocol types
- `ui/message_view.rs` — renders body strings + `AttachmentData`, no protocol types
- `ui/compose_dialog.rs` — renders `AttachmentData`, no protocol types
- Optimistic update / rollback logic — same pattern, different ID types
- Lane epochs / stale-apply protection — unchanged
- DnD architecture — unchanged

### What dies in the GUI

- `watch.rs` IDLE death cascade, reconnect backoff, stuck-refresh detection — all replaced by HTTP reconnect
- `SmtpConfig` setup fields
- IMAP-specific connection health diagnostics
- The entire `conn_state` state machine simplifies (HTTP is stateless; connected = "last request succeeded")

---

## Branch Strategy

| Repo                   | Branch | Base   |
|------------------------|--------|--------|
| `neverlight-mail-core` | `jmap` | `main` |
| `neverlight-mail`      | `jmap` | `main` |

Rules:
- `main` stays as-is (IMAP/melib) as the rollback point
- `jmap` branch is a hard fork — no incremental migration, clean break
- Merge to `main` only after Phase 2 gate passes (read + write + send all work)
- The umbrella repo tracks `jmap` branch SHAs during development

---

## Risk

**Low-medium.** The main risk is completeness of the JMAP implementation, not architectural soundness. Mitigations:

- Fastmail's JMAP implementation is the reference — excellent docs, predictable behavior
- RFC 8620/8621 are well-specified and JSON-based — no binary protocol parsing
- `mail-parser` is battle-tested for RFC 5322 body extraction
- The cache layer and UI are almost entirely reusable
- `main` branch is the escape hatch if something goes sideways

**The biggest win is what we DON'T have to maintain:** no IDLE, no SMTP, no melib patches, no imap-codec pins, no connection health state machine, no GPL.

---

## References

- [RFC 8620 — JMAP Core](https://www.rfc-editor.org/rfc/rfc8620)
- [RFC 8621 — JMAP Mail](https://www.rfc-editor.org/rfc/rfc8621)
- [Fastmail JMAP documentation](https://www.fastmail.com/dev/)
- [mail-parser crate](https://crates.io/crates/mail-parser) (MIT/Apache-2.0)
- [reqwest crate](https://crates.io/crates/reqwest) (MIT/Apache-2.0)

---

## Historical: melib JMAP Audit

The melib audit that motivated this pivot is preserved below for reference.

<details>
<summary>melib 0.8.13 JMAP source audit (2026-03-06)</summary>

### Delta sync: `email_changes()` (connection.rs:446-704)

1. Calls `Email/changes` with `sinceState`
2. Back-references `created` IDs into `Email/get` to fetch new envelopes in the same batch
3. Calls `Email/queryChanges` per-mailbox for per-folder tracking
4. Loops if `has_more_changes` is true

**Confirmed gaps:**

1. **`updated` items from `Email/changes` completely ignored** (connection.rs:549, `// [ref:TODO]: process changes_response.updated too`)
2. **`queryChanges.added` items completely dropped** (connection.rs:651, `// [ref:TODO] do something with added items`)
3. **No `Mailbox/changes` call anywhere**

### Flag mapping: TRASHED → $junk (mod.rs:1102, 1119)

`Flag::TRASHED` maps to JMAP keyword `$junk` (spam). Semantically wrong.

### Operations that worked

| Operation | Implementation |
|-----------|----------------|
| `set_flags` | Batches into `Email/set`, re-fetches keywords |
| `copy_messages` | Patches `mailboxIds` via `Email/set` |
| `delete_messages` | `Email/set { destroy: [...] }` |
| `watch` | 60s `sleep` loop calling `email_changes()` |

</details>

# Docs
https://www.fastmail.com/dev/
https://datatracker.ietf.org/doc/html/rfc8620




