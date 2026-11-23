#!/usr/bin/env python3
"""Fetch GStreamer (and libass) for Windows from MSYS2's UCRT64 packages,
with everything they need, into a sysroot to cross-compile against and to
bundle with the app.

    scripts/windows/fetch-gstreamer.py <dir>

Makes <dir>/ucrt64 (bin, lib, include, lib/pkgconfig, lib/gstreamer-1.0).
Packages are checked against the repository's SHA-256 sums."""

import hashlib
import io
import os
import re
import subprocess
import sys
import tarfile
import urllib.request

MIRROR = "https://mirror.msys2.org/mingw/ucrt64"
PREFIX = "mingw-w64-ucrt-x86_64-"
ROOTS = [
    "gstreamer",
    "gst-plugins-base",
    "gst-plugins-good",
    "gst-plugins-bad",
    "gst-libav",
    "libass",
]
# Not needed to play video in the app (and large): the Python bindings,
# GTK and Qt, documentation tools.
SKIP = re.compile(r"^(python|gtk[34]|qt[56]|tk|tcl|gobject-introspection|gi-docgen|vala)")


def get(url, tries=5):
    """The URL's bytes; tried again, a little later each time, when the
    network drops."""
    import time

    for attempt in range(tries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "ichioshi-build"})
            with urllib.request.urlopen(req, timeout=600) as r:
                return r.read()
        except OSError as e:
            if attempt == tries - 1:
                raise
            print(f"{url}: {e}; trying again", file=sys.stderr)
            time.sleep(5 * (attempt + 1))


def index():
    """Every package in the repository: its file, sum, deps, provides."""
    raw = get(f"{MIRROR}/ucrt64.db")
    # The database is a (zstd- or gzip-compressed) tar of desc files.
    data = subprocess.run(["bsdtar", "-xOf", "-"], input=raw, capture_output=True, check=True).stdout
    pkgs, provides = {}, {}
    for block in data.decode("utf-8", "replace").split("%FILENAME%")[1:]:
        fields, key = {}, "FILENAME"
        lines = block.split("\n")
        fields["FILENAME"] = [lines[1]]
        for line in lines[2:]:
            if line.startswith("%") and line.endswith("%"):
                key = line.strip("%")
                fields[key] = []
            elif line:
                fields.setdefault(key, []).append(line)
        name = fields["NAME"][0]
        pkgs[name] = fields
        for p in fields.get("PROVIDES", []):
            provides.setdefault(re.split(r"[<>=]", p)[0], name)
    return pkgs, provides


def closure(pkgs, provides):
    todo = [PREFIX + r for r in ROOTS]
    seen = set()
    while todo:
        name = todo.pop()
        if name in seen:
            continue
        name = name if name in pkgs else provides.get(name, name)
        if name not in pkgs or name in seen:
            continue
        if SKIP.match(name[len(PREFIX):]):
            continue
        seen.add(name)
        for d in pkgs[name].get("DEPENDS", []):
            todo.append(re.split(r"[<>=]", d)[0])
    return sorted(seen)


def main():
    out = os.path.abspath(sys.argv[1])
    cache = os.path.join(out, "packages")
    os.makedirs(cache, exist_ok=True)
    pkgs, provides = index()
    wanted = closure(pkgs, provides)
    print(f"{len(wanted)} packages")
    for name in wanted:
        f = pkgs[name]
        file, want = f["FILENAME"][0], f["SHA256SUM"][0]
        path = os.path.join(cache, file)
        if not os.path.exists(path):
            blob = get(f"{MIRROR}/{file}")
            if hashlib.sha256(blob).hexdigest() != want:
                sys.exit(f"{file}: checksum mismatch")
            with open(path, "wb") as fh:
                fh.write(blob)
        # Everything but pacman's own files (some packages, like cc-libs,
        # are only dependencies and have none).
        subprocess.run(
            ["bsdtar", "-xf", path, "-C", out]
            + [f"--exclude={x}" for x in (".PKGINFO", ".BUILDINFO", ".MTREE", ".INSTALL")],
            check=True,
        )
    print(f"unpacked into {out}/ucrt64")


if __name__ == "__main__":
    main()
