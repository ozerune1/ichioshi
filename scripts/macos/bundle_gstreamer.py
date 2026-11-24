#!/usr/bin/env python3
"""Put the parts of GStreamer the app plays with in its macOS bundle, at
Contents/Resources/GStreamer, laid out as GStreamer.framework's Versions/1.0
(so the libraries' @rpath/lib/... names resolve as they do there): the
plugins, the plugin scanner, GLib's TLS module (HTTPS), libass and libsoup
(loaded by name at run time), and every library they load. The program is
pointed at them, nothing is left pointing at /Library/Frameworks, and every
piece of code is signed ad hoc (an Apple Silicon Mac runs nothing unsigned).
(Resources, not Frameworks: codesign takes Frameworks to hold only
frameworks and libraries, and GStreamer's plugin folders and scanner aren't
laid out that way. Notarizing would want a real GStreamer.framework.)

    scripts/macos/bundle_gstreamer.py <Ichioshi.app> <GStreamer.framework/Versions/1.0>"""

import os
import shutil
import subprocess
import sys

# As scripts/windows/package.py, with the Mac's own audio out and decoders
# (osxaudio, applemedia: VideoToolbox).
PLUGINS = [
    "coreelements", "playback", "typefindfunctions", "app", "videoconvertscale",
    "audioconvert", "audioresample", "volume", "autodetect", "matroska", "isomp4",
    "soup", "libav", "opus", "vorbis", "flac", "audioparsers", "videoparsersbad",
    "osxaudio", "applemedia", "subparse", "overlaycomposition", "rawparse",
    "id3demux", "ogg", "deinterlace", "videofilter", "audiomixer", "compositor",
    "dvdspu",
]
LOADED = ["lib/libass.9.dylib", "lib/libsoup-3.0.0.dylib"]
FRAMEWORK = "/Library/Frameworks/GStreamer.framework"
# The framework's libraries are named @rpath/libX.dylib, so @rpath is its
# lib folder.
RPATH = "@executable_path/../Resources/GStreamer/lib"


def run(*args):
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout


