<p align="center"><img src="assets/icon/png/ichioshi-128.png" width="96" alt=""></p>

<h1 align="center">Ichioshi</h1>

<p align="center"><i>イチオシ — "top pick"</i><br>
An anime library that gets you the best release of every show, downloads only the episodes you want, and plays them.<br>
For Linux, Windows and Android.</p>

<p align="center"><a href="../../releases/latest"><b>Download</b></a></p>

## What it does

- **Finds the best release.** For each anime, Ichioshi uses what
  [SeaDex](https://releases.moe) rates best. For shows SeaDex doesn't
  cover (most of the current season), it ranks Nyaa's releases by the
  [TRaSH Guides](https://trash-guides.info) release-group tiers.
- **Downloads only what you pick.** Choose single episodes or a whole series.
  - **BitTorrent** is built in. It **never seeds**: uploading is switched
    off entirely. (The one exception is a watch party, below.) On Linux it
    can be tied to your VPN's connection, with port forwarding for PIA and
    ProtonVPN.
  - **Debrid services:** Real-Debrid, AllDebrid, Premiumize, TorBox and
    Debrid-Link.
  - **Usenet:** your own providers, with NZBs from AnimeTosho and TsukiHime.
- **Keeps a library.** Each series gets a folder, with every episode listed,
  including ones you don't have yet. You can:
  - set audio and subtitle languages (for all series, or per series);
  - remux copies that keep only the tracks you choose;
  - build playlists and export them.
- **Plays it.** The built-in player draws subtitles with libass and can
  skip openings and endings. It moves on to the next episode and remembers
  where you stopped.
- **Watches together.** Start a watch party and send friends its code.
  When you play an episode, it starts for everyone once they all have it
  on disk, and their players follow yours: play, pause and seek. Only the
  host controls playback; a member's pause button asks the host.
  - Members connect to the host over [iroh](https://iroh.computer), through
    its public relays when a direct connection isn't possible. Only the
    party's own small messages go that way, never video.
  - A member downloading the episode with BitTorrent can get it from
    members who already have it (however they got it). This is the one
    time Ichioshi seeds: only to that member, on a port of its own that
    nothing announces, and only until they have the episode. From outside
    your network, it needs your router to allow a port mapping (UPnP,
    NAT-PMP or PCP); otherwise the member downloads it as usual.
- **Tracks it.** Signs in to AniList and MyAnimeList with one button.
  Status, score and progress update as you watch.
- **Discovers.** Browse trending and seasonal anime, see an airing calendar,
  and read each episode's discussion threads.
- **Works with your server.** Jellyfin: stream your library, or save
  episodes to watch offline. Sonarr and Radarr: manage series and films,
  with split cours mapped right. You can also upload to Google Drive.
- **Updates itself.** When a new release comes out, Ichioshi offers it
  (Settings › Updates, or More › Updates on a phone). It checks each
  download against the SHA-256 GitHub gives for it, then installs it:
  - on the desktop, it restarts as the new version;
  - on Android, the system's installer asks first. The first time, Android
    also asks you to allow Ichioshi to install apps.

## Download

From the [releases page](../../releases/latest):

| | File | Needs |
|---|---|---|
| **Windows** | `Ichioshi-windows-x86_64.zip` | 64-bit Windows 10 or later. Unzip it and run `Ichioshi.exe`; everything it needs is in the folder. |
| **Linux** | `Ichioshi-linux-x86_64.tar.gz` | glibc 2.35 or later (Ubuntu 22.04, Debian 12, Fedora 36 and newer), plus GStreamer 1.20+ and libass (see below). Extract it and run `./ichioshi`, or `./install.sh` to add it to your app menu. |
| **Android** | `Ichioshi-android-arm64.apk` | Android 11 or later, on a 64-bit ARM phone (almost all phones from the last several years). Your library goes in `Download/Ichioshi`. |

Versions from 0.2.0 on update themselves. On the desktop, `ichioshi --update`
updates without opening the window.

GStreamer and libass on Linux:

```sh
# Debian, Ubuntu
sudo apt install gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad gstreamer1.0-libav libass9
# Fedora (with RPM Fusion's codecs for H.264 and HEVC)
sudo dnf install gstreamer1-plugins-base gstreamer1-plugins-good \
    gstreamer1-plugins-bad-free gstreamer1-plugin-libav libass
# Arch
sudo pacman -S gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav libass
```

## Building

The desktop app is Rust, with an [iced](https://iced.rs) interface:

```sh
cargo run --release
```

On Linux this needs GStreamer's development files. The phone app (`mobile/`)
uses the same core, with a Svelte interface through [Tauri](https://tauri.app),
and plays video with libmpv.

The release builds are made by these scripts:

| Script | Makes | Needs |
|---|---|---|
| `scripts/linux/build.sh` | the Linux download | cargo-zigbuild and Zig |
| `scripts/windows/build.sh` | the Windows download, cross-compiled from Linux | cargo-zigbuild, Zig, MSYS2's GStreamer |
| `scripts/android/build.sh` | the APK | the Android SDK and NDK, a JDK, Node |

The release builds carry Ichioshi's own AniList and MyAnimeList API clients,
so signing in is one button. They're read when building from a file outside
the repository (see `scripts/release-keys.sh`). A build without them asks for
your own client: make one on AniList or MyAnimeList, with the redirect URL
Settings shows.

Tests: `cargo test`. Tests that reach real sites are `#[ignore]`d;
`cargo test -- --include-ignored` runs them too.

## Thanks

- Release picks: [SeaDex](https://releases.moe). Anime details: [AniList](https://anilist.co) and [Kitsu](https://kitsu.app).
- Release-group tiers: [TRaSH Guides](https://trash-guides.info). Episode mapping for Sonarr: [Fribb's anime-lists](https://github.com/Fribb/anime-lists).
- Built on [librqbit](https://github.com/ikatson/rqbit), [GStreamer](https://gstreamer.freedesktop.org), [libass](https://github.com/libass/libass) and [mpv](https://mpv.io).
- Icons: [Lucide](https://lucide.dev). Font: [Inter](https://rsms.me/inter).

Ichioshi isn't affiliated with SeaDex, AniList, MyAnimeList, Nyaa or any
of the services it works with. It doesn't host anything. What you
download, and from where, is up to you and the laws where you live.
