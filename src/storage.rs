//! Torrent storage that only keeps the files you selected.
//!
//! librqbit's own filesystem storage creates every file in a torrent up
//! front. And pieces don't line up with file boundaries: the piece where one
//! episode ends and the next begins has to be downloaded whole, which leaves a
//! "sliver" of the neighbouring, unselected episode on disk.
//!
//! This storage:
//! - creates real files only for selected files, when their data arrives;
//! - writes slivers of unselected files to an anonymous temporary file that
//!   never shows up in the folder, and drops it as soon as librqbit has
//!   verified the pieces it belongs to, so slivers are deleted right away;
//! - reads deleted sliver regions back as zeros. librqbit's quick resume check
//!   at startup verifies one claimed piece per file, which for an unselected
//!   file is a shared piece; zeros make that a hash mismatch, which the check
//!   tolerates, whereas a read error would make librqbit discard its resume
//!   data and re-hash the whole torrent on every start. Everywhere else, reads
//!   of missing data are errors, which lets full re-checks skip quickly.
//!
//! Since the slivers are gone, selecting a neighbouring file later means
//! downloading those shared pieces again; [`crate::engine`] handles that by
//! having librqbit re-check the torrent.
//!
//! librqbit doesn't tell storage which files are selected or where the
//! torrent's output folder is, so [`Registry`] tracks that per info hash.

use std::any::TypeId;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result, bail};
use librqbit::dht::Id20;
use librqbit::storage::filesystem::FilesystemStorageFactory;
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrentShared, TorrentMetadata};
use librqbit_core::lengths::ValidPieceIndex;

/// Hidden folder where an earlier version of this app kept slivers. Cleaned
/// up on sight.
const LEGACY_PARTS_DIR: &str = ".seadex-parts";

/// Where each torrent lives and which of its files are selected.
#[derive(Default)]
pub struct Registry {
    torrents: RwLock<HashMap<Id20, Arc<Entry>>>,
}

pub struct Entry {
    output_folder: PathBuf,
    /// `None` means every file.
    wanted: RwLock<Option<HashSet<usize>>>,
}

impl Entry {
    fn is_wanted(&self, file: usize) -> bool {
        self.wanted
            .read()
            .unwrap()
            .as_ref()
            .is_none_or(|w| w.contains(&file))
    }
}

impl Registry {
    /// Record (or update) a torrent's output folder and selection. Call this
    /// before adding the torrent or changing its selection.
    pub fn set(&self, info_hash: Id20, output_folder: &Path, wanted: Option<&[usize]>) {
        let wanted = wanted.map(|w| w.iter().copied().collect());
        let mut torrents = self.torrents.write().unwrap();
        match torrents.get(&info_hash) {
            // Keep the folder: a torrent already in the session doesn't move.
            Some(entry) => *entry.wanted.write().unwrap() = wanted,
            None => {
                torrents.insert(
                    info_hash,
                    Arc::new(Entry { output_folder: output_folder.to_owned(), wanted: RwLock::new(wanted) }),
                );
            }
        }
    }

    pub fn remove(&self, info_hash: &Id20) {
        self.torrents.write().unwrap().remove(info_hash);
    }

    fn get(&self, info_hash: &Id20) -> Option<Arc<Entry>> {
        self.torrents.read().unwrap().get(info_hash).cloned()
    }

    /// Fill the registry from librqbit's saved session, so torrents restored
    /// at startup use this storage too. Best effort: a torrent missing here
    /// falls back to librqbit's own storage.
    pub fn load_librqbit_session(&self, session_json: &Path) {
        #[derive(serde::Deserialize)]
        struct Saved {
            torrents: HashMap<String, SavedTorrent>,
        }
        #[derive(serde::Deserialize)]
        struct SavedTorrent {
            info_hash: String,
            output_folder: PathBuf,
            only_files: Option<Vec<usize>>,
        }
        let Ok(text) = std::fs::read_to_string(session_json) else { return };
        let saved: Saved = match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("could not read {}: {e}", session_json.display());
                return;
            }
        };
        for t in saved.torrents.into_values() {
            if let Ok(hash) = Id20::from_str(&t.info_hash) {
                self.set(hash, &t.output_folder, t.only_files.as_deref());
            }
        }
    }
}

/// Creates [`SelectiveStorage`] for torrents in the registry, and librqbit's
/// plain filesystem storage for any it doesn't know.
#[derive(Clone)]
pub struct SelectiveStorageFactory {
    registry: Arc<Registry>,
}

impl SelectiveStorageFactory {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }
}

impl StorageFactory for SelectiveStorageFactory {
    type Storage = Box<dyn TorrentStorage>;

