//! Language preferences: which audio and subtitles play by default. Set
//! once for everything and, if wanted, per series. They only change which
//! tracks are marked default; every track stays in the file.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::mkv::{TrackInfo, TrackKind};
use crate::remux::{language_name, normalize_language};

/// What's in a subtitle track (the kinds the remuxer tells apart).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SubType {
    /// Full subtitles, or signs & songs only when the audio is already in
    /// the subtitle language.
    #[default]
    Auto,
    Full,
    /// Signs & songs only (for dubs).
    Signs,
    /// A transcript of the dub.
    Dubtitles,
    /// For the deaf and hard of hearing.
    Sdh,
    Commentary,
}

impl SubType {
    /// The kinds to look for, in order, when this one is wanted. Signs only
    /// never fall back to full subtitles.
    pub fn fallbacks(self) -> &'static [SubType] {
        match self {
            SubType::Full | SubType::Auto => &[SubType::Full, SubType::Sdh, SubType::Dubtitles],
            SubType::Signs => &[SubType::Signs],
            SubType::Dubtitles => &[SubType::Dubtitles, SubType::Full, SubType::Sdh],
            SubType::Sdh => &[SubType::Sdh, SubType::Full],
            SubType::Commentary => &[SubType::Commentary],
        }
    }

    pub const ALL: [SubType; 6] = [SubType::Auto, SubType::Full, SubType::Signs, SubType::Dubtitles, SubType::Sdh, SubType::Commentary];

    pub fn name(self) -> &'static str {
        match self {
            SubType::Auto => "Automatic",
            SubType::Full => "Full subtitles",
            SubType::Signs => "Signs & songs",
            SubType::Dubtitles => "Dubtitles",
            SubType::Sdh => "SDH (closed captions)",
            SubType::Commentary => "Commentary",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            SubType::Auto => "Full subtitles, or signs & songs only when the audio is in your subtitle language",
            SubType::Full => "Everything that's said",
            SubType::Signs => "Only on-screen text and song lyrics, for watching a dub",
            SubType::Dubtitles => "What the dub says, word for word",
            SubType::Sdh => "Dialogue plus sounds, for the deaf and hard of hearing",
            SubType::Commentary => "A commentary track",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(default)]
pub struct LangPrefs {
    /// ISO 639-2 code, e.g. "jpn".
    pub audio: String,
    /// `None`: no subtitles.
    pub subtitles: Option<String>,
    pub sub_type: SubType,
}

impl Default for LangPrefs {
    fn default() -> Self {
        Self { audio: "jpn".into(), subtitles: Some("eng".into()), sub_type: SubType::Auto }
    }
}

impl LangPrefs {
    /// e.g. "Japanese audio · English full subtitles".
    pub fn summary(&self) -> String {
        let audio = format!("{} audio", language_name(&self.audio));
        match &self.subtitles {
            None => format!("{audio} · no subtitles"),
            Some(lang) => {
                let kind = match self.sub_type {
                    SubType::Auto => "subtitles",
                    SubType::Full => "full subtitles",
                    SubType::Signs => "signs & songs",
                    SubType::Dubtitles => "dubtitles",
                    SubType::Sdh => "SDH subtitles",
                    SubType::Commentary => "commentary subtitles",
                };
                format!("{audio} · {} {kind}", language_name(lang))
            }
        }
    }

    /// Releases with this audio are worth preferring (a dub needs a
    /// dual-audio release).
    pub fn wants_dub(&self) -> bool {
        self.audio != "jpn"
    }

    /// A short key that changes whenever the choice would (to know a file is
    /// done).
    pub fn key(&self) -> String {
        format!("{}|{}|{:?}", self.audio, self.subtitles.as_deref().unwrap_or("-"), self.sub_type)
    }
}

