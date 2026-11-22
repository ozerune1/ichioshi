//! Parsing anime release names like
//! `[SubsPlease] Sousou no Frieren S2 (01-10) (1080p) [Batch]` or
//! `Frieren Beyond Journeys End S02E10 1080p CR WEB-DL DUAL AAC2.0 H 264-VARYG`.
//! Heuristic by nature: good enough to group and rank search results.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Bluray,
    Web,
    Unknown,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Bluray => "BD",
            Source::Web => "WEB",
            Source::Unknown => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTitle {
    pub group: Option<String>,
    /// The show's name as written, normalized (see [`normalize`]), without
    /// season or episode markers.
    pub series: String,
    /// Inclusive season range, if the title names one.
    pub seasons: Option<(u32, u32)>,
    /// Inclusive episode range; a single episode is `(n, n)`.
    pub episodes: Option<(u32, u32)>,
    pub resolution: Option<u32>,
    pub source: Source,
    pub dual_audio: bool,
    pub multi_audio: bool,
    pub dub_only: bool,
    /// Says "batch" or "complete".
    pub batch_word: bool,
}

impl ParsedTitle {
    /// The episode number, if this is a single-episode release.
    pub fn single_episode(&self) -> Option<u32> {
        match self.episodes {
            Some((a, b)) if a == b && !self.batch_word => Some(a),
            _ => None,
        }
    }
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

re!(GROUP_BRACKET, r"^\s*\[([^\]]+)\]");
re!(LEADING_BRACKETS, r"^\s*(?:\[[^\]]*\]\s*)+");
re!(
    GROUP_SCENE,
    r"-([A-Za-z0-9][A-Za-z0-9_.]*)\s*(?:[(\[][^)\]]*[)\]]\s*)*$"
);
re!(
    SXXEYY,
    r"(?i)\bS(\d{1,2})E(\d{1,4})(?:v\d)?(?:\s*[-~]\s*(?:S\d{1,2})?E?(\d{1,4}))?\b"
);
re!(SEASON_RANGE, r"(?i)\bS(\d{1,2})\s*[-~]\s*S?(\d{1,2})\b");
re!(SEASON_SHORT, r"(?i)\bS(\d{1,2})(?:v\d)?\b");
re!(SEASON_WORD, r"(?i)\bSeason\s*(\d{1,2})\b");
re!(SEASON_ORDINAL, r"(?i)\b(\d{1,2})(?:st|nd|rd|th)\s+Season\b");
re!(EP_RANGE_PAREN, r"[(\[](\d{1,4})\s*[-~]\s*(\d{1,4})[)\]]");
re!(
    EP_RANGE_DASH,
    r"\s-\s(\d{1,4})\s*[-~]\s*(\d{1,4})(?:\s|$|[(\[])"
);
re!(EP_SINGLE_DASH, r"\s-\s(\d{1,4})(?:v\d)?(?:\s|$|[(\[])");
re!(EP_WORD, r"(?i)\b(?:(?:EP|Episode)\s?|E)(\d{1,4})\b");
re!(RESOLUTION, r"(?i)(2160|1080|720|576|480)[pi]\b");
re!(UHD, r"(?i)\b(4K|UHD)\b");
re!(
    BLURAY,
    r"(?i)\b(blu-?ray|bd|bdrip|bd-rip|bdremux|bdmv|remux|dvd|dvdrip)\b|(?i)\bbd\d{3,4}p\b"
);
re!(
    WEB,
    r"(?i)\b(web|web-?dl|web-?rip|webrip|cr|amzn|nf|dsnp|hidive|adn|b-global|bilibili|baha|funi|yt)\b"
);
re!(DUAL_AUDIO, r"(?i)dual[ ._-]?audio|\bDUAL\b");
re!(MULTI_AUDIO, r"(?i)multi[ ._-]?audio|\bMULTi\b");
re!(DUB, r"(?i)\b(english\s+dub|dubbed|dub)\b");
re!(BATCH, r"(?i)\b(batch|complete)\b");
re!(NON_ALNUM, r"[^\p{L}\p{N}]+");

/// Lowercase, drop apostrophes, and turn other punctuation into single spaces,
/// so `Frieren: Beyond Journey's End` and `Frieren Beyond Journeys End` compare equal.
pub fn normalize(s: &str) -> String {
    let no_apostrophes: String = s
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '`'))
        .collect();
    NON_ALNUM
        .replace_all(&no_apostrophes.to_lowercase(), " ")
        .trim()
        .to_owned()
}

