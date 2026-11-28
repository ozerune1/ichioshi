//! Your anime library: the series you've added, each with its own folder
//! under the library folder, the downloads that belong to it, its language
//! preferences, and its AniList details. Saved as JSON in the app's state
//! folder.
//!
//! The first version of this file only remembered which AniList anime each
//! torrent belonged to (by info hash); that's read and turned into series.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::engine::sanitize_folder_name;
use crate::prefs::LangPrefs;

/// Which anime something belongs to (names remuxes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimeRef {
    pub anilist_id: u64,
    pub english: Option<String>,
    pub romaji: Option<String>,
}

impl AnimeRef {
    /// The English title, or the romaji one if AniList has no English title.
    pub fn title(&self) -> Option<&str> {
        self.english.as_deref().or(self.romaji.as_deref())
    }
}

/// AniList's details about a series.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Details {
    /// The anime on MyAnimeList.
    pub id_mal: Option<u64>,
    pub native: Option<String>,
    pub synonyms: Vec<String>,
    /// Plain text.
    pub description: Option<String>,
    pub genres: Vec<String>,
    /// Out of 100.
    pub score: Option<u32>,
    /// FINISHED, RELEASING, NOT_YET_RELEASED, CANCELLED, HIATUS.
    pub status: Option<String>,
    pub format: Option<String>,
    pub episodes: Option<u32>,
    /// Minutes per episode.
    pub duration: Option<u32>,
    pub season: Option<String>,
    pub year: Option<i32>,
    pub source: Option<String>,
    pub studios: Vec<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    /// The next episode and when it airs (Unix time).
    pub next_episode: Option<(u32, i64)>,
    pub cover: Option<String>,
    pub banner: Option<String>,
    /// Episode titles and air times, where known.
    pub episode_list: Vec<EpisodeInfo>,
    /// When these were fetched (Unix time).
    pub fetched: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EpisodeInfo {
    pub number: u32,
    pub title: Option<String>,
    /// When it aired (or airs), Unix time.
    pub airs: Option<i64>,
}

impl Details {
    pub fn episode(&self, number: u32) -> Option<&EpisodeInfo> {
        self.episode_list.iter().find(|e| e.number == number)
    }

    /// Episodes out so far: all of them once it's finished.
    pub fn aired(&self) -> Option<u32> {
        match (self.status.as_deref(), self.next_episode) {
            (_, Some((next, _))) => Some(next.saturating_sub(1)),
            (Some("NOT_YET_RELEASED"), _) => Some(0),
            _ => self.episodes,
        }
    }
}

/// A file whose default tracks have been set: with which preferences, and
/// the file's size and modification time then (a changed file is redone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub key: String,
    pub len: u64,
    pub modified: i64,
    /// Why it couldn't be done (not Matroska, say); not retried until the file changes.
    #[serde(default)]
    pub error: Option<String>,
    /// What the first edit in place replaced, so the file can be read back
    /// as it came (`crate::party_seed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Original>,
}

/// Bytes a file had at `at`, before it was edited there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Original {
    pub at: u64,
    /// Base64.
    pub bytes: String,
}

