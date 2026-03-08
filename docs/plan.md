# Build Plan: JMAP-Native Engine

**Reference specs:**
- [RFC 8620 — JMAP Core](https://datatracker.ietf.org/doc/html/rfc8620)
- [RFC 8621 — JMAP Mail](https://datatracker.ietf.org/doc/html/rfc8621)
- [Fastmail Developer Docs](https://www.fastmail.com/dev/)

Each phase is independently testable. No phase depends on a GUI.

---

## Phase 0: Scaffold (Done)

What shipped:
- Owned types (`EmailId`, `MailboxId`, `State`, `Flags`, `SyncEvent`, etc.)
- `JmapClient` with batched request transport
- `JmapSession` with capability parsing
- `parse_body()` via mail-parser
- SQLite cache with string IDs throughout
- 43 tests passing, clippy clean, zero melib in dep tree

---

## Phase 1: Read Path

**Goal:** Connect to Fastmail, list mailboxes, query emails, fetch bodies. Enough for a read-only mail viewer.

### 1a. Session + Mailbox Discovery

**Module:** `session.rs`, `mailbox.rs`

Connect with a bearer token, discover capabilities, fetch all mailboxes.

| RFC reference | Method |
|---|---|
| RFC 8620 §2 | Session resource discovery |
| RFC 8621 §2 | Mailbox/get |

**Implementation:**
- `JmapSession::connect(url, auth)` — already scaffolded
- `mailbox::fetch_all(client) -> Vec<Folder>` — `Mailbox/get` with properties: `id`, `name`, `parentId`, `role`, `sortOrder`, `totalEmails`, `unreadEmails`, `myRights`
- Map JMAP `Mailbox` → `models::Folder` (with `role`, `sort_order`)
- Cache mailboxes via `CacheHandle::save_folders()`

**Tests:**
- **Unit:** Parse a Mailbox/get response fixture → correct `Vec<Folder>`
- **Unit:** Role-based sort order (inbox first, then drafts/sent/trash/archive, then alphabetical)
- **Integration:** `session_connect_and_list_mailboxes` — connects to Fastmail, asserts ≥1 mailbox with role "inbox"

### 1b. Email Query + Get

**Module:** `email.rs`

List emails in a mailbox, fetch metadata for display.

| RFC reference | Method |
|---|---|
| RFC 8621 §4.4 | Email/query |
| RFC 8621 §4.1 | Email/get |

**Implementation:**
- `email::query(client, mailbox_id, limit, position) -> (Vec<EmailId>, QueryState)` — `Email/query` with filter `{ inMailbox }`, sort `receivedAt DESC`
- `email::get_summaries(client, ids) -> Vec<MessageSummary>` — `Email/get` with properties: `id`, `threadId`, `mailboxIds`, `keywords`, `from`, `to`, `subject`, `receivedAt`, `size`, `hasAttachment`, `preview`
- Batch query+get using result references (RFC 8620 §3.7): query returns IDs, get uses `#ids` back-reference
- Map JMAP response → `Vec<MessageSummary>`
- Cache via `CacheHandle::save_messages()`

**Tests:**
- **Unit:** Parse Email/query response fixture → correct ID list and state
- **Unit:** Parse Email/get response fixture → correct `Vec<MessageSummary>` with flags decoded from keywords
- **Unit:** Result reference serialization (`#methodResponses/0/1/ids`)
- **Integration:** `query_inbox_messages` — queries inbox, asserts ≥1 message with non-empty subject

### 1c. Body Fetch

**Module:** `email.rs`, `parse.rs`, `mime.rs`

Download full message body for reading.

| RFC reference | Method |
|---|---|
| RFC 8621 §4.2 | Email/get (bodyValues) |
| RFC 8620 §6.2 | Blob download |

**Two strategies (try bodyValues first, fall back to blob):**

1. **bodyValues approach:** `Email/get` with `properties: [bodyValues, textBody, htmlBody]` and `fetchAllBodyValues: true`. Server returns body text inline — no separate download needed.
2. **Blob fallback:** `Email/get` with `blobId` property, then `client.download_blob(blobId)` → raw RFC 5322 → `parse_body()`.

**Implementation:**
- `email::get_body(client, email_id) -> (String, String, Vec<AttachmentData>)` — returns (markdown, plain, attachments)
- Try bodyValues first. If `textBody`/`htmlBody` parts have content, use them directly.
- If bodyValues empty or server doesn't support it, download blob and parse with `parse_body()`.
- HTML → markdown via `html-safe-md`
- Cache via `CacheHandle::save_body()`

**Tests:**
- **Unit:** Parse Email/get bodyValues response → correct text extraction
- **Unit:** Blob download + parse_body pipeline with fixture email
- **Unit:** HTML → markdown rendering (existing mime tests)
- **Integration:** `fetch_message_body` — fetches a real message body, asserts non-empty text

### Phase 1 exit criteria

- Can connect, list mailboxes, query messages, read bodies
- All data cached in SQLite
- Works end-to-end against Fastmail with `NEVERLIGHT_MAIL_JMAP_TOKEN` + `NEVERLIGHT_MAIL_USER`

---

## Phase 2: Write Path

**Goal:** Modify flags, move messages, delete, send email. Full read-write mail client.

### 2a. Flag Operations

**Module:** `email.rs`

Toggle read/unread, star/unstar, archive.

| RFC reference | Method |
|---|---|
| RFC 8621 §4.3 | Email/set (keywords patch) |

**Implementation:**
- `email::set_flags(client, email_id, flag_op: FlagOp) -> Result<(), JmapError>`
- JMAP keywords: `$seen` (read), `$flagged` (starred), `$draft`, `$answered`, `$forwarded`
- Use JSON Patch semantics: `update: { "email_id": { "keywords/$seen": true } }`
- Optimistic local update: `CacheHandle::update_flags()` immediately, then sync to server
- On server success: `CacheHandle::clear_pending_op()`
- On server failure: `CacheHandle::revert_pending_op()`

**Tests:**
- **Unit:** Flag op → correct Email/set request body
- **Unit:** Keyword patch serialization (`keywords/$seen: true` vs `keywords/$seen: null`)
- **Integration:** `toggle_read_flag` — mark a message read, re-fetch, verify `$seen` keyword present

### 2b. Move + Delete

**Module:** `email.rs`

Move between mailboxes, trash, permanent delete.

| RFC reference | Method |
|---|---|
| RFC 8621 §4.3 | Email/set (mailboxIds patch) |

**Implementation:**
- `email::move_to(client, email_id, from_mailbox, to_mailbox)` — patch `mailboxIds`
- `email::trash(client, email_id, trash_mailbox_id)` — move to trash
- `email::destroy(client, email_ids)` — `Email/set { destroy: [...] }` for permanent delete
- Batch support: multiple moves/deletes in one `Email/set` call

**Tests:**
- **Unit:** Move request body serialization (mailboxIds patch)
- **Unit:** Destroy request body serialization
- **Integration:** `move_message_to_trash` — move a message to trash, verify it's in trash mailbox

### 2c. Send Email

**Module:** `submit.rs`

Compose and send via JMAP (no SMTP).

| RFC reference | Method |
|---|---|
| RFC 8621 §7 | EmailSubmission/set |
| RFC 8621 §6 | Identity/get |

**Implementation:**
- `submit::get_identities(client) -> Vec<Identity>` — fetch available sender identities
- `submit::send(client, identity_id, draft_email_id)` — `EmailSubmission/set { create }` with `onSuccessUpdateEmail` to move from Drafts to Sent
- Draft creation: `Email/set { create }` with `mailboxIds: { drafts_id: true }`, `keywords: { $draft: true }`, body parts
- Batch: create draft + submit in single request using creation ID references (`#emailId`)

**Tests:**
- **Unit:** Identity/get response parsing
- **Unit:** EmailSubmission/set request serialization with creation ID references
- **Unit:** Draft Email/set create body (RFC 5322 structure in JMAP)
- **Integration:** `send_test_email` — send to self, verify arrival (gated behind `NEVERLIGHT_MAIL_TEST_SEND=true`)

### Phase 2 exit criteria

- Can toggle flags, move/trash/delete messages, send email
- All write ops use optimistic local update + server confirmation
- Sending works without SMTP — pure JMAP

---

## Phase 3: Sync + Push

**Goal:** Delta sync for efficient updates, push notifications for real-time.

### 3a. Delta Sync

**Module:** `sync.rs`

Efficient polling: only fetch what changed since last sync.

| RFC reference | Method |
|---|---|
| RFC 8620 §5.2 | Foo/changes |
| RFC 8621 §4.5 | Email/changes |
| RFC 8621 §2.5 | Mailbox/changes |

**Implementation:**
- `sync::sync_mailboxes(client, cache, since_state) -> NewState` — `Mailbox/changes` → fetch changed, remove destroyed
- `sync::sync_emails(client, cache, mailbox_id, since_state) -> NewState` — `Email/changes` → fetch created+updated IDs via `Email/get`, remove destroyed from cache
- State tracking: store `State` per (account, resource_type) in cache
- Handle `cannotCalculateChanges` error → full resync
- Batch: `Mailbox/changes` + `Email/changes` in single request

**Tests:**
- **Unit:** Parse Email/changes response (created, updated, destroyed lists)
- **Unit:** Parse Mailbox/changes response
- **Unit:** `cannotCalculateChanges` error handling → triggers full resync
- **Integration:** `delta_sync_after_flag_change` — set a flag, run sync, verify only the changed email appears in delta

### 3b. State Storage

**Module:** `store/`

Persist sync state tokens.

**Implementation:**
- New table: `sync_state (account_id TEXT, resource TEXT, state TEXT, PRIMARY KEY (account_id, resource))`
- `CacheHandle::get_state(account_id, resource) -> Option<State>`
- `CacheHandle::set_state(account_id, resource, state)`
- Resources: `"Mailbox"`, `"Email"`, `"Email:{mailbox_id}"`

**Tests:**
- **Unit:** State round-trip (save + load)
- **Unit:** State isolation per account

### 3c. EventSource Push

**Module:** `push.rs`

Real-time notifications via SSE (replaces IMAP IDLE).

| RFC reference | Method |
|---|---|
| RFC 8620 §7.3 | EventSource |

**Implementation:**
- `push::listen(client, types, close_after) -> impl Stream<Item = StateChange>`
- Connect to `eventSourceUrl` from session with `types` filter (e.g., `Email,Mailbox`)
- Parse SSE `state` events → `StateChange { changed: HashMap<AccountId, HashMap<TypeName, State>> }`
- On state change → trigger delta sync for affected types
- Reconnect on connection drop with exponential backoff
- `ping` interval from session capabilities

**Tests:**
- **Unit:** Parse SSE state change event fixture
- **Unit:** Type filter serialization
- **Integration:** `eventsource_receives_state_change` — connect SSE, set a flag via API, verify state change arrives within 30s

### Phase 3 exit criteria

- Delta sync reduces bandwidth to changed items only
- Push provides real-time notification of changes
- State persists across restarts — no full resync on relaunch
- Graceful fallback: if push unavailable, poll at interval

---

## Phase 4: Polish

**Goal:** Search, mailbox management, identity selection, error recovery.

### 4a. Server-Side Search

**Module:** `email.rs`

| RFC reference | Method |
|---|---|
| RFC 8621 §4.4 | Email/query (filter) |

**Implementation:**
- `email::search(client, query_text, mailbox_id?) -> Vec<EmailId>` — `Email/query` with `filter: { text: "..." }` (full-text) or structured filters (`from`, `to`, `subject`, `hasAttachment`, `before`, `after`)
- Combine with local FTS: search cache first, then server for uncached results
- Result merging: deduplicate by `email_id`

**Tests:**
- **Unit:** Filter serialization for various query types
- **Integration:** `search_by_text` — search for a known subject, verify it appears in results

### 4b. Mailbox Management

**Module:** `mailbox.rs`

| RFC reference | Method |
|---|---|
| RFC 8621 §2.4 | Mailbox/set |

**Implementation:**
- `mailbox::create(client, name, parent_id?)` — create new mailbox/folder
- `mailbox::rename(client, mailbox_id, new_name)`
- `mailbox::destroy(client, mailbox_id)` — delete empty mailbox
- Respect `myRights` from Mailbox/get (don't offer operations the server forbids)

**Tests:**
- **Unit:** Mailbox/set create/update/destroy request serialization
- **Integration:** `create_and_delete_mailbox` — create a test mailbox, verify it appears, delete it

### 4c. Multi-Identity

**Module:** `submit.rs`

Select which identity (sender address) to use when composing.

**Implementation:**
- Cache identities from `Identity/get` (already in 2c)
- Default to primary identity
- Allow identity selection per-compose

**Tests:**
- **Unit:** Identity selection logic (primary vs explicit)

### 4d. Error Recovery

**Module:** `client.rs`, `sync.rs`

Handle network errors, rate limits, server errors gracefully.

**Implementation:**
- Retry with exponential backoff on HTTP 429 / 503
- Surface `JmapError::MethodError` with actionable context
- On `accountNotFound` / `accountReadOnly` → clear session, re-discover
- Pending write queue: if offline, queue flag/move ops and flush on reconnect

**Tests:**
- **Unit:** Retry logic with mock HTTP responses
- **Unit:** Pending queue serialization/deserialization
- **Integration:** Network interruption recovery (if feasible in CI)

### Phase 4 exit criteria

- Search works (server-side + local FTS merge)
- Users can create/rename/delete mailboxes
- Multiple sender identities supported
- Engine recovers gracefully from transient errors

---

## Config Cleanup (Cross-Cutting)

After Phase 2 ships, remove dead IMAP/SMTP config artifacts:

- Delete `SmtpOverrides`, `SmtpConfig`, `FileConfig` (legacy) from `config.rs`
- Simplify `AccountConfig`: remove `imap_server`/`imap_port`/`use_starttls`/`smtp`/`smtp_overrides` — replace with `jmap_url`, `auth_token`
- Simplify `FileAccountConfig` similarly
- Update `setup.rs` for JMAP-only flow (URL + token, not server/port/starttls)
- Update keyring to store bearer tokens, not passwords

---

## Environment Variables for Integration Tests

```bash
# Required for any integration test
NEVERLIGHT_MAIL_JMAP_TOKEN=fmu1-...    # Fastmail API token
NEVERLIGHT_MAIL_USER=user@fastmail.com  # Account email

# Optional gates
NEVERLIGHT_MAIL_TEST_SEND=true          # Enable send tests (creates real emails)
```

Source from `.envrc` at repo root.

---

## Branch Strategy

All work happens on the `jmap` branch of `neverlight-mail-core`. Merge to `main` after Phase 1 proves the read path works end-to-end. The `main` branch's melib-based code is archived, not maintained.
