# neverlight-mail-core

Headless email engine for [Neverlight Mail](https://github.com/jstelzer/neverlight-mail). IMAP, SMTP, MIME rendering, credential storage, and a SQLite cache — everything a mail client needs except the UI.

Zero GUI dependencies. Built on [melib](https://git.meli-email.org/meli/meli) (from the meli project) for IMAP and MIME, [lettre](https://crates.io/crates/lettre) for SMTP, and [rusqlite](https://crates.io/crates/rusqlite) for local caching.

## Usage

```toml
[dependencies]
neverlight-mail-core = "0.0.2"
```

## Modules

| Module    | Purpose                                                                             |
|-----------|-------------------------------------------------------------------------------------|
| `config`  | Multi-account config resolution (env vars, config file, keyring)                    |
| `imap`    | `ImapSession` — connect, fetch folders/messages/bodies, set flags, move, IDLE watch |
| `smtp`    | Send email via SMTP with attachments                                                |
| `mime`    | Render email bodies as plain text or markdown, open links                           |
| `keyring` | OS credential storage (get/set/delete passwords)                                    |
| `models`  | `Folder`, `MessageSummary`, `AttachmentData`                                        |
| `store`   | SQLite cache with async facade, FTS5 search, flag tracking                          |

## Re-exports

Key melib types are re-exported so consumers don't need a direct melib dependency:

```rust
use neverlight_mail_core::{EnvelopeHash, MailboxHash, FlagOp, Flag, BackendEvent, RefreshEventKind};
```

## Example

```rust
use neverlight_mail_core::config::Config;
use neverlight_mail_core::imap::ImapSession;
use neverlight_mail_core::store::CacheHandle;

// Resolve accounts from env vars or config file
let accounts = Config::resolve_all_accounts()?;
let config = accounts[0].to_imap_config();

// Connect and fetch
let session = ImapSession::connect(config).await?;
let folders = session.fetch_folders().await?;
```

## Consumers

- [neverlight-mail](https://github.com/jstelzer/neverlight-mail) — COSMIC desktop email client
- neverlight-mail-tui (planned) — ratatui terminal client

## License

GPL-3.0-or-later
