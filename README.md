# neverlight-mail-core

JMAP-native headless email engine for [Neverlight Mail](https://github.com/jstelzer/neverlight-mail). Implements RFC 8620 (JMAP Core) and RFC 8621 (JMAP Mail) directly — no IMAP, no SMTP, no melib.

Zero GUI dependencies. Built on [reqwest](https://crates.io/crates/reqwest) for HTTP transport, [mail-parser](https://crates.io/crates/mail-parser) for RFC 5322 parsing, and [rusqlite](https://crates.io/crates/rusqlite) for local caching.

## Usage

```toml
[dependencies]
neverlight-mail-core = "0.1.0"
```

## Modules

| Module      | Purpose                                                          |
|-------------|------------------------------------------------------------------|
| `client`    | `JmapClient` — HTTP transport, request batching, blob ops       |
| `session`   | Session discovery, capability negotiation                        |
| `email`     | Email/query, Email/get, Email/set, Email/changes                |
| `mailbox`   | Mailbox/get, Mailbox/changes, Mailbox/set                       |
| `submit`    | EmailSubmission/set (sending via JMAP, replaces SMTP)            |
| `sync`      | Delta sync loop via Email/changes + Mailbox/changes              |
| `push`      | EventSource SSE notifications (RFC 8620 §7.3)                   |
| `parse`     | RFC 5322 body extraction via mail-parser                         |
| `mime`      | Body rendering (plaintext, markdown), link opening               |
| `config`    | Multi-account config resolution (env vars, config file, keyring) |
| `discovery` | `.well-known/jmap` probe (RFC 8620 §2.2)                        |
| `oauth`     | OAuth 2.0 with PKCE (RFC 9728 discovery, dynamic registration)  |
| `keyring`   | OS credential storage (app passwords + OAuth refresh tokens)     |
| `models`    | `Folder`, `MessageSummary`, `AttachmentData`                     |
| `setup`     | UI-agnostic account setup state machine                          |
| `store`     | SQLite cache with async facade, FTS5 search, flag tracking       |

## Re-exports

Core types are re-exported so consumers import from the crate root:

```rust
use neverlight_mail_core::{
    EmailId, MailboxId, ThreadId, BlobId, IdentityId,
    Flags, FlagOp, State, MailboxRole, SyncEvent,
};
```

## Example

```rust
use neverlight_mail_core::config;
use neverlight_mail_core::client::JmapClient;
use neverlight_mail_core::store::CacheHandle;

// Resolve accounts from env vars or config file
let accounts = config::resolve_all_accounts()?;
let account = &accounts[0];

// Connect via JMAP
let client = JmapClient::connect(&account.jmap_url, &account.auth).await?;
```

## Consumers

- [neverlight-mail](https://github.com/jstelzer/neverlight-mail) — COSMIC desktop email client
- [neverlight-mail-tui](https://github.com/jstelzer/neverlight-mail-tui) — ratatui terminal client

## License

MIT OR Apache-2.0
