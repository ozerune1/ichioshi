//! Ichioshi Setup: installs the app it carries, with a window, or without
//! one: `setup.exe --silent [--dir <folder>] [--no-start-menu]
//! [--desktop-shortcut] [--launch]` (exit code 0 when installed).
#![windows_subsystem = "windows"]

use std::path::PathBuf;

use ichioshi_setup::system::{self, Shortcuts};
use ichioshi_setup::{files, ui, work};

/// The app's folder with the uninstaller in it, as a tar compressed with
/// zstd (scripts/windows/build.sh).
static PAYLOAD: &[u8] = include_bytes!(env!("SETUP_PAYLOAD"));

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |name: &str| args.iter().any(|a| a.eq_ignore_ascii_case(name));
    if has("--silent") || has("/S") {
        let dir = args
            .iter()
            .position(|a| a == "--dir")
            .and_then(|i| args.get(i + 1))
            .map(|d| files::install_dir(&PathBuf::from(d)))
            .or_else(|| system::installed().map(|i| i.dir))
            .unwrap_or_else(files::default_dir);
        let shortcuts = Shortcuts {
            start_menu: !has("--no-start-menu"),
            desktop: has("--desktop-shortcut"),
        };
        match work::install(PAYLOAD, &dir, shortcuts, |_| {}) {
            Ok(()) => {
                println!(
                    "Installed Ichioshi {} in {}",
                    ichioshi_setup::VERSION,
                    dir.display()
                );
                if has("--launch") {
                    let _ = system::launch(&dir.join(files::PROGRAM));
                }
            }
            Err(e) => {
                eprintln!("Couldn't install Ichioshi: {e:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(e) = ui::run(ui::Installer::setup(PAYLOAD)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