/// Languages offered in the pickers.
pub const LANGUAGES: [&str; 20] = [
    "jpn", "eng", "spa", "por", "fre", "ger", "ita", "rus", "ara", "chi", "kor", "ind", "may", "tha", "vie", "pol", "tur", "dut", "swe",
    "hin",
];

/// A track's language: its tag, or, when that's "und", the language its name
/// starts with ("English (Full Subtitle)").
pub fn track_language(t: &TrackInfo) -> String {
    let code = normalize_language(&t.language);
    if code != "und" {
        return code;
    }
    static WORDS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
        let extra = [("castilian", "spa"), ("brazilian", "por"), ("mandarin", "chi"), ("cantonese", "chi"), ("norwegian", "nor"), ("danish", "dan"), ("finnish", "fin"), ("czech", "cze"), ("hungarian", "hun"), ("romanian", "rum"), ("greek", "gre"), ("hebrew", "heb"), ("ukrainian", "ukr")];
        LANGUAGES
            .iter()
            .map(|c| (language_name(c).to_lowercase(), *c))
            .chain(extra.iter().map(|(w, c)| ((*w).to_owned(), *c)))
            .map(|(w, c)| (Regex::new(&format!(r"(?i)\b{w}\b")).unwrap(), c))
            .collect()
    });
    WORDS
        .iter()
        .filter_map(|(re, c)| re.find(&t.name).map(|m| (m.start(), *c)))
        .min()
        .map_or(code, |(_, c)| c.to_owned())
}

/// What kind of subtitles a track holds, from its flags and name.
pub fn sub_type(t: &TrackInfo) -> SubType {
    sub_type_of(&t.name, t.forced, t.hearing_impaired, t.commentary)
}

/// What kind of subtitles a track with this name and these flags holds.
pub fn sub_type_of(name: &str, forced: bool, hearing_impaired: bool, commentary: bool) -> SubType {
    static COMMENTARY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bcommentary\b").unwrap());
    static SDH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(sdh|cc|hearing)\b").unwrap());
    static DUB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)dub").unwrap());
    static SIGNS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)sign|song|forced|karaoke|lyrics").unwrap());
    if commentary || COMMENTARY.is_match(name) {
        SubType::Commentary
    } else if hearing_impaired || SDH.is_match(name) {
        SubType::Sdh
    } else if DUB.is_match(name) {
        SubType::Dubtitles
    } else if forced || SIGNS.is_match(name) {
        SubType::Signs
    } else {
        SubType::Full
    }
}

/// Commentary or audio description: never the main audio.
fn audio_extra(t: &TrackInfo) -> bool {
    static EXTRA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)commentary|description").unwrap());
    t.commentary || EXTRA.is_match(&t.name)
}

