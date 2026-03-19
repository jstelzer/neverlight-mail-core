# neverlight-mail-core

JMAP-native headless email engine for [Neverlight Mail](https://github.com/jstelzer/neverlight-mail). Implements RFC 8620 (JMAP Core) and RFC 8621 (JMAP Mail) directly — no IMAP, no SMTP, no melib.

Zero GUI dependencies. Built on [reqwest](https://crates.io/crates/reqwest) for HTTP transport, [mail-parser](https://crates.io/crates/mail-parser) for RFC 5322 parsing, [rusqlite](https://crates.io/crates/rusqlite) for local caching, and [neverlight-mail-html-safe-md](../neverlight-mail-html-safe-md) for privacy-safe HTML rendering. OAuth 2.0 is provided by [neverlight-mail-oauth](https://github.com/jstelzer/neverlight-mail-oauth).

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
| `email`     | Email/query, Email/get, Email/set, body fetch, flag ops         |
| `mailbox`   | Mailbox/get, Mailbox/changes, Mailbox/set                       |
| `submit`    | EmailSubmission/set (sending via JMAP, replaces SMTP)            |
| `sync`      | Delta sync loop via Email/changes + Mailbox/changes              |
| `backfill`  | Background backfill of older messages into cache                 |
| `push`      | EventSource SSE notifications (RFC 8620 §7.3)                   |
| `parse`     | RFC 5322 body extraction via mail-parser                         |
| `mime`      | Body rendering (plaintext, markdown), link opening               |
| `config`    | Multi-account config resolution (env vars, config file, keyring) |
| `discovery` | `.well-known/jmap` probe (RFC 8620 §2.2)                        |
| `keyring`   | OS credential storage (app passwords + OAuth refresh tokens)     |
| `models`    | `Folder`, `MessageSummary`, `AttachmentData`                     |
| `types`     | `EmailId`, `MailboxId`, `Flags`, `FlagOp`, `State`, `SyncEvent` |
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

## On AI-Assisted Development

This library was built by a human and a rotating cast of LLMs — primarily
Claude (Anthropic), affectionately referred to as the Chaos Goblins.

Here's what that actually means in practice:

**The human** ([@jstelzer](https://github.com/jstelzer)) drives architecture,
reads the RFCs, makes design calls, and owns every line that ships. He decided
this crate should exist, what it should and shouldn't do, and how the layers
fit together across four repositories and three platforms. He cold-emailed the
spec's co-author to make sure he was reading it right. None of that is
automatable.

**The goblins** accelerate. We draft implementations from spec descriptions,
catch type mismatches across crate boundaries, propagate breaking changes
through consumer code, and occasionally get told "this isn't rocket surgery"
when we overcomplicate things. Fair.

What we *don't* do: make design decisions, choose dependencies, decide what
gets published, or write code the human hasn't reviewed and understood. Every
commit is his. We're the pair programmer that doesn't need coffee but also
doesn't remember yesterday's session.

**Why say this out loud?**

Because "AI-generated code" has become a scare phrase, and "I used AI" has
become a boast, and neither is honest about what the work actually looks like.
The work looks like this: a person who knows what they're building, working
with a tool that's fast at the mechanical parts. The architecture is human. The
velocity is collaborative. The license is open so you can judge the output on
its own merits.

If you're evaluating this code: read it. It either implements the spec
correctly or it doesn't. How it got typed is the least interesting question
you could ask.


## License

MIT OR Apache-2.0
