#!/bin/sh
# Builds the Linux and Windows executables to distribute, in dist/.
# Source paths are rewritten (home folder -> ~) so that the binaries do not carry the
# builder's user name. Needs the x86_64-pc-windows-gnu target and MinGW-w64.
set -eu
cd "$(dirname "$0")"
export RUSTFLAGS="--remap-path-prefix=$HOME=~ --remap-path-prefix=$PWD=."
export CFLAGS="-ffile-prefix-map=$HOME=~ -ffile-prefix-map=$PWD=."
export CARGO_TARGET_DIR=target/dist
# LAME is built by a configure script that does not watch CFLAGS: rebuild it.
cargo clean --release -p mp3lame-sys >/dev/null 2>&1 || true
cargo clean --release --target x86_64-pc-windows-gnu -p mp3lame-sys >/dev/null 2>&1 || true
cargo build --release
cargo build --release --target x86_64-pc-windows-gnu
mkdir -p dist
cp target/dist/release/bootleg-helper dist/bootleg-helper
cp target/dist/x86_64-pc-windows-gnu/release/bootleg-helper.exe dist/bootleg-helper.exe
for f in dist/bootleg-helper dist/bootleg-helper.exe; do
    if strings "$f" | grep -q "$HOME"; then
        echo "warning: $f still contains $HOME" >&2
        exit 1
    fi
done
ls -l dist
