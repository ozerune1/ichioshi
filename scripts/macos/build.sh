#!/bin/sh
# Build the macOS app, for Apple Silicon and Intel Macs in one, and package
# it:
#   dist/Ichioshi-macos-universal.dmg     the app, to drag to Applications
#   dist/Ichioshi-macos-universal.tar.gz  the same, for the in-app updater
# GStreamer goes inside the app (Contents/Resources/GStreamer: the parts of
# GStreamer.framework it plays with), from gstreamer.freedesktop.org's
# runtime and devel packages, installed in /Library/Frameworks. Runs on a
# Mac (.github/workflows/macos.yml builds it on GitHub's). It's signed ad
# hoc, not with an Apple Developer ID, so the first time it's opened macOS
# asks: System Settings › Privacy & Security › Open Anyway.
set -eu
cd "$(dirname "$0")/../.."
. scripts/release-keys.sh
GST=${GST:-/Library/Frameworks/GStreamer.framework/Versions/1.0}
export PKG_CONFIG_PATH="$GST/lib/pkgconfig" PKG_CONFIG_ALLOW_CROSS=1
export MACOSX_DEPLOYMENT_TARGET=11.0
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t" >/dev/null 2>&1 || true
    cargo build --release --target "$t"
done

app=target/macos/Ichioshi.app
rm -rf target/macos
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" dist
lipo -create -output "$app/Contents/MacOS/ichioshi" \
    target/aarch64-apple-darwin/release/ichioshi target/x86_64-apple-darwin/release/ichioshi
icons=target/macos/ichioshi.iconset
mkdir -p "$icons"
for s in 16 32 128 256 512; do
    cp "assets/icon/png/ichioshi-$s.png" "$icons/icon_${s}x$s.png"
    cp "assets/icon/png/ichioshi-$((s * 2)).png" "$icons/icon_${s}x$s@2x.png"
done
iconutil -c icns "$icons" -o "$app/Contents/Resources/ichioshi.icns"
sed "s/@VERSION@/$version/g" assets/macos/Info.plist > "$app/Contents/Info.plist"
python3 scripts/macos/bundle_gstreamer.py "$app" "$GST"
codesign --force --sign - "$app/Contents/MacOS/ichioshi"
codesign --force --sign - "$app"
codesign --verify --strict --verbose=1 "$app"

stage=target/macos/dmg
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -quiet -volname Ichioshi -srcfolder "$stage" -ov -format UDZO \
    dist/Ichioshi-macos-universal.dmg
COPYFILE_DISABLE=1 tar -C target/macos --no-mac-metadata -czf \
    dist/Ichioshi-macos-universal.tar.gz Ichioshi.app
ls -la dist/Ichioshi-macos-universal.dmg dist/Ichioshi-macos-universal.tar.gz
