//! The playlist: remuxed episodes in the order you want them, exported as a
//! zip named after the current date and time, e.g. `2026-11-21 16-45-03.zip`,
//! holding a folder of the same name with the files renamed `1) <title>.mkv`,
//! `2) <title>.mkv`... Saved as JSON in the app's state folder.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::remux::sanitize_filename;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub path: PathBuf,
    /// e.g. "Frieren: Beyond Journey’s End - 01".
    pub title: String,
}

pub struct Playlist {
    file: PathBuf,
    pub items: Vec<Item>,
}

impl Playlist {
    /// Load from `file`; a missing or unreadable file gives an empty playlist.
    pub fn load(file: PathBuf) -> Self {
        let items = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| match serde_json::from_str(&text) {
                Ok(items) => Some(items),
                Err(e) => {
                    tracing::warn!("ignoring unreadable {}: {e}", file.display());
                    None
                }
            })
            .unwrap_or_default();
        Self { file, items }
    }

    /// Append the items that aren't in the playlist yet; returns how many were added.
    pub fn add(&mut self, items: impl IntoIterator<Item = Item>) -> usize {
        let before = self.items.len();
        for item in items {
            if !self.items.iter().any(|i| i.path == item.path) {
                self.items.push(item);
            }
        }
        self.save();
        self.items.len() - before
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
            self.save();
        }
    }

    /// Move the item at `from` to position `to`.
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from < self.items.len() && to < self.items.len() && from != to {
            let item = self.items.remove(from);
            self.items.insert(to, item);
            self.save();
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.save();
    }

    /// A file moved: point its entries at the new place.
    pub fn relocate(&mut self, from: &Path, to: &Path) {
        let mut changed = false;
        for item in self.items.iter_mut().filter(|i| i.path == from) {
            item.path = to.to_owned();
            changed = true;
        }
        if changed {
            self.save();
        }
    }

    fn save(&self) {
        let result = (|| -> Result<()> {
            if let Some(dir) = self.file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.file.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&self.items)?)?;
            std::fs::rename(&tmp, &self.file)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("could not save {}: {e:#}", self.file.display());
        }
    }
}

/// The exported file names, `<n>) <title>.<ext>`, numbered from 1. Numbers
/// are zero-padded to the same width (`01)` ... `12)`) so the files sort in
/// playlist order everywhere, not just in file managers that sort smartly.
pub fn export_names(items: &[Item]) -> Vec<String> {
    let width = items.len().to_string().len();
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let ext = item.path.extension().and_then(|e| e.to_str()).unwrap_or("mkv");
            format!("{}.{ext}", sanitize_filename(&format!("{:0width$}) {}", i + 1, item.title)))
        })
        .collect()
}

/// The zip's (and its folder's) name for an export made at `now`, without
/// characters that are trouble on other systems: `2026-11-21 16-45-03`.
pub fn export_name(now: &DateTime<Local>) -> String {
    now.format("%Y-%m-%d %H-%M-%S").to_string()
}

