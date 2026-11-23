#!/bin/sh
# Build the Windows app and package it: dist/Ichioshi-windows-x86_64.zip.
# Needs (once): rustup target add x86_64-pc-windows-gnu; cargo install
# cargo-zigbuild; Zig in ~/.local/share/ichioshi-windows; and GStreamer:
#   scripts/windows/fetch-gstreamer.py ~/.local/share/ichioshi-windows/msys2
set -eu
cd "$(dirname "$0")/../.."
. scripts/release-keys.sh
. scripts/windows/env.sh
cargo zigbuild --release --target x86_64-pc-windows-gnu
python3 scripts/windows/package.py target/x86_64-pc-windows-gnu/release/ichioshi.exe \
    "$TOOLS/msys2/ucrt64" dist