pub fn parse(title: &str) -> ParsedTitle {
    let group = GROUP_BRACKET
        .captures(title)
        .map(|c| c[1].trim().to_owned())
        .or_else(|| GROUP_SCENE.captures(title).map(|c| c[1].to_owned()));

    let mut seasons = None;
    let mut episodes = None;
    if let Some(c) = SXXEYY.captures(title) {
        let s: u32 = c[1].parse().unwrap_or(1);
        let a: u32 = c[2].parse().unwrap_or(0);
        let b = c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(a);
        seasons = Some((s, s));
        episodes = Some((a, b.max(a)));
    }
    if seasons.is_none() {
        seasons = SEASON_RANGE
            .captures(title)
            .map(|c| (c[1].parse().unwrap_or(1), c[2].parse().unwrap_or(1)))
            .or_else(|| {
                [&*SEASON_ORDINAL, &*SEASON_WORD, &*SEASON_SHORT]
                    .iter()
                    .find_map(|re| re.captures(title))
                    .map(|c| {
                        let s = c[1].parse().unwrap_or(1);
                        (s, s)
                    })
            });
    }
    if episodes.is_none() {
        episodes = [&*EP_RANGE_PAREN, &*EP_RANGE_DASH]
            .iter()
            .find_map(|re| re.captures(title))
            .map(|c| (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0)))
            .filter(|(a, b)| a < b)
            .or_else(|| {
                [&*EP_SINGLE_DASH, &*EP_WORD]
                    .iter()
                    .find_map(|re| re.captures(title))
                    .and_then(|c| c[1].parse().ok())
                    .map(|n| (n, n))
            });
    }

    let resolution = RESOLUTION
        .captures(title)
        .and_then(|c| c[1].parse().ok())
        .or_else(|| UHD.is_match(title).then_some(2160));
    let source = if BLURAY.is_match(title) {
        Source::Bluray
    } else if WEB.is_match(title) {
        Source::Web
    } else {
        Source::Unknown
    };
    let dual_audio = DUAL_AUDIO.is_match(title);

    ParsedTitle {
        group,
        series: series_name(title),
        seasons,
        episodes,
        resolution,
        source,
        dual_audio,
        multi_audio: MULTI_AUDIO.is_match(title),
        dub_only: !dual_audio && DUB.is_match(title),
        batch_word: BATCH.is_match(title),
    }
}

/// The show name: the text after any leading `[...]` tags, up to the first
/// season/episode marker, bracket, or ` | `.
fn series_name(title: &str) -> String {
    let rest = LEADING_BRACKETS.replace(title, "");
    let mut end = rest.len();
    for re in [
        &*SXXEYY,
        &*SEASON_RANGE,
        &*SEASON_ORDINAL,
        &*SEASON_WORD,
        &*SEASON_SHORT,
        &*EP_RANGE_DASH,
        &*EP_SINGLE_DASH,
        &*RESOLUTION,
    ] {
        if let Some(m) = re.find(&rest) {
            end = end.min(m.start());
        }
    }
    for pat in ["[", "(", " | "] {
        if let Some(i) = rest.find(pat) {
            end = end.min(i);
        }
    }
    normalize(&rest[..end])
}

/// An AniList search query for a release or folder name: the show's name,
/// plus the season when it's past the first.
pub fn anilist_query(name: &str) -> String {
    let p = parse(name);
    match p.seasons {
        _ if p.series.is_empty() => name.to_owned(),
        Some((s, _)) if s > 1 => format!("{} season {s}", p.series),
        _ => p.series,
    }
}

