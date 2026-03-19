# neverlight-mail-core local dev automation

# Default: lint + test
default: check

# Quick check: fmt, clippy, tests
check: fmt clippy test

# Format check (no writes)
fmt:
    cargo fmt -- --check

# Clippy with warnings as errors
clippy:
    cargo clippy --all-targets -- -D warnings

# Unit + doc tests
test:
    cargo test --lib

# Integration tests (requires NEVERLIGHT_MAIL_JMAP_TOKEN + NEVERLIGHT_MAIL_USER)
test-integration:
    cargo test --test mailbox_query_body --test flags_move_delete --test sync_push_search --test send_identity

# All tests
test-all:
    cargo test

# Release build
release: check
    cargo build --release

# Format in-place (fix, not just check)
fmt-fix:
    cargo fmt
