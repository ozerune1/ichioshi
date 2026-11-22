//! Finding the best Nyaa release for anime that SeaDex doesn't cover.
//!
//! Search Nyaa by the anime's names, keep results for the right show and
//! season, group per-episode torrents from the same group and quality into
//! one release, and rank releases by TRaSH Guides release-group tier.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::nyaa::{self, NyaaItem};
use crate::seadex::Anime;
use crate::tiers::{Rank, tiers};
use crate::title::{self, Source};

/// A release on Nyaa: one torrent, or a set of single-episode torrents.
#[derive(Debug, Clone)]
pub struct NyaaRelease {
    pub group: String,
    pub rank: Rank,
    pub resolution: Option<u32>,
    pub source: Source,
    pub dual_audio: bool,
    pub kind: ReleaseKind,
    pub size: u64,
    pub min_seeders: u32,
    pub max_seeders: u32,
    pub remake: bool,
    pub trusted: bool,
    /// Has clearly fewer episodes than the most complete usable release.
    pub incomplete: bool,
}

#[derive(Debug, Clone)]
pub enum ReleaseKind {
    /// A batch, a movie, or anything else that is one torrent.
    Torrent {
        item: NyaaItem,
        episodes: Option<(u32, u32)>,
    },
    /// One torrent per episode, sorted by episode.
    Episodes(Vec<(u32, NyaaItem)>),
}

impl NyaaRelease {
    /// A representative release title.
    pub fn title(&self) -> &str {
        match &self.kind {
            ReleaseKind::Torrent { item, .. } => &item.title,
            ReleaseKind::Episodes(eps) => &eps[0].1.title,
        }
    }

    pub fn usable(&self) -> bool {
        !self.rank.is_blocked() && !self.remake && self.max_seeders > 0
    }

    /// Why a release isn't usable, for display.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rank.is_blocked() {
            Some("Low-quality group or raw")
        } else if self.remake {
            Some("Marked as a remake on Nyaa")
        } else if self.max_seeders == 0 {
            Some("No seeders")
        } else {
            None
        }
    }

    pub fn episodes_label(&self) -> String {
        match &self.kind {
            ReleaseKind::Torrent { episodes: Some((a, b)), .. } if a < b => format!("{a:02}–{b:02}"),
            ReleaseKind::Torrent { episodes: Some((a, _)), .. } => format!("Episode {a:02}"),
            ReleaseKind::Torrent { episodes: None, .. } => "Whole release".to_owned(),
            ReleaseKind::Episodes(eps) => {
                let n = eps.len();
                let list = compress_ranges(eps.iter().map(|(e, _)| *e));
                if n == 1 { format!("Episode {list}") } else { format!("{n} episodes ({list})") }
            }
        }
    }

    pub fn quality_label(&self) -> String {
        let res = self.resolution.map(|r| format!("{r}p"));
        [res.as_deref(), Some(self.source.label())]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn seeders_label(&self) -> String {
        if self.min_seeders == self.max_seeders {
            self.max_seeders.to_string()
        } else {
            format!("{}–{}", self.min_seeders, self.max_seeders)
        }
    }

    /// How many episodes this has, if known (a season batch that doesn't
    /// list its episode range isn't).
    fn episode_count(&self) -> Option<u32> {
        match &self.kind {
            ReleaseKind::Torrent { episodes: Some((a, b)), .. } => Some(b - a + 1),
            ReleaseKind::Torrent { episodes: None, .. } => None,
            ReleaseKind::Episodes(eps) => Some(eps.len() as u32),
        }
    }

    /// Unknown counts are assumed to be whole-season batches.
    fn coverage(&self) -> u32 {
        self.episode_count().unwrap_or(10_000)
    }
}

/// What to search for and how to filter the results.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPlan {
    pub queries: Vec<String>,
    /// Normalized show names a result's title must contain (empty = no filter).
    pub names: Vec<String>,
    /// The season results must be from (`None` = no filter).
    pub season: Option<u32>,
}

impl SearchPlan {
    /// The default plan for an anime: search its romaji and English names
    /// (without "2nd Season" and the like) and filter to its season.
    pub fn for_anime(anime: &Anime) -> Self {
        let mut names = Vec::new();
        let mut season = None;
        for t in [&anime.romaji, &anime.english].into_iter().flatten() {
            let (base, s) = title::split_season(t);
            season = season.or(s);
            let base = title::normalize(&base);
            if !base.is_empty() && !names.contains(&base) {
                names.push(base);
            }
        }
        // Seasons only make sense for series; movies and specials are named freely.
        let is_series = matches!(anime.format.as_deref(), Some("TV" | "TV_SHORT" | "ONA") | None);
        Self {
            queries: names.clone(),
            names,
            season: is_series.then_some(season.unwrap_or(1)),
        }
    }

