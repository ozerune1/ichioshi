mod app;
mod debrid;
mod engine;
mod fallback;
mod fsutil;
mod gdrive;
mod library;
#[cfg(test)]
mod live_tests;
mod mkv;
#[cfg(test)]
mod mock_http;
mod nyaa;
mod playlist;
mod prefs;
mod remux;
mod seadex;
mod storage;
mod tiers;
mod title;
mod usenet;
mod vpn;

use std::time::Duration;

use eframe::egui;
use tracing_subscriber::EnvFilter;

const APP_NAME: &str = "SeaDex Downloader";

fn main() -> eframe::Result {
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
    let handle = runtime.handle().clone();
    let state_dir = dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("seadex-downloader");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_app_id("seadex-downloader")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([860.0, 520.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc, handle, state_dir)))
        }),
    );

    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}