impl Original {
    pub fn new(at: u64, bytes: &[u8]) -> Self {
        use base64::Engine as _;
        Self {
            at,
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    pub fn decoded(&self) -> Option<crate::mkv::Replaced> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.bytes)
            .ok()?;
        Some((self.at, bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub anilist_id: u64,
    pub english: Option<String>,
    pub romaji: Option<String>,
    /// Its folder in the library.
    pub folder: PathBuf,
    /// When it was added (Unix time).
    #[serde(default)]
    pub added: i64,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub episodes: Option<u32>,
    /// Its own language preferences (else the library's).
    #[serde(default)]
    pub prefs: Option<LangPrefs>,
    /// Torrents downloaded for it, by info hash.
    #[serde(default)]
    pub downloads: BTreeSet<String>,
    #[serde(default)]
    pub details: Option<Details>,
    /// Files whose default tracks are set, by path within the folder.
    #[serde(default)]
    pub applied: BTreeMap<String, Applied>,
    /// How far each episode has been watched, by episode number.
    #[serde(default)]
    pub watched: BTreeMap<u32, Watch>,
    /// Where it is on your servers (when the library is there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Remote>,
    /// Each episode file's release, by path within the folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub releases: BTreeMap<String, Described>,
    /// Copies of episodes on the servers kept here, to watch offline.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub offline: BTreeMap<u32, PathBuf>,
    /// An 18+ series: always in a folder here, even when the library is
    /// on your servers (it's never sent to Sonarr or Jellyfin).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adult: bool,
}

/// Who released an episode's file, and how: its group, BD or WEB, dual
/// audio, censored or not (`None`: not known).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct EpisodeRelease {
    pub group: Option<String>,
    /// "BD" or "WEB".
    pub source: Option<String>,
    pub dual_audio: Option<bool>,
    pub censored: Option<bool>,
}

impl EpisodeRelease {
    /// What a release's names say (the file's, then its folders'), and its
    /// audio languages if they're known (ISO 639-2).
    pub fn from_names(names: &[&str], audio: Option<&[String]>) -> Self {
        let parsed: Vec<crate::title::ParsedTitle> =
            names.iter().map(|n| crate::title::parse(n)).collect();
        let source = parsed
            .iter()
            .map(|p| p.source.label())
            .find(|l| !l.is_empty())
            .map(str::to_owned);
        let named_dual = parsed.iter().any(|p| p.dual_audio || p.multi_audio);
        let dual_audio = match audio {
            // Japanese and something else (an English dub, say).
            Some(langs) if !langs.is_empty() => {
                let mut distinct: Vec<&String> = langs.iter().filter(|l| *l != "und").collect();
                distinct.sort();
                distinct.dedup();
                Some(distinct.len() >= 2 && distinct.iter().any(|l| *l == "jpn") || named_dual)
            }
            _ => named_dual.then_some(true),
        };
        Self {
            group: parsed.iter().find_map(|p| p.group.clone()),
            source,
            dual_audio,
            censored: names.iter().find_map(|n| crate::title::censored(n)),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// An episode file's release, as read (redone when the file changes size).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Described {
    pub len: u64,
    pub release: EpisodeRelease,
}

/// What a file on disk is: from its name, its folders' names (within the
/// series' folder) and its audio tracks.
pub fn describe_file(path: &Path, rel: &str) -> EpisodeRelease {
    let mut names: Vec<&str> = rel.rsplit('/').collect();
    // File name first, without its extension.
    if let Some(first) = names.first_mut() {
        *first = first.rsplit_once('.').map_or(*first, |(stem, _)| stem);
    }
    let audio: Option<Vec<String>> = crate::mkv::probe(path).ok().map(|info| {
        info.tracks
            .iter()
            .filter(|t| t.kind == crate::mkv::TrackKind::Audio)
            .map(crate::prefs::track_language)
            .collect()
    });
    EpisodeRelease::from_names(&names, audio.as_deref())
}

/// A series on your servers: Sonarr's series (one season of it) or
/// Radarr's film, and Jellyfin's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Remote {
    pub arr: ArrRef,
    /// Its folder on the server.
    pub path: String,
    /// Its page's name in Sonarr or Radarr.
    pub slug: String,
    pub monitored: bool,
    /// Jellyfin's series or film.
    pub jellyfin: Option<String>,
    /// Its episodes, by AniList's numbering (a film is episode 1).
    pub episodes: BTreeMap<u32, RemoteEpisode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArrRef {
    /// The series in Sonarr; which TVDB season this anime is, and how many
    /// of the season's episodes come before its first.
    Sonarr {
        id: u64,
        tvdb: u64,
        season: u32,
        offset: u32,
    },
    Radarr {
        id: u64,
        tmdb: u64,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteEpisode {
    /// Sonarr's episode.
    pub arr_id: Option<u64>,
    pub monitored: bool,
    /// Its file: name and size.
    pub file: Option<(String, u64)>,
    /// The file in Sonarr or Radarr.
    #[serde(default)]
    pub file_id: Option<u64>,
    /// Its release (as Sonarr or Radarr has it).
    #[serde(default)]
    pub release: Option<EpisodeRelease>,
    /// Jellyfin's episode (or film).
    pub jellyfin: Option<String>,
}

/// How far an episode has been watched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Watch {
    /// Where it was left (seconds).
    pub position: f64,
    pub duration: f64,
    /// Seen to the end (or into the ending).
    pub done: bool,
    /// When (Unix time).
    pub at: i64,
}

impl Watch {
    /// Where to start again: where it was left, unless that's the very
    /// beginning or the end.
    pub fn resume_at(&self) -> Option<f64> {
        (!self.done
            && self.position > 10.0
            && (self.duration <= 0.0 || self.position < self.duration - 30.0))
            .then_some(self.position)
    }
}

impl Series {
    pub fn title(&self) -> &str {
        self.english
            .as_deref()
            .or(self.romaji.as_deref())
            .unwrap_or("Untitled")
    }

    pub fn anime_ref(&self) -> AnimeRef {
        AnimeRef {
            anilist_id: self.anilist_id,
            english: self.english.clone(),
            romaji: self.romaji.clone(),
        }
    }

    /// A film, or anything else that's one episode.
    pub fn one_episode(&self) -> bool {
        let d = self.details.as_ref();
        self.episodes.or(d.and_then(|d| d.episodes)) == Some(1)
            || self
                .format
                .as_deref()
                .or(d.and_then(|d| d.format.as_deref()))
                == Some("MOVIE")
    }

    /// The video files in its folder (and folders inside it), sorted.
    pub fn video_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_videos(&self.folder, 3, &mut files);
        files.sort();
        files
    }
}

fn collect_videos(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        // Staging folders and partial downloads aren't episodes yet.
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        // Linked episodes count too: follow symlinks.
        let kind = match e.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(&path).map(|m| m.file_type()),
            other => other,
        };
        match kind {
            Ok(t) if t.is_dir() && depth > 0 => collect_videos(&path, depth - 1, out),
            Ok(t) if t.is_file() && crate::remux::is_video(&path) => out.push(path),
            _ => {}
        }
    }
}

/// What to know about an anime to add it.
#[derive(Debug, Clone, Default)]
pub struct NewSeries {
    pub anilist_id: u64,
    pub english: Option<String>,
    pub romaji: Option<String>,
    pub cover: Option<String>,
    pub format: Option<String>,
    pub year: Option<i32>,
    pub episodes: Option<u32>,
    /// An 18+ anime (kept local).
    pub adult: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    series: Vec<Series>,
}

pub struct Library {
    path: PathBuf,
    pub series: Vec<Series>,
}

impl Library {
    /// Load from `path`; series added from the old format get folders
    /// under `root`. A missing or unreadable file gives an empty library.
    pub fn load(path: PathBuf, root: &Path) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let series = if text.trim().is_empty() {
            Vec::new()
        } else if let Ok(saved) = serde_json::from_str::<Saved>(&text) {
            saved.series
        } else if let Ok(old) = serde_json::from_str::<HashMap<String, AnimeRef>>(&text) {
            let mut lib = Self {
                path: path.clone(),
                series: Vec::new(),
            };
            let mut old: Vec<(String, AnimeRef)> = old.into_iter().collect();
            old.sort_by(|a, b| a.0.cmp(&b.0));
            for (hash, anime) in old {
                let new = NewSeries {
                    anilist_id: anime.anilist_id,
                    english: anime.english,
                    romaji: anime.romaji,
                    ..Default::default()
                };
                lib.add(&new, root)
                    .downloads
                    .insert(hash.to_ascii_lowercase());
            }
            lib.save_or_warn();
            lib.series
        } else {
            tracing::warn!("ignoring unreadable {}", path.display());
            Vec::new()
        };
        let mut lib = Self { path, series };
        if crate::fsutil::NAMES_NEED_CARE && lib.fix_folder_names() {
            lib.save_or_warn();
        }
        lib
    }

    /// Series folders named with what this system's file names can't hold
    /// (0.3.9 and before kept "?" and ":", so nothing could be saved in
    /// them on a phone or Windows): named as they're named now, the folder
    /// renamed too if it's there (or its files moved into one already there).
    /// Whether any changed.
    pub fn fix_folder_names(&mut self) -> bool {
        let mut changed = false;
        for s in &mut self.series {
            let Some(name) = s.folder.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let fixed = crate::fsutil::portable_name(name);
            if fixed == name || fixed.is_empty() {
                continue;
            }
            let new = s.folder.with_file_name(&fixed);
            if s.folder.is_dir()
                && let Err(e) = move_folder(&s.folder, &new)
            {
                tracing::warn!(
                    "couldn't rename {} to {}: {e}",
                    s.folder.display(),
                    new.display()
                );
                continue;
            }
            tracing::info!("{} is now {}", s.folder.display(), new.display());
            s.folder = new;
            changed = true;
        }
        changed
    }

    fn save(&self) -> Result<()> {
        crate::fsutil::write_private(
            &self.path,
            &serde_json::to_vec_pretty(&Saved {
                series: self.series.clone(),
            })?,
        )
    }

    pub fn save_or_warn(&self) {
        if let Err(e) = self.save() {
            tracing::warn!("could not save {}: {e:#}", self.path.display());
        }
    }

    /// The anime a torrent belongs to.
    pub fn get(&self, info_hash: &str) -> Option<AnimeRef> {
        self.series_for(info_hash).map(Series::anime_ref)
    }

    pub fn series_for(&self, info_hash: &str) -> Option<&Series> {
        let hash = info_hash.to_ascii_lowercase();
        self.series.iter().find(|s| s.downloads.contains(&hash))
    }

    pub fn find(&self, anilist_id: u64) -> Option<&Series> {
        self.series.iter().find(|s| s.anilist_id == anilist_id)
    }

    pub fn find_mut(&mut self, anilist_id: u64) -> Option<&mut Series> {
        self.series.iter_mut().find(|s| s.anilist_id == anilist_id)
    }

    /// The folder a series has, or would get under `root`.
    pub fn folder_for(&self, new: &NewSeries, root: &Path) -> PathBuf {
        if let Some(s) = self.find(new.anilist_id) {
            return s.folder.clone();
        }
        let title = new
            .english
            .as_deref()
            .or(new.romaji.as_deref())
            .unwrap_or("Untitled");
        let name = sanitize_folder_name(title);
        let taken = |p: &Path| self.series.iter().any(|s| s.folder == p);
        let plain = root.join(&name);
        if !taken(&plain) {
            return plain;
        }
        let with_year = new.year.map(|y| root.join(format!("{name} ({y})")));
        match with_year {
            Some(p) if !taken(&p) => p,
            _ => root.join(format!("{name} [{}]", new.anilist_id)),
        }
    }

    /// Add a series (or return it if it's there), and save.
    pub fn add(&mut self, new: &NewSeries, root: &Path) -> &mut Series {
        if let Some(i) = self
            .series
            .iter()
            .position(|s| s.anilist_id == new.anilist_id)
        {
            // Fill in what wasn't known before.
            let s = &mut self.series[i];
            s.cover = s.cover.take().or(new.cover.clone());
            s.format = s.format.take().or(new.format.clone());
            s.year = s.year.or(new.year);
            s.episodes = s.episodes.or(new.episodes);
            s.adult |= new.adult;
            return &mut self.series[i];
        }
        let folder = self.folder_for(new, root);
        self.series.push(Series {
            anilist_id: new.anilist_id,
            english: new.english.clone(),
            romaji: new.romaji.clone(),
            folder,
            added: chrono::Utc::now().timestamp(),
            cover: new.cover.clone(),
            format: new.format.clone(),
            year: new.year,
            episodes: new.episodes,
            prefs: None,
            downloads: BTreeSet::new(),
            details: None,
            applied: BTreeMap::new(),
            watched: BTreeMap::new(),
            remote: None,
            releases: BTreeMap::new(),
            offline: BTreeMap::new(),
            adult: new.adult,
        });
        self.save_or_warn();
        self.series.last_mut().unwrap()
    }

    /// Remember that these torrents belong to a series (added if needed), and save.
    pub fn record(
        &mut self,
        info_hashes: impl IntoIterator<Item = String>,
        new: &NewSeries,
        root: &Path,
    ) {
        let series = self.add(new, root);
        for hash in info_hashes {
            series.downloads.insert(hash.to_ascii_lowercase());
        }
        self.save_or_warn();
    }

    /// Take a series out of the library (its files are left alone).
    pub fn remove(&mut self, anilist_id: u64) -> Option<Series> {
        let i = self
            .series
            .iter()
            .position(|s| s.anilist_id == anilist_id)?;
        let s = self.series.remove(i);
        self.save_or_warn();
        Some(s)
    }
}

/// Move `files` (paths relative to `from`) to the same places under `to`.
/// A name already taken gets " (2)" and so on; across drives, files are
/// copied and then deleted. Folders left empty are removed, except
/// `protected` ones. Returns what moved where.
pub fn move_files(
    from: &Path,
    to: &Path,
    files: &[String],
    protected: &[PathBuf],
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut moved = Vec::new();
    for rel in files {
        let src = from.join(rel);
        if !src.is_file() {
            continue;
        }
        let dest = free_name(&to.join(rel));
        move_file(&src, &dest).with_context(|| format!("moving {}", src.display()))?;
        moved.push((src, dest));
    }
    // Tidy up the folders the files were in.
    let mut dirs: Vec<PathBuf> = moved
        .iter()
        .filter_map(|(src, _)| src.parent().map(Path::to_owned))
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for dir in dirs {
        let mut d = dir.as_path();
        while d.starts_with(from) && !is_protected(d, protected) && std::fs::remove_dir(d).is_ok() {
            match d.parent() {
                Some(p) => d = p,
                None => break,
            }
        }
    }
    Ok(moved)
}

fn is_protected(dir: &Path, protected: &[PathBuf]) -> bool {
    dir.parent().is_none()
        || dirs::home_dir().is_some_and(|h| h == dir)
        || protected.iter().any(|p| p.starts_with(dir))
}

/// `path`, or "name (2).ext" and so on if that's taken.
fn free_name(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_owned();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let mut part = to.as_os_str().to_owned();
            part.push(".moving");
            let part = PathBuf::from(part);
            std::fs::copy(from, &part)?;
            std::fs::rename(&part, to)?;
            std::fs::remove_file(from)
        }
        other => other,
    }
}

