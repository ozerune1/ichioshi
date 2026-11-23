//! SeaDex: find the best releases of anime (SeaDex, Nyaa), download them
//! (BitTorrent, debrid services, Usenet) into a library on this computer or
//! on your own servers (Sonarr, Radarr, Jellyfin), and watch them.
//!
//! The desktop app (the `gui` feature, iced) runs `desktop_main`. The phone
//! app (mobile/, Tauri) builds this without the GUI and runs the core
//! through [`Bridge`].

// Android has no player of its own (videos open in a player app): the
// player's messages and settings go unused there; and without the GUI
// (the phone app, mobile/, has its own), parts of the core it alone uses.
#![cfg_attr(
    any(target_os = "android", not(feature = "gui")),
    allow(dead_code, unused_imports)
)]

mod app;

pub use app::bridge::{Bridge, Command};
mod browse;
mod debrid;
mod discuss;
mod engine;
mod fallback;
mod fsutil;
mod gdrive;
#[cfg(all(feature = "gui", not(target_os = "android")))]
mod gst_video;
mod library;
#[cfg(test)]
mod live_tests;
mod mkv;
#[cfg(test)]
mod mock_http;
mod nyaa;
mod pick;
mod player;
mod playlist;
mod prefs;
mod remux;
mod seadex;
mod servers;
mod storage;
mod subs;
mod tiers;
mod title;
mod tracking;
mod usenet;
mod vpn;

#[cfg(feature = "gui")]
use std::time::Duration;

#[cfg(feature = "gui")]
use tracing_subscriber::EnvFilter;

#[cfg(all(feature = "gui", target_os = "android"))]
mod android;

/// Where GStreamer's plugins are when the app is bundled with them
/// (Windows): `lib/gstreamer-1.0` and its plugin scanner next to the
/// executable; its plugin cache goes in the app's own folder.
#[cfg(all(feature = "gui", not(target_os = "android")))]
fn use_bundled_gstreamer(state_dir: &std::path::Path) {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let plugins = dir.join("lib").join("gstreamer-1.0");
    if !plugins.is_dir() {
        return;
    }
    // SAFETY: still single-threaded: nothing else reads the environment yet.
    unsafe {
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
        std::env::set_var("GST_PLUGIN_PATH_1_0", &plugins);
        let scanner = dir
            .join("libexec")
            .join("gstreamer-1.0")
            .join("gst-plugin-scanner.exe");
        if scanner.is_file() {
            std::env::set_var("GST_PLUGIN_SCANNER_1_0", scanner);
        }
        std::env::set_var("GST_REGISTRY_1_0", state_dir.join("gstreamer-registry.bin"));
        // HTTPS for streams (GLib's TLS module), and fonts for libass.
        let gio = dir.join("lib").join("gio").join("modules");
        if gio.is_dir() {
            std::env::set_var("GIO_MODULE_DIR", gio);
        }
        let fonts = dir.join("etc").join("fonts");
        if fonts.is_dir() {
            std::env::set_var("FONTCONFIG_PATH", fonts);
        }
    }
}

/// Play a video to its end through playbin, showing and sounding nothing.
#[cfg(all(feature = "gui", not(target_os = "android")))]
fn play_through(video: &std::path::Path) -> Result<(), String> {
    use gstreamer::prelude::*;
    let uri = gst_video::file_uri(video).map_err(|e| e.to_string())?;
    let sink = |name| {
        gstreamer::ElementFactory::make("fakesink")
            .property("sync", false)
            .name(name)
            .build()
            .map_err(|e| e.to_string())
    };
    let playbin = gstreamer::ElementFactory::make("playbin")
        .property("uri", uri)
        .property("video-sink", sink("v")?)
        .property("audio-sink", sink("a")?)
        .build()
        .map_err(|e| e.to_string())?;
    playbin
        .set_state(gstreamer::State::Playing)
        .map_err(|e| e.to_string())?;
    let bus = playbin.bus().ok_or("no bus")?;
    let result = match bus.timed_pop_filtered(
        gstreamer::ClockTime::from_seconds(30),
        &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
    ) {
        Some(m) => match m.view() {
            gstreamer::MessageView::Eos(_) => Ok(()),
            gstreamer::MessageView::Error(e) => Err(e.error().to_string()),
            _ => Err("?".into()),
        },
        None => Err("it didn't finish in 30 s".into()),
    };
    let _ = playbin.set_state(gstreamer::State::Null);
    result
}

