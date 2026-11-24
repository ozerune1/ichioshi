//! Installing and uninstalling, step by step (the window runs these on
//! their own thread; `--silent` runs them as they are).

use std::path::Path;

use anyhow::{Result, bail};

use crate::files;
use crate::system::{self, Shortcuts};

/// Put the app and its uninstaller (`payload`) in `dir`, make the
/// shortcuts asked for, and list it in Settings › Apps.
pub fn install(
    payload: &[u8],
    dir: &Path,
    shortcuts: Shortcuts,
    mut progress: impl FnMut(f32),
) -> Result<()> {
    if files::running(dir) {
        bail!("Ichioshi is open: close it, then try again");
    }
    // An earlier install's files go first: this version's may differ.
    files::clear(dir)?;
    progress(0.0);
    files::unpack(payload, dir, |p| progress(p * 0.95))?;
    system::set_shortcuts(dir, shortcuts)?;
    system::register(dir, crate::VERSION, files::size_kib(dir))?;
    progress(1.0);
    Ok(())
}

/// Take the app in `dir` away: its shortcuts, its folder, its entry in
/// Settings › Apps and (if asked) its settings and records. Videos aren't
/// touched.
pub fn uninstall(dir: &Path, remove_data: bool, mut progress: impl FnMut(f32)) -> Result<()> {
    if files::running(dir) {
        bail!("Ichioshi is open: close it, then try again");
    }
    progress(0.0);
    if let Ok((start_menu, desktop)) = system::shortcut_paths() {
        let _ = std::fs::remove_file(start_menu);
        let _ = std::fs::remove_file(desktop);
    }
    progress(0.1);
    files::remove_install(dir)?;
    progress(0.8);
    // The entry, if it's this folder's (another copy may be installed).
    if system::installed().is_none_or(|i| same_dir(&i.dir, dir)) {
        system::unregister()?;
    }
    if remove_data
        && let Some(data) = system::data_dir()
        && data.exists()
    {
        std::fs::remove_dir_all(&data)?;
    }
    progress(1.0);
    Ok(())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    norm(a) == norm(b)
}
