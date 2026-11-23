#!/usr/bin/env python3
"""Package the Windows build: SeaDex.exe with the GStreamer plugins
playing needs, libass, and every DLL they load (from the MSYS2 sysroot made
by fetch-gstreamer.py), zipped.

    scripts/windows/package.py <exe> <sysroot ucrt64 dir> <out dir>"""

import os
import shutil
import sys
import zipfile

sys.path.insert(0, os.path.dirname(__file__))
from peimports import imports  # noqa: E402

# GStreamer plugins: playbin and its parts, Matroska and MP4, HTTP (Jellyfin),
# decoders (libav, Opus, FLAC, Vorbis), parsers, audio out, Direct3D 11,
# bitmap subtitles.
PLUGINS = [
    "coreelements", "playback", "typefindfunctions", "app", "videoconvertscale",
    "audioconvert", "audioresample", "volume", "autodetect", "matroska", "isomp4",
    "soup", "libav", "opus", "vorbis", "flac", "audioparsers", "videoparsersbad",
    "wasapi", "wasapi2", "directsound", "d3d11", "subparse",
    "overlaycomposition", "rawparse", "id3demux", "ogg", "deinterlace",
    "videofilter", "audiomixer", "compositor",
]
# Loaded by name at run time, not imported: libass (by the app), libsoup
# (by the soup plugin).
LOADED = ["libass-9.dll", "libsoup-3.0-0.dll"]


def main():
    exe, root, out = sys.argv[1], sys.argv[2], sys.argv[3]
    bin_dir = os.path.join(root, "bin")
    dist = os.path.join(out, "SeaDex")
    if os.path.exists(dist):
        shutil.rmtree(dist)
    os.makedirs(dist)
    shutil.copy2(exe, os.path.join(dist, "SeaDex.exe"))

    plugins_out = os.path.join(dist, "lib", "gstreamer-1.0")
    os.makedirs(plugins_out)
    todo = [os.path.join(dist, "SeaDex.exe")]
    for p in PLUGINS:
        src = os.path.join(root, "lib", "gstreamer-1.0", f"libgst{p}.dll")
        if not os.path.exists(src):
            print(f"no plugin {p}", file=sys.stderr)
            continue
        shutil.copy2(src, plugins_out)
        todo.append(os.path.join(plugins_out, os.path.basename(src)))
    scanner_dir = os.path.join(dist, "libexec", "gstreamer-1.0")
    os.makedirs(scanner_dir)
    shutil.copy2(os.path.join(root, "libexec", "gstreamer-1.0", "gst-plugin-scanner.exe"), scanner_dir)
    todo.append(os.path.join(scanner_dir, "gst-plugin-scanner.exe"))
    # HTTPS for libsoup: GLib's TLS module (and its own DLLs).
    gio_out = os.path.join(dist, "lib", "gio", "modules")
    os.makedirs(gio_out)
    for m in os.listdir(os.path.join(root, "lib", "gio", "modules")):
        if m.endswith(".dll") and ("openssl" in m or "gnutls" in m):
            shutil.copy2(os.path.join(root, "lib", "gio", "modules", m), gio_out)
            todo.append(os.path.join(gio_out, m))
    for dll in LOADED:
        todo.append(os.path.join(bin_dir, dll))

    # Every DLL they need from the sysroot (the rest are Windows' own).
    have = {}
    while todo:
        f = todo.pop()
        for dll in imports(f):
            key = dll.lower()
            if key in have:
                continue
            src = os.path.join(bin_dir, dll)
            if not os.path.exists(src):
                have[key] = None  # Windows' own
                continue
            have[key] = src
            shutil.copy2(src, dist)
            todo.append(os.path.join(dist, dll))
        if f.startswith(bin_dir) and os.path.basename(f).lower() not in have:
            have[os.path.basename(f).lower()] = f
            shutil.copy2(f, dist)

    # fontconfig's settings (libass finds fonts with it).
    shutil.copytree(os.path.join(root, "etc", "fonts"), os.path.join(dist, "etc", "fonts"))

    zip_path = os.path.join(out, "SeaDex-windows-x86_64.zip")
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as z:
        for base, _, files in os.walk(dist):
            for name in files:
                full = os.path.join(base, name)
                z.write(full, os.path.relpath(full, out))
    size = sum(os.path.getsize(os.path.join(b, n)) for b, _, fs in os.walk(dist) for n in fs)
    dlls = sum(1 for v in have.values() if v)
    print(f"{dist}: {dlls} DLLs, {size // 1_000_000} MB; {zip_path}: "
          f"{os.path.getsize(zip_path) // 1_000_000} MB")


if __name__ == "__main__":
    main()
