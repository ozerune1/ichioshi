//! Ichioshi for phones: the app's core (ichioshi, without its
//! desktop GUI) behind a web UI. The UI asks for the view and sends
//! commands; the core tells it things as events ("changed": ask for the
//! view again; "play": a video to play).

use ichioshi::{Bridge, Command};
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

/// Install a downloaded update (Android): the system's installer asks, and
/// takes over.
#[tauri::command]
async fn install_update(app: tauri::AppHandle, path: String) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let player = app.state::<Player>();
        player
            .0
            .run_mobile_plugin_async::<serde_json::Value>(
                "installApk",
                serde_json::json!({ "path": path }),
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, path);
        Err("Installing is the phone's".into())
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

/// The watch party and the player, a few times a second while there's a
/// party (else now and then): what the player's doing, to the core; what
/// the party wants it to do, to the player. Not through the web UI, which
/// sleeps while the player's open.
#[cfg(target_os = "android")]
fn party_poll(app: tauri::AppHandle, bridge: Bridge) {
    use std::time::Duration;
    std::thread::Builder::new()
        .name("party-poll".into())
        .spawn(move || {
            let mut active = false;
            loop {
                std::thread::sleep(Duration::from_millis(if active { 300 } else { 1500 }));
                let player = app.state::<Player>();
                let state = if active {
                    tauri::async_runtime::block_on(
                        player
                            .0
                            .run_mobile_plugin_async::<serde_json::Value>("partyState", ()),
                    )
                    .ok()
                } else {
                    None
                };
                let Ok(said) = tauri::async_runtime::block_on(bridge.party(state)) else {
                    continue;
                };
                active = said["active"] == true;
                // The party's episode: the player opens; how far it got goes
                // to the core when it closes.
                if said["play"].is_object() {
                    let (app, bridge, request) =
                        (app.clone(), bridge.clone(), said["play"].clone());
                    tauri::async_runtime::spawn(async move {
                        let player = app.state::<Player>();
                        let r = player
                            .0
                            .run_mobile_plugin_async::<serde_json::Value>("play", request.clone())
                            .await
                            .unwrap_or_default();
                        let played = serde_json::json!({
                            "do": "played",
                            "id": request["id"],
                            "episode": request["episode"],
                            "position": r["position"].as_f64().unwrap_or(0.0),
                            "duration": r["duration"].as_f64().unwrap_or(0.0),
                            "then": r["then"],
                        });
                        if let Ok(c) = serde_json::from_value::<Command>(played) {
                            let _ = bridge.command(c).await;
                        }
                    });
                }
                let control = &said["control"];
                if !control.is_null() {
                    let _ = tauri::async_runtime::block_on(
                        player.0.run_mobile_plugin_async::<serde_json::Value>(
                            "partyControl",
                            control.clone(),
                        ),
                    );
                }
            }
        })
        .expect("failed to start the party's poll");
}

/// The phone's player (PlayerPlugin, in the Android project).
#[cfg(target_os = "android")]
struct Player(tauri::plugin::PluginHandle<tauri::Wry>);

fn player_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("player")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle = _api.register_android_plugin("app.ichioshi", "PlayerPlugin")?;
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

/// Android: the core's log and panics go to logcat (tag "Ichioshi").
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
                __android_log_write(4, c"Ichioshi".as_ptr(), text.as_ptr());
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
            #[cfg(target_os = "android")]
            party_poll(app.handle().clone(), bridge.clone());
            app.manage(bridge);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            view,
            command,
            play,
            install_update,
            to_background
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ichioshi");
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
            .join("Ichioshi");
        // SAFETY: before the core (and its threads) start.
        unsafe {
            std::env::set_var("ICHIOSHI_LIBRARY_DIR", shared.join("Library"));
            std::env::set_var("ICHIOSHI_DOWNLOAD_DIR", shared.join("Downloads"));
            // Android 14 and later keep the CAs in Conscrypt's module.
            std::env::set_var(
                "SSL_CERT_DIR",
                "/apex/com.android.conscrypt/cacerts:/system/etc/security/cacerts",
            );
        }
    }
    let dir = app.path().app_data_dir()?;
    // Android: in the app's files folder, where the earlier app (iced)
    // kept its settings and library, so they carry over.
    #[cfg(target_os = "android")]
    let dir = dir.join("files");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
