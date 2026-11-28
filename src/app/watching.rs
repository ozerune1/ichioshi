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
    let files: Vec<(PathBuf, String, u64)> = series
        .video_files()
        .into_iter()
        .filter(|p| !busy.contains(p))
        .map(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            (p, name, len)
        })
        .collect();
    let named: Vec<(&str, u64)> = files.iter().map(|(_, n, l)| (n.as_str(), *l)).collect();
    let eps = crate::title::episodes_of_files(&named, series.one_episode());
    for ((path, _, _), ep) in files.into_iter().zip(eps) {
        if let Some(ep) = ep {
            out.entry(ep).or_insert(path);
        }
    }
    out
}

/// The episode to watch next (of those on disk, in order): the first after
/// the last one seen, or the first of all once everything's been seen.
/// What's been seen is what your list says (`listed`, its episodes
/// watched) when the anime's on one, else what was watched here.
pub(super) fn up_next(
    series: &Series,
    mut on_disk: impl Iterator<Item = u32> + Clone,
    listed: Option<u32>,
) -> Option<u32> {
    if let Some(progress) = listed {
        return on_disk
            .clone()
            .find(|e| *e > progress)
            .or_else(|| on_disk.next());
    }
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

impl super::App {
    /// How far an episode was watched (`done`: to the end, near enough):
    /// kept in the library, and told to Jellyfin and your lists.
    pub(super) fn save_watch(
        &mut self,
        id: u64,
        episode: u32,
        position: f64,
        duration: f64,
        done: bool,
    ) {
        let Some(series) = self.library.find_mut(id) else {
            return;
        };
        let before = series.watched.get(&episode).is_some_and(|w| w.done);
        series.watched.insert(
            episode,
            crate::library::Watch {
                position,
                duration,
                done: done || before,
                at: chrono::Utc::now().timestamp(),
            },
        );
        self.library.save_or_warn();
        if self.on_servers(id) {
            self.report_watched(id, episode, position, done || before);
        }
        if done && !before {
            self.sync_watched(id, episode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Watch;

    /// A series with `done` seen to the end here.
    fn seen(done: &[u32]) -> Series {
        let mut s: Series =
            serde_json::from_value(serde_json::json!({"anilist_id": 1, "folder": "/x"})).unwrap();
        for &n in done {
            s.watched.insert(
                n,
                Watch {
                    position: 1400.0,
                    duration: 1420.0,
                    done: true,
                    at: 0,
                },
            );
        }
        s
    }

    /// What plays next: after the last episode seen here, or, when the
    /// anime's on one of your lists, after its episodes watched there.
    #[test]
    fn up_next_follows_your_list() {
        let disk = || [1, 2, 3, 4, 5].into_iter();
        // Seen here: after the last one; the first again once all are.
        assert_eq!(up_next(&seen(&[]), disk(), None), Some(1));
        assert_eq!(up_next(&seen(&[1, 2]), disk(), None), Some(3));
        assert_eq!(up_next(&seen(&[1, 2, 3, 4, 5]), disk(), None), Some(1));
        // The list says 3 (watched somewhere else): the 4th, though none
        // were seen here.
        assert_eq!(up_next(&seen(&[]), disk(), Some(3)), Some(4));
        // Rewatching: all seen here before, the list back at 2.
        assert_eq!(up_next(&seen(&[1, 2, 3, 4, 5]), disk(), Some(2)), Some(3));
        // The list says none yet.
        assert_eq!(up_next(&seen(&[1, 2]), disk(), Some(0)), Some(1));
        // Watched to the last on disk: the first again.
        assert_eq!(up_next(&seen(&[]), disk(), Some(5)), Some(1));
        // The next one there is, past a gap.
        assert_eq!(up_next(&seen(&[]), [1, 2, 6].into_iter(), Some(3)), Some(6));
        assert_eq!(up_next(&seen(&[]), std::iter::empty(), Some(3)), None);
    }
}
