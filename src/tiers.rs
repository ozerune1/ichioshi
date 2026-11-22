//! Release-group rankings from the TRaSH Guides anime custom formats
//! (assets/trash, MIT license), applied the way Sonarr applies them: a
//! release gets a tier's score if its title matches one of the tier's group
//! patterns and its source (Blu-ray or web) fits the tier.

use std::sync::LazyLock;

use fancy_regex::Regex;
use serde::Deserialize;

use crate::title::Source;

const FILES: &[&str] = &[
    include_str!("../assets/trash/anime-bd-tier-01.json"),
    include_str!("../assets/trash/anime-bd-tier-02.json"),
    include_str!("../assets/trash/anime-bd-tier-03.json"),
    include_str!("../assets/trash/anime-bd-tier-04.json"),
    include_str!("../assets/trash/anime-bd-tier-05.json"),
    include_str!("../assets/trash/anime-bd-tier-06.json"),
    include_str!("../assets/trash/anime-bd-tier-07.json"),
    include_str!("../assets/trash/anime-bd-tier-08.json"),
    include_str!("../assets/trash/anime-web-tier-01.json"),
    include_str!("../assets/trash/anime-web-tier-02.json"),
    include_str!("../assets/trash/anime-web-tier-03.json"),
    include_str!("../assets/trash/anime-web-tier-04.json"),
    include_str!("../assets/trash/anime-web-tier-05.json"),
    include_str!("../assets/trash/anime-web-tier-06.json"),
    include_str!("../assets/trash/anime-lq-groups.json"),
    include_str!("../assets/trash/anime-raws.json"),
];

/// Sonarr's source IDs, as used in the tiers' `SourceSpecification`s.
const SONARR_WEB_SOURCES: [i64; 3] = [1, 3, 4]; // WEB, WEBDL, WEBRIP
const SONARR_BD_SOURCES: [i64; 3] = [5, 6, 7]; // DVD, Bluray, Bluray Remux

#[derive(Deserialize)]
struct CustomFormat {
    name: String,
    #[serde(default)]
    trash_scores: Scores,
    specifications: Vec<Specification>,
}

#[derive(Deserialize, Default)]
struct Scores {
    default: Option<i32>,
}

#[derive(Deserialize)]
struct Specification {
    name: String,
    implementation: String,
    #[serde(default)]
    negate: bool,
    fields: Fields,
}

#[derive(Deserialize)]
struct Fields {
    value: serde_json::Value,
}

struct Tier {
    label: String,
    score: i32,
    /// `None` if the tier applies to any source.
    source: Option<Source>,
    groups: Vec<(String, Regex)>,
}

/// How a release ranks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rank {
    /// TRaSH score: 1400 (BD Tier 01) down to 100 (Web Tier 06), 0 if the
    /// group isn't ranked, very negative for low-quality groups and raws.
    pub score: i32,
    /// e.g. "BD Tier 01", "Web Tier 05", "Unranked", "Low-quality group".
    pub label: String,
}

impl Rank {
    pub fn is_blocked(&self) -> bool {
        self.score < 0
    }
}

pub struct Tiers {
    tiers: Vec<Tier>,
}

static TIERS: LazyLock<Tiers> =
    LazyLock::new(|| Tiers::from_json(FILES).expect("bundled TRaSH tiers are valid"));

pub fn tiers() -> &'static Tiers {
    &TIERS
}

impl Tiers {
    fn from_json(files: &[&str]) -> anyhow::Result<Self> {
        let mut tiers = Vec::new();
        for json in files {
            let cf: CustomFormat = serde_json::from_str(json)?;
            let Some(score) = cf.trash_scores.default else {
                continue;
            };
            let mut source = None;
            let mut groups = Vec::new();
            for spec in &cf.specifications {
                match spec.implementation.as_str() {
                    "SourceSpecification" if !spec.negate => {
                        let id = spec.fields.value.as_i64().unwrap_or_default();
                        if SONARR_WEB_SOURCES.contains(&id) {
                            source = Some(Source::Web);
                        } else if SONARR_BD_SOURCES.contains(&id) {
                            source = Some(Source::Bluray);
                        }
                    }
                    "ReleaseTitleSpecification" if !spec.negate => {
                        let Some(pattern) = spec.fields.value.as_str() else {
                            continue;
                        };
                        // Sonarr matches release titles case-insensitively.
                        match Regex::new(&format!("(?i){pattern}")) {
                            Ok(re) => groups.push((spec.name.clone(), re)),
                            Err(e) => tracing::warn!("skipping TRaSH pattern {:?}: {e}", spec.name),
                        }
                    }
                    _ => {}
                }
            }
            tiers.push(Tier {
                label: tier_label(&cf.name),
                score,
                source,
                groups,
            });
        }
        Ok(Self { tiers })
    }

