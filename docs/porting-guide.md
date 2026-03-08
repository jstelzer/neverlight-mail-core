# Porting Guide: IMAP/SMTP → JMAP

This guide covers migrating frontends (COSMIC, TUI, macOS) from the old
melib-based IMAP/SMTP engine to the new JMAP-native `neverlight-mail-core`.

---

## TL;DR — What Changed

| Before (melib/IMAP)              | After (JMAP)                        |
|----------------------------------|-------------------------------------|
| `melib::backends::MailBackend`   | Direct module functions             |
| `EnvelopeHash` (u64 hash)       | `EmailId(String)` — server-assigned |
| `MailboxHash` (u64 hash)        | `MailboxId(String)`                 |
| `Flag` bitfield                 | `Flags { seen, flagged, draft, answered }` |
| `FlagOp` (melib)                | `FlagOp::SetSeen(bool) \| SetFlagged(bool)` |
| `BackendEvent` / `RefreshEvent` | `SyncEvent` enum                    |
| IMAP session + SMTP connection  | `JmapSession::connect()` → `JmapClient` |
| IMAP FETCH                      | `email::query_and_get()`            |
| IMAP STORE                      | `email::set_flag()`                 |
| IMAP COPY/MOVE                  | `email::move_to()`                  |
| IMAP IDLE                       | `push::listen()` (EventSource SSE) |
| SMTP send                       | `submit::send()` (JSON POST)       |
| 12 setup fields                 | 5 setup fields                      |
| `Config` struct                 | `resolve_all_accounts()` free fn    |

---

## 1. Dependencies

### Cargo.toml

Remove:
```toml
melib = "0.8"
```

The engine has no melib dependency. All types are owned.

### Imports

Old:
```rust
use melib::{
    backends::{MailBackend, BackendEvent, RefreshEventKind},
    Envelope, EnvelopeHash, MailboxHash, Flag, FlagOp,
};
```

New:
```rust
use neverlight_mail_core::{
    EmailId, MailboxId, Flags, FlagOp, SyncEvent,
    State, BlobId, ThreadId, MailboxRole, IdentityId,
};
use neverlight_mail_core::config::{AccountConfig, resolve_all_accounts, ConfigNeedsInput};
use neverlight_mail_core::session::JmapSession;
use neverlight_mail_core::client::JmapClient;
use neverlight_mail_core::models::{Folder, MessageSummary, AttachmentData};
```

---

## 2. Configuration

### Old: IMAP/SMTP fields (12+ fields)

```
server, port, starttls, username, password,
smtp_server, smtp_port, smtp_starttls, smtp_username, smtp_password,
protocol (IMAP/JMAP), label
```

### New: JMAP fields (5 fields)

```
label, jmap_url, username, token, email_addresses
```

### Config resolution

Old:
```rust
let config = Config::load()?;
```

New:
```rust
match config::resolve_all_accounts() {
    Ok(accounts) => {
        // Vec<AccountConfig> — ready to use
        for account in &accounts {
            let (session, client) = JmapSession::connect(account).await?;
            // ...
        }
    }
    Err(ConfigNeedsInput::FullSetup) => {
        // Show full setup form (5 fields)
    }
    Err(ConfigNeedsInput::TokenOnly { account_id, jmap_url, username, error }) => {
        // Config exists but keyring token missing — show token prompt
    }
}
```

Resolution order: env vars → config file + keyring → `Err(ConfigNeedsInput)`.

### Env vars

Old: `NEVERLIGHT_MAIL_SERVER`, `NEVERLIGHT_MAIL_USER`, `NEVERLIGHT_MAIL_PASSWORD`

New: `NEVERLIGHT_MAIL_JMAP_TOKEN`, `NEVERLIGHT_MAIL_USER`, `NEVERLIGHT_MAIL_JMAP_URL` (optional, defaults to Fastmail)

### Auth detection

Token prefix determines auth method (handled automatically by `JmapSession::connect`):
- `fmu1-` → Bearer token (Fastmail API token)
- anything else → Basic auth (username:token as app password)

---

## 3. Connection Lifecycle

### Old: IMAP session + SMTP connection

```rust
// Two separate connections, both TCP
let imap = ImapSession::connect(&config)?;
let smtp = SmtpTransport::new(&smtp_config)?;
```

### New: Single JMAP client

```rust
let (session, client) = JmapSession::connect(&account_config).await?;
// `client` handles everything: read, write, send, push
// `session` has metadata: account_id, limits, URLs
```

