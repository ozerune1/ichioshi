//! The app's files: where they go, putting them there and taking them away.

use std::cell::Cell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result, bail};

/// The app, in its folder.
pub const PROGRAM: &str = "Ichioshi.exe";
/// The uninstaller, beside it.
pub const UNINSTALLER: &str = "Uninstall Ichioshi.exe";

/// Where Ichioshi goes unless another folder's chosen: this user's own
/// programs folder (`%LOCALAPPDATA%\Programs\Ichioshi`), which needs no
/// administrator.
pub fn default_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData").join("Local"))
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("Programs")
        .join("Ichioshi")
}

/// The folder to install in, for one picked: itself if it's empty or
/// Ichioshi's already, else an Ichioshi folder in it, so the folder is
/// Ichioshi's alone and uninstalling can remove all of it.
pub fn install_dir(picked: &Path) -> PathBuf {
    let ours = picked.join(PROGRAM).is_file();
    let empty = std::fs::read_dir(picked).map_or(true, |mut d| d.next().is_none());
    if ours || empty {
        picked.to_owned()
    } else {
        picked.join("Ichioshi")
    }
}

/// Whether `dir` is a folder Ichioshi was installed in (so it may be
/// removed whole): it has the app or its uninstaller, and isn't a drive's
/// top folder.
pub fn is_install(dir: &Path) -> bool {
    dir.components().count() > 2 && (dir.join(PROGRAM).is_file() || dir.join(UNINSTALLER).is_file())
}

/// Whether Ichioshi (in `dir`) is running: Windows doesn't let a running
/// program be opened for writing.
pub fn running(dir: &Path) -> bool {
    const SHARING_VIOLATION: i32 = 32;
    cfg!(windows)
        && std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join(PROGRAM))
            .is_err_and(|e| e.raw_os_error() == Some(SHARING_VIOLATION))
}

/// Empty `dir` of an earlier install (its files may differ from this
/// version's), keeping the folder.
pub fn clear(dir: &Path) -> Result<()> {
    if !is_install(dir) {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        retry(|| {
            if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
        })
        .with_context(|| format!("couldn't remove {}", path.display()))?;
    }
    Ok(())
}

/// Unpack `payload` (the app's folder: a tar compressed with zstd) into
/// `dir`, saying how far along it is (0 to 1) as it goes.
pub fn unpack(payload: &[u8], dir: &Path, mut progress: impl FnMut(f32)) -> Result<()> {
    if payload.is_empty() {
        bail!("this setup has no Ichioshi in it (it was built for tests)");
    }
    std::fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let read = Rc::new(Cell::new(0usize));
    let counted = Counted {
        data: payload,
        read: read.clone(),
    };
    let mut decoder = zstd::stream::read::Decoder::new(counted)?;
    decoder.window_log_max(31)?;
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("the setup is damaged")? {
        let mut entry = entry.context("the setup is damaged")?;
        // (Refuses anything that would land outside `dir`.)
        entry.unpack_in(dir).with_context(|| {
            format!(
                "couldn't write {}",
                entry.path().unwrap_or_default().display()
            )
        })?;
        progress(read.get() as f32 / payload.len() as f32);
    }
    progress(1.0);
    Ok(())
}

/// A reader of a slice that counts what's been read.
struct Counted<'a> {
    data: &'a [u8],
    read: Rc<Cell<usize>>,
}

impl Read for Counted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let at = self.read.get();
        let n = buf.len().min(self.data.len() - at);
        buf[..n].copy_from_slice(&self.data[at..at + n]);
        self.read.set(at + n);
        Ok(n)
    }
}

/// How much is in `dir`, in KiB (for Settings › Apps).
pub fn size_kib(dir: &Path) -> u64 {
    fn walk(dir: &Path) -> u64 {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&e.path()),
                    _ => e.metadata().map_or(0, |m| m.len()),
                })
                .sum()
        })
    }
    walk(dir) / 1024
}

/// Remove the folder Ichioshi was installed in, and everything in it.
pub fn remove_install(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if !is_install(dir) {
        bail!(
            "{} doesn't look like Ichioshi's folder, so it's left as it is",
            dir.display()
        );
    }
    retry(|| std::fs::remove_dir_all(dir))
        .with_context(|| format!("couldn't remove {}", dir.display()))
}

/// Try `f` for a few seconds: something may still be letting go of a file
/// (the app closing, the uninstaller that started this one).
fn retry(mut f: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match f() {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound && tries < 20 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ichioshi-setup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A payload as scripts/windows/build.sh makes it.
    fn payload(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            // Written as is (the builder won't take "..": a damaged one).
            let path = format!("./{name}");
            h.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append(&h, *data).unwrap();
        }
        zstd::encode_all(&tar.into_inner().unwrap()[..], 19).unwrap()
    }

    #[test]
    fn unpacks_the_app_and_says_how_far() {
        let dir = temp("unpack").join("Ichioshi");
        let data = payload(&[
            ("Ichioshi.exe", b"app"),
            ("lib/gstreamer-1.0/libgstplayback.dll", b"plugin"),
        ]);
        let mut seen = Vec::new();
        unpack(&data, &dir, |p| seen.push(p)).unwrap();
        assert_eq!(std::fs::read(dir.join("Ichioshi.exe")).unwrap(), b"app");
        assert!(dir.join("lib/gstreamer-1.0/libgstplayback.dll").is_file());
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]));
        assert!(unpack(&[], &dir, |_| {}).is_err());
        // Out of its folder: refused.
        let evil = payload(&[("../outside.txt", b"no")]);
        let _ = unpack(&evil, &dir, |_| {});
        assert!(!dir.parent().unwrap().join("outside.txt").exists());
    }

    #[test]
    fn a_folder_of_its_own() {
        let base = temp("dir");
        // Empty: itself. Holding other things: an Ichioshi folder in it.
        assert_eq!(install_dir(&base), base);
        std::fs::write(base.join("notes.txt"), "mine").unwrap();
        assert_eq!(install_dir(&base), base.join("Ichioshi"));
        // Ichioshi's already: itself, and it may be cleared and removed.
        let ours = base.join("Ichioshi");
        std::fs::create_dir_all(ours.join("lib")).unwrap();
        std::fs::write(ours.join(PROGRAM), "app").unwrap();
        assert_eq!(install_dir(&ours), ours);
        assert!(is_install(&ours) && !is_install(&base));
        clear(&ours).unwrap();
        assert!(ours.is_dir() && std::fs::read_dir(&ours).unwrap().next().is_none());
        // Not Ichioshi's: kept.
        assert!(remove_install(&base).is_err());
        assert!(base.join("notes.txt").is_file());
        std::fs::write(ours.join(UNINSTALLER), "u").unwrap();
        remove_install(&ours).unwrap();
        assert!(!ours.exists());
    }
}
