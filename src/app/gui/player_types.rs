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
    /// Play at this speed (1 is normal).
    Speed(f64),
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

/// The speeds to pick from (1 is normal).
pub const SPEEDS: [f64; 7] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

/// The next speed up (or down) from `speed`, or the same at the ends.
pub fn speed_step(speed: f64, up: bool) -> f64 {
    let found = if up {
        SPEEDS.iter().find(|s| **s > speed + 1e-6)
    } else {
        SPEEDS.iter().rev().find(|s| **s < speed - 1e-6)
    };
    found.copied().unwrap_or(speed)
}

/// "1×", "1.25×", "0.5×".
pub fn speed_label(speed: f64) -> String {
    let s = format!("{speed:.2}");
    format!("{}×", s.trim_end_matches('0').trim_end_matches('.'))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_step_and_read() {
        assert_eq!(speed_step(1.0, true), 1.25);
        assert_eq!(speed_step(1.0, false), 0.75);
        assert_eq!(speed_step(2.0, true), 2.0);
        assert_eq!(speed_step(0.5, false), 0.5);
        // Between the steps (a host's 1.1): the nearest one each way.
        assert_eq!(speed_step(1.1, true), 1.25);
        assert_eq!(speed_step(1.1, false), 1.0);
        assert_eq!(speed_label(1.0), "1×");
        assert_eq!(speed_label(1.25), "1.25×");
        assert_eq!(speed_label(0.5), "0.5×");
        assert_eq!(speed_label(2.0), "2×");
    }
}
