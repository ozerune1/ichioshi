//! Remuxing downloaded episodes (Matroska), built in: see [`crate::mkv`].
//!
//! For a series you pick an audio language and a subtitle language (or no
//! subtitles) from what's in its files, and, when a language has several
//! tracks (e.g. full subtitles vs signs only), which one. Each remux keeps the
//! video, that one audio track and that one subtitle track; every other audio
//! and subtitle track is dropped. Chapters are kept, and so are attached fonts
//! when the subtitles are styled (ASS/SSA) and need them. Files are named, and
//! titled, after the anime's English AniList title: `<title> - 01.mkv`.
//!
//! Originals are never modified: remuxes go to their own folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use regex::Regex;

use crate::{mkv, title};

pub use crate::mkv::SubtitleSample;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "webm", "mov", "wmv", "ts", "m2ts",
];

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(ext)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Video,
    Audio,
    Subtitles,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// The Matroska track number.
    pub id: u64,
    pub kind: Kind,
    /// e.g. "AAC", "ASS".
    pub codec: String,
    /// e.g. "A_AAC", "S_TEXT/ASS".
    pub codec_id: String,
    /// ISO 639-2 (bibliographic), e.g. "jpn"; "und" if not set.
    pub language: String,
    pub name: String,
    pub default: bool,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub commentary: bool,
    pub channels: Option<u32>,
}

impl Track {
    pub fn sig(&self) -> TrackSig {
        TrackSig {
            language: self.language.clone(),
            name: self.name.clone(),
            codec: self.codec.clone(),
        }
    }

    fn is_styled_subtitles(&self) -> bool {
        self.codec_id.starts_with("S_TEXT/ASS") || self.codec_id.starts_with("S_TEXT/SSA")
    }

    /// "“English (Full)” ASS", for showing which track an episode uses.
    pub fn short_label(&self) -> String {
        let codec = &self.codec;
        if self.name.is_empty() {
            format!("{} {codec}", language_name(&self.language))
        } else {
            format!("“{}” {codec}", self.name)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaFile {
    pub path: PathBuf,
    pub tracks: Vec<Track>,
}

/// Identifies "the same track" across episodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackSig {
    pub language: String,
    pub name: String,
    pub codec: String,
}

/// One choice of track within a language, with how many episodes have it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub sig: TrackSig,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub commentary: bool,
    pub channels: Option<u32>,
    /// Episodes that have this track.
    pub files: usize,
    /// Episodes where it's the default track.
    pub default_in: usize,
}

impl Candidate {
    /// e.g. "“English (Signs)” · ASS · forced".
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        // Unnamed tracks go by their language; what's in them shows in the
        // line count and sample lines (see `sample_subtitles`).
        parts.push(if self.sig.name.is_empty() {
            language_name(&self.sig.language)
        } else {
            format!("“{}”", self.sig.name)
        });
        let mut codec = self.sig.codec.clone();
        if let Some(ch) = self.channels {
            codec.push(' ');
            codec.push_str(&channel_layout(ch));
        }
        parts.push(codec);
        if self.forced {
            parts.push("forced".into());
        }
        if self.hearing_impaired {
            parts.push("SDH".into());
        }
        if self.commentary {
            parts.push("commentary".into());
        }
        if self.default_in > 0 {
            parts.push("default".into());
        }
        parts.join(" · ")
    }

    fn is_signs(&self) -> bool {
        static SIGNS: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)sign|song|forced").unwrap());
        self.forced || SIGNS.is_match(&self.sig.name)
    }

    /// SDH, commentary, a dub transcript ("dubtitle"), karaoke...
    fn is_extra(&self) -> bool {
        static EXTRA: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)\b(sdh|cc|hearing|commentary|karaoke|lyrics)\b|dub").unwrap()
        });
        self.hearing_impaired || self.commentary || EXTRA.is_match(&self.sig.name)
    }
}

pub struct LanguageOption {
    pub code: String,
    /// Episodes with at least one track in this language.
    pub files: usize,
}

/// The tracks in a series' files.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub files: Vec<MediaFile>,
    /// Files that couldn't be read (e.g. not Matroska), with why.
    pub unreadable: Vec<(PathBuf, String)>,
}