/// `--self-test <file> [video]`: whether what playing needs is there (GStreamer,
/// its plugins, libass), written to the file; for checking a build where
/// no window can open (Windows builds, under Wine).
#[cfg(all(feature = "gui", not(target_os = "android")))]
fn self_test(out: &std::path::Path, video: Option<&std::path::Path>) -> bool {
    let mut lines = Vec::new();
    let mut ok = true;
    match gstreamer::init() {
        Ok(()) => lines.push(format!("gstreamer {}", gstreamer::version_string())),
        Err(e) => {
            lines.push(format!("gstreamer FAILED: {e}"));
            ok = false;
        }
    }
    for element in [
        "playbin",
        "appsink",
        "matroskademux",
        "qtdemux",
        "souphttpsrc",
        "avdec_h264",
        "avdec_h265",
        "avdec_aac",
        "avdec_flac",
        "opusdec",
        "videoconvert",
        "videoscale",
        "audioconvert",
        "audioresample",
        "autoaudiosink",
    ] {
        let found = gstreamer::ElementFactory::find(element).is_some();
        ok &= found;
        lines.push(format!(
            "{element} {}",
            if found { "ok" } else { "MISSING" }
        ));
    }
    let ass = subs::available();
    ok &= ass;
    lines.push(format!("libass {}", if ass { "ok" } else { "MISSING" }));
    // libass draws a line (in the app's own font).
    if ass {
        let drawn = (|| {
            let fonts = mkv::TextSubtitles {
                tracks: Default::default(),
                fonts: vec![("Inter-Regular.ttf".into(), app::gui::FONTS[0].to_vec())],
            };
            let mut r = subs::Renderer::new(&fonts).ok()?;
            let track = mkv::SubTrack {
                codec_id: "S_TEXT/UTF8".into(),
                codec_private: Vec::new(),
                events: vec![mkv::SubEvent {
                    start_ms: 0,
                    duration_ms: 5000,
                    data: b"Subtitles work".to_vec(),
                }],
            };
            r.set_track(Some(&track), (1280, 720));
            match r.draw(1000, (1280, 720), 0.0) {
                subs::Drawn::Image(rgba) => Some(rgba.chunks(4).filter(|p| p[3] > 0).count()),
                _ => None,
            }
        })();
        let drew = drawn.is_some_and(|n| n > 500);
        ok &= drew;
        lines.push(match drawn {
            Some(n) if drew => format!("libass draws ok ({n} pixels)"),
            _ => "libass draws NOTHING".into(),
        });
    }
    // A video plays through (to nowhere).
    if let Some(video) = video {
        let played = play_through(video);
        ok &= played.is_ok();
        lines.push(match played {
            Ok(()) => format!("played {} ok", video.display()),
            Err(e) => format!("playing {} FAILED: {e}", video.display()),
        });
    }
    lines.push(if ok {
        "all ok".into()
    } else {
        "PROBLEMS".into()
    });
    let _ = std::fs::write(out, lines.join("\n") + "\n");
    ok
}

/// The desktop app (Linux, Windows); `--self-test` checks a build.
#[cfg(all(feature = "gui", not(target_os = "android")))]
pub fn desktop_main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--self-test") {
        let out = args.get(i + 1).map_or("self-test.txt", String::as_str);
        let video = args.get(i + 2).map(std::path::Path::new);
        let state = std::env::temp_dir().join("seadex-self-test");
        use_bundled_gstreamer(&state);
        let ok = self_test(std::path::Path::new(out), video);
        std::process::exit(if ok { 0 } else { 1 });
    }
    let state_dir = std::env::var_os("SEADEX_STATE_DIR").map_or_else(
        || {
            dirs::data_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("seadex-downloader")
        },
        std::path::PathBuf::from,
    );
    // Before any other thread starts.
    use_bundled_gstreamer(&state_dir);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    // librqbit and the HTTP requests run here; the GUI owns the main thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the tokio runtime");
    if let Err(e) = app::gui::run(runtime.handle().clone(), state_dir) {
        eprintln!("{e}");
    }
    runtime.shutdown_timeout(Duration::from_secs(2));
}

/// Android starts the app here (NativeActivity): its state goes in the
/// app's own folder; the window is the activity's.
#[cfg(all(feature = "gui", target_os = "android"))]
#[unsafe(no_mangle)]
fn android_main(app: iced_winit::android::AndroidApp) {
    android::start_logging();
    let state_dir = app
        .internal_data_path()
        .unwrap_or_else(|| std::path::PathBuf::from("/data/local/tmp/seadex"));
    // The library and downloads: in the phone's Download folder, where
    // player apps can read them (an app's own storage is closed to others),
    // and which an app can write to without asking for a permission.
    // (/sdcard is a link to it: the real path, which MediaStore uses.)
    let storage = std::env::var_os("EXTERNAL_STORAGE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/storage/emulated/0".into());
    let shared = std::fs::canonicalize(&storage)
        .unwrap_or(storage)
        .join("Download")
        .join("SeaDex");
    // SAFETY: nothing else runs yet.
    unsafe {
        std::env::set_var("SEADEX_LIBRARY_DIR", shared.join("Library"));
        std::env::set_var("SEADEX_DOWNLOAD_DIR", shared.join("Downloads"));
        // Servers' certificates are checked against the phone's own CAs
        // (Android 14 and later keep them in Conscrypt's module).
        std::env::set_var(
            "SSL_CERT_DIR",
            "/apex/com.android.conscrypt/cacerts:/system/etc/security/cacerts",
        );
    }
    android::keep(&app);
    iced_winit::android::set_app(app);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the tokio runtime");
    if let Err(e) = app::gui::run(runtime.handle().clone(), state_dir) {
        tracing::error!("{e}");
    }
    runtime.shutdown_timeout(Duration::from_secs(2));
}