/// The tracks to mark default in a file with these tracks: one audio track
/// and at most one subtitle track. Audio in the preferred language, else
/// whatever the file already plays; subtitles of the preferred kind, with
/// close substitutes (SDH for full) but never full subtitles instead of
/// signs only.
pub fn pick_defaults(tracks: &[TrackInfo], prefs: &LangPrefs) -> HashSet<u64> {
    let mut on = HashSet::new();
    let audio: Vec<&TrackInfo> = tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect();
    let rank = |t: &&&TrackInfo| (audio_extra(t), !t.default, t.number);
    let chosen = audio
        .iter()
        .filter(|t| track_language(t) == prefs.audio)
        .min_by_key(rank)
        .or_else(|| audio.iter().filter(|t| t.default).min_by_key(rank))
        .or_else(|| audio.iter().min_by_key(rank));
    if let Some(a) = chosen {
        on.insert(a.number);
    }
    let Some(lang) = &prefs.subtitles else { return on };
    let wanted = match prefs.sub_type {
        SubType::Auto if chosen.is_some_and(|a| track_language(a) == *lang) => SubType::Signs,
        SubType::Auto => SubType::Full,
        t => t,
    };
    let order = wanted.fallbacks();
    let subs: Vec<(&TrackInfo, SubType)> = tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Subtitles && track_language(t) == *lang)
        .map(|t| (t, sub_type(t)))
        .collect();
    // Styled text first (ASS, then SRT), then the file's own default.
    let codec_rank = |t: &TrackInfo| match t.codec_id.as_str() {
        "S_TEXT/ASS" | "S_ASS" | "S_TEXT/SSA" | "S_SSA" => 0,
        id if id.starts_with("S_TEXT") => 1,
        _ => 2,
    };
    for kind in order {
        if let Some((t, _)) = subs.iter().filter(|(_, k)| k == kind).min_by_key(|(t, _)| (codec_rank(t), !t.default, t.number)) {
            on.insert(t.number);
            break;
        }
    }
    on
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(number: u64, kind: TrackKind, lang: &str, name: &str, default: bool, forced: bool) -> TrackInfo {
        TrackInfo {
            number,
            uid: number,
            kind,
            codec_id: if kind == TrackKind::Subtitles { "S_TEXT/ASS".into() } else { "A_EAC3".into() },
            language: lang.into(),
            name: name.into(),
            default,
            forced,
            hearing_impaired: false,
            commentary: false,
            channels: None,
        }
    }

    /// The tracks of a real AnoZu WEB-DL (languages mostly "und", named instead).
    fn anozu() -> Vec<TrackInfo> {
        use TrackKind::*;
        vec![
            t(1, Video, "und", "CR WEB-DL", true, false),
            t(2, Audio, "jpn", "Japanese", true, false),
            t(3, Audio, "und", "English", true, false),
            t(4, Subtitles, "und", "English (Full Subtitle)", true, false),
            t(5, Subtitles, "und", "English (Signs) [Generated]", false, true),
            t(6, Subtitles, "und", "English (CC)", false, false),
            t(7, Subtitles, "ara", "Arabic (Saudi Arabia)", true, false),
            t(8, Subtitles, "spa", "Spanish (Latin America)", true, false),
        ]
    }

    fn prefs(audio: &str, subs: Option<&str>, sub_type: SubType) -> LangPrefs {
        LangPrefs { audio: audio.into(), subtitles: subs.map(Into::into), sub_type }
    }

    fn picked(tracks: &[TrackInfo], p: &LangPrefs) -> Vec<u64> {
        let mut v: Vec<u64> = pick_defaults(tracks, p).into_iter().collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn reads_languages_and_kinds_from_names() {
        let tr = anozu();
        let langs: Vec<String> = tr.iter().map(track_language).collect();
        assert_eq!(langs, ["und", "jpn", "eng", "eng", "eng", "eng", "ara", "spa"]);
        let kinds: Vec<SubType> = tr[3..6].iter().map(sub_type).collect();
        assert_eq!(kinds, [SubType::Full, SubType::Signs, SubType::Sdh]);
        assert_eq!(track_language(&t(1, TrackKind::Audio, "und", "Castilian", false, false)), "spa");
        assert_eq!(track_language(&t(1, TrackKind::Audio, "und", "Brazilian Portuguese", false, false)), "por");
        assert_eq!(sub_type(&t(1, TrackKind::Subtitles, "eng", "Dubtitle", false, false)), SubType::Dubtitles);
        assert_eq!(sub_type(&t(1, TrackKind::Subtitles, "eng", "Signs & Songs", false, false)), SubType::Signs);
    }

    #[test]
    fn picks_tracks_by_preference() {
        let tr = anozu();
        // Japanese with full English subtitles (the defaults).
        assert_eq!(picked(&tr, &LangPrefs::default()), [2, 4]);
        // The English dub: signs only, automatically.
        assert_eq!(picked(&tr, &prefs("eng", Some("eng"), SubType::Auto)), [3, 5]);
        assert_eq!(picked(&tr, &prefs("eng", Some("eng"), SubType::Full)), [3, 4]);
        assert_eq!(picked(&tr, &prefs("jpn", Some("eng"), SubType::Sdh)), [2, 6]);
        assert_eq!(picked(&tr, &prefs("jpn", None, SubType::Auto)), [2]);
        assert_eq!(picked(&tr, &prefs("jpn", Some("spa"), SubType::Auto)), [2, 8]);
        // No dubtitles: the full subtitles instead. No commentary: none.
        assert_eq!(picked(&tr, &prefs("jpn", Some("eng"), SubType::Dubtitles)), [2, 4]);
        assert_eq!(picked(&tr, &prefs("jpn", Some("eng"), SubType::Commentary)), [2]);
        // No such audio: the file's own default stays.
        assert_eq!(picked(&tr, &prefs("ger", Some("eng"), SubType::Auto)), [2, 4]);
        // Signs only are never swapped for full subtitles.
        let mut no_signs = tr.clone();
        no_signs.remove(4);
        assert_eq!(picked(&no_signs, &prefs("eng", Some("eng"), SubType::Auto)), [3]);
    }

    #[test]
    fn skips_audio_description() {
        use TrackKind::*;
        let tr = vec![
            t(2, Audio, "eng", "English", true, false),
            t(10, Audio, "jpn", "Japanese (Audio Description)", false, false),
            t(11, Audio, "jpn", "Japanese", false, false),
        ];
        assert_eq!(picked(&tr, &prefs("jpn", None, SubType::Auto)), [11]);
    }

    /// `DEFAULTS_SAMPLE=<real .mkv>`: on a copy of its first 32 MB, set the
    /// defaults for an English dub, in place, and check what the file says
    /// after. The original is only read.
    #[test]
    #[ignore = "needs DEFAULTS_SAMPLE"]
    fn sets_defaults_on_a_real_file() {
        use std::io::Read;
        let Some(sample) = std::env::var_os("DEFAULTS_SAMPLE") else { return };
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("sample.mkv");
        let mut head = Vec::new();
        std::fs::File::open(&sample).unwrap().take(32 << 20).read_to_end(&mut head).unwrap();
        std::fs::write(&copy, &head).unwrap();
        let before = crate::mkv::probe(&copy).unwrap().tracks;
        let dub = prefs("eng", Some("eng"), SubType::Auto);
        let on = pick_defaults(&before, &dub);
        println!("defaults for {}: {on:?}", dub.summary());
        let edit = crate::mkv::set_default_tracks(&copy, &on).unwrap();
        println!("edit: {edit:?}");
        assert_eq!(edit, crate::mkv::DefaultsEdit::InPlace);
        let after = crate::mkv::probe(&copy).unwrap().tracks;
        for t in &after {
            if matches!(t.kind, TrackKind::Audio | TrackKind::Subtitles) {
                assert_eq!(t.default, on.contains(&t.number), "track {} {:?}", t.number, t.name);
            }
            println!("  #{:<2} {:?} {:<4} {:<5} {}", t.number, t.kind, track_language(t), t.default, t.name);
        }
        let edited = std::fs::read(&copy).unwrap();
        let changed = head.iter().zip(&edited).filter(|(a, b)| a != b).count();
        let last = head.iter().zip(&edited).rposition(|(a, b)| a != b).unwrap_or(0);
        println!("{changed} bytes changed, the last at offset {last}");
        assert!(last < 64 << 10, "only the header changes");
        assert_eq!(edited.len(), head.len());
    }

    #[test]
    fn summaries() {
        assert_eq!(LangPrefs::default().summary(), "Japanese audio · English subtitles");
        assert_eq!(prefs("eng", Some("eng"), SubType::Signs).summary(), "English audio · English signs & songs");
        assert_eq!(prefs("jpn", None, SubType::Full).summary(), "Japanese audio · no subtitles");
        assert!(prefs("eng", None, SubType::Auto).wants_dub());
    }
}