impl Analysis {
    /// Languages with tracks of this kind, most widespread first, then by name.
    pub fn languages(&self, kind: Kind) -> Vec<LanguageOption> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for f in &self.files {
            let mut seen: Vec<&str> = f
                .tracks
                .iter()
                .filter(|t| t.kind == kind)
                .map(|t| t.language.as_str())
                .collect();
            seen.sort_unstable();
            seen.dedup();
            for lang in seen {
                *counts.entry(lang).or_default() += 1;
            }
        }
        let mut out: Vec<LanguageOption> = counts
            .into_iter()
            .map(|(code, files)| LanguageOption {
                code: code.to_owned(),
                files,
            })
            .collect();
        out.sort_by(|a, b| {
            b.files
                .cmp(&a.files)
                .then_with(|| language_name(&a.code).cmp(&language_name(&b.code)))
        });
        out
    }

    /// Distinct tracks of this kind in a language, in the order they appear.
    pub fn candidates(&self, kind: Kind, language: &str) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        for f in &self.files {
            for t in f
                .tracks
                .iter()
                .filter(|t| t.kind == kind && t.language == language)
            {
                let sig = t.sig();
                match out.iter_mut().find(|c| c.sig == sig) {
                    Some(c) => {
                        c.files += 1;
                        c.default_in += usize::from(t.default);
                    }
                    None => out.push(Candidate {
                        sig,
                        forced: t.forced,
                        hearing_impaired: t.hearing_impaired,
                        commentary: t.commentary,
                        channels: t.channels,
                        files: 1,
                        default_in: usize::from(t.default),
                    }),
                }
            }
        }
        out
    }

    /// The language of the audio track most episodes play by default.
    pub fn default_audio_language(&self) -> Option<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for f in &self.files {
            let audio = f.tracks.iter().filter(|t| t.kind == Kind::Audio);
            let Some(t) = audio
                .clone()
                .find(|t| t.default)
                .or_else(|| audio.clone().next())
            else {
                continue;
            };
            match counts.iter_mut().find(|(l, _)| *l == t.language) {
                Some((_, n)) => *n += 1,
                None => counts.push((t.language.clone(), 1)),
            }
        }
        first_max_by_key(counts, |(_, n)| *n).map(|(l, _)| l)
    }

    /// English if there are English subtitles, else the most common default.
    pub fn default_subtitle_language(&self) -> Option<String> {
        let langs = self.languages(Kind::Subtitles);
        if langs.iter().any(|l| l.code == "eng") {
            return Some("eng".to_owned());
        }
        let mut counts: Vec<(String, usize)> = Vec::new();
        for t in self
            .files
            .iter()
            .flat_map(|f| &f.tracks)
            .filter(|t| t.kind == Kind::Subtitles && t.default)
        {
            match counts.iter_mut().find(|(l, _)| *l == t.language) {
                Some((_, n)) => *n += 1,
                None => counts.push((t.language.clone(), 1)),
            }
        }
        first_max_by_key(counts, |(_, n)| *n)
            .map(|(l, _)| l)
            .or_else(|| langs.first().map(|l| l.code.clone()))
    }

    /// The track to suggest within a language. For subtitles in the audio's
    /// language (watching a dub) that's signs-only; otherwise full subtitles,
    /// preferring styled ASS over plain text.
    pub fn default_candidate(
        &self,
        kind: Kind,
        language: &str,
        audio_language: Option<&str>,
    ) -> Option<TrackSig> {
        let candidates = self.candidates(kind, language);
        let best = match kind {
            Kind::Subtitles => {
                let want_signs = audio_language == Some(language);
                first_max_by_key(candidates, |c| {
                    let ass = c.sig.codec == "ASS";
                    if want_signs {
                        (c.is_signs(), !c.is_extra(), ass, c.default_in, c.files)
                    } else {
                        (!c.is_signs(), !c.is_extra(), ass, c.default_in, c.files)
                    }
                })
            }
            _ => first_max_by_key(candidates, |c| (!c.commentary, c.default_in, c.files)),
        };
        best.map(|c| c.sig)
    }
}

/// Like `max_by_key`, but the first of equal maximums wins.
fn first_max_by_key<T, K: Ord>(items: Vec<T>, key: impl Fn(&T) -> K) -> Option<T> {
    let mut best: Option<(K, T)> = None;
    for item in items {
        let k = key(&item);
        if best.as_ref().is_none_or(|(bk, _)| k > *bk) {
            best = Some((k, item));
        }
    }
    best.map(|(_, t)| t)
}

/// What to keep.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub audio: TrackSig,
    /// `None` for no subtitles.
    pub subtitles: Option<TrackSig>,
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub input: PathBuf,
    pub output: PathBuf,
    /// The Matroska title, e.g. "Frieren: Beyond Journey's End - 01".
    pub title: String,
    /// Video tracks (all are kept).
    pub video: Vec<u64>,
    pub audio: Option<Track>,
    pub subtitles: Option<Track>,
    pub keep_attachments: bool,
    /// Departures from the choice, e.g. a different track name in this episode.
    pub notes: Vec<String>,
}

impl PlannedFile {
    /// Why this episode can't be remuxed, if it can't.
    pub fn problem(&self) -> Option<String> {
        if self.audio.is_none() {
            Some("no track in the chosen audio language".to_owned())
        } else if self.output == self.input {
            Some("the remux would overwrite the original".to_owned())
        } else {
            None
        }
    }
}

/// Work out, for every episode, which tracks to keep and the output name.
pub fn plan(
    analysis: &Analysis,
    choice: &Choice,
    series_title: &str,
    out_dir: &Path,
) -> Vec<PlannedFile> {
    let inputs: Vec<PathBuf> = analysis.files.iter().map(|f| f.path.clone()).collect();
    let titles = episode_titles(series_title, &inputs);
    analysis
        .files
        .iter()
        .zip(titles)
        .map(|(file, title)| {
            let mut notes = Vec::new();
            let audio = pick(analysis, file, Kind::Audio, &choice.audio, &mut notes);
            if audio.is_none() {
                notes.push(format!(
                    "No {} audio",
                    language_name(&choice.audio.language)
                ));
            }
            let subtitles = choice.subtitles.as_ref().and_then(|want| {
                let t = pick(analysis, file, Kind::Subtitles, want, &mut notes);
                if t.is_none() {
                    notes.push(format!("No {} subtitles", language_name(&want.language)));
                }
                t
            });
            PlannedFile {
                input: file.path.clone(),
                output: out_dir.join(format!("{}.mkv", sanitize_filename(&title))),
                title,
                video: file
                    .tracks
                    .iter()
                    .filter(|t| t.kind == Kind::Video)
                    .map(|t| t.id)
                    .collect(),
                keep_attachments: subtitles.is_some_and(|t| t.is_styled_subtitles()),
                audio: audio.cloned(),
                subtitles: subtitles.cloned(),
                notes,
            }
        })
        .collect()
}