    /// A user-typed query: search exactly that, without filtering.
    pub fn custom(query: &str) -> Self {
        Self { queries: vec![query.trim().to_owned()], names: Vec::new(), season: None }
    }

    pub fn display_query(&self) -> String {
        self.queries.first().cloned().unwrap_or_default()
    }
}

/// How many of the top per-episode releases get a follow-up search.
const FILL_IN_GROUPS: usize = 6;

/// Search Nyaa and rank what it finds, best first.
pub async fn find_releases(client: &reqwest::Client, plan: &SearchPlan, prefer_dual_audio: bool) -> Result<Vec<NyaaRelease>> {
    let queries: Vec<String> = plan.queries.iter().filter(|q| !q.is_empty()).cloned().collect();
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut last_error = None;
    for result in search_all(client, queries.clone()).await {
        match result {
            Ok(found) => items.extend(found.into_iter().filter(|i| seen.insert(i.id))),
            Err(e) => last_error = Some(e),
        }
    }
    if items.is_empty()
        && let Some(e) = last_error
    {
        bail!(e.context("Nyaa search failed"));
    }
    let releases = rank_items(items.clone(), plan, prefer_dual_audio);

    // Nyaa returns at most 75 results per search, most-seeded first, which
    // cuts off older episodes of per-episode releases. Search again for the
    // top groups so their episode lists are complete before ranking for real.
    // Pick them by tier alone: their episode lists are still cut off, so
    // they may look incomplete for now.
    let mut candidates: Vec<&NyaaRelease> = releases
        .iter()
        .filter(|r| r.usable() && matches!(r.kind, ReleaseKind::Episodes(_)))
        .collect();
    candidates.sort_by_key(|r| std::cmp::Reverse(r.rank.score));
    let mut groups: Vec<&str> = Vec::new();
    for r in candidates {
        if !groups.contains(&r.group.as_str()) {
            groups.push(&r.group);
        }
    }
    let follow_ups: Vec<String> = groups
        .iter()
        .take(FILL_IN_GROUPS)
        .flat_map(|g| queries.iter().map(move |q| format!("{g} {q}")))
        .collect();
    if follow_ups.is_empty() {
        return Ok(releases);
    }
    for found in search_all(client, follow_ups).await.into_iter().flatten() {
        items.extend(found.into_iter().filter(|i| seen.insert(i.id)));
    }
    Ok(rank_items(items, plan, prefer_dual_audio))
}

/// Run Nyaa searches, a few at a time. Results are in query order.
async fn search_all(client: &reqwest::Client, queries: Vec<String>) -> Vec<Result<Vec<NyaaItem>>> {
    let limit = Arc::new(tokio::sync::Semaphore::new(3));
    let tasks: Vec<_> = queries
        .into_iter()
        .map(|q| {
            let client = client.clone();
            let limit = limit.clone();
            tokio::spawn(async move {
                let _permit = limit.acquire_owned().await;
                nyaa::search(&client, &q).await
            })
        })
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap_or_else(|e| Err(anyhow::anyhow!("search task failed: {e}"))));
    }
    results
}

