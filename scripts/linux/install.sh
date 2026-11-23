#!/bin/sh
# Install Ichioshi for this user (no root needed): the program in
# ~/.local/bin, and Ichioshi in the app menu.
set -eu
cd "$(dirname "$0")"
bin=${XDG_BIN_HOME:-$HOME/.local/bin}
data=${XDG_DATA_HOME:-$HOME/.local/share}
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/256x256/apps" \
    "$data/icons/hicolor/scalable/apps"
cp ichioshi "$bin/ichioshi"
sed "s|^Exec=.*|Exec=$bin/ichioshi|" ichioshi.desktop > "$data/applications/ichioshi.desktop"
cp ichioshi.png "$data/icons/hicolor/256x256/apps/ichioshi.png"
cp ichioshi.svg "$data/icons/hicolor/scalable/apps/ichioshi.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
echo "Installed $bin/ichioshi; Ichioshi is in your app menu."
