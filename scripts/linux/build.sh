#!/bin/sh
# Build the Linux app and package it: dist/Ichioshi-linux-x86_64.tar.gz
# (the program, its menu entry and icon, and install.sh). It's built
# against glibc 2.35 (Ubuntu 22.04, Debian 12 and later) with
# cargo-zigbuild, and uses the system's GStreamer (1.20 or later) and
# libass.
# Needs: cargo-zigbuild, Zig (on PATH, or the one scripts/windows uses),
# and GStreamer's development files.
set -eu
cd "$(dirname "$0")/../.."
command -v zig >/dev/null ||
    export PATH="${ICHIOSHI_WINDOWS_TOOLS:-$HOME/.local/share/ichioshi-windows}/zig-x86_64-linux-0.15.2:$PATH"
# Zig doesn't look in the system's library folder by itself; GLib and
# GStreamer keep their own glibc references, resolved where the app runs.
libdir=$(pkg-config --variable=libdir gstreamer-1.0)
RUSTFLAGS="-L native=$libdir -C link-arg=-Wl,--allow-shlib-undefined" \
    cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.35
stage=target/package-linux/Ichioshi
rm -rf "$stage" && mkdir -p "$stage" dist
cp target/x86_64-unknown-linux-gnu/release/ichioshi assets/linux/ichioshi.desktop \
    assets/icon/ichioshi.svg scripts/linux/install.sh "$stage/"
cp assets/icon/png/ichioshi-256.png "$stage/ichioshi.png"
tar -C "$(dirname "$stage")" -czf dist/Ichioshi-linux-x86_64.tar.gz Ichioshi
ls -la dist/Ichioshi-linux-x86_64.tar.gz
