set shell := ["bash", "-euo", "pipefail", "-c"]

build:
    cargo build --locked --all-features

test:
    cargo fmt --check
    cargo check --locked --no-default-features
    cargo clippy --locked --all-targets --all-features -- -D warnings
    cargo test --locked --all-features -- --nocapture

validate:
    python3 scripts/fetch-ebu.py
    TRUEPEAK_REQUIRE_ORACLES=1 just test

bench:
    cargo bench --locked --bench meter --bench limiter

wasm:
    RUSTC="$(rustup which --toolchain 1.96.0 rustc)" cargo check --locked --lib --all-features --target wasm32-unknown-unknown
