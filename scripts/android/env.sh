# Source this to build for Android: the NDK's compilers (Android 11, API 30,
# and later) for ARM phones and x86_64 emulators, and the SDK.
NDK=${ANDROID_NDK_HOME:-/opt/android-ndk}
export ANDROID_HOME=${ANDROID_HOME:-$HOME/.local/share/android-sdk}
export ANDROID_NDK_HOME=$NDK
TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
for t in aarch64-linux-android x86_64-linux-android; do
    up=$(echo "$t" | tr 'a-z-' 'A-Z_')
    low=$(echo "$t" | tr '-' '_')
    export "CARGO_TARGET_${up}_LINKER=$TC/${t}30-clang"
    export "CC_${low}=$TC/${t}30-clang"
    export "CXX_${low}=$TC/${t}30-clang++"
    export "AR_${low}=$TC/llvm-ar"
done
export PATH="$TC:$PATH"