    fn create(&self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> Result<Self::Storage> {
        match self.registry.get(&shared.info_hash) {
            Some(entry) => Ok(Box::new(SelectiveStorage {
                entry,
                layout: Arc::new(Layout::default()),
                taken: AtomicBool::new(false),
            })),
            None => {
                tracing::warn!(info_hash = ?shared.info_hash, "torrent not registered; creating all of its files");
                Ok(Box::new(FilesystemStorageFactory::default().create(shared, metadata)?))
            }
        }
    }

    /// It's a layer over the filesystem storage (selected files are plain
    /// files in the same places), which librqbit's session persistence requires.
    fn is_type_id(&self, type_id: TypeId) -> bool {
        type_id == TypeId::of::<Self>() || FilesystemStorageFactory::default().is_type_id(type_id)
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

enum Backing {
    /// Nothing open: the file isn't on disk, or hasn't been touched yet.
    Closed,
    /// The file in the output folder.
    Real(File),
    /// An anonymous temporary file holding slivers of an unselected file
    /// until their pieces are verified.
    Scratch(File),
}

struct Slot {
    real: PathBuf,
    /// Where this file is in the torrent's byte stream.
    offset: u64,
    len: u64,
    padding: bool,
    backing: RwLock<Backing>,
    /// Pieces with data in the scratch file that librqbit hasn't verified yet.
    pending: Mutex<BTreeSet<u32>>,
}

#[derive(Default)]
struct Layout {
    output_folder: PathBuf,
    piece_length: u64,
    slots: Vec<Slot>,
}

pub struct SelectiveStorage {
    entry: Arc<Entry>,
    layout: Arc<Layout>,
    taken: AtomicBool,
}

impl SelectiveStorage {
    fn slot(&self, file_id: usize) -> Result<&Slot> {
        if self.taken.load(Ordering::Relaxed) {
            bail!("storage was handed over");
        }
        self.layout.slots.get(file_id).context("no such file")
    }

    /// Pieces covering `len` bytes at `offset` in a file.
    fn pieces(&self, slot: &Slot, offset: u64, len: usize) -> std::ops::RangeInclusive<u32> {
        let l = self.layout.piece_length.max(1);
        let start = slot.offset + offset;
        let end = start + (len as u64).max(1) - 1;
        (start / l) as u32..=(end / l) as u32
    }

    /// Whether a piece overlaps any selected file.
    fn piece_is_selected(&self, piece: u32) -> bool {
        let l = self.layout.piece_length;
        let (start, end) = (u64::from(piece) * l, (u64::from(piece) + 1) * l);
        self.layout.slots.iter().enumerate().any(|(i, s)| {
            !s.padding && s.len > 0 && s.offset < end && s.offset + s.len > start && self.entry.is_wanted(i)
        })
    }

    fn note_pending(&self, slot: &Slot, offset: u64, len: usize) {
        slot.pending.lock().unwrap().extend(self.pieces(slot, offset, len));
    }

    /// Open the file for writing: the real file if it's selected (or already
    /// on disk), otherwise a scratch file for slivers.
    fn open_for_write(&self, file_id: usize, slot: &Slot, backing: &mut Backing) -> Result<()> {
        let real = self.entry.is_wanted(file_id) || slot.real.exists();
        match backing {
            Backing::Real(_) => return Ok(()),
            Backing::Scratch(_) if !real => return Ok(()),
            // Selected while it held slivers: those are dropped, their pieces
            // fail verification and are downloaded again into the real file.
            _ => {}
        }
        *backing = if real {
            create_parent(&slot.real)?;
            slot.pending.lock().unwrap().clear();
            Backing::Real(open_rw(&slot.real, true)?)
        } else {
            std::fs::create_dir_all(&self.layout.output_folder)?;
            Backing::Scratch(
                tempfile::tempfile_in(&self.layout.output_folder).context("creating a temporary file")?,
            )
        };
        Ok(())
    }
}

impl TorrentStorage for SelectiveStorage {
    fn init(&mut self, _shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> Result<()> {
        let output_folder = self.entry.output_folder.clone();
        let piece_length = u64::from(metadata.info.info().piece_length);
        let slots = metadata
            .file_infos
            .iter()
            .enumerate()
            .map(|(i, fi)| {
                let real = output_folder.join(&fi.relative_filename);
                if !fi.attrs.padding {
                    let legacy = output_folder.join(LEGACY_PARTS_DIR).join(&fi.relative_filename);
                    tidy(&real, &legacy, &output_folder, self.entry.is_wanted(i), fi.len, piece_length);
                }
                Slot {
                    real,
                    offset: fi.offset_in_torrent,
                    len: fi.len,
                    padding: fi.attrs.padding,
                    backing: RwLock::new(Backing::Closed),
                    pending: Mutex::new(BTreeSet::new()),
                }
            })
            .collect();
        self.layout = Arc::new(Layout { output_folder, piece_length, slots });
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> Result<()> {
        let slot = self.slot(file_id)?;
        if slot.padding {
            buf.fill(0);
            return Ok(());
        }
        {
            let backing = slot.backing.read().unwrap();
            if let Backing::Real(f) | Backing::Scratch(f) = &*backing {
                return Ok(read_exact_at(f, buf, offset)?);
            }
        }
        if slot.real.exists() {
            let mut backing = slot.backing.write().unwrap();
            if matches!(*backing, Backing::Closed) {
                *backing = Backing::Real(open_rw(&slot.real, false)?);
            }
            if let Backing::Real(f) | Backing::Scratch(f) = &*backing {
                return Ok(read_exact_at(f, buf, offset)?);
            }
        }
        // A deleted sliver: see the module docs for why this reads as zeros.
        if !self.entry.is_wanted(file_id) && self.pieces(slot, offset, buf.len()).all(|p| self.piece_is_selected(p)) {
            buf.fill(0);
            return Ok(());
        }
        bail!("{} hasn't been downloaded", slot.real.display())
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> Result<()> {
        let slot = self.slot(file_id)?;
        if slot.padding {
            return Ok(());
        }
        let wanted = self.entry.is_wanted(file_id);
        {
            let backing = slot.backing.read().unwrap();
            match &*backing {
                Backing::Real(f) => return Ok(write_all_at(f, buf, offset)?),
                Backing::Scratch(f) if !wanted => {
                    self.note_pending(slot, offset, buf.len());
                    return Ok(write_all_at(f, buf, offset)?);
                }
                _ => {}
            }
        }
        let mut backing = slot.backing.write().unwrap();
        self.open_for_write(file_id, slot, &mut backing)?;
        match &*backing {
            Backing::Real(f) => Ok(write_all_at(f, buf, offset)?),
            Backing::Scratch(f) => {
                self.note_pending(slot, offset, buf.len());
                Ok(write_all_at(f, buf, offset)?)
            }
            Backing::Closed => bail!("bug: file not open after opening"),
        }
    }

    /// A piece passed its hash check: slivers that were only kept for it are
    /// deleted (the scratch file is closed, which frees it).
    fn on_piece_completed(&self, piece: ValidPieceIndex) -> Result<()> {
        if self.taken.load(Ordering::Relaxed) {
            return Ok(());
        }
        for slot in &self.layout.slots {
            let done = {
                let mut pending = slot.pending.lock().unwrap();
                pending.remove(&piece.get()) && pending.is_empty()
            };
            if done {
                let mut backing = slot.backing.write().unwrap();
                if matches!(*backing, Backing::Scratch(_)) {
                    *backing = Backing::Closed;
                }
            }
        }
        Ok(())
    }

    fn remove_file(&self, file_id: usize, _filename: &Path) -> Result<()> {
        let slot = self.slot(file_id)?;
        *slot.backing.write().unwrap() = Backing::Closed;
        match std::fs::remove_file(&slot.real) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("removing {}", slot.real.display()));
            }
            _ => {}
        }
        remove_empty_dirs(&slot.real, &self.layout.output_folder);
        Ok(())
    }

    fn remove_directory_if_empty(&self, path: &Path) -> Result<()> {
        let dir = self.entry.output_folder.join(path);
        if dir.is_dir() && std::fs::read_dir(&dir)?.next().is_none() {
            std::fs::remove_dir(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }

    /// librqbit only calls this for selected files, to set their final size.
    fn ensure_file_length(&self, file_id: usize, length: u64) -> Result<()> {
        let slot = self.slot(file_id)?;
        if slot.padding {
            return Ok(());
        }
        let mut backing = slot.backing.write().unwrap();
        self.open_for_write(file_id, slot, &mut backing)?;
        match &*backing {
            Backing::Real(f) => Ok(f.set_len(length)?),
            _ => Ok(()),
        }
    }

    fn take(&self) -> Result<Box<dyn TorrentStorage>> {
        self.taken.store(true, Ordering::Relaxed);
        Ok(Box::new(SelectiveStorage {
            entry: self.entry.clone(),
            layout: self.layout.clone(),
            taken: AtomicBool::new(false),
        }))
    }
}

/// Clean up what earlier versions left for one file:
/// - slivers this app used to keep in a hidden folder: moved into place if
///   the file is selected now (the data is valid), deleted otherwise;
/// - for an unselected file, an empty placeholder, or a big file holding at
///   most two pieces of data (a sliver left by librqbit's own storage):
///   deleted. Anything else, such as an episode downloaded before it was
///   deselected or a small subtitle file, is left alone.
fn tidy(real: &Path, legacy_part: &Path, root: &Path, wanted: bool, file_len: u64, piece_length: u64) {
    let result = (|| -> Result<()> {
        if legacy_part.exists() {
            if wanted && !real.exists() {
                create_parent(real)?;
                std::fs::rename(legacy_part, real)?;
            } else {
                std::fs::remove_file(legacy_part)?;
            }
            remove_empty_dirs(legacy_part, root);
        }
        if wanted {
            return Ok(());
        }
        let Ok(meta) = std::fs::metadata(real) else { return Ok(()) };
        let sliver_only = file_len > 2 * piece_length && bytes_on_disk(&meta) <= 2 * piece_length;
        if meta.is_file() && (meta.len() == 0 || sliver_only) {
            std::fs::remove_file(real)?;
            remove_empty_dirs(real, root);
        }
        Ok(())
    })();
    if let Err(e) = result {
        tracing::warn!("could not tidy {}: {e:#}", real.display());
    }
}

#[cfg(unix)]
fn bytes_on_disk(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn bytes_on_disk(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}

fn open_rw(path: &Path, create: bool) -> Result<File> {
    OpenOptions::new()
        .create(create)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    Ok(())
}

/// Remove now-empty directories above `path`, up to but never including
/// `root` (the torrent's output folder) or anything outside it.
fn remove_empty_dirs(path: &Path, root: &Path) {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(d).is_err() {
            break; // reached the output folder, or not empty
        }
        dir = d.parent();
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("seadex-storage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn tidies_leftovers_but_keeps_real_downloads() {
        use std::io::{Seek, SeekFrom, Write};
        let dir = scratch("tidy");
        let piece = 1024 * 1024;
        let tidy_unselected = |name: &str, len: u64| {
            tidy(&dir.join(name), &dir.join(LEGACY_PARTS_DIR).join(name), &dir, false, len, piece)
        };

        // An empty placeholder: deleted.
        File::create(dir.join("empty.mkv")).unwrap();
        tidy_unselected("empty.mkv", 100 * piece);
        assert!(!dir.join("empty.mkv").exists());

        // Sparse, full length, a sliver of data at the end: deleted.
        let mut f = File::create(dir.join("before.mkv")).unwrap();
        f.seek(SeekFrom::Start(100 * piece - 4096)).unwrap();
        f.write_all(&[7u8; 4096]).unwrap();
        drop(f);
        tidy_unselected("before.mkv", 100 * piece);
        assert!(!dir.join("before.mkv").exists());

        // A sliver at the start of the next episode: deleted.
        std::fs::write(dir.join("after.mkv"), vec![3u8; 4096]).unwrap();
        tidy_unselected("after.mkv", 100 * piece);
        assert!(!dir.join("after.mkv").exists());

        // A file that was really downloaded: kept.
        std::fs::write(dir.join("watched.mkv"), vec![1u8; (3 * piece) as usize]).unwrap();
        tidy_unselected("watched.mkv", 3 * piece);
        assert!(dir.join("watched.mkv").exists());

        // A small file (e.g. subtitles) is never mistaken for a sliver.
        std::fs::write(dir.join("subs.ass"), b"[Script Info]").unwrap();
        tidy_unselected("subs.ass", 13);
        assert!(dir.join("subs.ass").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_parts_are_moved_if_selected_and_deleted_otherwise() {
        let dir = scratch("legacy");
        let parts = dir.join(LEGACY_PARTS_DIR);
        std::fs::create_dir_all(parts.join("Show")).unwrap();
        std::fs::write(parts.join("Show/E01.mkv"), b"wanted").unwrap();
        std::fs::write(parts.join("Show/E02.mkv"), b"unwanted").unwrap();

        tidy(&dir.join("Show/E01.mkv"), &parts.join("Show/E01.mkv"), &dir, true, 6, 1024);
        tidy(&dir.join("Show/E02.mkv"), &parts.join("Show/E02.mkv"), &dir, false, 8, 1024);
        assert_eq!(std::fs::read(dir.join("Show/E01.mkv")).unwrap(), b"wanted");
        assert!(!dir.join("Show/E02.mkv").exists());
        assert!(!parts.exists(), "the hidden folder is gone once empty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_stops_at_the_output_folder() {
        let parent = scratch("root");
        let root = parent.join("Show");
        std::fs::create_dir_all(root.join("Extras")).unwrap();
        remove_empty_dirs(&root.join("Extras/NCOP.mkv"), &root);
        assert!(!root.join("Extras").exists());
        assert!(root.exists(), "the output folder itself is kept even when empty");
        assert!(parent.exists());
        let _ = std::fs::remove_dir_all(&parent);
    }
}