/// Filter, group and rank search results. Best release first.
pub fn rank_items(items: Vec<NyaaItem>, plan: &SearchPlan, prefer_dual_audio: bool) -> Vec<NyaaRelease> {
    let tiers = tiers();
    let mut releases = Vec::new();
    // Single-episode torrents, keyed by everything that makes them "the same release".
    type Key = (String, String, Option<(u32, u32)>, Option<u32>, Source, bool, bool, bool);
    let mut episodic: BTreeMap<Key, Vec<(u32, NyaaItem, Rank)>> = BTreeMap::new();

    for item in items {
        if is_archive(&item.title) || !matches_plan(&item.title, plan) {
            continue;
        }
        let parsed = title::parse(&item.title);
        if let Some(target) = plan.season {
            let (a, b) = parsed.seasons.unwrap_or((1, 1));
            if !(a..=b).contains(&target) {
                continue;
            }
        }
        let rank = tiers.rank(&item.title, parsed.source);
        if let Some(ep) = parsed.single_episode() {
            let key = (
                parsed.group.clone().unwrap_or_default().to_lowercase(),
                parsed.series.clone(),
                parsed.seasons,
                parsed.resolution,
                parsed.source,
                parsed.dual_audio,
                parsed.multi_audio,
                parsed.dub_only,
            );
            episodic.entry(key).or_default().push((ep, item, rank));
        } else {
            releases.push(NyaaRelease {
                group: parsed.group.unwrap_or_else(|| "Unknown group".to_owned()),
                rank,
                resolution: parsed.resolution,
                source: parsed.source,
                dual_audio: parsed.dual_audio,
                size: item.size,
                min_seeders: item.seeders,
                max_seeders: item.seeders,
                remake: item.remake,
                trusted: item.trusted,
                incomplete: false,
                kind: ReleaseKind::Torrent { episodes: parsed.episodes, item },
            });
        }
    }

    for ((_, _, _, resolution, source, dual_audio, _, _), mut eps) in episodic {
        // Keep the best-seeded torrent for each episode (e.g. a v2 over a v1).
        eps.sort_by_key(|(ep, item, _)| (*ep, std::cmp::Reverse(item.seeders)));
        eps.dedup_by_key(|(ep, _, _)| *ep);
        let group = title::parse(&eps[0].1.title)
            .group
            .unwrap_or_else(|| "Unknown group".to_owned());
        let rank = eps.iter().map(|(_, _, r)| r.clone()).max_by_key(|r| r.score).unwrap();
        releases.push(NyaaRelease {
            group,
            rank,
            resolution,
            source,
            dual_audio,
            size: eps.iter().map(|(_, i, _)| i.size).sum(),
            min_seeders: eps.iter().map(|(_, i, _)| i.seeders).min().unwrap_or(0),
            max_seeders: eps.iter().map(|(_, i, _)| i.seeders).max().unwrap_or(0),
            remake: eps.iter().all(|(_, i, _)| i.remake),
            trusted: eps.iter().all(|(_, i, _)| i.trusted),
            incomplete: false,
            kind: ReleaseKind::Episodes(eps.into_iter().map(|(e, i, _)| (e, i)).collect()),
        });
    }

    // A release with far fewer episodes than others can't give you the
    // season, however well its group ranks.
    let most_episodes = releases
        .iter()
        .filter(|r| r.usable())
        .filter_map(|r| r.episode_count())
        .max()
        .unwrap_or(0);
    for r in &mut releases {
        r.incomplete = r.episode_count().is_some_and(|n| n * 5 < most_episodes * 4);
    }

    releases.sort_by_key(|r| {
        std::cmp::Reverse((
            r.usable(),
            !r.incomplete,
            r.rank.score,
            prefer_dual_audio && r.dual_audio,
            resolution_preference(r.resolution),
            r.coverage(),
            r.trusted,
            r.max_seeders,
        ))
    });
    releases
}

/// 1080p first, then unknown (usually 1080p), 2160p, 720p, anything else.
fn resolution_preference(res: Option<u32>) -> u8 {
    match res {
        Some(1080) => 4,
        None => 3,
        Some(2160) => 2,
        Some(720) => 1,
        _ => 0,
    }
}

fn matches_plan(title: &str, plan: &SearchPlan) -> bool {
    if plan.names.is_empty() {
        return true;
    }
    let padded = format!(" {} ", title::normalize(title));
    plan.names.iter().any(|n| padded.contains(&format!(" {n} ")))
}

fn is_archive(title: &str) -> bool {
    let t = title.to_lowercase();
    [".zip", ".rar", ".7z"].iter().any(|ext| t.ends_with(ext))
}

