# Claude Context: neverlight-mail-core

**Last Updated:** 2026-02-26

## What This Is

Headless email engine library extracted from [Neverlight Mail](../). Zero GUI dependencies. Provides IMAP, SMTP, MIME rendering, credential storage, config resolution, and a SQLite cache — everything a mail client needs except the UI.

Designed so multiple frontends can share the same engine: the COSMIC GUI (`neverlight-mail`) today, a ratatui TUI (`neverlight-mail-tui`) tomorrow.

## Crate Structure

```
neverlight-mail-core/
├── Cargo.toml
├── src/
│   ├── lib.rs          — pub mod declarations + melib re-exports
│   ├── config.rs       — Config resolution (env → file+keyring → error enum)
│   ├── imap.rs         — ImapSession: connect, fetch, flags, move, watch
│   ├── smtp.rs         — SMTP send via lettre (OutgoingEmail struct)
│   ├── mime.rs         — render_body, render_body_markdown, open_link
│   ├── keyring.rs      — OS keyring credential backend (get/set/delete)
│   ├── models.rs       — Folder, MessageSummary, AttachmentData
│   └── store/
│       ├── mod.rs      — Re-exports (CacheHandle, flags_to_u8, DEFAULT_PAGE_SIZE)
│       ├── schema.rs   — DDL + forward-only migrations + FTS5 setup
│       ├── flags.rs    — Flag encode/decode (compact 2-bit encoding)
│       ├── commands.rs — CacheCmd enum (channel message types)
│       ├── queries.rs  — All do_* SQL functions + shared row_to_summary
│       └── handle.rs   — CacheHandle async facade + background thread run_loop
└── tests/fixtures/     — Real email fixtures for MIME tests
```

## Key Design Decisions

### No COSMIC deps
This crate must never depend on `libcosmic`, `iced`, or any GUI framework. DnD types (`DraggedFiles`, `DraggedMessage`) live in the GUI crate because they need `cosmic::iced::clipboard::mime` traits.

### melib re-exports
Consumers should not depend on melib directly. `lib.rs` re-exports the 6 types the GUI needs:
- `EnvelopeHash`, `MailboxHash` — hash newtypes
- `FlagOp`, `Flag` — flag manipulation
- `BackendEvent`, `RefreshEventKind` — IMAP watch events

If a consumer needs more melib types, add them to the re-export list rather than adding a direct melib dependency.

### ImapSession design
- Wraps `Arc<Mutex<Box<ImapType>>>` for interior mutability (`fetch()` requires `&mut self`)
- Lives behind `Arc<ImapSession>` so it can be cloned into async tasks
- melib's `ResultFuture<T>` is `Result<BoxFuture<'static, Result<T>>>` — double-unwrap pattern
- Streams from `fetch()` are `'static` — safe to drop the lock before consuming

### CacheHandle pattern
- `CacheHandle` is a `Clone + Send + Sync` async facade over a dedicated background thread
- All SQLite access happens on one thread via `mpsc::UnboundedSender<CacheCmd>`
- Each command carries a `oneshot::Sender` for the reply
- This avoids `rusqlite::Connection` Send/Sync issues entirely

### Config resolution order
`Config::resolve_all_accounts()`:
1. Environment variables (`NEVERLIGHT_MAIL_SERVER`, etc.) → single env account
2. Config file (`~/.config/neverlight-mail/config.json`) → multi-account with keyring
3. Returns `Err(ConfigNeedsInput)` if UI input is needed (full setup or password only)

## Critical: Version Pinning

melib 0.8.13's `imap` feature depends on `imap-codec` and `imap-types`. Newer alpha versions introduced a breaking change (missing `modifiers` field) that prevents compilation.

**The lockfile pins these to working versions:**
- `imap-codec = 2.0.0-alpha.4`
- `imap-types = 2.0.0-alpha.4`

**DO NOT run `cargo update` without verifying these pins are preserved.** If they drift, re-pin with:
```bash
cargo update -p imap-codec --precise 2.0.0-alpha.4
cargo update -p imap-types --precise 2.0.0-alpha.4
```

This is an upstream melib bug. Monitor melib releases for a fix. Any consumer with its own lockfile (neverlight-mail, neverlight-mail-tui) must also maintain these pins.

## Dependencies

| Crate | Purpose |
|-------|---------|
| melib | IMAP backend, envelope/mailbox types, MIME parsing |
| lettre | SMTP sending |
| rusqlite | SQLite cache (bundled) |
| html-safe-md | Privacy-safe HTML → markdown/plaintext |
| keyring | OS credential storage |
| tokio | Async runtime |
| futures | Stream combinators |
| serde/serde_json | Config serialization |
| dirs | XDG directory resolution |
| open | Open URLs in system browser |
| uuid | Account ID generation |
| indexmap | Ordered maps for melib AccountSettings |

## Testing

```bash
cargo test -p neverlight-mail-core          # core tests only
cargo test --workspace                # everything
```

Tests include real-world email fixtures (1Password invoice HTML) to verify MIME rendering doesn't produce markdown soup.

## What to Avoid

- Adding any GUI dependency (cosmic, iced, winit, wgpu)
- Running `cargo update` without verifying imap-codec/imap-types pins
- Making `CacheHandle` or store internals public beyond what `mod.rs` re-exports
- Adding melib types to the public API without going through `lib.rs` re-exports
