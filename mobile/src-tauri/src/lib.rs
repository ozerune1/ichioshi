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

/// Do something; what the app says back.
#[tauri::command]
async fn command(
    bridge: tauri::State<'_, Bridge>,
    command: Command,
) -> Result<serde_json::Value, String> {
    bridge.command(command).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
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
        .invoke_handler(tauri::generate_handler![view, command])
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