/// The episode's track matching `want`, or else the most similar one in the
/// same language (noted): same forced/SDH/commentary flags first, so full
/// subtitles never fall back to signs only, then codec, then name.
fn pick<'a>(
    analysis: &Analysis,
    file: &'a MediaFile,
    kind: Kind,
    want: &TrackSig,
    notes: &mut Vec<String>,
) -> Option<&'a Track> {
    let in_language: Vec<&Track> = file
        .tracks
        .iter()
        .filter(|t| t.kind == kind && t.language == want.language)
        .collect();
    if let Some(t) = in_language.iter().find(|t| t.sig() == *want) {
        return Some(t);
    }
    let reference = analysis
        .files
        .iter()
        .flat_map(|f| &f.tracks)
        .find(|t| t.kind == kind && t.sig() == *want);
    let fallback = first_max_by_key(in_language, |t| {
        let same_flags = reference.is_none_or(|r| {
            (t.forced, t.hearing_impaired, t.commentary)
                == (r.forced, r.hearing_impaired, r.commentary)
        });
        (
            same_flags,
            t.codec == want.codec,
            t.name.eq_ignore_ascii_case(&want.name),
        )
    })?;
    let what = if kind == Kind::Audio {
        "Audio"
    } else {
        "Subtitles"
    };
    notes.push(format!(
        "{what}: using {} (no exact match)",
        fallback.short_label()
    ));
    Some(fallback)
}

/// "<title> - 01" for each file, from the episode number in its name.
/// Specials are "S00E01"; seasons are spelled out only when files span
/// several; files without a number use their extra's name (NCOP1...) or,
/// failing that, their own name. A single unnumbered file is just "<title>".
pub fn episode_titles(series_title: &str, inputs: &[PathBuf]) -> Vec<String> {
    static EXTRA: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(NC ?(?:OP|ED)\s?\d*[a-z]?|OVA\s?\d*|OAD\s?\d*|SP\s?\d+|Specials?\s?\d*|Menu\s?\d*|PV\s?\d*|CM\s?\d*|Preview\s?\d*|Trailer\s?\d*)\b")
            .unwrap()
    });
    let infos: Vec<(title::ParsedTitle, String)> = inputs
        .iter()
        .map(|p| {
            let stem = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (title::parse(&stem), stem)
        })
        .collect();
    let regular_seasons: std::collections::HashSet<u32> = infos
        .iter()
        .filter(|(p, _)| p.episodes.is_some())
        .map(|(p, _)| p.seasons.map_or(1, |s| s.0))
        .filter(|&s| s != 0)
        .collect();
    let spell_seasons = regular_seasons.len() > 1;
    let max_episode = infos
        .iter()
        .filter_map(|(p, _)| p.episodes.map(|e| e.1))
        .max()
        .unwrap_or(0);
    let width = max_episode.to_string().len().max(2);
    let pad = |n: u32| format!("{n:0width$}");

    let labels: Vec<Option<String>> = infos
        .iter()
        .map(|(p, stem)| match p.episodes {
            Some((a, b)) => {
                let ep = if a == b {
                    pad(a)
                } else {
                    format!("{}-{}", pad(a), pad(b))
                };
                match p.seasons.map(|s| s.0) {
                    Some(0) => Some(format!("S00E{ep}")),
                    s if spell_seasons => Some(format!("S{:02}E{ep}", s.unwrap_or(1))),
                    _ => Some(ep),
                }
            }
            None => EXTRA
                .captures(stem)
                .map(|c| c[1].to_owned())
                .or_else(|| (inputs.len() > 1).then(|| stem.clone())),
        })
        .collect();

    let mut used: HashMap<String, usize> = HashMap::new();
    labels
        .into_iter()
        .map(|label| {
            let base = match label {
                Some(l) => format!("{series_title} - {l}"),
                None => series_title.to_owned(),
            };
            let n = used.entry(base.clone()).or_default();
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{base} ({n})")
            }
        })
        .collect()
}

/// A title as a file name: everything kept except path separators and
/// control characters (titles like "Fate/Zero" become "Fate-Zero").
pub fn sanitize_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let cleaned = cleaned
        .trim()
        .trim_end_matches('.')
        .trim_start_matches('.')
        .trim();
    if cleaned.is_empty() {
        "Untitled".to_owned()
    } else {
        cleaned.to_owned()
    }
}

// ── Probing and remuxing ────────────────────────────────────────────────

/// Line counts and opening lines for every subtitle choice, each read from
/// the first episode that has it, so tracks can be told apart by what they
/// contain (signs only, full dialogue, a dub transcript...). Blocking.
pub fn sample_subtitles(analysis: &Analysis) -> Vec<(TrackSig, SubtitleSample)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for file in &analysis.files {
        let new: Vec<&Track> = file
            .tracks
            .iter()
            .filter(|t| t.kind == Kind::Subtitles && seen.insert(t.sig()))
            .collect();
        if new.is_empty() {
            continue;
        }
        let ids: Vec<u64> = new.iter().map(|t| t.id).collect();
        match mkv::subtitle_samples(&file.path, &ids, 3) {
            Ok(mut samples) => out.extend(
                new.iter()
                    .filter_map(|t| Some((t.sig(), samples.remove(&t.id)?))),
            ),
            Err(e) => tracing::warn!("couldn't read subtitles of {}: {e:#}", file.path.display()),
        }
    }
    out
}

/// Read a file's tracks. Blocking (reads the file).
pub fn probe(path: &Path) -> Result<MediaFile> {
    let info = mkv::probe(path)?;
    let tracks = info
        .tracks
        .into_iter()
        .map(|t| Track {
            // "und" tracks named "English (Full)" are English.
            language: crate::prefs::track_language(&t),
            id: t.number,
            kind: match t.kind {
                mkv::TrackKind::Video => Kind::Video,
                mkv::TrackKind::Audio => Kind::Audio,
                mkv::TrackKind::Subtitles => Kind::Subtitles,
                mkv::TrackKind::Other => Kind::Other,
            },
            codec: mkv::codec_name(&t.codec_id),
            codec_id: t.codec_id,
            name: t.name,
            default: t.default,
            forced: t.forced,
            hearing_impaired: t.hearing_impaired,
            commentary: t.commentary,
            channels: t.channels.and_then(|c| u32::try_from(c).ok()),
        })
        .collect();
    Ok(MediaFile {
        path: path.to_owned(),
        tracks,
    })
}

