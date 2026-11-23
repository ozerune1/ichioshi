//! The player's messages, settings and panels (the same everywhere; the
//! player itself isn't on Android).

use std::path::PathBuf;
use std::sync::Arc;

/// A track to pick (or none, for subtitles).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackChoice {
    pub index: Option<usize>,
    pub label: String,
}

impl std::fmt::Display for TrackChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// A panel over the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    /// Audio and subtitles.
    Tracks,
    Settings,
}

/// The player's settings (switches in its settings panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    SkipOpenings,
    SkipEndings,
    Autoplay,
}

#[derive(Clone, Debug)]
pub enum Msg {
    TogglePause,
    /// A click on the picture: closes a panel, else play/pause.
    Surface,
    Panel(Panel),
    Setting(Setting, bool),
    /// Other languages: for what plays from now on, and this episode.
    Languages(crate::prefs::LangPrefs),
    /// A new frame is up: subtitles for its moment.
    Frame,
    /// The file's subtitles (and fonts), read in the background.
    SubsLoaded(PathBuf, Option<Arc<crate::mkv::TextSubtitles>>),
    /// Subtitles later (positive) or earlier, in milliseconds.
    SubDelay(i64),
    Seek(f64),
    SeekDone,
    SeekBy(f64),
    Volume(f64),
    Mute,
    Audio(TrackChoice),
    Subs(TrackChoice),
    SkipPart,
    Next,
    Prev,
    Fullscreen,
    Ended,
    Moved,
    Back,
}

/// How much of the window's bottom the controls take, in points.
pub const CONTROLS_HEIGHT: f64 = 140.0;

/// How far "Skip 85s" (and the J and L keys) go: an anime opening's usual
/// length.
pub const SKIP: f64 = 85.0;

/// "1:23", "1:02:03".
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
