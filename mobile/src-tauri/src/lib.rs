//! SeaDex for phones: the app's core (seadex-downloader, without its
//! desktop GUI) behind a web UI. The UI asks for the view and sends
//! commands; the core tells it things as events ("changed": ask for the
//! view again; "play": a video to play).

use seadex_downloader::{Bridge, Command};
use tauri::{Emitter, Manager};

/// What to show: the page and what's around it.
#[tauri::command]
async fn view(bridge: tauri::State<'_, Bridge>) -> Result<serde_json::Value, String> {
    bridge.view().await
}

/// Play a video in the app's player (Android): what the player says when
/// it's closed (how far it got, what next).
#[tauri::command]
async fn play(
    app: tauri::AppHandle,
    request: serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "android")]
    {
        let player = app.state::<Player>();
        player
            .0
            .run_mobile_plugin_async("play", request)
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, request);
        Err("The player is the phone's".into())
    }
}

/// Back from the first page: the app goes to the background (Android).
#[tauri::command]
async fn to_background(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let player = app.state::<Player>();
        player
            .0
            .run_mobile_plugin_async::<serde_json::Value>("toBackground", ())
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(())
    }
}

/// The phone's player (PlayerPlugin, in the Android project).
#[cfg(target_os = "android")]
struct Player(tauri::plugin::PluginHandle<tauri::Wry>);

fn player_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("player")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle =
                    _api.register_android_plugin("app.seadex.downloader", "PlayerPlugin")?;
                _app.manage(Player(handle));
            }
            Ok(())
        })
        .build()
}

/// Do something; what the app says back.
#[tauri::command]
async fn command(
    bridge: tauri::State<'_, Bridge>,
    command: Command,
) -> Result<serde_json::Value, String> {
    bridge.command(command).await
}

/// Android: the core's log and panics go to logcat (tag "SeaDex").
#[cfg(target_os = "android")]
fn log_to_logcat() {
    use std::ffi::{CString, c_char, c_int};
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
    fn write(text: &str) {
        if let Ok(text) = CString::new(text.replace('\0', " ")) {
            // SAFETY: both strings are NUL-terminated and outlive the call.
            unsafe {
                __android_log_write(4, c"SeaDex".as_ptr(), text.as_ptr());
            }
        }
    }
    struct Logcat;
    impl std::io::Write for Logcat {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            write(String::from_utf8_lossy(buf).trim_end());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .with_ansi(false)
        .without_time()
        .with_writer(|| Logcat)
        .try_init();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write(&format!("panic: {info}"));
        default(info);
    }));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "android")]
    log_to_logcat();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(player_plugin())
        .setup(|app| {
            let state_dir = state_dir(app)?;
            let events = app.handle().clone();
            let opener = app.handle().clone();
            let bridge = Bridge::start(
                state_dir,
                move |name, payload| {
                    let _ = events.emit(name, payload);
                },
                move |url| {
                    use tauri_plugin_opener::OpenerExt;
                    let _ = opener.opener().open_url(url, None::<&str>);
                },
            );
            app.manage(bridge);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![view, command, play, to_background])
        .run(tauri::generate_context!())
        .expect("error while running SeaDex");
}

/// Where the app keeps its state; on Android, also where the library and
/// downloads go (the phone's Download folder, which player apps can read)
/// and where servers' certificates are checked against.
fn state_dir(app: &tauri::App) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    #[cfg(target_os = "android")]
    {
        let storage = std::path::PathBuf::from(
            std::env::var_os("EXTERNAL_STORAGE").unwrap_or_else(|| "/storage/emulated/0".into()),
        );
        // (/sdcard is a link: the real path, which MediaStore uses.)
        let shared = std::fs::canonicalize(&storage)
            .unwrap_or(storage)
            .join("Download")
            .join("SeaDex");
        // SAFETY: before the core (and its threads) start.
        unsafe {
            std::env::set_var("SEADEX_LIBRARY_DIR", shared.join("Library"));
            std::env::set_var("SEADEX_DOWNLOAD_DIR", shared.join("Downloads"));
            // Android 14 and later keep the CAs in Conscrypt's module.
            std::env::set_var(
                "SSL_CERT_DIR",
                "/apex/com.android.conscrypt/cacerts:/system/etc/security/cacerts",
            );
        }
    }
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