/// Read every file's tracks. Files that can't be read (e.g. not Matroska)
/// are listed in `unreadable` instead of failing the series. Blocking.
pub fn probe_all(paths: Vec<PathBuf>) -> Result<Analysis> {
    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for path in paths {
        match probe(&path) {
            Ok(file) => files.push(file),
            Err(e) => unreadable.push((path, format!("{e:#}"))),
        }
    }
    if files.is_empty()
        && let Some((path, e)) = unreadable.first()
    {
        bail!("{}: {e}", path.display());
    }
    Ok(Analysis { files, unreadable })
}

/// Remux one episode. `progress` gets 0.0–1.0; setting `cancel` stops it.
/// Writes to a hidden temporary file, checks the result, then renames it
/// into place (replacing an earlier remux of the same name). Blocking: run it
/// off the UI thread.
pub fn remux(item: &PlannedFile, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<()> {
    if let Some(problem) = item.problem() {
        bail!("{problem}");
    }
    let audio = item.audio.as_ref().context("no audio track chosen")?;
    let out_dir = item.output.parent().context("output has no folder")?;
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let file_name = item
        .output
        .file_name()
        .context("output has no name")?
        .to_string_lossy();
    let tmp = out_dir.join(format!(".{file_name}.partial"));

    let mut keep = mkv::Keep {
        tracks: item.video.clone(),
        default: vec![audio.id],
        not_default: Vec::new(),
        title: item.title.clone(),
        attachments: item.keep_attachments,
    };
    keep.tracks.push(audio.id);
    if let Some(subtitles) = &item.subtitles {
        keep.tracks.push(subtitles.id);
        keep.default.push(subtitles.id);
    }
    let result = mkv::remux(&item.input, &tmp, &keep, cancel, &mut progress).and_then(|()| {
        // Read the result back: exactly the chosen tracks must be there.
        let mut written: Vec<u64> = probe(&tmp)?.tracks.iter().map(|t| t.id).collect();
        let mut wanted = keep.tracks.clone();
        written.sort_unstable();
        wanted.sort_unstable();
        if written != wanted {
            bail!("the remuxed file came out with the wrong tracks");
        }
        Ok(())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &item.output)
        .with_context(|| format!("saving {}", item.output.display()))?;
    progress(1.0);
    Ok(())
}

// ── Languages ───────────────────────────────────────────────────────────

/// Matroska's ISO 639-2 code for a language tag: two-letter and
/// "terminology" codes are mapped to the bibliographic codes Matroska uses
/// (e.g. "en" → "eng", "deu" → "ger"), so the same language groups together.
pub fn normalize_language(code: &str) -> String {
    let code = code.trim().to_ascii_lowercase();
    let primary = code.split(['-', '_']).next().unwrap_or("");
    let mapped = match primary {
        "" => "und",
        "ja" => "jpn",
        "en" => "eng",
        "es" => "spa",
        "pt" => "por",
        "fr" | "fra" => "fre",
        "de" | "deu" => "ger",
        "it" => "ita",
        "ru" => "rus",
        "ar" => "ara",
        "zh" | "zho" => "chi",
        "ko" => "kor",
        "th" => "tha",
        "vi" => "vie",
        "id" => "ind",
        "ms" | "msa" => "may",
        "hi" => "hin",
        "pl" => "pol",
        "tr" => "tur",
        "uk" => "ukr",
        "he" => "heb",
        "nl" | "nld" => "dut",
        "sv" => "swe",
        "no" => "nor",
        "nb" => "nob",
        "nn" => "nno",
        "da" => "dan",
        "fi" => "fin",
        "cs" | "ces" => "cze",
        "sk" | "slk" => "slo",
        "hu" => "hun",
        "ro" | "ron" => "rum",
        "el" | "ell" => "gre",
        "bg" => "bul",
        "hr" => "hrv",
        "sr" => "srp",
        "sl" => "slv",
        "ca" => "cat",
        "gl" => "glg",
        "eu" | "eus" => "baq",
        "fa" | "fas" => "per",
        "is" | "isl" => "ice",
        "tl" => "tgl",
        other => other,
    };
    mapped.to_owned()
}

/// English name of a (normalized) language code.
pub fn language_name(code: &str) -> String {
    let name = match code {
        "jpn" => "Japanese",
        "eng" => "English",
        "spa" => "Spanish",
        "por" => "Portuguese",
        "fre" => "French",
        "ger" => "German",
        "ita" => "Italian",
        "rus" => "Russian",
        "ara" => "Arabic",
        "chi" => "Chinese",
        "kor" => "Korean",
        "tha" => "Thai",
        "vie" => "Vietnamese",
        "ind" => "Indonesian",
        "may" => "Malay",
        "hin" => "Hindi",
        "pol" => "Polish",
        "tur" => "Turkish",
        "ukr" => "Ukrainian",
        "heb" => "Hebrew",
        "dut" => "Dutch",
        "swe" => "Swedish",
        "nor" => "Norwegian",
        "nob" => "Norwegian Bokmål",
        "nno" => "Norwegian Nynorsk",
        "dan" => "Danish",
        "fin" => "Finnish",
        "cze" => "Czech",
        "slo" => "Slovak",
        "hun" => "Hungarian",
        "rum" => "Romanian",
        "gre" => "Greek",
        "bul" => "Bulgarian",
        "hrv" => "Croatian",
        "srp" => "Serbian",
        "slv" => "Slovenian",
        "cat" => "Catalan",
        "glg" => "Galician",
        "baq" => "Basque",
        "fil" => "Filipino",
        "tgl" => "Tagalog",
        "per" => "Persian",
        "ben" => "Bengali",
        "tam" => "Tamil",
        "tel" => "Telugu",
        "mal" => "Malayalam",
        "kan" => "Kannada",
        "mar" => "Marathi",
        "urd" => "Urdu",
        "lit" => "Lithuanian",
        "lav" => "Latvian",
        "est" => "Estonian",
        "ice" => "Icelandic",
        "und" => "Unknown language",
        "zxx" => "No language",
        "mul" => "Multiple languages",
        other => return other.to_owned(),
    };
    name.to_owned()
}

fn channel_layout(channels: u32) -> String {
    match channels {
        1 => "mono".into(),
        2 => "2.0".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n}ch"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, kind: Kind, codec: &str, lang: &str, name: &str) -> Track {
        Track {
            id,
            kind,
            codec: codec.into(),
            codec_id: match codec {
                "ASS" => "S_TEXT/ASS".into(),
                "SRT" => "S_TEXT/UTF8".into(),
                _ => String::new(),
            },
            language: lang.into(),
            name: name.into(),
            default: false,
            forced: false,
            hearing_impaired: false,
            commentary: false,
            channels: (kind == Kind::Audio).then_some(2),
        }
    }

    /// Tracks like the VARYG Netflix release: dual audio, three kinds of
    /// English subtitles, regional Spanish.
    fn varyg_episode(path: &str) -> MediaFile {
        let mut tracks = vec![
            track(0, Kind::Video, "H.264", "und", ""),
            track(1, Kind::Audio, "AAC", "jpn", ""),
            track(2, Kind::Audio, "AAC", "eng", ""),
            track(3, Kind::Subtitles, "SRT", "eng", "Forced"),
            track(4, Kind::Subtitles, "SRT", "eng", ""),
            track(5, Kind::Subtitles, "SRT", "eng", "Dubtitle"),
            track(6, Kind::Subtitles, "SRT", "jpn", "SDH"),
            track(7, Kind::Subtitles, "SRT", "spa", "Latin American"),
            track(8, Kind::Subtitles, "SRT", "spa", "European"),
        ];
        tracks[1].default = true;
        tracks[3].forced = true;
        tracks[4].default = true;
        tracks[6].hearing_impaired = true;
        MediaFile {
            path: path.into(),
            tracks,
        }
    }

    #[test]
    fn languages_and_candidates() {
        let a = Analysis {
            files: vec![varyg_episode("/s/E01.mkv"), varyg_episode("/s/E02.mkv")],
            unreadable: vec![],
        };
        let audio: Vec<_> = a
            .languages(Kind::Audio)
            .into_iter()
            .map(|l| (l.code, l.files))
            .collect();
        assert_eq!(audio, vec![("eng".to_owned(), 2), ("jpn".to_owned(), 2)]);
        let spanish = a.candidates(Kind::Subtitles, "spa");
        assert_eq!(spanish.len(), 2);
        assert_eq!(spanish[0].sig.name, "Latin American");
        assert_eq!(spanish[0].files, 2);
        assert_eq!(
            a.candidates(Kind::Subtitles, "eng")[0].label(),
            "“Forced” · SRT · forced"
        );
    }

    #[test]
    fn defaults_suit_subs_or_dub() {
        let a = Analysis {
            files: vec![varyg_episode("/s/E01.mkv")],
            unreadable: vec![],
        };
        assert_eq!(a.default_audio_language().as_deref(), Some("jpn"));
        assert_eq!(a.default_subtitle_language().as_deref(), Some("eng"));
        // Japanese audio: full English subtitles, not signs or the dub transcript.
        let subs = a
            .default_candidate(Kind::Subtitles, "eng", Some("jpn"))
            .unwrap();
        assert_eq!(subs.name, "");
        // English dub: signs only.
        let subs = a
            .default_candidate(Kind::Subtitles, "eng", Some("eng"))
            .unwrap();
        assert_eq!(subs.name, "Forced");
    }

    #[test]
    fn plan_keeps_chosen_tracks_and_falls_back_with_a_note() {
        let mut odd = varyg_episode("/s/E02.mkv");
        odd.tracks[4].name = "English CC".into(); // named differently in this episode
        odd.tracks
            .retain(|t| t.language != "jpn" || t.kind != Kind::Audio); // and no Japanese audio
        let a = Analysis {
            files: vec![
                varyg_episode("/s/E01.mkv"),
                odd,
                varyg_episode("/s/E03.mkv"),
            ],
            unreadable: vec![],
        };
        let choice = Choice {
            audio: a.default_candidate(Kind::Audio, "jpn", None).unwrap(),
            subtitles: a.default_candidate(Kind::Subtitles, "eng", Some("jpn")),
        };
        let plan = plan(&a, &choice, "Show: Part/Two", Path::new("/out"));
        assert_eq!(plan[0].audio.as_ref().unwrap().id, 1);
        assert_eq!(plan[0].subtitles.as_ref().unwrap().id, 4);
        assert!(!plan[0].keep_attachments, "SRT needs no fonts");
        assert_eq!(plan[0].output, Path::new("/out/Show: Part-Two - 01.mkv"));
        assert_eq!(plan[0].title, "Show: Part/Two - 01");
        assert!(plan[0].notes.is_empty() && plan[0].problem().is_none());
        // E02: no Japanese audio → can't be remuxed; subtitles fall back to
        // the renamed full track, not to signs only.
        assert!(plan[1].problem().is_some());
        assert_eq!(plan[1].subtitles.as_ref().unwrap().name, "English CC");
        assert!(plan[1].notes.iter().any(|n| n.contains("no exact match")));
    }

    #[test]
    fn episode_titles_follow_the_files() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            episode_titles(
                "Ramparts",
                &paths(&[
                    "The.Ramparts.of.Ice.S02E01.1080p.mkv",
                    "The.Ramparts.of.Ice.S02E02.1080p.mkv"
                ])
            ),
            vec!["Ramparts - 01", "Ramparts - 02"]
        );
        assert_eq!(
            episode_titles(
                "Gundam",
                &paths(&[
                    "Gundam.S00E01.Prologue.mkv",
                    "Gundam.S01E01.mkv",
                    "Gundam.S02E01.mkv"
                ])
            ),
            vec!["Gundam - S00E01", "Gundam - S01E01", "Gundam - S02E01"]
        );
        assert_eq!(
            episode_titles(
                "Frieren",
                &paths(&[
                    "[PMR] Frieren - 05 (BD 1080p).mkv",
                    "[PMR] Frieren - NCOP1 (BD 1080p).mkv",
                    "[PMR] Frieren - 05v2 (BD 1080p).mkv",
                ])
            ),
            vec!["Frieren - 05", "Frieren - NCOP1", "Frieren - 05 (2)"]
        );
        assert_eq!(
            episode_titles("Your Name.", &paths(&["Kimi no Na wa (BD 1080p).mkv"])),
            vec!["Your Name."]
        );
        let hundred: Vec<PathBuf> = (1..=100)
            .map(|n| PathBuf::from(format!("[G] Show - {n:03}.mkv")))
            .collect();
        assert_eq!(episode_titles("Show", &hundred)[4], "Show - 005");
    }

    #[test]
    fn file_names_keep_the_title() {
        assert_eq!(
            sanitize_filename("Frieren: Beyond Journey’s End - 01"),
            "Frieren: Beyond Journey’s End - 01"
        );
        assert_eq!(sanitize_filename("Fate/Zero - 01"), "Fate-Zero - 01");
        assert_eq!(sanitize_filename("Your Name."), "Your Name");
        assert_eq!(sanitize_filename(".hack//Sign - 01"), "hack--Sign - 01");
        assert_eq!(sanitize_filename("  "), "Untitled");
    }

    #[test]
    fn normalizes_languages() {
        assert_eq!(normalize_language("en"), "eng");
        assert_eq!(normalize_language("es-419"), "spa");
        assert_eq!(normalize_language("deu"), "ger");
        assert_eq!(normalize_language("ger"), "ger");
        assert_eq!(normalize_language(""), "und");
        assert_eq!(normalize_language("FIL"), "fil");
        assert_eq!(language_name("chi"), "Chinese");
        assert_eq!(language_name("xyz"), "xyz");
    }
}

