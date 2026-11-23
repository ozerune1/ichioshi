//! Covers and banners: fetched once, kept on disk, decoded by iced.
//!
//! Pages ask for an image by URL while they're drawn; what isn't here yet
//! is fetched after the next update.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use iced::Task;
use iced::widget::image::Handle;

use super::Message;

/// An image, and its size in pixels (from its header).
pub(super) type Picture = (Handle, Option<(u32, u32)>);

/// What `bytes` is, ready to draw.
pub(super) fn picture(bytes: Vec<u8>) -> Picture {
    let size = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    (Handle::from_bytes(bytes), size)
}

pub(super) struct Images {
    dir: PathBuf,
    ready: HashMap<String, Picture>,
    failed: HashSet<String>,
    loading: HashSet<String>,
    /// Asked for while drawing, not fetched yet.
    wanted: RefCell<HashSet<String>>,
}

impl Images {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            ready: HashMap::new(),
            failed: HashSet::new(),
            loading: HashSet::new(),
            wanted: RefCell::new(HashSet::new()),
        }
    }

    /// The image, if it's here; otherwise it's fetched soon.
    pub(super) fn get(&self, url: Option<&str>) -> Option<Handle> {
        let url = url.filter(|u| !u.is_empty())?;
        if let Some((h, _)) = self.ready.get(url) {
            return Some(h.clone());
        }
        if !self.failed.contains(url) && !self.loading.contains(url) {
            self.wanted.borrow_mut().insert(url.to_owned());
        }
        None
    }

    /// It couldn't be fetched (or read).
    pub(super) fn failed(&self, url: &str) -> bool {
        self.failed.contains(url)
    }

    /// Fetch what pages asked for.
    pub(super) fn fetch(&mut self, http: &reqwest::Client) -> Task<Message> {
        let wanted: Vec<String> = self.wanted.borrow_mut().drain().collect();
        let tasks = wanted.into_iter().filter_map(|url| {
            if !self.loading.insert(url.clone()) {
                return None;
            }
            let (http, file) = (http.clone(), self.dir.join(file_name(&url)));
            Some(Task::perform(
                {
                    let url = url.clone();
                    async move { load(&http, &url, &file).await }
                },
                move |bytes| Message::Image(url.clone(), bytes.map(picture)),
            ))
        });
        Task::batch(tasks)
    }

    /// What pages asked for, with where each is kept (tests fetch them
    /// themselves: the harness doesn't run tasks).
    #[cfg(test)]
    pub(super) fn take_wanted(&mut self) -> Vec<(String, PathBuf)> {
        let wanted: Vec<String> = self.wanted.borrow_mut().drain().collect();
        wanted
            .into_iter()
            .map(|url| {
                let file = self.dir.join(file_name(&url));
                (url, file)
            })
            .collect()
    }

    /// An image with its size (asked for like `get`): to lay it out at
    /// the right size before it's decoded.
    pub(super) fn get_sized(&self, url: &str) -> Option<Picture> {
        let found = self.ready.get(url).cloned();
        if found.is_none() {
            let _ = self.get(Some(url));
        }
        found
    }

    pub(super) fn loaded(&mut self, url: String, handle: Option<Picture>) {
        self.loading.remove(&url);
        match handle {
            Some(h) => {
                self.ready.insert(url, h);
            }
            None => {
                self.failed.insert(url);
            }
        }
    }
}

/// From the disk copy, else the web (and kept).
pub(super) async fn load(http: &reqwest::Client, url: &str, file: &Path) -> Option<Vec<u8>> {
    if let Ok(bytes) = tokio::fs::read(file).await {
        return Some(bytes);
    }
    let bytes = async {
        let resp = http.get(url).send().await?.error_for_status()?;
        resp.bytes().await
    }
    .await
    .map_err(|e| tracing::debug!("couldn't fetch {url}: {e}"))
    .ok()?;
    if let Some(dir) = file.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    let _ = tokio::fs::write(file, &bytes).await;
    Some(bytes.to_vec())
}

/// A stable name for a URL's copy (FNV-1a).
fn file_name(url: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_are_stable() {
        assert_eq!(
            super::file_name("https://a/b.jpg"),
            super::file_name("https://a/b.jpg")
        );
        assert_ne!(
            super::file_name("https://a/b.jpg"),
            super::file_name("https://a/c.jpg")
        );
        assert_eq!(super::file_name("").len(), 16);
    }
}
