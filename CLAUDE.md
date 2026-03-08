# Claude Context: neverlight-mail-core

**Last Updated:** 2026-03-07

## What This Is

JMAP-native headless email engine. Zero GUI dependencies. Implements RFC 8620 (JMAP Core) and RFC 8621 (JMAP Mail) directly — no IMAP, no SMTP, no melib.

Licensed MIT/Apache-2.0. Designed so multiple frontends share the same engine: the COSMIC GUI (`neverlight-mail`) today, a ratatui TUI (`neverlight-mail-tui`) tomorrow.

Target provider: Fastmail.

## Read First

- `docs/code-conventions.md` — Code style, state modeling, error handling. **Follow this.**
- `docs/jmap.md` — Full architecture, implementation phases, design rationale for the JMAP pivot.

When in doubt about Rust idioms, the Rust Book is canon:
https://doc.rust-lang.org/book/

## Crate Structure

```
neverlight-mail-core/
├── Cargo.toml
├── CLAUDE.md               — This file
├── docs/
│   ├── code-conventions.md — Code style and patterns
│   └── jmap.md             — JMAP architecture and implementation plan
├── src/
│   ├── lib.rs              — pub mod declarations + type re-exports
│   ├── types.rs            — Owned types: EmailId, MailboxId, Flags, SyncEvent, etc.
│   ├── client.rs           — JmapClient: HTTP transport, request batching, blob ops
│   ├── session.rs          — Session discovery, capability negotiation
│   ├── email.rs            — Email/query, Email/get, Email/set, Email/changes
│   ├── mailbox.rs          — Mailbox/get, Mailbox/changes, Mailbox/set
│   ├── submit.rs           — EmailSubmission/set (replaces SMTP)
│   ├── sync.rs             — Delta sync loop: Email/changes + Mailbox/changes
│   ├── push.rs             — EventSource SSE (RFC 8620 §7.3)
│   ├── parse.rs            — RFC 5322 body extraction via mail-parser
│   ├── mime.rs             — render_body, render_body_markdown, open_link
│   ├── config.rs           — Config resolution (env → file+keyring → error enum)
│   ├── discovery.rs        — .well-known/jmap probe (RFC 8620 §2.2)
│   ├── keyring.rs          — OS keyring credential backend
│   ├── models.rs           — Folder, MessageSummary, AttachmentData
│   ├── setup.rs            — UI-agnostic setup state machine
│   └── store/
│       ├── mod.rs           — Re-exports (CacheHandle, flags_to_u8, DEFAULT_PAGE_SIZE)
│       ├── schema.rs        — DDL + migrations + FTS5
│       ├── flags.rs         — Flag encode/decode (compact 2-bit encoding)
│       ├── commands.rs      — CacheCmd enum
│       ├── queries.rs       — All do_* SQL functions
│       └── handle.rs        — CacheHandle async facade + background thread
└── tests/fixtures/          — Real email fixtures for MIME tests
```

## Key Design Decisions

### JMAP-only, no protocol abstraction

This engine speaks JMAP. There is no `MailBackend` trait, no protocol enum dispatch, no IMAP fallback. If you need IMAP, use the `main` branch.

### Owned types replace melib

`lib.rs` re-exports types from `types.rs`. Consumers import from `neverlight_mail_core`:
- `EmailId`, `MailboxId`, `ThreadId` — server-assigned string IDs
- `Flags`, `FlagOp` — JMAP keyword mapping
- `SyncEvent` — delta sync events (replaces melib's BackendEvent)
- `BlobId`, `IdentityId`, `State`, `MailboxRole`

### No COSMIC deps

This crate must never depend on `libcosmic`, `iced`, or any GUI framework.

### CacheHandle pattern

- `CacheHandle` is a `Clone + Send + Sync` async facade over a dedicated background thread
- All SQLite access happens on one thread via `mpsc::UnboundedSender<CacheCmd>`
- Each command carries a `oneshot::Sender` for the reply
- This avoids `rusqlite::Connection` Send/Sync issues entirely

### Config resolution order

`Config::resolve_all_accounts()`:
1. Environment variables (`NEVERLIGHT_MAIL_SERVER`, etc.) → single env account
2. Config file (`~/.config/neverlight-mail/config.json`) → multi-account with keyring
3. Returns `Err(ConfigNeedsInput)` if UI input is needed

## Dependencies

| Crate            | Purpose                                                    |
|------------------|------------------------------------------------------------|
| reqwest          | JMAP HTTP transport (API calls, SSE, blob upload/download) |
| mail-parser      | RFC 5322 parsing (body extraction, MIME)                   |
| rusqlite         | SQLite cache (bundled)                                     |
| html-safe-md     | Privacy-safe HTML → markdown/plaintext                     |
| keyring          | OS credential storage                                      |
| tokio            | Async runtime                                              |
| serde/serde_json | JMAP request/response serialization                        |
| thiserror        | Error type derivation                                      |
| dirs             | XDG directory resolution                                   |
| open             | Open URLs in system browser                                |
| uuid             | Account ID generation                                      |
| log              | Logging                                                    |

## Testing

Source the `.envrc` at the repo root. This defines:
```
NEVERLIGHT_MAIL_JMAP_TOKEN
NEVERLIGHT_MAIL_USER
```

```bash
cargo test -p neverlight-mail-core    # core tests only
cargo test --workspace                # everything
```

Tests include:
- Unit tests for types, client serialization, session parsing, body extraction
- Cache tests (schema, queries, FTS, multi-account isolation)
- MIME rendering tests with real-world email fixtures
- Integration tests against Fastmail (require env vars above)

## What to Avoid

- Adding any GUI dependency (cosmic, iced, winit, wgpu)
- Nested `if let` trees — see `docs/code-conventions.md`
- Boolean flags to represent states — use enums with context
- Making `CacheHandle` or store internals public beyond `mod.rs` re-exports
- Protocol abstraction layers — this is JMAP-only, keep it direct
