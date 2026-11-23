//! Watching a series: its episodes on disk, which one is next, and how a
//! play request looks (the GUI's player plays it).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::library::Series;

#[derive(Debug, Clone)]
pub(super) enum PlayerAction {
    Play { id: u64, episode: u32 },
}

/// "1:23", "1:02:03".
pub(super) fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A series' episodes on disk, by number (files still downloading left out).
pub(super) fn episode_files(series: &Series, busy: &HashSet<PathBuf>) -> BTreeMap<u32, PathBuf> {
    let mut out = BTreeMap::new();
    for path in series.video_files() {
        if busy.contains(&path) {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Some(ep) = crate::title::episode_of_file(&name) {
            out.entry(ep).or_insert(path);
        }
    }
    out
}

/// The episode to watch next (of those on disk, in order): the first after
/// the last one seen, or the first of all once everything's been seen.
pub(super) fn up_next(
    series: &Series,
    mut on_disk: impl Iterator<Item = u32> + Clone,
) -> Option<u32> {
    let last_done = series
        .watched
        .iter()
        .filter(|(_, w)| w.done)
        .map(|(e, _)| *e)
        .max();
    let unseen = |e: &u32| {
        last_done.is_none_or(|d| *e > d) && !series.watched.get(e).is_some_and(|w| w.done)
    };
    on_disk.clone().find(unseen).or_else(|| on_disk.next())
}
