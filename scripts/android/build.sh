#!/bin/sh
# Build the phone app (mobile/: the core with a web UI, through Tauri) for
# Android: dist/Ichioshi-android.apk, for ARM phones, signed with this
# computer's key (~/.local/share/ichioshi-android, made on the first run,
# with a password of its own in keystore.properties beside it), so each
# build installs over the last.
# TARGETS="aarch64 x86_64" also builds for emulators.
# Needs: the Android SDK in ~/.local/share/android-sdk, the NDK, a JDK 17+,
# Node and npm.
set -eu
cd "$(dirname "$0")/../.."
. scripts/release-keys.sh
mkdir -p dist
cd mobile
export ANDROID_HOME="${ANDROID_HOME:-$HOME/.local/share/android-sdk}"
export NDK_HOME="${NDK_HOME:-/opt/android-ndk}"
export JAVA_HOME="${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")}"
[ -d node_modules ] || npm install --no-audit --no-fund
KEY="$HOME/.local/share/ichioshi-android/ichioshi.keystore"
if [ ! -f "$KEY" ]; then
    mkdir -p "$(dirname "$KEY")"
    PASS=$(head -c 24 /dev/urandom | base64 | tr -d '/+=')
    (umask 077 && printf 'password=%s\n' "$PASS" > "$(dirname "$KEY")/keystore.properties")
    (umask 077 && "$JAVA_HOME/bin/keytool" -genkeypair -keystore "$KEY" -alias ichioshi \
        -keyalg RSA -keysize 2048 -validity 10000 -storepass "$PASS" -keypass "$PASS" \
        -dname "CN=Ichioshi" >/dev/null)
fi
# shellcheck disable=SC2086
npx tauri android build --apk --target ${TARGETS:-aarch64}
cp src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk \
    ../dist/Ichioshi-android.apk
ls -la ../dist/Ichioshi-android.apk
