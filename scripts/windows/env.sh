# Source this to cross-compile for Windows: Zig as the C compiler and
# linker (cargo-zigbuild), GStreamer from MSYS2 (fetch-gstreamer.py) for
# pkg-config.
TOOLS=${SEADEX_WINDOWS_TOOLS:-$HOME/.local/share/seadex-windows}
export PATH="$TOOLS/zig-x86_64-linux-0.15.2:$HOME/.cargo/bin:$PATH"
export ZIG="$TOOLS/zig-x86_64-linux-0.15.2/zig"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_LIBDIR="$TOOLS/msys2/ucrt64/lib/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="$TOOLS/msys2"
unset PKG_CONFIG_PATH
# Zig's linker looks for X.lib or libX.a, not MinGW's import libraries
# (libX.dll.a): the same files under a name it looks for.
IMPLIBS="$TOOLS/implibs"
if [ ! -d "$IMPLIBS" ]; then
    mkdir -p "$IMPLIBS"
    for f in "$TOOLS"/msys2/ucrt64/lib/lib*.dll.a; do
        name=$(basename "$f" .dll.a)
        ln -sf "$f" "$IMPLIBS/${name#lib}.lib"
    done
fi
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L native=$IMPLIBS"
