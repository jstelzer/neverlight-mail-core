# Build Plan: JMAP-Native Engine

**Reference specs:**
- [RFC 8620 — JMAP Core](https://datatracker.ietf.org/doc/html/rfc8620)
- [RFC 8621 — JMAP Mail](https://datatracker.ietf.org/doc/html/rfc8621)
- [Fastmail Developer Docs](https://www.fastmail.com/dev/)

Each phase is independently testable. No phase depends on a GUI.

For cache layer design and invariants, see [cache.md](cache.md).

---

## Phase 0: Scaffold (Done)

What shipped:
- Owned types (`EmailId`, `MailboxId`, `State`, `Flags`, `SyncEvent`, etc.)
- `JmapClient` with batched request transport
- `JmapSession` with capability parsing
- `parse_body()` via mail-parser
- SQLite cache with string IDs throughout

---

## Phase 1: Read Path (Done)

**Goal:** Connect to Fastmail, list mailboxes, query emails, fetch bodies. Read-only mail viewer.

### 1a. Session + Mailbox Discovery

**Module:** `session.rs`, `mailbox.rs`

| RFC reference | Method                     |
|---------------|----------------------------|
| RFC 8620 §2   | Session resource discovery |
| RFC 8621 §2   | Mailbox/get                |

**Implementation:**
- `JmapSession::connect(url, auth)` — session discovery + capability negotiation
- `mailbox::fetch_all(client) -> Vec<Folder>` — `Mailbox/get` with role, sortOrder, counts
- Cache mailboxes via `CacheHandle::save_folders()`

### 1b. Email Query + Get

**Module:** `email.rs`

| RFC reference | Method      |
|---------------|-------------|
| RFC 8621 §4.4 | Email/query |
| RFC 8621 §4.1 | Email/get   |

**Implementation:**
- Batch query+get using result references (RFC 8620 §3.7)
- Map JMAP response → `Vec<MessageSummary>`
- Cache via `CacheHandle::save_messages()`

### 1c. Body Fetch

**Module:** `email.rs`, `parse.rs`, `mime.rs`

| RFC reference | Method                 |
|---------------|------------------------|
| RFC 8621 §4.2 | Email/get (bodyValues) |
| RFC 8620 §6.2 | Blob download          |

**Implementation:**
- Try bodyValues first, fall back to blob download + `parse_body()`
- HTML → markdown via `html-safe-md`
- Cache via `CacheHandle::save_body()`

---

## Phase 2: Write Path (Done)

**Goal:** Modify flags, move messages, delete, send email. Full read-write.

### 2a. Flag Operations

**Module:** `email.rs`

| RFC reference | Method                     |
|---------------|----------------------------|
| RFC 8621 §4.3 | Email/set (keywords patch) |

Uses optimistic local update via the dual-truth flag model (see [cache.md](cache.md#dual-truth-flag-model)).

### 2b. Move + Delete

**Module:** `email.rs`

| RFC reference | Method                       |
|---------------|------------------------------|
| RFC 8621 §4.3 | Email/set (mailboxIds patch) |

### 2c. Send Email

**Module:** `submit.rs`

| RFC reference | Method              |
|---------------|---------------------|
| RFC 8621 §7   | EmailSubmission/set |
| RFC 8621 §6   | Identity/get        |

Sending is pure JMAP — no SMTP. Draft creation + submission in a single batched request. Identity matching via `find_identity_for_address()` (exact → wildcard domain → fallback).

---

## Phase 3: Sync + Push (Done)

**Goal:** Delta sync for efficient updates, push notifications for real-time.

### 3a. Delta Sync

**Module:** `sync.rs`

| RFC reference | Method          |
|---------------|-----------------|
| RFC 8620 §5.2 | Foo/changes     |
| RFC 8621 §4.5 | Email/changes   |
| RFC 8621 §2.5 | Mailbox/changes |

For sync ↔ cache interaction details (full vs delta, prune, pending-op preservation), see [cache.md](cache.md#sync--cache-interaction).

### 3b. State Storage

**Module:** `store/`

State tokens stored in `sync_state` table, keyed by `(account_id, resource)`. See [cache.md](cache.md#tables).

### 3c. EventSource Push

**Module:** `push.rs`

| RFC reference | Method      |
|---------------|-------------|
| RFC 8620 §7.3 | EventSource |

SSE stream over HTTP. On state change → trigger delta sync. Reconnect on drop. Falls back to polling.

---

## Phase 4: Polish (In Progress)

**Goal:** Search, mailbox management, identity selection, error recovery.

### 4a. Server-Side Search (Done)

**Module:** `email.rs`

| RFC reference | Method               |
|---------------|----------------------|
| RFC 8621 §4.4 | Email/query (filter) |

Server-side + local FTS search. Local FTS uses prefix expansion for 3+ char tokens. See [cache.md](cache.md#message_fts).

### 4b. Mailbox Management (Done)

**Module:** `mailbox.rs`

| RFC reference | Method      |
|---------------|-------------|
| RFC 8621 §2.4 | Mailbox/set |

Create, rename, destroy mailboxes.

### 4c. Multi-Identity (Done)

**Module:** `submit.rs`

Identity selection via `find_identity_for_address()` with wildcard domain support.

### 4d. Error Recovery (Partial)

**Module:** `client.rs`, `sync.rs`

- `cannotCalculateChanges` → full resync (done)
- HTTP error propagation via `JmapError` (done)
- Retry with backoff, offline queue (not started)

### 4e. OAuth (Done — extracted)

OAuth implementation extracted to standalone crate `neverlight-mail-oauth`. See that crate's README for details. Mail-core calls `neverlight_mail_oauth::refresh_access_token()` from `session.rs` with scope `"urn:ietf:params:oauth:scope:mail"`.

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