/// Synthetic episodes for tests: H.264 video, Japanese FLAC and English AAC
/// audio, English full + signs ASS subtitles, Spanish SRT (Latin American
/// everywhere, European missing from the last episode), a font, chapters.
/// Needs ffmpeg and mkvmerge.
#[cfg(test)]
pub(crate) fn make_test_series(dir: &Path, episodes: u32) -> Vec<PathBuf> {
    use std::process::Command;
    let run = |cmd: &mut Command| {
        let out = cmd.output().expect("ffmpeg and mkvmerge must be installed");
        assert!(
            out.status.success(),
            "{cmd:?} failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    };
    let parts = dir.join("parts");
    std::fs::create_dir_all(&parts).unwrap();
    let ffmpeg = |args: &[&str], out: &str| {
        run(Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y"])
            .args(args)
            .arg(parts.join(out)));
    };
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=24",
            "-t",
            "3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        "v.mkv",
    );
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "3",
            "-c:a",
            "flac",
        ],
        "jpn.mka",
    );
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=660",
            "-ac",
            "2",
            "-t",
            "3",
            "-c:a",
            "aac",
        ],
        "eng.mka",
    );
    let ass = |text: &str| {
        format!(
            "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Test,20\n\n\
             [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:00.50,0:00:02.50,Default,,0,0,0,,{text}\n"
        )
    };
    std::fs::write(parts.join("full.ass"), ass("Full dialogue")).unwrap();
    std::fs::write(parts.join("signs.ass"), ass("SIGN: Shop")).unwrap();
    std::fs::write(
        parts.join("la.srt"),
        "1\n00:00:00,500 --> 00:00:02,500\nLatinoamérica\n",
    )
    .unwrap();
    std::fs::write(
        parts.join("es.srt"),
        "1\n00:00:00,500 --> 00:00:02,500\nEspaña\n",
    )
    .unwrap();
    let font = [
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
    .or_else(|| {
        std::fs::read_dir("/usr/share/fonts")
            .ok()?
            .flatten()
            .find_map(|d| {
                std::fs::read_dir(d.path())
                    .ok()?
                    .flatten()
                    .map(|f| f.path())
                    .find(|p| p.extension().is_some_and(|e| e == "ttf"))
            })
    })
    .expect("a .ttf font to attach");

    (1..=episodes)
        .map(|ep| {
            let out = dir.join(format!("[Group] Show - {ep:02} (1080p).mkv"));
            let p = |name: &str| parts.join(name);
            let mut cmd = Command::new("mkvmerge");
            cmd.arg("-q").arg("-o").arg(&out);
            cmd.args([
                "--title",
                &format!("[Group] Show - {ep:02}"),
                "--generate-chapters",
                "interval:1s",
            ])
            .arg(p("v.mkv"));
            cmd.args([
                "--language",
                "0:jpn",
                "--track-name",
                "0:Japanese",
                "--default-track-flag",
                "0:1",
            ])
            .arg(p("jpn.mka"));
            cmd.args([
                "--language",
                "0:eng",
                "--track-name",
                "0:English",
                "--default-track-flag",
                "0:0",
            ])
            .arg(p("eng.mka"));
            cmd.args([
                "--language",
                "0:eng",
                "--track-name",
                "0:English (Full)",
                "--default-track-flag",
                "0:1",
            ])
            .arg(p("full.ass"));
            cmd.args([
                "--language",
                "0:eng",
                "--track-name",
                "0:English (Signs)",
                "--forced-display-flag",
                "0:1",
            ])
            .args(["--default-track-flag", "0:0"])
            .arg(p("signs.ass"));
            cmd.args([
                "--language",
                "0:es-419",
                "--track-name",
                "0:Latin American",
                "--default-track-flag",
                "0:0",
            ])
            .arg(p("la.srt"));
            if ep < episodes {
                cmd.args([
                    "--language",
                    "0:es-ES",
                    "--track-name",
                    "0:European",
                    "--default-track-flag",
                    "0:0",
                ])
                .arg(p("es.srt"));
            }
            cmd.args(["--attachment-mime-type", "font/ttf", "--attach-file"])
                .arg(&font);
            run(&mut cmd);
            out
        })
        .collect()
}

