//! What Windows keeps about an installed program: its Start menu and desktop
//! shortcuts, and its entry in Settings › Apps (in the registry, for this
//! user only). Elsewhere (tests on Linux) there's none of it.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Settings › Apps' entry for Ichioshi, under HKEY_CURRENT_USER.
pub const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Ichioshi";

/// Ichioshi as installed before.
#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub dir: PathBuf,
    pub version: String,
}

/// Which shortcuts to make.
#[derive(Clone, Copy, Debug)]
pub struct Shortcuts {
    pub start_menu: bool,
    pub desktop: bool,
}

/// The app's own settings, sign-ins and library records (`%APPDATA%\ichioshi`,
/// where the app keeps them). Videos are elsewhere.
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("ichioshi"))
}

/// Start `program` (Ichioshi, once installed), on its own.
pub fn launch(program: &Path) -> Result<()> {
    let mut c = std::process::Command::new(program);
    if let Some(dir) = program.parent() {
        c.current_dir(dir);
    }
    c.spawn()?;
    Ok(())
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
pub use elsewhere::*;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::files::{PROGRAM, UNINSTALLER};
    use anyhow::Context;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, IPersistFile,
    };
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
        ShellLink,
    };
    use windows::core::{GUID, HSTRING, Interface};

    fn known_folder(id: &GUID) -> Result<PathBuf> {
        // SAFETY: the path Windows gives is freed once copied.
        unsafe {
            let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?;
            let path = p.to_string();
            CoTaskMemFree(Some(p.0 as *const _));
            Ok(PathBuf::from(path?))
        }
    }

    /// Where the Start menu and desktop shortcuts go.
    pub fn shortcut_paths() -> Result<(PathBuf, PathBuf)> {
        Ok((
            known_folder(&FOLDERID_Programs)?.join("Ichioshi.lnk"),
            known_folder(&FOLDERID_Desktop)?.join("Ichioshi.lnk"),
        ))
    }

    /// Make (or take away) the shortcuts to the app in `dir`.
    pub fn set_shortcuts(dir: &Path, want: Shortcuts) -> Result<()> {
        let (start_menu, desktop) = shortcut_paths()?;
        let program = dir.join(PROGRAM);
        for (link, wanted) in [(start_menu, want.start_menu), (desktop, want.desktop)] {
            if wanted {
                shortcut(&link, &program)
                    .with_context(|| format!("couldn't make the shortcut {}", link.display()))?;
            } else {
                let _ = std::fs::remove_file(&link);
            }
        }
        Ok(())
    }

    fn shortcut(link: &Path, program: &Path) -> Result<()> {
        // SAFETY: plain COM calls on an object made here.
        unsafe {
            // (Already set up on this thread is fine.)
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let l: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            l.SetPath(&HSTRING::from(program.as_os_str()))?;
            if let Some(dir) = program.parent() {
                l.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            l.SetIconLocation(&HSTRING::from(program.as_os_str()), 0)?;
            l.SetDescription(&HSTRING::from("Download, organize and watch anime"))?;
            let file: IPersistFile = l.cast()?;
            file.Save(&HSTRING::from(link.as_os_str()), true)?;
        }
        Ok(())
    }

    /// Ichioshi's entry in Settings › Apps, if it's installed.
    pub fn installed() -> Option<Installed> {
        let key = windows_registry::CURRENT_USER.open(KEY).ok()?;
        Some(Installed {
            dir: PathBuf::from(key.get_string("InstallLocation").ok()?),
            version: key.get_string("DisplayVersion").unwrap_or_default(),
        })
    }

    /// Say in Settings › Apps that Ichioshi `version` is installed in `dir`.
    pub fn register(dir: &Path, version: &str, size_kib: u64) -> Result<()> {
        let key = windows_registry::CURRENT_USER.create(KEY)?;
        let program = dir.join(PROGRAM);
        let uninstaller = dir.join(UNINSTALLER);
        key.set_string("DisplayName", "Ichioshi")?;
        key.set_string("DisplayVersion", version)?;
        key.set_string("Publisher", "Ichioshi")?;
        key.set_string("DisplayIcon", format!("{},0", program.display()))?;
        key.set_string("InstallLocation", dir.display().to_string())?;
        key.set_string("UninstallString", format!("\"{}\"", uninstaller.display()))?;
        key.set_string(
            "QuietUninstallString",
            format!("\"{}\" --silent", uninstaller.display()),
        )?;
        key.set_string("URLInfoAbout", "https://github.com/ozerune1/ichioshi")?;
        key.set_string("HelpLink", "https://github.com/ozerune1/ichioshi/issues")?;
        key.set_u32("NoModify", 1)?;
        key.set_u32("NoRepair", 1)?;
        key.set_u32("EstimatedSize", u32::try_from(size_kib).unwrap_or(u32::MAX))?;
        Ok(())
    }

    /// Take Ichioshi out of Settings › Apps.
    pub fn unregister() -> Result<()> {
        if windows_registry::CURRENT_USER.open(KEY).is_ok() {
            windows_registry::CURRENT_USER.remove_tree(KEY)?;
        }
        Ok(())
    }

    /// Delete `file` (this program, run from the temporary folder) once it
    /// has closed: a hidden command prompt waits a moment, then deletes it.
    pub fn delete_after_exit(file: &Path) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd.exe")
            .raw_arg(format!(
                "/d /c ping -n 3 127.0.0.1 >nul & del /f /q \"{}\"",
                file.display()
            ))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

/// Not Windows: nothing's kept (the UI's tests run here).
#[cfg(not(windows))]
mod elsewhere {
    use super::*;

    pub fn shortcut_paths() -> Result<(PathBuf, PathBuf)> {
        anyhow::bail!("no Start menu here")
    }

    pub fn set_shortcuts(_: &Path, _: Shortcuts) -> Result<()> {
        Ok(())
    }

    pub fn installed() -> Option<Installed> {
        None
    }

    pub fn register(_: &Path, _: &str, _: u64) -> Result<()> {
        Ok(())
    }

    pub fn unregister() -> Result<()> {
        Ok(())
    }

    pub fn delete_after_exit(_: &Path) {}
}