Key differences:
- **One connection** — `JmapClient` does reads, writes, and sending
- **HTTP-based** — no persistent TCP connection to manage
- **Stateless** — each request is independent, no sequence numbers
- **Auto-retry** — client retries on HTTP 429/503 with exponential backoff

### JmapSession fields

```rust
session.account_id          // Server-assigned account ID (used in most calls)
session.api_url             // Where to POST JMAP requests
session.upload_url          // Blob upload endpoint
session.download_url        // Blob download template
session.event_source_url    // SSE push endpoint (Option)
session.max_objects_in_get  // Server limit for Email/get batch size
session.max_calls_in_request // Server limit for batched method calls
```

---

## 4. Operation Mapping

### Listing mailboxes

Old: backend-specific mailbox list, `MailboxHash` identifiers

New:
```rust
let folders: Vec<Folder> = mailbox::fetch_all(&client).await?;

// Find specific role
let inbox_id = mailbox::find_by_role(&folders, "inbox");
let trash_id = mailbox::find_by_role(&folders, "trash");
```

`Folder` struct:
```rust
pub struct Folder {
    pub name: String,
    pub path: String,
    pub unread_count: u32,
    pub total_count: u32,
    pub mailbox_id: String,     // Stable server ID
    pub role: Option<String>,   // "inbox", "sent", "trash", etc.
    pub sort_order: u32,
}
```

### Fetching messages (list view)

Old: IMAP FETCH with envelope data

New:
```rust
// Fetch page of messages for a mailbox
let (summaries, query_result) = email::query_and_get(
    &client,
    &mailbox_id,
    50,           // page size
    0,            // position (offset)
).await?;

// summaries: Vec<MessageSummary>
// query_result.state: queryState (for queryChanges)
// query_result.get_state: Email/get state (for Email/changes delta sync)
// query_result.total: total count
```

### Fetching message body

Old: IMAP FETCH BODY[]

New:
```rust
let (html, text, attachments) = email::get_body(&client, &email_id).await?;
// html: Option<String>
// text: Option<String>
// attachments: Vec<AttachmentData>
```

### Flags

Old:
```rust
// melib Flag bitfield
if envelope.flags().contains(Flag::SEEN) { ... }
backend.set_flags(hash, smallvec![FlagOp::Set(Flag::SEEN)])?;
```

New:
```rust
// Struct with named booleans
if summary.is_read { ... }  // MessageSummary has pre-extracted flags

// From raw JMAP keywords
let flags = Flags::from_keywords(&keywords_json);
if flags.seen { ... }

// Set a flag
email::set_flag(&client, &email_id, FlagOp::SetSeen(true)).await?;
email::set_flag(&client, &email_id, FlagOp::SetFlagged(false)).await?;

// Batch flag updates
email::set_flags_batch(&client, &email_ids, FlagOp::SetSeen(true)).await?;
```

### Moving messages

Old: IMAP COPY + delete from source

New:
```rust
email::move_to(&client, &email_id, &target_mailbox_id).await?;
```

### Trashing messages

Old: Move to trash folder (IMAP COPY + STORE \Deleted)

New:
```rust
email::trash(&client, &email_id, &trash_mailbox_id).await?;
```

### Permanently deleting

Old: IMAP STORE \Deleted + EXPUNGE

New:
```rust
email::destroy(&client, &email_id).await?;
```

### Sending email

Old: SMTP connection + envelope + raw RFC 5322

New:
```rust
// 1. Get sender identities
let identities = submit::get_identities(&client).await?;

// 2. Find mailbox IDs for drafts and sent
let drafts_id = mailbox::find_by_role(&folders, "drafts").unwrap();
let sent_id = mailbox::find_by_role(&folders, "sent").unwrap();

// 3. Send (single batched HTTP request: create draft + submit + move to sent)
let email_id = submit::send(&client, &submit::SendRequest {
    identity_id: &identities[0].id,
    from: "you@example.com",
    to: &["recipient@example.com".into()],
    cc: &[],
    subject: "Hello",
    text_body: "Plain text body",
    html_body: Some("<p>HTML body</p>"),  // None for plain-text only
    drafts_mailbox_id: &drafts_id,
    sent_mailbox_id: &sent_id,
}).await?;
```

No SMTP connection. No raw RFC 5322. The server handles MIME structure.

### Searching