/// Split an AniList title like `Sousou no Frieren 2nd Season` into the
/// base name and season number.
pub fn split_season(title: &str) -> (String, Option<u32>) {
    static TRAILING: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
            r"(?i)\s*\b(\d{1,2})(?:st|nd|rd|th)\s+Season\s*$",
            r"(?i)\s*\bSeason\s*(\d{1,2})\s*$",
            r"(?i)\s*\bS(\d{1,2})\s*$",
            r"\s+(II|III|IV|V|VI)\s*$",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });
    static PART: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\s*(?:\bPart\s*\d{1,2}|\bCour\s*\d{1,2})\s*$").unwrap());

    let without_part = PART.replace(title.trim(), "").into_owned();
    for re in TRAILING.iter() {
        if let Some(c) = re.captures(&without_part) {
            let n = match &c[1] {
                "II" => 2,
                "III" => 3,
                "IV" => 4,
                "V" => 5,
                "VI" => 6,
                digits => digits.parse().unwrap_or(1),
            };
            let base = without_part[..c.get(0).unwrap().start()]
                .trim_end_matches([' ', ':', '-'])
                .to_owned();
            return (base, Some(n));
        }
    }
    (without_part, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsplease_batch() {
        let p = parse("[SubsPlease] Sousou no Frieren S2 (01-10) (1080p) [Batch]");
        assert_eq!(p.group.as_deref(), Some("SubsPlease"));
        assert_eq!(p.series, "sousou no frieren");
        assert_eq!(p.seasons, Some((2, 2)));
        assert_eq!(p.episodes, Some((1, 10)));
        assert_eq!(p.resolution, Some(1080));
        assert!(p.batch_word);
        assert_eq!(p.single_episode(), None);
    }

    #[test]
    fn subsplease_episode() {
        let p = parse("[SubsPlease] Ore dake Level Up na Ken - 07 (1080p) [ABCD1234].mkv");
        assert_eq!(p.series, "ore dake level up na ken");
        assert_eq!(p.single_episode(), Some(7));
        assert_eq!(p.seasons, None);
        assert_eq!(p.source, Source::Unknown);
    }

    #[test]
    fn scene_style_episode() {
        let p = parse(
            "Frieren Beyond Journeys End S02E10 A Beautiful Sight 1080p CR WEB-DL DUAL AAC2.0 H 264-VARYG (Sousou no Frieren, Dual-Audio, Multi-Subs)",
        );
        assert_eq!(p.group.as_deref(), Some("VARYG"));
        assert_eq!(p.series, "frieren beyond journeys end");
        assert_eq!(p.seasons, Some((2, 2)));
        assert_eq!(p.single_episode(), Some(10));
        assert_eq!(p.source, Source::Web);
        assert!(p.dual_audio);
    }

    #[test]
    fn season_words_and_bd() {
        let p = parse(
            "[Erai-raws] Sousou no Frieren 2nd Season [1080p CR WEBRip HEVC AAC][MultiSub] (unofficial batch)",
        );
        assert_eq!(p.seasons, Some((2, 2)));
        assert_eq!(p.series, "sousou no frieren");
        assert!(p.batch_word);

        let p = parse(
            "[DB] Sousou no Frieren 2nd Season | Frieren: Beyond Journey's End Season 2 [Dual Audio 10bit BD1080p][HEVC-x265]",
        );
        assert_eq!(p.source, Source::Bluray);
        assert_eq!(p.resolution, Some(1080));
        assert!(p.dual_audio);

        let p = parse(
            "[AUTISM] Sousou no Frieren - S01v2 (BD Remux 1080p AVC FLAC/TrueHD/AAC 2.0/5.1) [Dual-Audio]",
        );
        assert_eq!(p.seasons, Some((1, 1)));
        assert_eq!(p.episodes, None);
        assert_eq!(p.series, "sousou no frieren");

        let p = parse(
            "[Judas] Sousou no Frieren (Frieren Beyond Journey`s End) (Season 02) [1080p][HEVC x265 10bit][Dual-Audio][Multi-Subs] (Batch)",
        );
        assert_eq!(p.seasons, Some((2, 2)));
    }

    #[test]
    fn dash_episode_with_bracket_after() {
        let p = parse(
            "[Piyoko] Sousou no Frieren - Marumaru no Mahou (Mini Anime) - 24 [WEB 1080p YT VP9 AAC 2.0]",
        );
        assert_eq!(p.single_episode(), Some(24));
        assert_eq!(p.source, Source::Web);
    }

    #[test]
    fn dub_only() {
        let p = parse(
            "[Yameii] Frieren: Beyond Journey's End - S02E10 [English Dub] [CR WEB-DL 1080p H264 AAC] [60B4DD4F]",
        );
        assert!(p.dub_only);
        assert_eq!(p.single_episode(), Some(10));
    }

    #[test]
    fn splits_anilist_seasons() {
        assert_eq!(
            split_season("Sousou no Frieren 2nd Season"),
            ("Sousou no Frieren".into(), Some(2))
        );
        assert_eq!(
            split_season("Frieren: Beyond Journey’s End Season 2"),
            ("Frieren: Beyond Journey’s End".into(), Some(2))
        );
        assert_eq!(split_season("Overlord II"), ("Overlord".into(), Some(2)));
        assert_eq!(
            split_season("Dr. Stone: Science Future Part 2"),
            ("Dr. Stone: Science Future".into(), None)
        );
        assert_eq!(
            split_season("Sousou no Frieren"),
            ("Sousou no Frieren".into(), None)
        );
    }

    #[test]
    fn bare_episode_markers() {
        assert_eq!(parse("Show E07 [1080p]").single_episode(), Some(7));
        assert_eq!(parse("E01").single_episode(), Some(1));
        assert_eq!(
            parse("Show - 03 [1080p E-AC-3 EAC3]").single_episode(),
            Some(3)
        );
        assert_eq!(parse("Show S01 [1080p WEB E-AC-3]").episodes, None);
    }

    #[test]
    fn builds_anilist_queries() {
        assert_eq!(
            anilist_query("The Ramparts of Ice Season 2 [VARYG]"),
            "the ramparts of ice season 2"
        );
        assert_eq!(
            anilist_query(
                "Mobile.Suit.Gundam.The.Witch.from.Mercury.S01.1080p.BluRay.Remux-DemiHuman"
            ),
            "mobile suit gundam the witch from mercury"
        );
        assert_eq!(anilist_query("[PMR] Frieren (BD 1080p)"), "frieren");
    }

    #[test]
    fn normalizes() {
        assert_eq!(
            normalize("Frieren: Beyond Journey’s End"),
            "frieren beyond journeys end"
        );
        assert_eq!(
            normalize("  Re:Zero -Starting Life-  "),
            "re zero starting life"
        );
    }
}

