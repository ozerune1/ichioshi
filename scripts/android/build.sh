#!/bin/sh
# Build the Android app: dist/SeaDex-android.apk, for ARM phones (arm64) and
# x86_64 emulators, signed with a key of this computer's own.
# Needs (once): the Android SDK (platforms;android-35, build-tools;35.0.0)
# in ~/.local/share/android-sdk, the NDK, and
#   rustup target add aarch64-linux-android x86_64-linux-android
set -eu
cd "$(dirname "$0")/../.."
. scripts/android/env.sh
PROFILE=${PROFILE:-release}
BT="$ANDROID_HOME/build-tools/35.0.0"
JAR="$ANDROID_HOME/platforms/android-35/android.jar"
OUT=target/android-apk
rm -rf "$OUT" && mkdir -p "$OUT/lib/arm64-v8a" "$OUT/lib/x86_64" dist

# TARGETS="x86_64-linux-android:x86_64" builds for emulators only (quicker).
for t in ${TARGETS:-aarch64-linux-android:arm64-v8a x86_64-linux-android:x86_64}; do
    triple=${t%%:*}
    abi=${t##*:}
    if [ "$PROFILE" = release ]; then flag=--release; else flag=; fi
    cargo rustc --lib $flag --target "$triple" --crate-type cdylib
    llvm-strip -o "$OUT/lib/$abi/libseadex_downloader.so" \
        "target/$triple/$PROFILE/libseadex_downloader.so"
done

# Resources (the icons) and the manifest, then the native code.
"$BT/aapt2" compile --dir android/app/src/main/res -o "$OUT/res.zip"
"$BT/aapt2" link -o "$OUT/unsigned.apk" -I "$JAR" \
    --manifest android/app/src/main/AndroidManifest.xml \
    --min-sdk-version 30 --target-sdk-version 35 "$OUT/res.zip"
(cd "$OUT" && zip -qr unsigned.apk lib)
"$BT/zipalign" -f -P 16 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"

# A signing key of this computer's own (made once).
KEY="$HOME/.local/share/seadex-android/seadex.keystore"
if [ ! -f "$KEY" ]; then
    mkdir -p "$(dirname "$KEY")"
    keytool -genkeypair -keystore "$KEY" -alias seadex -keyalg RSA -keysize 2048 \
        -validity 10000 -storepass seadex-android -keypass seadex-android \
        -dname "CN=SeaDex" >/dev/null
fi
"$BT/apksigner" sign --ks "$KEY" --ks-pass pass:seadex-android --v4-signing-enabled false \
    --out dist/SeaDex-android.apk "$OUT/aligned.apk"
ls -la dist/SeaDex-android.apk
