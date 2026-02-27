release: build
    cargo build --release

build:
    cargo clippy 
    cargo build
    cargo test