/// The episode a video file holds, from its name ("Show - 07 [1080p].mkv",
/// "Show.S01E07.1080p.mkv"); `None` for openings, endings, OVAs, specials
/// and the like.
pub fn episode_of_file(name: &str) -> Option<u32> {
    static EXTRA: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(NC ?(?:OP|ED)|OVA|OAD|SP ?\d|Specials?|Menu|PV|CM|Preview|Trailer|Creditless|Opening|Ending)\b").unwrap()
    });
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    if EXTRA.is_match(stem) {
        return None;
    }
    parse(stem).single_episode()
}

#[cfg(test)]
mod episode_tests {
    use super::episode_of_file;

    #[test]
    fn reads_episode_numbers_from_file_names() {
        for (name, ep) in [
            ("Show - 01.mkv", Some(1)),
            (
                "The.Way.of.the.Househusband.S01E01.Episode.1.1080p.NF.WEB-DL.DDP5.1.H.264-ZigZag.mkv",
                Some(1),
            ),
            (
                "[Erai-raws] Ojisan wa Kawaii Mono ga Osuki - 08 [480p CR WEB-DL AVC AAC][MultiSub][EC4CCA8C].mkv",
                Some(8),
            ),
            (
                "The.Laid-Off.Cheat-Granting.Mage.Enjoys.a.Second.Lease.on.Life.2026.S01E06.1080p.CR.WEB-DL.DUAL.DDP2.0.H.264-AnoZu.mkv",
                Some(6),
            ),
            ("Frieren: Beyond Journey’s End - 03.mkv", Some(3)),
            (
                "[SubsPlease] Ojisan wa Kawaii Mono ga Osuki. - 08 (1080p) [1C1B1F5D].mkv",
                Some(8),
            ),
            ("Show - 03v2.mkv", Some(3)),
            (
                "[DragsterPS] The Way of the Househusband S01E01 [1080p] [Multi-Audio] [Multi-Sub] [B11F2F18].mkv",
                Some(1),
            ),
            ("NCOP.mkv", None),
            ("[Group] Show - NCED 2 [1080p].mkv", None),
            ("[Group] Show - OVA 01 [1080p].mkv", None),
        ] {
            assert_eq!(episode_of_file(name), ep, "{name}");
        }
    }
}
