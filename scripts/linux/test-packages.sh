#!/bin/sh
# Install each Linux package (scripts/linux/packages.sh; in dist/, or $OUT)
# in a fresh container of a system it's for, with that system's package
# manager pulling in what it needs, and run the program's self-test there
# with a video:  scripts/linux/test-packages.sh <video.mkv>
# Prints each system's self-test; fails if any isn't "all ok".
set -eu
cd "$(dirname "$0")/../.."
dir=${OUT:-$PWD/dist}
video=$(realpath "$1")
logs=$(mktemp -d)
# Fedora's own libav plugin has no H.264 or HEVC: RPM Fusion's has.
fusion='dnf install -y -q https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm >/dev/null &&
    dnf swap -y -q ffmpeg-free ffmpeg --allowerasing >/dev/null &&'
set -- \
    "ubuntu:22.04|apt-get update -qq && apt-get install -y -qq /p/Ichioshi-linux-x86_64.deb" \
    "ubuntu:24.04|apt-get update -qq && apt-get install -y -qq /p/Ichioshi-linux-x86_64.deb" \
    "debian:12|apt-get update -qq && apt-get install -y -qq /p/Ichioshi-linux-x86_64.deb" \
    "fedora:43|$fusion dnf install -y -q /p/Ichioshi-linux-x86_64.rpm" \
    "archlinux:latest|pacman -Syu --noconfirm -q && pacman -U --noconfirm /p/Ichioshi-linux-x86_64.pkg.tar.zst"
for t in "$@"; do
    image=${t%%|*}
    install=${t#*|}
    log=$logs/$(echo "$image" | tr ':/' '__').log
    docker run --rm -e DEBIAN_FRONTEND=noninteractive -v "$dir:/p:ro" -v "$video:/v.mkv:ro" "$image" \
        sh -euc "($install) >/tmp/install.log 2>&1 || { tail -20 /tmp/install.log; exit 1; }
            cat /usr/share/ichioshi/package; ichioshi --version
            ichioshi --self-test /tmp/self-test.txt /v.mkv >/dev/null 2>&1 || true
            cat /tmp/self-test.txt" > "$log" 2>&1 &
done
wait
failed=0
for log in "$logs"/*.log; do
    echo "== $(basename "$log" .log)"
    grep -v ' ok$' "$log" || true
    tail -n1 "$log" | grep -qx 'all ok' || failed=1
done
exit $failed