    /// Rank a release by its title. Like Sonarr, penalties (low-quality
    /// groups, raws) add up with the best matching tier. When the title
    /// doesn't say whether it's Blu-ray or web, either kind of tier counts.
    pub fn rank(&self, title: &str, source: Source) -> Rank {
        let mut best: Option<&Tier> = None;
        let mut penalty = 0;
        let mut penalty_label = None;
        for tier in &self.tiers {
            let source_fits = match (tier.source, source) {
                (None, _) | (_, Source::Unknown) => true,
                (Some(want), have) => want == have,
            };
            if !source_fits
                || !tier
                    .groups
                    .iter()
                    .any(|(_, re)| re.is_match(title).unwrap_or(false))
            {
                continue;
            }
            if tier.score < 0 {
                penalty += tier.score;
                penalty_label.get_or_insert_with(|| tier.label.clone());
            } else if best.is_none_or(|b| tier.score > b.score) {
                best = Some(tier);
            }
        }
        let score = best.map_or(0, |t| t.score) + penalty;
        let label = match (penalty_label, best) {
            (Some(p), _) => p,
            (None, Some(t)) => t.label.clone(),
            (None, None) => "Unranked".to_owned(),
        };
        Rank { score, label }
    }
}

/// "Anime BD Tier 01" -> "BD Tier 01"; "Anime LQ Groups" -> "Low-quality group".
fn tier_label(name: &str) -> String {
    match name {
        "Anime LQ Groups" => "Low-quality group".to_owned(),
        "Anime Raws" => "Raw (no subtitles)".to_owned(),
        other => other.strip_prefix("Anime ").unwrap_or(other).to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_bundled_patterns_compile() {
        let t = tiers();
        assert_eq!(t.tiers.len(), 16);
        let patterns: usize = t.tiers.iter().map(|t| t.groups.len()).sum();
        assert!(patterns > 350, "only {patterns} patterns compiled");
    }

    #[test]
    fn ranks_known_groups() {
        let t = tiers();
        let r = t.rank(
            "[SubsPlease] Sousou no Frieren S2 (01-10) (1080p) [Batch]",
            Source::Unknown,
        );
        assert_eq!((r.label.as_str(), r.score), ("Web Tier 05", 200));

        let r = t.rank(
            "Frieren Beyond Journeys End S02 1080p CR WEB-DL DUAL AAC2.0 H 264-VARYG",
            Source::Web,
        );
        assert_eq!(r.label, "Web Tier 04");

        // Same group, different tier depending on source.
        let bd = t.rank("[LostYears] Show S01 (BD 1080p)", Source::Bluray);
        let web = t.rank("[LostYears] Show S01 (WEB 1080p)", Source::Web);
        assert_eq!(bd.label, "BD Tier 02");
        assert_eq!(web.label, "Web Tier 01");

        let r = t.rank("[Some Random Group] Show - 01 (1080p)", Source::Unknown);
        assert_eq!((r.label.as_str(), r.score), ("Unranked", 0));
    }

    #[test]
    fn lookaround_patterns_work() {
        let t = tiers();
        // PMR only counts for remuxes; Aergia must not match Aergia-raws.
        assert_eq!(
            t.rank("[PMR] Show (BD Remux 1080p)", Source::Bluray).label,
            "BD Tier 03"
        );
        assert_eq!(
            t.rank("[PMR] Show (BD 1080p)", Source::Bluray).label,
            "Unranked"
        );
        assert_ne!(
            t.rank("Show S01 1080p BluRay-Aergia-raws", Source::Bluray)
                .label,
            "BD Tier 02"
        );
    }

    #[test]
    fn low_quality_groups_are_blocked() {
        let r = tiers().rank(
            "[Anime Time] Show - S02E10 [1080p][HEVC 10bit x265][AAC][Dual-Audio]",
            Source::Unknown,
        );
        assert!(r.is_blocked(), "{r:?}");
    }
}
