release: build
    cargo build --release

build:
    cargo clippy --bin "nevermail" -p nevermail
    cargo build
    cargo test

