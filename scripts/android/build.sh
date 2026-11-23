#!/bin/sh
# Build the phone app (mobile/: the core with a web UI, through Tauri) for
# Android: dist/SeaDex-android.apk, for ARM phones, signed with this
# computer's key (~/.local/share/seadex-android, made by build-iced.sh's
# first run, or keytool), so it installs over earlier builds.
# TARGETS="aarch64 x86_64" also builds for emulators.
# Needs: the Android SDK in ~/.local/share/android-sdk, the NDK, a JDK 17+,
# Node and npm.
set -eu
cd "$(dirname "$0")/../.."
mkdir -p dist
cd mobile
export ANDROID_HOME="${ANDROID_HOME:-$HOME/.local/share/android-sdk}"
export NDK_HOME="${NDK_HOME:-/opt/android-ndk}"
export JAVA_HOME="${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")}"
[ -d node_modules ] || npm install --no-audit --no-fund
# shellcheck disable=SC2086
npx tauri android build --apk --target ${TARGETS:-aarch64}
cp src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk \
    ../dist/SeaDex-android.apk
ls -la ../dist/SeaDex-android.apk
