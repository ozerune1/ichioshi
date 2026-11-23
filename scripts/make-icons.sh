#!/bin/sh
# Make the app's icons from assets/icon/*.svg (needs resvg, ImageMagick, and
# for the phone app's set, Node): PNGs, the Windows .ico, RustRover's
# project icon, the phone app's (Tauri's) icons, and Android's launcher
# icons (plain, round, and adaptive layers).
set -eu
cd "$(dirname "$0")/.."
icon=assets/icon
mkdir -p "$icon/png"
for s in 16 24 32 48 64 128 256 512 1024; do
    resvg -w "$s" -h "$s" "$icon/ichioshi.svg" "$icon/png/ichioshi-$s.png"
done
magick "$icon/png/ichioshi-16.png" "$icon/png/ichioshi-24.png" "$icon/png/ichioshi-32.png" \
    "$icon/png/ichioshi-48.png" "$icon/png/ichioshi-64.png" "$icon/png/ichioshi-128.png" \
    "$icon/png/ichioshi-256.png" "$icon/ichioshi.ico"

# RustRover shows .idea/icon.svg as the project's icon.
cp "$icon/ichioshi.svg" .idea/icon.svg

# The phone app's icon set (Tauri's; it writes plain launcher icons into
# its Android project too, which the adaptive ones below then replace).
(cd mobile && npx tauri icon "../$icon/png/ichioshi-1024.png" >/dev/null)
# (No iPhone app; and the adaptive icon below has a background of its own.)
rm -rf mobile/src-tauri/icons/ios mobile/src-tauri/gen/android/app/src/main/res/values/ic_launcher_background.xml

# Android: launcher icons by density (48dp), and adaptive layers (108dp),
# for the phone app (Tauri) and the earlier one (iced).
for res in mobile/src-tauri/gen/android/app/src/main/res android/app/src/main/res; do
for d in mdpi:1 hdpi:1.5 xhdpi:2 xxhdpi:3 xxxhdpi:4; do
    name=${d%%:*}
    scale=${d##*:}
    launcher=$(awk "BEGIN { print int(48 * $scale) }")
    layer=$(awk "BEGIN { print int(108 * $scale) }")
    dir="$res/mipmap-$name"
    mkdir -p "$dir"
    resvg -w "$launcher" -h "$launcher" "$icon/ichioshi.svg" "$dir/ic_launcher.png"
    resvg -w "$layer" -h "$layer" "$icon/ichioshi-android-background.svg" "$dir/ic_launcher_background.png"
    resvg -w "$layer" -h "$layer" "$icon/ichioshi-android-foreground.svg" "$dir/ic_launcher_foreground.png"
    resvg -w "$layer" -h "$layer" "$icon/ichioshi-android-monochrome.svg" "$dir/ic_launcher_monochrome.png"
    # Round (older launchers): the layers, as a launcher shows them, in a circle.
    magick "$dir/ic_launcher_background.png" "$dir/ic_launcher_foreground.png" -composite \
        -gravity center -crop 66.667%x66.667%+0+0 +repage -resize "${launcher}x${launcher}" \
        \( -size "${launcher}x${launcher}" xc:none -fill white \
        -draw "circle $((launcher / 2)),$((launcher / 2)) $((launcher / 2)),0" \) \
        -compose DstIn -composite "$dir/ic_launcher_round.png"
done
mkdir -p "$res/mipmap-anydpi-v26"
for name in ic_launcher ic_launcher_round; do
    cat > "$res/mipmap-anydpi-v26/$name.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@mipmap/ic_launcher_background" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
    <monochrome android:drawable="@mipmap/ic_launcher_monochrome" />
</adaptive-icon>
XML
done
done
# The Play Store's listing icon.
resvg -w 512 -h 512 "$icon/ichioshi.svg" android/app/src/main/ic_launcher-playstore.png
echo "icons made"