/// Write `<dest_dir>/<date time>.zip` holding `<date time>/<n>) <title>.mkv`
/// for every item. Files are stored, not compressed (video doesn't shrink),
/// with ZIP64 for big ones. `progress` gets 0.0–1.0; setting `cancel` stops
/// it. Returns the zip's path. Blocking.
pub fn export(
    items: &[Item],
    dest_dir: &Path,
    now: DateTime<Local>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<PathBuf> {
    if items.is_empty() {
        bail!("the playlist is empty");
    }
    let missing: Vec<String> = items.iter().filter(|i| !i.path.is_file()).map(|i| i.title.clone()).collect();
    if !missing.is_empty() {
        bail!("{} missing: {}", if missing.len() == 1 { "a file is" } else { "files are" }, missing.join(", "));
    }
    let sizes: Vec<u64> = items.iter().map(|i| std::fs::metadata(&i.path).map(|m| m.len())).collect::<Result<_, _>>()?;
    let total = sizes.iter().sum::<u64>().max(1);

    std::fs::create_dir_all(dest_dir).with_context(|| format!("creating {}", dest_dir.display()))?;
    let name = export_name(&now);
    let mut zip_path = dest_dir.join(format!("{name}.zip"));
    for n in 2.. {
        if !zip_path.exists() {
            break;
        }
        zip_path = dest_dir.join(format!("{name} ({n}).zip"));
    }
    let tmp = dest_dir.join(format!(".{name}.zip.partial"));

    let result = (|| -> Result<()> {
        let mut zip = ZipWriter::new(BufWriter::with_capacity(1 << 20, File::create(&tmp)?));
        let modified = zip::DateTime::try_from(now.naive_local()).unwrap_or_default();
        let folder = SimpleFileOptions::default().last_modified_time(modified);
        zip.add_directory(format!("{name}/"), folder)?;
        let mut done = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        for ((item, file_name), size) in items.iter().zip(export_names(items)).zip(&sizes) {
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .large_file(*size >= u64::from(u32::MAX))
                .last_modified_time(modified)
                .unix_permissions(0o644);
            zip.start_file(format!("{name}/{file_name}"), options)?;
            let mut src = File::open(&item.path).with_context(|| format!("opening {}", item.path.display()))?;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                zip.write_all(&buf[..n])?;
                done += n as u64;
                progress((done as f32 / total as f32).min(0.99));
            }
        }
        zip.finish()?.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &zip_path).with_context(|| format!("saving {}", zip_path.display()))?;
    progress(1.0);
    Ok(zip_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("LIVE_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("seadex-playlist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(path: &Path, title: &str) -> Item {
        Item { path: path.to_owned(), title: title.into() }
    }

    #[test]
    fn names_are_numbered_and_padded() {
        let items: Vec<Item> = (1..=12)
            .map(|n| item(Path::new(&format!("/r/Show - {n:02}.mkv")), &format!("Show: Part/Two - {n:02}")))
            .collect();
        let names = export_names(&items);
        assert_eq!(names[0], "01) Show: Part-Two - 01.mkv");
        assert_eq!(names[11], "12) Show: Part-Two - 12.mkv");
        assert_eq!(export_names(&items[..3])[2], "3) Show: Part-Two - 03.mkv");
        let now = Local.with_ymd_and_hms(2026, 11, 21, 16, 5, 3).unwrap();
        assert_eq!(export_name(&now), "2026-11-21 16-05-03");
    }

    #[test]
    fn keeps_order_and_skips_duplicates() {
        let dir = scratch("list");
        let file = dir.join("playlist.json");
        let mut p = Playlist::load(file.clone());
        let (a, b, c) = (item(Path::new("/a.mkv"), "A"), item(Path::new("/b.mkv"), "B"), item(Path::new("/c.mkv"), "C"));
        assert_eq!(p.add([a.clone(), b.clone()]), 2);
        assert_eq!(p.add([b.clone(), c.clone()]), 1, "b is already there");
        p.move_item(2, 0);
        assert_eq!(Playlist::load(file.clone()).items, vec![c.clone(), a.clone(), b.clone()], "saved");
        p.remove(1);
        assert_eq!(p.items, vec![c, b]);
        p.clear();
        assert!(Playlist::load(file).items.is_empty());
    }

    #[test]
    fn exports_a_zip_with_a_dated_folder() {
        let dir = scratch("export");
        let first = dir.join("Show - 01.mkv");
        let second = dir.join("Other - 05.mkv");
        std::fs::write(&first, b"first episode").unwrap();
        std::fs::write(&second, vec![7u8; 3 << 20]).unwrap();
        let items = vec![item(&second, "Other: Thing - 05"), item(&first, "Show - 01")];
        let now = Local.with_ymd_and_hms(2026, 11, 21, 16, 45, 3).unwrap();
        let out = dir.join("exports");

        let mut last = 0.0;
        let zip_path = export(&items, &out, now, &AtomicBool::new(false), |p| last = p).unwrap();
        assert_eq!(last, 1.0);
        assert_eq!(zip_path, out.join("2026-11-21 16-45-03.zip"));
        let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned()).collect();
        assert_eq!(names, vec![
            "2026-11-21 16-45-03/",
            "2026-11-21 16-45-03/1) Other: Thing - 05.mkv",
            "2026-11-21 16-45-03/2) Show - 01.mkv",
        ]);
        let mut entry = archive.by_name("2026-11-21 16-45-03/2) Show - 01.mkv").unwrap();
        assert_eq!(entry.compression(), CompressionMethod::Stored);
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"first episode");
        drop(entry);

        // A second export in the same second doesn't overwrite the first.
        let again = export(&items, &out, now, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(again, out.join("2026-11-21 16-45-03 (2).zip"));
        // Cancelling leaves nothing behind; missing files are refused up front.
        assert!(export(&items, &dir.join("cancelled"), now, &AtomicBool::new(true), |_| {}).is_err());
        assert!(std::fs::read_dir(dir.join("cancelled")).is_ok_and(|mut d| d.next().is_none()));
        std::fs::remove_file(&first).unwrap();
        let err = export(&items, &out, now, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(err.to_string().contains("Show - 01"), "{err}");
    }

    /// Files over 4 GiB need ZIP64; checked with Python's zipfile and 7-Zip.
    #[test]
    #[ignore = "writes ~9 GB; needs python3 and 7z"]
    fn exports_files_over_4_gib() {
        let dir = scratch("zip64");
        let big = dir.join("big.mkv");
        let f = File::create(&big).unwrap();
        f.set_len((4 << 30) + 12345).unwrap(); // sparse, reads as zeros
        drop(f);
        let small = dir.join("small.mkv");
        std::fs::write(&small, b"after the big one").unwrap();
        let items = vec![item(&big, "Big - 01"), item(&small, "Small - 02")];
        let now = Local.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap();
        let zip_path = export(&items, &dir.join("out"), now, &AtomicBool::new(false), |_| {}).unwrap();
        let check = std::process::Command::new("python3")
            .args(["-I", "-c", "import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); assert z.testzip() is None; print([(i.filename, i.file_size) for i in z.infolist()])"])
            .arg(&zip_path)
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&check.stdout);
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
        assert!(listing.contains(&format!("'2026-11-21 09-00-00/1) Big - 01.mkv', {}", (4u64 << 30) + 12345)), "{listing}");
        let seven = std::process::Command::new("7z").arg("t").arg(&zip_path).output().unwrap();
        assert!(String::from_utf8_lossy(&seven.stdout).contains("Everything is Ok"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