def is_macho(path):
    with open(path, "rb") as f:
        magic = f.read(4)
    return magic in (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca")


def own_names(path):
    """A library's own install name (otool -L lists it among what it loads)."""
    return {l.strip() for l in run("otool", "-D", path).splitlines()[1:] if l.strip() and not l.endswith(":")}


def deps(path):
    """The libraries a Mach-O file loads (each architecture's, once)."""
    own, seen = own_names(path), []
    for line in run("otool", "-L", path).splitlines():
        line = line.strip()
        if not line or line.endswith(":"):
            continue
        name = line.split(" (compatibility")[0].strip()
        if name not in seen and name not in own:
            seen.append(name)
    return seen


def rpaths(path):
    out, lines = [], run("otool", "-l", path).splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "cmd LC_RPATH":
            out.append(lines[i + 2].strip().split(" (offset")[0].removeprefix("path "))
    return sorted(set(out))


def in_framework(name, gst, loader_rel):
    """Where `name` (as a file loading it says it) is in the framework, as a
    path relative to Versions/1.0 (None: a system library)."""
    if name.startswith("/usr/lib/") or name.startswith("/System/"):
        return None
    if name.startswith("@rpath/"):
        rest = name[len("@rpath/"):]
        for rel in (rest, os.path.join("lib", rest)):
            if os.path.exists(os.path.join(gst, rel)):
                return rel
    elif name.startswith("@loader_path/"):
        rel = os.path.normpath(os.path.join(os.path.dirname(loader_rel), name[len("@loader_path/"):]))
        if os.path.exists(os.path.join(gst, rel)):
            return rel
    else:
        real_gst = os.path.realpath(gst)
        for root in (gst, real_gst, FRAMEWORK + "/Versions/1.0", FRAMEWORK + "/Versions/Current"):
            if name.startswith(root + "/"):
                return name[len(root) + 1:]
    sys.exit(f"{name} (needed by {loader_rel}) isn't in GStreamer's framework or the system")


def main():
    app, gst = sys.argv[1], sys.argv[2]
    out = os.path.join(app, "Contents", "Resources", "GStreamer")
    program = os.path.join(app, "Contents", "MacOS", "ichioshi")

    todo = [f"lib/gstreamer-1.0/libgst{p}.dylib" for p in PLUGINS]
    todo += ["libexec/gstreamer-1.0/gst-plugin-scanner"] + LOADED
    gio = os.path.join(gst, "lib", "gio", "modules")
    todo += [f"lib/gio/modules/{m}" for m in sorted(os.listdir(gio))
             if ("openssl" in m or "gnutls" in m) and (m.endswith(".so") or m.endswith(".dylib"))]
    missing = [t for t in todo if not os.path.exists(os.path.join(gst, t))]
    for t in missing:
        print(f"not in GStreamer's framework: {t}", file=sys.stderr)
    if any("libgstplayback" in m or "libass" in m for m in missing):
        sys.exit("GStreamer's framework lacks what playing needs")
    todo = [t for t in todo if t not in missing]
    # Every library those load, and those load, …
    copied = set()
    while todo:
        rel = todo.pop()
        if rel in copied:
            continue
        copied.add(rel)
        dest = os.path.join(out, rel)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        shutil.copyfile(os.path.join(gst, rel), dest)  # the file, not a link
        os.chmod(dest, 0o755)
        for name in deps(dest):
            found = in_framework(name, gst, rel)
            if found and found != rel:
                todo.append(found)
    # Fonts' settings, for libass's fontconfig.
    fonts = os.path.join(gst, "etc", "fonts")
    if os.path.isdir(fonts):
        shutil.copytree(fonts, os.path.join(out, "etc", "fonts"), symlinks=False, dirs_exist_ok=True)

    # The program (and anything else) loads GStreamer through the bundle.
    for path in [program] + [os.path.join(out, r) for r in sorted(copied)]:
        for name in deps(path):
            for root in (gst, os.path.realpath(gst), FRAMEWORK + "/Versions/1.0", FRAMEWORK + "/Versions/Current"):
                if name.startswith(root + "/lib/"):
                    run("install_name_tool", "-change", name, "@rpath/" + name[len(root) + 5:], path)
        for r in rpaths(path):
            if r.startswith(FRAMEWORK) or r.startswith(gst):
                run("install_name_tool", "-delete_rpath", r, path)
        if path == program and RPATH not in rpaths(path):
            run("install_name_tool", "-add_rpath", RPATH, path)
        # The scanner finds the libraries from libexec/gstreamer-1.0.
        if path.endswith("gst-plugin-scanner") and "@executable_path/../../lib" not in rpaths(path):
            run("install_name_tool", "-add_rpath", "@executable_path/../../lib", path)

    # Nothing may still need /Library/Frameworks (or Homebrew's /opt).
    bad = []
    for path in [program] + [os.path.join(out, r) for r in sorted(copied)]:
        for name in deps(path):
            if name.startswith("/") and not (name.startswith("/usr/lib/") or name.startswith("/System/")):
                bad.append(f"{path} loads {name}")
        for r in rpaths(path):
            if r.startswith("/"):
                bad.append(f"{path} looks in {r}")
    if bad:
        sys.exit("still outside the bundle:\n" + "\n".join(bad))

    # Signed ad hoc, inside out.
    code = [os.path.join(d, f) for d, _, fs in os.walk(out) for f in fs]
    code = [p for p in code if not os.path.islink(p) and is_macho(p)]
    for i in range(0, len(code), 50):
        run("codesign", "--force", "--sign", "-", *code[i:i + 50])
    size = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(out) for f in fs)
    print(f"GStreamer in the bundle: {len(copied)} files, {size / 1e6:.0f} MB")


if __name__ == "__main__":
    main()
