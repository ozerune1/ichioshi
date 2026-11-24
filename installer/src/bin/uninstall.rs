//! Uninstall Ichioshi: left in the app's folder by setup, and run from
//! Settings › Apps. It can't remove the folder it's running from, so it
//! first runs a copy of itself from the temporary folder, which does the
//! work and is deleted once it closes. Without a window: `--silent`
//! (`--remove-data` takes the app's settings and records too).
#![windows_subsystem = "windows"]

use std::path::PathBuf;

use ichioshi_setup::{system, ui, work};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |name: &str| args.iter().any(|a| a.eq_ignore_ascii_case(name));
    let me = std::env::current_exe().unwrap_or_default();
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(|| me.parent().map(PathBuf::from))
        .unwrap_or_default();
    let silent = has("--silent") || has("/S");
    if cfg!(windows) && !has("--from-temp") {
        let copy =
            std::env::temp_dir().join(format!("ichioshi-uninstall-{}.exe", std::process::id()));
        let started = std::fs::copy(&me, &copy).and_then(|_| {
            std::process::Command::new(&copy)
                .arg("--from-temp")
                .arg("--dir")
                .arg(&dir)
                .args(
                    args.iter()
                        .filter(|a| *a != "--dir" && !a.contains(['\\', '/']) || *a == "/S"),
                )
                .spawn()
        });
        if let Err(e) = started {
            eprintln!("Couldn't start the uninstaller: {e}");
            std::process::exit(1);
        }
        return;
    }
    let code = if silent {
        match work::uninstall(&dir, has("--remove-data"), |_| {}) {
            Ok(()) => {
                println!("Removed Ichioshi from {}", dir.display());
                0
            }
            Err(e) => {
                eprintln!("Couldn't remove Ichioshi: {e:#}");
                1
            }
        }
    } else {
        i32::from(ui::run(ui::Installer::uninstall(dir)).is_err())
    };
    if has("--from-temp") {
        system::delete_after_exit(&me);
    }
    std::process::exit(code);
}
