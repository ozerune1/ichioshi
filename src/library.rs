//! Which AniList anime each download belongs to, by torrent info hash.
//! Remuxes are named after it. Saved as JSON in the app's state folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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

pub struct Library {
    path: PathBuf,
    by_info_hash: HashMap<String, AnimeRef>,
}

impl Library {
    /// Load from `path`; a missing or unreadable file gives an empty library.
    pub fn load(path: PathBuf) -> Self {
        let by_info_hash = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| match serde_json::from_str(&text) {
                Ok(map) => Some(map),
                Err(e) => {
                    tracing::warn!("ignoring unreadable {}: {e}", path.display());
                    None
                }
            })
            .unwrap_or_default();
        Self { path, by_info_hash }
    }

    pub fn get(&self, info_hash: &str) -> Option<&AnimeRef> {
        self.by_info_hash.get(&info_hash.to_ascii_lowercase())
    }

    /// Remember that these torrents belong to `anime`, and save.
    pub fn record(&mut self, info_hashes: impl IntoIterator<Item = String>, anime: AnimeRef) {
        for hash in info_hashes {
            self.by_info_hash.insert(hash.to_ascii_lowercase(), anime.clone());
        }
        if let Err(e) = self.save() {
            tracing::warn!("could not save {}: {e:#}", self.path.display());
        }
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = tmp_path(&self.path);
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.by_info_hash)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_hash_case() {
        let dir = std::env::temp_dir().join(format!("seadex-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("library.json");
        let anime = AnimeRef { anilist_id: 1, english: None, romaji: Some("Sousou no Frieren".into()) };

        let mut lib = Library::load(path.clone());
        assert!(lib.get("ABC").is_none());
        lib.record(["ABCdef".to_owned()], anime.clone());
        let lib = Library::load(path);
        assert_eq!(lib.get("abcDEF"), Some(&anime));
        assert_eq!(lib.get("abcdef").unwrap().title(), Some("Sousou no Frieren"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