/// `from` renamed `to`; if `to` is there already, `from`'s files moved into
/// it (those it hasn't got) and `from` removed if that empties it.
fn move_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    if !to.exists() {
        return std::fs::rename(from, to);
    }
    for entry in std::fs::read_dir(from)?.flatten() {
        let dest = to.join(entry.file_name());
        if !dest.exists() {
            std::fs::rename(entry.path(), dest)?;
        }
    }
    let _ = std::fs::remove_dir(from);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_folders_named_as_phones_cant() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut lib = Library {
            path: root.join("library.json"),
            series: Vec::new(),
        };
        let new = |id, t: &str| NewSeries {
            anilist_id: id,
            romaji: Some(t.into()),
            ..Default::default()
        };
        // As 0.3.9 named them ("?" and ":" kept; Linux allows them).
        lib.add(&new(1, "Who Came Home!?"), root).folder = root.join("Who Came Home!?");
        lib.add(&new(2, "Sample: The Show"), root).folder = root.join("Sample: The Show");
        lib.add(&new(3, "Plain"), root).folder = root.join("Plain");
        std::fs::create_dir_all(root.join("Who Came Home!?")).unwrap();
        std::fs::write(root.join("Who Came Home!?/ep 01.mkv"), b"1").unwrap();
        // The other's new name is taken already: its files go in there.
        std::fs::create_dir_all(root.join("Sample: The Show")).unwrap();
        std::fs::write(root.join("Sample: The Show/ep 02.mkv"), b"2").unwrap();
        std::fs::create_dir_all(root.join("Sample - The Show")).unwrap();
        std::fs::write(root.join("Sample - The Show/ep 01.mkv"), b"1").unwrap();

        assert!(lib.fix_folder_names());
        assert_eq!(lib.find(1).unwrap().folder, root.join("Who Came Home!_"));
        assert!(root.join("Who Came Home!_/ep 01.mkv").exists());
        assert!(!root.join("Who Came Home!?").exists());
        assert_eq!(lib.find(2).unwrap().folder, root.join("Sample - The Show"));
        assert!(root.join("Sample - The Show/ep 02.mkv").exists());
        assert!(
            !root.join("Sample: The Show").exists(),
            "emptied, so removed"
        );
        assert_eq!(lib.find(3).unwrap().folder, root.join("Plain"));
        assert!(!lib.fix_folder_names(), "nothing more to do");
    }

    /// What names and audio tracks say about a release.
    #[test]
    fn reads_releases() {
        let langs = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = EpisodeRelease::from_names(&["[SubsPlease] Frieren - 01 (1080p) [ABCD1234]"], None);
        assert_eq!(r.group.as_deref(), Some("SubsPlease"));
        assert_eq!(
            (r.source.as_deref(), r.dual_audio, r.censored),
            (None, None, None)
        );
        // The folder says what the file doesn't; the tracks say dual audio.
        let r = EpisodeRelease::from_names(
            &["Show - 01", "[Group] Show S01 (BD 1080p HEVC) [Uncensored]"],
            Some(&langs(&["jpn", "eng"])),
        );
        assert_eq!(
            r,
            EpisodeRelease {
                group: Some("Group".into()),
                source: Some("BD".into()),
                dual_audio: Some(true),
                censored: Some(false),
            }
        );
        let r = EpisodeRelease::from_names(
            &["Show.S01E01.1080p.CR.WEB-DL.AAC2.0.H.264-VARYG (Censored)"],
            Some(&langs(&["jpn"])),
        );
        assert_eq!(r.group.as_deref(), Some("VARYG"));
        assert_eq!(r.source.as_deref(), Some("WEB"));
        assert_eq!(r.dual_audio, Some(false), "Japanese only");
        assert_eq!(r.censored, Some(true));
        assert_eq!(
            EpisodeRelease::from_names(&["Show - 01"], Some(&langs(&["jpn", "und"]))).dual_audio,
            Some(false),
            "an unlabelled track isn't a second language"
        );
    }

    fn new(id: u64, english: &str, year: i32) -> NewSeries {
        NewSeries {
            anilist_id: id,
            english: Some(english.into()),
            year: Some(year),
            ..Default::default()
        }
    }

    #[test]
    fn adds_series_with_their_own_folders_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Anime");
        let path = dir.path().join("library.json");
        let mut lib = Library::load(path.clone(), &root);
        assert!(lib.series.is_empty() && lib.get("ABC").is_none());

        lib.record(
            ["ABCdef".to_owned()],
            &new(1, "Frieren: Beyond Journey's End", 2023),
            &root,
        );
        assert_eq!(
            lib.find(1).unwrap().folder,
            root.join("Frieren: Beyond Journey's End")
        );
        // Same title, another anime: the year tells them apart, then the ID.
        lib.add(&new(2, "Frieren: Beyond Journey's End", 2026), &root);
        lib.add(&new(3, "Frieren: Beyond Journey's End", 2026), &root);
        assert_eq!(
            lib.find(2).unwrap().folder,
            root.join("Frieren: Beyond Journey's End (2026)")
        );
        assert_eq!(
            lib.find(3).unwrap().folder,
            root.join("Frieren: Beyond Journey's End [3]")
        );
        // Adding again changes nothing but what was missing.
        lib.add(
            &NewSeries {
                cover: Some("c.png".into()),
                ..new(1, "Other name", 2000)
            },
            &root,
        );
        assert_eq!(lib.series.len(), 3);
        assert_eq!(lib.find(1).unwrap().cover.as_deref(), Some("c.png"));

        let lib = Library::load(path, &root);
        assert_eq!(lib.get("abcDEF").unwrap().anilist_id, 1);
        assert_eq!(
            lib.series_for("ABCDEF").unwrap().title(),
            "Frieren: Beyond Journey's End"
        );
        assert_eq!(lib.series.len(), 3);
    }

    #[test]
    fn turns_the_old_torrent_map_into_series() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Anime");
        let path = dir.path().join("library.json");
        let a = AnimeRef {
            anilist_id: 7,
            english: None,
            romaji: Some("Sousou no Frieren".into()),
        };
        let old: HashMap<&str, &AnimeRef> = [("AAA", &a), ("bbb", &a)].into();
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let lib = Library::load(path.clone(), &root);
        assert_eq!(lib.series.len(), 1);
        let s = &lib.series[0];
        assert_eq!(
            (s.anilist_id, s.title(), s.folder.clone()),
            (7, "Sousou no Frieren", root.join("Sousou no Frieren"))
        );
        assert_eq!(s.downloads, ["aaa".to_owned(), "bbb".to_owned()].into());
        // Saved in the new format.
        assert_eq!(Library::load(path, &root).series, lib.series);
    }

    #[test]
    fn moves_files_and_tidies_up_after() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("Downloads");
        let from = downloads.join("[Group] Show S01");
        let to = dir.path().join("Anime").join("Show");
        std::fs::create_dir_all(from.join("Extras")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        for f in ["Show - 01.mkv", "Show - 02.mkv", "Extras/NCOP.mkv"] {
            std::fs::write(from.join(f), f).unwrap();
        }
        std::fs::write(to.join("Show - 02.mkv"), "already here").unwrap();
        let files: Vec<String> = [
            "Show - 01.mkv",
            "Show - 02.mkv",
            "Extras/NCOP.mkv",
            "gone.mkv",
        ]
        .map(String::from)
        .to_vec();
        let moved = move_files(&from, &to, &files, std::slice::from_ref(&downloads)).unwrap();
        assert_eq!(moved.len(), 3);
        assert_eq!(
            std::fs::read_to_string(to.join("Show - 01.mkv")).unwrap(),
            "Show - 01.mkv"
        );
        assert_eq!(
            std::fs::read_to_string(to.join("Show - 02 (2).mkv")).unwrap(),
            "Show - 02.mkv",
            "the one there is kept"
        );
        assert_eq!(
            std::fs::read_to_string(to.join("Show - 02.mkv")).unwrap(),
            "already here"
        );
        assert!(to.join("Extras/NCOP.mkv").exists());
        assert!(!from.exists(), "emptied folders go");
        assert!(downloads.exists(), "Downloads stays, even empty");
    }

    #[test]
    fn finds_episodes_but_not_partial_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::load(dir.path().join("l.json"), dir.path());
        let s = lib.add(&new(1, "Show", 2026), dir.path()).clone();
        std::fs::create_dir_all(s.folder.join("Extras")).unwrap();
        std::fs::create_dir_all(s.folder.join(".usenet-3")).unwrap();
        for f in [
            "Show - 01.mkv",
            "Show - 02.mkv",
            "Extras/NCOP.mkv",
            "notes.txt",
            ".usenet-3/Show - 03.mkv",
        ] {
            std::fs::write(s.folder.join(f), b"x").unwrap();
        }
        let names: Vec<String> = s
            .video_files()
            .iter()
            .map(|p| p.strip_prefix(&s.folder).unwrap().display().to_string())
            .collect();
        assert_eq!(names, ["Extras/NCOP.mkv", "Show - 01.mkv", "Show - 02.mkv"]);
    }
}
