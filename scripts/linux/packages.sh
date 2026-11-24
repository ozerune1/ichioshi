#!/bin/sh
# Linux packages of the built program (scripts/linux/build.sh first):
#   dist/Ichioshi-linux-x86_64.deb          Debian 12, Ubuntu 22.04 and later
#   dist/Ichioshi-linux-x86_64.rpm          Fedora
#   dist/Ichioshi-linux-x86_64.pkg.tar.zst  Arch
# Each has the same program, its menu entry and icons, and
# /usr/share/ichioshi/package saying which kind of package it is: the
# in-app updater installs the next release's package of that kind. They're
# made in containers of their own systems (Docker), so this works on any
# Linux. VERSION=x.y.z labels them another version (to test updates).
set -eu
cd "$(dirname "$0")/../.."
version=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)}
bin=target/x86_64-unknown-linux-gnu/release/ichioshi
[ -x "$bin" ] || { echo "no $bin: run scripts/linux/build.sh first" >&2; exit 1; }
work=$PWD/target/package-linux/packages
out=${OUT:-$PWD/dist}
rm -rf "$work" && mkdir -p "$work" "$out"

summary="Download, organize and watch anime"
about="Ichioshi downloads anime in the best releases there are, keeps your
library in order, keeps AniList or MyAnimeList up to date as you watch, and
plays it, on your own or in a watch party."

# The files as installed.
root=$work/root
install -Dm755 "$bin" "$root/usr/bin/ichioshi"
install -Dm644 assets/linux/ichioshi.desktop "$root/usr/share/applications/ichioshi.desktop"
install -Dm644 assets/icon/ichioshi.svg "$root/usr/share/icons/hicolor/scalable/apps/ichioshi.svg"
for s in 16 24 32 48 64 128 256 512; do
    install -Dm644 "assets/icon/png/ichioshi-$s.png" \
        "$root/usr/share/icons/hicolor/${s}x$s/apps/ichioshi.png"
done
mkdir -p "$root/usr/share/ichioshi"
# A copy of the files, saying which kind of package it is.
tree() {
    cp -a "$root" "$work/$1"
    echo "$1" > "$work/$1/usr/share/ichioshi/package"
}
# Run in a container of the system ($1), files back as this user.
contain() {
    image=$1
    shift
    docker run --rm -v "$work:/w" -e "V=$version" -e "U=$(id -u):$(id -g)" "$image" sh -euc "$*"
}

# Debian and Ubuntu.
tree deb
mkdir -p "$work/deb/DEBIAN"
cat > "$work/deb/DEBIAN/control" <<EOF
Package: ichioshi
Version: $version
Architecture: amd64
Maintainer: Ichioshi <https://github.com/ozerune1/ichioshi/issues>
Installed-Size: $(du -sk "$work/deb/usr" | cut -f1)
Depends: libc6 (>= 2.35), gstreamer1.0-plugins-base (>= 1.20), gstreamer1.0-plugins-good, gstreamer1.0-libav, libass9, libsoup-3.0-0, glib-networking, libxkbcommon0
Recommends: gstreamer1.0-pulseaudio | gstreamer1.0-pipewire, libvulkan1, mesa-vulkan-drivers, libegl1
Section: video
Priority: optional
Homepage: https://github.com/ozerune1/ichioshi
Description: $summary
$(echo "$about" | sed 's/^/ /')
EOF
(cd "$work/deb" && find usr -type f -exec md5sum {} + > DEBIAN/md5sums)
contain debian:12 'dpkg-deb --root-owner-group -Zxz --build /w/deb /w/ichioshi.deb >/dev/null; chown "$U" /w/ichioshi.deb'
cp "$work/ichioshi.deb" "$out/Ichioshi-linux-x86_64.deb"

# Fedora. (H.264 and HEVC need RPM Fusion's gstreamer1-libav; Fedora's own
# plugin-libav leaves them out.)
tree rpm
cat > "$work/ichioshi.spec" <<EOF
%global debug_package %{nil}
%global __os_install_post %{nil}
Name: ichioshi
Version: %{v}
Release: 1
Summary: $summary
License: LicenseRef-Unspecified
URL: https://github.com/ozerune1/ichioshi
BuildArch: x86_64
Requires: gstreamer1-plugins-base >= 1.20, gstreamer1-plugins-good, (gstreamer1-libav or gstreamer1-plugin-libav), libass, libsoup3, glib-networking, libxkbcommon
Recommends: gstreamer1-plugin-openh264, vulkan-loader, mesa-vulkan-drivers

%description
$about

%install
cp -a /w/rpm/. %{buildroot}/

%files
/usr/bin/ichioshi
/usr/share/applications/ichioshi.desktop
/usr/share/icons/hicolor/*/apps/ichioshi.*
/usr/share/ichioshi
EOF
contain fedora:43 'dnf install -q -y rpm-build >/dev/null 2>&1;
    rpmbuild -bb --quiet --define "source_date_epoch_from_changelog 0" --define "_topdir /tmp/rpm" --define "v $V" /w/ichioshi.spec;
    cp /tmp/rpm/RPMS/x86_64/ichioshi-$V-1.x86_64.rpm /w/ichioshi.rpm; chown "$U" /w/ichioshi.rpm'
cp "$work/ichioshi.rpm" "$out/Ichioshi-linux-x86_64.rpm"

# Arch.
tree pacman
cat > "$work/PKGBUILD" <<EOF
pkgname=ichioshi
pkgver=$version
pkgrel=1
pkgdesc="$summary"
arch=(x86_64)
url="https://github.com/ozerune1/ichioshi"
license=(LicenseRef-Unspecified)
depends=(glibc gstreamer gst-plugins-base gst-plugins-good gst-libav libass libsoup3 glib-networking libxkbcommon)
optdepends=('vulkan-icd-loader: drawing with the GPU')
options=(!strip !debug)
package() {
    cp -dr --preserve=mode,timestamps /w/pacman/. "\$pkgdir/"
}
EOF
contain archlinux:latest 'pacman -Sy --noconfirm --needed fakeroot >/dev/null 2>&1;
    useradd -m b; mkdir /tmp/arch; cp /w/PKGBUILD /tmp/arch/; chown -R b /tmp/arch;
    su b -c "cd /tmp/arch && PACKAGER=\"Ichioshi <https://github.com/ozerune1/ichioshi>\" makepkg -d --noconfirm >/dev/null";
    cp /tmp/arch/ichioshi-$V-1-x86_64.pkg.tar.zst /w/ichioshi.pkg.tar.zst; chown "$U" /w/ichioshi.pkg.tar.zst'
cp "$work/ichioshi.pkg.tar.zst" "$out/Ichioshi-linux-x86_64.pkg.tar.zst"

ls -la "$out"/Ichioshi-linux-x86_64.deb "$out"/Ichioshi-linux-x86_64.rpm \
    "$out"/Ichioshi-linux-x86_64.pkg.tar.zst