Old: Client-side search or IMAP SEARCH

New:
```rust
use neverlight_mail_core::email::SearchFilter;

let (results, query) = email::search(&client, &SearchFilter {
    text: Some("quarterly report".into()),
    from: Some("boss@example.com".into()),
    in_mailbox: Some(inbox_id.clone()),
    has_attachment: Some(true),
    after: Some("2025-01-01T00:00:00Z".into()),
    ..Default::default()
}, 50).await?;
```

All search is server-side. The filter maps to RFC 8621 §4.4.1 `FilterCondition`.

---

## 5. Sync and Push

### Old: IMAP IDLE + periodic NOOP

```rust
// Blocking wait for changes
backend.watch()?;  // blocks until IDLE notification
// Then re-fetch everything
```

### New: Delta sync + EventSource push

**Delta sync** — only fetch what changed:
```rust
// Sync mailboxes (auto-detects full vs delta)
let folders = sync::sync_mailboxes(&client, &cache, &session.account_id).await?;

// Sync emails for a mailbox (auto-detects full vs delta)
let summaries = sync::sync_emails(
    &client, &cache, &session.account_id,
    &mailbox_id, "Email", 50,
).await?;
```

The sync module tracks state tokens in SQLite (`CacheHandle`). First call does
a full fetch; subsequent calls use `Email/changes` / `Mailbox/changes` to get
only the delta. If the server returns `cannotCalculateChanges`, it automatically
falls back to a full resync.

**EventSource push** — server pushes state changes:
```rust
use neverlight_mail_core::push;

// Build SSE URL from session
let url = push::build_event_source_url(
    session.event_source_url.as_deref().unwrap(),
    &push::EventSourceConfig::default(),
);

// Listen for changes (runs until connection drops)
push::listen(&client, &url, |state_change| {
    // state_change.changed: HashMap<String, String>
    // e.g. { "Email" => "s42", "Mailbox" => "s15" }
    // Trigger delta sync for changed types
}).await?;
```

### Recommended pattern

```
┌──────────────────────────────────────┐
│  EventSource SSE                     │
│  push::listen() → state change event │
│         │                            │
│         ▼                            │
│  sync::sync_mailboxes()             │
│  sync::sync_emails()                │
│  (delta: only changed items)         │
│         │                            │
│         ▼                            │
│  Update UI                           │
└──────────────────────────────────────┘
```

---

## 6. Caching (CacheHandle)

The engine includes a SQLite cache for offline state and delta sync tokens.

```rust
let cache = CacheHandle::open(&account_config.id).await?;

// The sync functions use the cache internally:
sync::sync_emails(&client, &cache, ...).await?;

// Direct cache queries (if needed):
let cached_summaries = cache.list_emails(account_id, mailbox_id, 50, 0).await?;
let state = cache.get_state(account_id, "Email".into()).await?;
```

`CacheHandle` is `Clone + Send + Sync` — safe to share across tasks.

---

## 7. Mailbox Management

New in Phase 4 — was not available in the IMAP engine:

```rust
// Create
let new_id = mailbox::create(&client, "Projects", None).await?;
let sub_id = mailbox::create(&client, "Active", Some(&new_id)).await?;

// Rename
mailbox::rename(&client, &new_id, "Work Projects").await?;

// Delete (with option to remove contained emails)
mailbox::destroy(&client, &sub_id, false).await?;  // keep emails
mailbox::destroy(&client, &new_id, true).await?;   // remove emails too
```

---

## 8. Setup Form

### Old: 12+ fields

```
Label, Protocol (IMAP/JMAP), Server, Port, STARTTLS,
Username, Password, Email,
SMTP Server, SMTP Port, SMTP STARTTLS, SMTP Password
```

### New: 5 fields

```
Label, JMAP URL, Username, Token, Email
```

The setup model (`setup::SetupModel`) handles:
- Three modes: `Full` (new account), `TokenOnly` (re-enter token), `Edit` (modify account)
- Field navigation (tab/shift-tab cycling)
- Validation
- Config persistence (keyring + config file)
- Token storage (keyring with plaintext fallback)

