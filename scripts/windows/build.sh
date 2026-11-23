#!/bin/sh
# Build the Windows app and package it: dist/SeaDex-windows-x86_64.zip.
# Needs (once): rustup target add x86_64-pc-windows-gnu; cargo install
# cargo-zigbuild; Zig in ~/.local/share/seadex-windows; and GStreamer:
#   scripts/windows/fetch-gstreamer.py ~/.local/share/seadex-windows/msys2
set -eu
cd "$(dirname "$0")/../.."
. scripts/windows/env.sh
cargo zigbuild --release --target x86_64-pc-windows-gnu
python3 scripts/windows/package.py target/x86_64-pc-windows-gnu/release/seadex-downloader.exe \
    "$TOOLS/msys2/ucrt64" dist