#[cfg(test)]
mod end_to_end {
    //! Remuxes real files and checks the results with ffmpeg and mkvmerge
    //! (used here only as independent checkers; the app needs neither).
    use super::*;
    use std::process::Command;

    fn identify(path: &Path) -> serde_json::Value {
        let out = Command::new("mkvmerge")
            .arg("-J")
            .arg(path)
            .output()
            .unwrap();
        let id: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            id["errors"].as_array().is_none_or(|e| e.is_empty()),
            "mkvmerge: {}",
            id["errors"]
        );
        id
    }

    /// MD5 of each stream's packets, by stream index: identical data in, identical data out.
    fn stream_hashes(path: &Path) -> Vec<String> {
        let out = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(path)
            .args([
                "-map",
                "0:v",
                "-map",
                "0:a?",
                "-map",
                "0:s?",
                "-c",
                "copy",
                "-f",
                "streamhash",
                "-hash",
                "md5",
                "-",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Lines are "<stream>, <bytes>, <md5>" after "#" comment lines.
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                l.split(',')
                    .map(str::trim)
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" bytes, md5 ")
            })
            .collect()
    }

    /// The hash of the original's stream with this track number (Matroska
    /// numbers tracks from 1, ffmpeg's streams from 0).
    fn original_hash(hashes: &[String], track: u64) -> &str {
        &hashes[track as usize - 1]
    }

    /// Decodes everything and seeks into the middle; both must be error-free.
    fn assert_plays(path: &Path) {
        for args in [&["-i"][..], &["-ss", "1.5", "-i"][..]] {
            let out = Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error"])
                .args(args)
                .arg(path)
                .args(["-f", "null", "-"])
                .output()
                .unwrap();
            let errors = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && errors.trim().is_empty(),
                "{args:?} {}: {errors}",
                path.display()
            );
        }
        let info = Command::new("mkvinfo")
            .arg("--all")
            .arg(path)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&info.stdout).contains("Cue point"),
            "no cues for seeking"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("LIVE_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("ichioshi-remux-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn remux_all(plan: &[PlannedFile]) {
        let cancel = AtomicBool::new(false);
        for item in plan {
            let mut last = 0.0;
            remux(item, &cancel, |p| last = p).unwrap();
            assert_eq!(last, 1.0);
        }
    }

    #[test]
    #[ignore = "needs ffmpeg and mkvmerge to make and check test files"]
    fn remuxes_a_series() {
        let dir = scratch("series");
        let inputs = make_test_series(&dir.join("src"), 3);
        let analysis = probe_all(inputs.clone()).unwrap();

        let subs: Vec<_> = analysis
            .languages(Kind::Subtitles)
            .into_iter()
            .map(|l| (l.code, l.files))
            .collect();
        assert_eq!(subs, vec![("eng".to_owned(), 3), ("spa".to_owned(), 3)]);
        let spanish = analysis.candidates(Kind::Subtitles, "spa");
        assert_eq!(
            spanish
                .iter()
                .map(|c| (c.sig.name.as_str(), c.files))
                .collect::<Vec<_>>(),
            vec![("Latin American", 3), ("European", 2)]
        );

        // Japanese audio, full English subtitles.
        let title = "Frieren: Beyond Journey’s End";
        let audio = analysis
            .default_candidate(Kind::Audio, "jpn", None)
            .unwrap();
        let subtitles = analysis.default_candidate(Kind::Subtitles, "eng", Some("jpn"));
        assert_eq!(subtitles.as_ref().unwrap().name, "English (Full)");
        let out = dir.join("out");
        let sub_plan = plan(&analysis, &Choice { audio, subtitles }, title, &out);
        remux_all(&sub_plan);

        for (n, item) in sub_plan.iter().enumerate() {
            let expected = out.join(format!("{title} - {:02}.mkv", n + 1));
            assert_eq!(item.output, expected);
            let id = identify(&expected);
            assert_eq!(
                id["container"]["properties"]["title"],
                format!("{title} - {:02}", n + 1)
            );
            let tracks: Vec<(String, String, String, bool)> = id["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    let p = &t["properties"];
                    (
                        t["type"].as_str().unwrap().to_owned(),
                        p["language"].as_str().unwrap_or("").to_owned(),
                        p["track_name"].as_str().unwrap_or("").to_owned(),
                        p["default_track"].as_bool().unwrap_or(false),
                    )
                })
                .collect();
            assert_eq!(
                tracks,
                vec![
                    // The video's flags are kept as they were (not default in these test files).
                    ("video".into(), "und".into(), "".into(), false),
                    ("audio".into(), "jpn".into(), "Japanese".into(), true),
                    (
                        "subtitles".into(),
                        "eng".into(),
                        "English (Full)".into(),
                        true
                    ),
                ]
            );
            assert_eq!(
                id["attachments"].as_array().unwrap().len(),
                1,
                "fonts kept for ASS"
            );
            assert!(
                id["chapters"].as_array().is_some_and(|c| !c.is_empty()),
                "chapters kept"
            );
            // Same data, track for track.
            let before = stream_hashes(&item.input);
            let after = stream_hashes(&expected);
            let kept = [
                item.video[0],
                item.audio.as_ref().unwrap().id,
                item.subtitles.as_ref().unwrap().id,
            ];
            let expected_hashes: Vec<&str> =
                kept.iter().map(|&t| original_hash(&before, t)).collect();
            assert_eq!(after, expected_hashes);
            assert_plays(&expected);
        }
        // Nothing half-written left behind, originals untouched.
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3);
        assert_eq!(identify(&inputs[0])["tracks"].as_array().unwrap().len(), 7);

        // English dub: signs-only English subtitles, still forced.
        let audio = analysis
            .default_candidate(Kind::Audio, "eng", None)
            .unwrap();
        let subtitles = analysis.default_candidate(Kind::Subtitles, "eng", Some("eng"));
        assert_eq!(subtitles.as_ref().unwrap().name, "English (Signs)");
        let dub_plan = plan(
            &analysis,
            &Choice { audio, subtitles },
            title,
            &dir.join("dub"),
        );
        remux_all(&dub_plan[..1]);
        let id = identify(&dub_plan[0].output);
        let subs = &id["tracks"][2]["properties"];
        assert_eq!(
            (subs["track_name"].as_str(), subs["forced_track"].as_bool()),
            (Some("English (Signs)"), Some(true))
        );
        assert_eq!(
            id["tracks"][1]["properties"]["default_track"].as_bool(),
            Some(true),
            "English audio is default"
        );

        // No subtitles: no fonts either.
        let audio = analysis
            .default_candidate(Kind::Audio, "jpn", None)
            .unwrap();
        let bare = plan(
            &analysis,
            &Choice {
                audio,
                subtitles: None,
            },
            title,
            &dir.join("bare"),
        );
        remux_all(&bare[..1]);
        let id = identify(&bare[0].output);
        assert_eq!(id["tracks"].as_array().unwrap().len(), 2);
        assert!(id["attachments"].as_array().is_none_or(|a| a.is_empty()));
        assert_plays(&bare[0].output);

        // Cancelling leaves nothing behind.
        let cancel = AtomicBool::new(true);
        let choice = Choice {
            audio: bare[0].audio.as_ref().unwrap().sig(),
            subtitles: None,
        };
        let cancelled = plan(&analysis, &choice, title, &dir.join("cancelled"));
        assert!(remux(&cancelled[0], &cancel, |_| {}).is_err());
        assert!(std::fs::read_dir(dir.join("cancelled")).is_ok_and(|mut d| d.next().is_none()));

        // What each subtitle choice contains.
        let samples: HashMap<String, SubtitleSample> = sample_subtitles(&analysis)
            .into_iter()
            .map(|(sig, s)| (sig.name, s))
            .collect();
        assert_eq!(samples["English (Full)"].lines, vec!["Full dialogue"]);
        assert_eq!(samples["English (Signs)"].lines, vec!["SIGN: Shop"]);
        assert_eq!(samples["Latin American"].events, 1);

        // Not Matroska: reported, not fatal.
        std::fs::write(dir.join("src/notes.mp4"), b"not a video").unwrap();
        let mut with_other = inputs.clone();
        with_other.push(dir.join("src/notes.mp4"));
        let analysis = probe_all(with_other).unwrap();
        assert_eq!((analysis.files.len(), analysis.unreadable.len()), (3, 1));
    }

    /// `REMUX_SAMPLE_DIR=<folder of real episodes>`: remux them with the
    /// default choices into the scratch folder (the originals are only read),
    /// and check every kept track's data is identical.
    #[test]
    #[ignore = "needs REMUX_SAMPLE_DIR, ffmpeg and mkvmerge"]
    fn remuxes_real_files_with_default_choices() {
        let Some(sample) = std::env::var_os("REMUX_SAMPLE_DIR").map(PathBuf::from) else {
            return;
        };
        let inputs: Vec<PathBuf> = std::fs::read_dir(&sample)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_video(p))
            .collect();
        let analysis = probe_all(inputs).unwrap();
        let audio_lang = analysis.default_audio_language().unwrap();
        let sub_lang = analysis.default_subtitle_language();
        let choice = Choice {
            audio: analysis
                .default_candidate(Kind::Audio, &audio_lang, None)
                .unwrap(),
            subtitles: sub_lang
                .and_then(|l| analysis.default_candidate(Kind::Subtitles, &l, Some(&audio_lang))),
        };
        println!("choice: {choice:?}");
        let started = std::time::Instant::now();
        let samples = sample_subtitles(&analysis);
        println!("read every subtitle track in {:.1?}:", started.elapsed());
        for (sig, sample) in samples.iter().filter(|(s, _)| s.language == "eng") {
            println!(
                "  {:?}: {} lines, starts {:?}",
                sig.name, sample.events, sample.lines
            );
        }
        let out = scratch("real");
        let real_plan = plan(&analysis, &choice, "Sample", &out);
        let started = std::time::Instant::now();
        remux_all(&real_plan);
        println!("remuxed in {:.1?}", started.elapsed());
        for item in &real_plan {
            let id = identify(&item.output);
            println!(
                "{} ({} MiB):",
                item.output.display(),
                std::fs::metadata(&item.output).unwrap().len() >> 20
            );
            println!("  title: {}", id["container"]["properties"]["title"]);
            for t in id["tracks"].as_array().unwrap() {
                let p = &t["properties"];
                println!(
                    "  {} {} {} default={}",
                    t["type"], p["language"], p["track_name"], p["default_track"]
                );
            }
            let before = stream_hashes(&item.input);
            let mut kept = item.video.clone();
            kept.push(item.audio.as_ref().unwrap().id);
            kept.extend(item.subtitles.as_ref().map(|s| s.id));
            let expected: Vec<&str> = kept.iter().map(|&t| original_hash(&before, t)).collect();
            assert_eq!(
                stream_hashes(&item.output),
                expected,
                "kept tracks' data must be identical"
            );
            println!(
                "  all {} kept tracks byte-identical to the original",
                kept.len()
            );
            let info = Command::new("mkvinfo")
                .arg("--all")
                .arg(&item.output)
                .output()
                .unwrap();
            let info = String::from_utf8_lossy(&info.stdout);
            println!(
                "  cues: {} points, {} for subtitle lines",
                info.matches("Cue point").count(),
                info.matches("Cue duration").count()
            );
            let seek = Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-ss", "600", "-i"])
                .arg(&item.output)
                .args(["-t", "5", "-f", "null", "-"])
                .output()
                .unwrap();
            assert!(
                seek.status.success() && seek.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&seek.stderr)
            );
            println!("  seeks to 10:00 and decodes cleanly");
        }
    }
}
