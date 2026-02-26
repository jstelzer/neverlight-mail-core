# Claude Context: nevermail-core

**Last Updated:** 2026-02-26

## What This Is

Headless email engine library extracted from [Nevermail](../). Zero GUI dependencies. Provides IMAP, SMTP, MIME rendering, credential storage, config resolution, and a SQLite cache — everything a mail client needs except the UI.

Designed so multiple frontends can share the same engine: the COSMIC GUI (`nevermail`) today, a ratatui TUI (`nevermail-tui`) tomorrow.

## Crate Structure

```
nevermail-core/
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
1. Environment variables (`NEVERMAIL_SERVER`, etc.) → single env account
2. Config file (`~/.config/nevermail/config.json`) → multi-account with keyring
3. Returns `Err(ConfigNeedsInput)` if UI input is needed (full setup or password only)

## Critical: Version Pinning

melib 0.8.13 depends on `imap-codec` and `imap-types`. The workspace lockfile pins these to `2.0.0-alpha.4`. Newer alpha versions have breaking changes. See the root `CLAUDE.md` for details.

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
cargo test -p nevermail-core          # core tests only
cargo test --workspace                # everything
```

Tests include real-world email fixtures (1Password invoice HTML) to verify MIME rendering doesn't produce markdown soup.

## What to Avoid

- Adding any GUI dependency (cosmic, iced, winit, wgpu)
- Running `cargo update` without verifying imap-codec/imap-types pins
- Making `CacheHandle` or store internals public beyond what `mod.rs` re-exports
- Adding melib types to the public API without going through `lib.rs` re-exports