Frontend mapping:
```rust
use neverlight_mail_core::setup::*;

// Create from config error
let model = SetupModel::from_config_needs(&needs);

// Or for editing
let model = SetupModel::for_edit(account_id, SetupFields {
    label, jmap_url, username, email,
});

// Map UI events to SetupInput
let transition = model.update(SetupInput::SetField(FieldId::JmapUrl, value));
let transition = model.update(SetupInput::NextField);
let transition = model.update(SetupInput::Submit);

match transition {
    SetupTransition::Continue => { /* re-render form */ }
    SetupTransition::Finished(SetupOutcome::Configured) => { /* restart with new config */ }
    SetupTransition::Finished(SetupOutcome::Cancelled) => { /* exit */ }
}
```

---

## 9. Error Handling

Old: melib error types, string errors

New: `JmapError` enum

```rust
use neverlight_mail_core::client::JmapError;

match result {
    Err(JmapError::HttpError(e)) => { /* network/HTTP failure */ }
    Err(JmapError::RequestError(msg)) => { /* malformed request/response */ }
    Err(JmapError::MethodError { method, error_type, description }) => {
        /* JMAP method-level error (e.g. notFound, forbidden) */
    }
    Err(JmapError::CannotCalculateChanges) => {
        /* Server can't compute delta — need full resync */
        /* (sync module handles this automatically) */
    }
    _ => {}
}
```

---

## 10. Type Migration Cheatsheet

| melib type              | JMAP type                  | Notes                              |
|-------------------------|----------------------------|------------------------------------|
| `EnvelopeHash` (u64)   | `EmailId(String)`          | Server-assigned, stable            |
| `MailboxHash` (u64)    | `MailboxId(String)`        | Server-assigned, stable            |
| `Flag` (bitfield)      | `Flags` (struct of bools)  | `$seen`, `$flagged`, etc.          |
| `FlagOp`               | `FlagOp`                   | `SetSeen(bool)`, `SetFlagged(bool)` |
| `BackendEvent`         | `SyncEvent`                | `Created`, `Updated`, `Destroyed`  |
| `Envelope`             | `MessageSummary`           | Pre-extracted, no raw parsing      |
| `RefreshEventKind`     | `push::StateChange`        | Type → new state token map         |
| `ThreadHash`           | `ThreadId(String)`         | Server-provided thread grouping    |
| `AttachmentPart`       | `AttachmentData`           | `{ filename, mime_type, data }`    |
| `SmtpTransport`        | `submit::send()`           | No connection — one HTTP request   |
| N/A                    | `State(String)`            | Opaque sync token from server      |
| N/A                    | `BlobId(String)`           | For upload/download                |
| N/A                    | `IdentityId(String)`       | Sender identity for submission     |

---

## 11. What You Can Delete

When porting a frontend, remove:

- Any `melib` imports and types
- IMAP session management code
- SMTP connection code
- Protocol selection UI (IMAP vs JMAP toggle)
- SMTP configuration fields (6 fields)
- STARTTLS configuration
- Port configuration
- Raw RFC 5322 message construction for sending
- Client-side email search (it's server-side now)
- IDLE polling loops
- Envelope parsing from raw bytes (server returns structured data)

---

## 12. Minimal Working Example

```rust
use neverlight_mail_core::{
    config, email, mailbox, session, submit, sync, push,
    EmailId, Flags, FlagOp, SyncEvent,
};
use neverlight_mail_core::store::CacheHandle;

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Resolve config
    let accounts = config::resolve_all_accounts()
        .map_err(|_| "Setup needed")?;
    let account = &accounts[0];

    // 2. Connect
    let (session, client) = session::JmapSession::connect(account).await
        .map_err(|e| format!("Connect failed: {e}"))?;

    // 3. Open cache
    let cache = CacheHandle::open(&account.id).await?;

    // 4. Sync mailboxes
    let folders = sync::sync_mailboxes(&client, &cache, &session.account_id).await?;
    let inbox_id = mailbox::find_by_role(&folders, "inbox").expect("no inbox");

    // 5. Sync emails
    let emails = sync::sync_emails(
        &client, &cache, &session.account_id,
        &inbox_id, "Email", 50,
    ).await?;

    // 6. Read first email body
    if let Some(msg) = emails.first() {
        let (html, text, attachments) = email::get_body(&client, &msg.email_id).await?;
        println!("Subject: {}", msg.subject);
        println!("Body: {}", text.unwrap_or_default());
    }

    // 7. Listen for push notifications
    if let Some(es_url) = &session.event_source_url {
        let url = push::build_event_source_url(es_url, &push::EventSourceConfig::default());
        // In practice, run this in a background task
        // push::listen(&client, &url, |change| { ... }).await?;
    }

    Ok(())
}
```