/// [1, 2, 3, 5, 7, 8] -> "01–03, 05, 07–08"
fn compress_ranges(eps: impl IntoIterator<Item = u32>) -> String {
    let mut parts = Vec::new();
    let mut run: Option<(u32, u32)> = None;
    for e in eps {
        run = match run {
            Some((a, b)) if e == b + 1 => Some((a, e)),
            Some((a, b)) => {
                parts.push((a, b));
                Some((e, e))
            }
            None => Some((e, e)),
        };
    }
    parts.extend(run);
    parts
        .into_iter()
        .map(|(a, b)| if a == b { format!("{a:02}") } else { format!("{a:02}–{b:02}") })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, title: &str, seeders: u32) -> NyaaItem {
        NyaaItem {
            id,
            title: title.into(),
            seeders,
            size: 1_000,
            info_hash: String::new(),
            trusted: false,
            remake: false,
        }
    }

    fn anime(romaji: &str, english: Option<&str>) -> Anime {
        Anime {
            anilist_id: 1,
            title: romaji.into(),
            alt_title: None,
            romaji: Some(romaji.into()),
            english: english.map(Into::into),
            format: Some("TV".into()),
            year: None,
            episodes: None,
            cover_small: None,
            cover_large: None,
            entry: None,
        }
    }

    #[test]
    fn plan_strips_season_and_filters_to_it() {
        let plan = SearchPlan::for_anime(&anime(
            "Sousou no Frieren 2nd Season",
            Some("Frieren: Beyond Journey’s End Season 2"),
        ));
        assert_eq!(plan.queries, vec!["sousou no frieren", "frieren beyond journeys end"]);
        assert_eq!(plan.season, Some(2));
    }

    #[test]
    fn ranks_by_tier_then_quality_then_coverage() {
        let plan = SearchPlan::for_anime(&anime("Sousou no Frieren 2nd Season", None));
        let items = vec![
            item(1, "[SubsPlease] Sousou no Frieren S2 (01-10) (1080p) [Batch]", 300),
            item(2, "[SubsPlease] Sousou no Frieren S2 (01-10) (720p) [Batch]", 500),
            item(3, "[Erai-raws] Sousou no Frieren 2nd Season - 01 [1080p CR WEBRip HEVC AAC][MultiSub]", 20),
            item(4, "[Erai-raws] Sousou no Frieren 2nd Season - 02 [1080p CR WEBRip HEVC AAC][MultiSub]", 25),
            item(5, "[Anime Time] Sousou no Frieren S02 [1080p][HEVC 10bit x265][AAC][Dual-Audio]", 900),
            item(6, "[SubsPlease] Sousou no Frieren (01-28) (1080p) [Batch]", 1000), // season 1
            item(7, "[Random] Some Other Show S02 (1080p)", 50),                       // wrong show
            item(8, "[KOTEX] Sousou no Frieren S2 Subs+Fonts.zip", 50),                // not video
        ];
        let releases = rank_items(items, &plan, false);
        let summary: Vec<_> = releases
            .iter()
            .map(|r| (r.group.as_str(), r.rank.label.as_str(), r.resolution))
            .collect();
        // Erai-raws ranks higher than SubsPlease, but only has 2 of 10
        // episodes, so the complete SubsPlease batches come first.
        assert_eq!(
            summary,
            vec![
                ("SubsPlease", "Web Tier 05", Some(1080)),
                ("SubsPlease", "Web Tier 05", Some(720)),
                ("Erai-raws", "Web Tier 04", Some(1080)),
                ("Anime Time", "Low-quality group", Some(1080)),
            ]
        );
        // Erai-raws' two episodes were grouped into one release.
        let ReleaseKind::Episodes(eps) = &releases[2].kind else { panic!() };
        assert_eq!(eps.iter().map(|(e, _)| *e).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(releases[2].episodes_label(), "2 episodes (01–02)");
        assert_eq!(releases[2].seeders_label(), "20–25");
        assert!(releases[2].incomplete && !releases[0].incomplete);
        assert!(!releases[3].usable());
    }

    #[test]
    fn higher_tier_wins_when_equally_complete() {
        let plan = SearchPlan::custom("show");
        let mut items = vec![item(1, "[SubsPlease] Show (01-04) (1080p) [Batch]", 300)];
        for ep in 1..=4 {
            items.push(item(10 + ep, &format!("[Erai-raws] Show - {ep:02} [1080p CR WEBRip HEVC AAC]"), 20));
        }
        let releases = rank_items(items, &plan, false);
        assert_eq!(releases[0].group, "Erai-raws");
        assert_eq!(releases[0].episodes_label(), "4 episodes (01–04)");
    }

    #[test]
    fn dead_and_remade_releases_sink() {
        let plan = SearchPlan::custom("show");
        let mut remake = item(2, "[SubsPlease] Show (01-12) (1080p) [Batch]", 99);
        remake.remake = true;
        let releases = rank_items(
            vec![
                item(1, "[LostYears] Show S01 (BD 1080p)", 0),
                remake,
                item(3, "[Nobody] Show S01 (1080p)", 3),
            ],
            &plan,
            false,
        );
        // The only usable release wins despite being unranked; unusable ones
        // follow in tier order.
        assert_eq!(releases[0].group, "Nobody");
        assert_eq!((releases[1].group.as_str(), releases[1].problem()), ("LostYears", Some("No seeders")));
        assert_eq!(
            (releases[2].group.as_str(), releases[2].problem()),
            ("SubsPlease", Some("Marked as a remake on Nyaa"))
        );
    }

    #[test]
    fn keeps_best_seeded_copy_of_each_episode() {
        let plan = SearchPlan::custom("show");
        let releases = rank_items(
            vec![
                item(1, "[SubsPlease] Show - 01 (1080p) [AAAA1111].mkv", 5),
                item(2, "[SubsPlease] Show - 01v2 (1080p) [BBBB2222].mkv", 9),
                item(3, "[SubsPlease] Show - 03 (1080p) [CCCC3333].mkv", 4),
            ],
            &plan,
            false,
        );
        assert_eq!(releases.len(), 1);
        let ReleaseKind::Episodes(eps) = &releases[0].kind else { panic!() };
        assert_eq!(eps.iter().map(|(e, i)| (*e, i.id)).collect::<Vec<_>>(), vec![(1, 2), (3, 3)]);
        assert_eq!(releases[0].episodes_label(), "2 episodes (01, 03)");
    }

    #[test]
    fn compresses_episode_lists() {
        assert_eq!(compress_ranges([1, 2, 3, 5, 7, 8]), "01–03, 05, 07–08");
        assert_eq!(compress_ranges([4]), "04");
    }
}
