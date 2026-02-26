# nevermail-core

Headless email engine for [Nevermail](https://github.com/neverlight/nevermail). IMAP, SMTP, MIME rendering, credential storage, and a SQLite cache — everything a mail client needs except the UI.

Zero GUI dependencies. Built on [melib](https://git.meli-email.org/meli/meli) (from the meli project) for IMAP and MIME, [lettre](https://crates.io/crates/lettre) for SMTP, and [rusqlite](https://crates.io/crates/rusqlite) for local caching.

## Usage

```toml
[dependencies]
nevermail-core = "0.0.2"
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
use nevermail_core::{EnvelopeHash, MailboxHash, FlagOp, Flag, BackendEvent, RefreshEventKind};
```

## Example

```rust
use nevermail_core::config::Config;
use nevermail_core::imap::ImapSession;
use nevermail_core::store::CacheHandle;

// Resolve accounts from env vars or config file
let accounts = Config::resolve_all_accounts()?;
let config = accounts[0].to_imap_config();

// Connect and fetch
let session = ImapSession::connect(config).await?;
let folders = session.fetch_folders().await?;
```

## Consumers

- [nevermail](https://github.com/neverlight/nevermail) — COSMIC desktop email client
- nevermail-tui (planned) — ratatui terminal client

## License

GPL-3.0-or-later
