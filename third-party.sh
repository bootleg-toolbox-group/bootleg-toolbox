#!/bin/sh
# Regenerates THIRD-PARTY.md from about.hbs (hand-written part) and Cargo.lock (crates and
# licence texts). Needs cargo-about: cargo install --locked cargo-about --features cli
set -eu
cd "$(dirname "$0")"
cargo about generate --workspace --fail -o THIRD-PARTY.md about.hbs
