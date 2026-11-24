#!/bin/sh
# Build the Windows app and package it: dist/Ichioshi-windows-x86_64.zip
# (the app's folder, which the in-app updater uses) and the installer,
# dist/Ichioshi-windows-x86_64-setup.exe (installer/: the same folder,
# compressed, with an uninstaller).
# Needs (once): rustup target add x86_64-pc-windows-gnu; cargo install
# cargo-zigbuild; Zig in ~/.local/share/ichioshi-windows; zstd; and GStreamer:
#   scripts/windows/fetch-gstreamer.py ~/.local/share/ichioshi-windows/msys2
set -eu
cd "$(dirname "$0")/../.."
. scripts/release-keys.sh
. scripts/windows/env.sh
cargo zigbuild --release --target x86_64-pc-windows-gnu
python3 scripts/windows/package.py target/x86_64-pc-windows-gnu/release/ichioshi.exe \
    "$TOOLS/msys2/ucrt64" dist

# The installer: setup.exe carries the app's folder with the uninstaller
# put in it (a tar, compressed with zstd).
out=target/x86_64-pc-windows-gnu/release
cargo zigbuild --release --target x86_64-pc-windows-gnu -p ichioshi-setup --bin uninstall
tar --sort=name --owner=0 --group=0 --numeric-owner -cf - -C dist/Ichioshi . \
    -C "$PWD/$out" --transform 's|^uninstall.exe$|./Uninstall Ichioshi.exe|' uninstall.exe |
    zstd -19 --long=27 -T0 -q -f -o "$out/ichioshi-payload.tar.zst"
ICHIOSHI_SETUP_PAYLOAD="$PWD/$out/ichioshi-payload.tar.zst" \
    cargo zigbuild --release --target x86_64-pc-windows-gnu -p ichioshi-setup --bin setup
cp "$out/setup.exe" dist/Ichioshi-windows-x86_64-setup.exe
ls -la dist/Ichioshi-windows-x86_64.zip dist/Ichioshi-windows-x86_64-setup.exe
