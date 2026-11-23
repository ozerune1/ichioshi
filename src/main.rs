// On Windows, no console window behind the app's.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

fn main() {
    seadex_downloader::desktop_main();
}
