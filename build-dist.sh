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
# cargo-auditable (cargo install cargo-auditable) embeds the list of dependencies and their
# versions, so that the executables can be checked later against new advisories
# (cargo audit bin <file>). Names and versions only: no path.
cargo auditable build --release
cargo auditable build --release --target x86_64-pc-windows-gnu
mkdir -p dist
cp target/dist/release/bootleg-toolbox dist/bootleg-toolbox
cp target/dist/x86_64-pc-windows-gnu/release/bootleg-toolbox.exe dist/bootleg-toolbox.exe
for f in dist/bootleg-toolbox dist/bootleg-toolbox.exe; do
    if strings "$f" | grep -q "$HOME"; then
        echo "warning: $f still contains $HOME" >&2
        exit 1
    fi
done
ls -l dist
