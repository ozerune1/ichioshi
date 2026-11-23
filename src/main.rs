mod app;
mod browse;
mod debrid;
mod discuss;
mod engine;
mod fallback;
mod fsutil;
mod gdrive;
mod gst_video;
mod library;
#[cfg(test)]
mod live_tests;
mod mkv;
#[cfg(test)]
mod mock_http;
mod nyaa;
mod player;
mod playlist;
mod prefs;
mod remux;
mod seadex;
mod storage;
mod subs;
mod tiers;
mod title;
mod tracking;
mod usenet;
mod vpn;

use std::time::Duration;

use tracing_subscriber::EnvFilter;

fn main() {
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
    let state_dir = std::env::var_os("SEADEX_STATE_DIR").map_or_else(
        || {
            dirs::data_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("seadex-downloader")
        },
        std::path::PathBuf::from,
    );
    if let Err(e) = app::gui::run(runtime.handle().clone(), state_dir) {
        eprintln!("{e}");
    }
    runtime.shutdown_timeout(Duration::from_secs(2));
}
