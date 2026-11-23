//! What a video player needs to know about an episode: its tracks and
//! chapters (which parts are the opening and ending), and the player's
//! settings.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// The player's settings, saved with the app's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub skip_openings: bool,
    pub skip_endings: bool,
    /// Go on to the next episode when one ends.
    pub autoplay: bool,
    pub volume: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            skip_openings: false,
            skip_endings: false,
            autoplay: true,
            volume: 100.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Sub,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub kind: TrackKind,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub codec: Option<String>,
    pub channels: Option<i64>,
    pub default: bool,
    pub forced: bool,
    pub external: bool,
    pub selected: bool,
}

impl Track {
    /// "Japanese · FLAC 2.0", "English · Signs & Songs".
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        match self.lang.as_deref() {
            // Players give "es" as often as "spa".
            Some(code) if !code.is_empty() && code != "und" => parts.push(
                crate::remux::language_name(&crate::remux::normalize_language(code)),
            ),
            _ => {}
        }
        if let Some(t) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            // Titles often repeat the language ("English (Full)").
            if parts.first().is_none_or(|l| t.trim() != l) {
                parts.push(t.trim().to_owned());
            }
        }
        if self.kind == TrackKind::Audio || parts.is_empty() {
            let codec = self.codec.as_deref().map(codec_name).unwrap_or_default();
            let ch = match self.channels {
                Some(1) => " 1.0",
                Some(2) => " 2.0",
                Some(6) => " 5.1",
                Some(8) => " 7.1",
                _ => "",
            };
            if !codec.is_empty() {
                parts.push(format!("{codec}{ch}"));
            }
        }
        // Unless the title says so already ("German (Forced)").
        if self.forced && !parts.iter().any(|p| p.to_lowercase().contains("forced")) {
            parts.push("forced".into());
        }
        if self.external {
            parts.push("external".into());
        }
        if parts.is_empty() {
            parts.push(format!("Track {}", self.id));
        }
        parts.join(" · ")
    }
}

fn codec_name(codec: &str) -> String {
    match codec {
        "flac" => "FLAC",
        "aac" => "AAC",
        "opus" => "Opus",
        "ac3" => "AC-3",
        "eac3" => "E-AC-3",
        "truehd" => "TrueHD",
        "dts" => "DTS",
        "mp3" => "MP3",
        "vorbis" => "Vorbis",
        "pcm_s16le" | "pcm_s24le" => "PCM",
        "ass" | "ssa" => "ASS",
        "subrip" => "SRT",
        "hdmv_pgs_subtitle" => "PGS",
        "dvd_subtitle" => "VobSub",
        other => return other.to_ascii_uppercase(),
    }
    .to_owned()
}

/// A part of an episode, by its chapter name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Opening,
    Ending,
    Preview,
    Other,
}

/// "Opening", "OP", "Intro", "OP1"; "Ending", "ED", "Credits"; "Preview".
pub fn part_of(title: &str) -> Part {
    static OP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(op\s*\d*|opening(\s+(song|theme|credits))?|intro)\s*\d*\s*$").unwrap()
    });
    static ED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(ed\s*\d*|ending(\s+(song|theme|credits))?|outro|credits|end\s+credits)\s*\d*\s*$").unwrap()
    });
    static PREVIEW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bpreview\b").unwrap());
    if OP.is_match(title) {
        Part::Opening
    } else if ED.is_match(title) {
        Part::Ending
    } else if PREVIEW.is_match(title) {
        Part::Preview
    } else {
        Part::Other
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub title: String,
    pub start: f64,
    pub part: Part,
}

#[cfg(test)]
pub(crate) mod tests;
