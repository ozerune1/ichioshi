//! The library as your servers have it: each anime in Sonarr (a season of
//! one of its series, or part of one) or Radarr, its episodes and files,
//! Jellyfin's items for them and how far you've watched each.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;

use super::Config;
use super::arr::{self, Arr, Kind};
use super::jellyfin::{self, Jellyfin};
use super::mapping::Mapping;
use crate::library::{ArrRef, Remote, RemoteEpisode, Series, Watch};

/// What the servers have.
#[derive(Debug, Default)]
pub struct Built {
    pub series: Vec<Series>,
    /// Episodes downloading, how far along (0–1), by AniList ID.
    pub progress: HashMap<u64, BTreeMap<u32, f32>>,
    /// Sonarr's and Radarr's queues.
    pub queue: Vec<(Kind, arr::Queued)>,
    /// What AniList said this time about anime's lengths and sequels.
    pub parts: HashMap<u64, crate::browse::Part>,
    /// What couldn't be read ("Radarr: couldn't reach …").
    pub problems: Vec<String>,
}

/// The file name at the end of a path (either kind of slash).
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Read everything; `known` are the AniList anime whose titles are already
/// known (the others' are fetched).
/// What AniList said before about anime's lengths and sequels; `fresh`
/// are recent enough not to ask about again.
#[derive(Clone, Debug, Default)]
pub struct Parts {
    pub known: HashMap<u64, crate::browse::Part>,
    pub fresh: HashSet<u64>,
}

pub async fn build(
    client: &reqwest::Client,
    config: &Config,
    mapping: &Mapping,
    known: &HashSet<u64>,
    cached: &Parts,
) -> Built {
    let mut built = Built::default();
    let sonarr = config
        .sonarr
        .is_set()
        .then(|| Arr::new(Kind::Sonarr, &config.sonarr, client));
    let radarr = config
        .radarr
        .is_set()
        .then(|| Arr::new(Kind::Radarr, &config.radarr, client));
    let jelly = config
        .jellyfin
        .signed_in()
        .then(|| Jellyfin::new(&config.jellyfin, client));

    let (shows, movies, items, sonarr_queue, radarr_queue) = tokio::join!(
        async {
            match &sonarr {
                Some(s) => Some(s.shows().await),
                None => None,
            }
        },
        async {
            match &radarr {
                Some(r) => Some(r.movies().await),
                None => None,
            }
        },
        async {
            match &jelly {
                Some(j) => Some(j.items().await),
                None => None,
            }
        },
        async {
            match &sonarr {
                Some(s) => s.queue().await.unwrap_or_default(),
                None => Vec::new(),
            }
        },
        async {
            match &radarr {
                Some(r) => r.queue().await.unwrap_or_default(),
                None => Vec::new(),
            }
        },
    );
    let mut problem = |what: &str, e: anyhow::Error| built.problems.push(format!("{what}: {e:#}"));
    let shows = match shows {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            problem("Sonarr", e);
            Vec::new()
        }
        None => Vec::new(),
    };
    let movies = match movies {
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            problem("Radarr", e);
            Vec::new()
        }
        None => Vec::new(),
    };
    let items = match items {
        Some(Ok(i)) => i,
        Some(Err(e)) => {
            problem("Jellyfin", e);
            Vec::new()
        }
        None => Vec::new(),
    };

    // Sonarr's anime: their episodes and files, a few series at a time.
    let anime_shows: Vec<arr::Show> = shows
        .into_iter()
        .filter(|s| !mapping.seasons_of(s.tvdb_id).is_empty())
        .collect();
    let movies: Vec<(u64, arr::Movie)> = movies
        .into_iter()
        .filter_map(|m| Some((mapping.film(m.tmdb_id)?, m)))
        .collect();
    let details: Vec<Result<(Vec<arr::Episode>, Vec<arr::File>)>> = match &sonarr {
        Some(s) => {
            super::each(anime_shows.iter().map(|x| x.id).collect(), 4, |id| {
                let s = s.clone();
                async move { Ok((s.episodes(id).await?, s.episode_files(id).await?)) }
            })
            .await
        }
        None => Vec::new(),
    };

    // Which anime each season's episodes are: as the mapping places them;
    // a season longer than the last anime placed in it goes on with that
    // anime's sequel (one the mapping doesn't place yet), and so on.
    let mut plans: Vec<Vec<(u64, u32, u32)>> = anime_shows
        .iter()
        .map(|s| mapping.seasons_of(s.tvdb_id).to_vec())
        .collect();
    // AniList is asked only about what it wasn't asked lately; when it
    // can't answer (it limits how often it's asked), what it said before
    // does.
    let mut parts: HashMap<u64, crate::browse::Part> = cached.known.clone();
    let mut asked: HashSet<u64> = cached.fresh.clone();
    let mut anilist_down = false;
    for _ in 0..4 {
        let mut open = Vec::new();
        for (i, (plan, got)) in plans.iter().zip(&details).enumerate() {
            let Ok((eps, _)) = got else { continue };
            let seasons: std::collections::BTreeSet<u32> = plan.iter().map(|p| p.1).collect();
            for season in seasons {
                let Some(last) = plan.iter().filter(|p| p.1 == season).max_by_key(|p| p.2) else {
                    continue;
                };
                let top = eps
                    .iter()
                    .filter(|e| e.season_number == season)
                    .map(|e| e.episode_number)
                    .max()
                    .unwrap_or(0);
                if top > last.2 {
                    open.push((i, *last, top));
                }
            }
        }
        let wanted: Vec<u64> = open
            .iter()
            .map(|(_, (id, _, _), _)| *id)
            .filter(|id| !asked.contains(id))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if !wanted.is_empty() && !anilist_down {
            asked.extend(&wanted);
            match crate::browse::parts(client, &wanted).await {
                Ok(p) => {
                    built.parts.extend(p.clone());
                    parts.extend(p);
                }
                Err(e) => {
                    tracing::warn!("AniList (lengths and sequels): {e:#}");
                    anilist_down = true;
                }
            }
        }
        let placed = |id: u64| mapping.place(id).is_some_and(|p| p.tvdb.is_some());
        let mut grew = false;
        for (i, (anilist, season, offset), top) in open {
            let Some(part) = parts.get(&anilist) else {
                continue;
            };
            let Some(count) = part.episodes.filter(|n| *n > 0) else {
                continue;
            };
            let end = offset + count;
            let next = part
                .sequels
                .iter()
                .copied()
                .find(|s| !placed(*s) && !plans[i].iter().any(|p| p.0 == *s));
            if top > end
                && let Some(next) = next
            {
                plans[i].push((next, season, end));
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    // Jellyfin's series (by TVDB ID, or by folder name) and their episodes.
    let folder_of = |path: &str| file_name(path.trim_end_matches(['/', '\\'])).to_owned();
    let jelly_show = |show: &arr::Show| -> Option<&jellyfin::Item> {
        let series = items.iter().filter(|i| i.kind == "Series");
        series
            .clone()
            .find(|i| i.tvdb == Some(show.tvdb_id))
            .or_else(|| {
                let folder = folder_of(&show.path);
                series
                    .clone()
                    .find(|i| i.path.as_deref().map(folder_of).as_deref() == Some(&folder))
            })
    };
    let wanted: Vec<String> = anime_shows
        .iter()
        .filter_map(|s| jelly_show(s).map(|i| i.id.clone()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let jelly_eps: HashMap<String, Vec<jellyfin::Episode>> = match &jelly {
        Some(j) => {
            let got = super::each(wanted.clone(), 4, |id| {
                let j = j.clone();
                async move { (id.clone(), j.episodes(&id).await) }
            })
            .await;
            got.into_iter()
                .filter_map(|(id, r)| r.ok().map(|e| (id, e)))
                .collect()
        }
        None => HashMap::new(),
    };

    let mut problem = |what: &str, e: anyhow::Error| built.problems.push(format!("{what}: {e:#}"));
    for ((show, got), seasons) in anime_shows.iter().zip(details).zip(&plans) {
        let (episodes, files) = match got {
            Ok(x) => x,
            Err(e) => {
                problem(&format!("Sonarr ({})", show.title), e);
                continue;
            }
        };
        let files: HashMap<u64, &arr::File> = files.iter().map(|f| (f.id, f)).collect();
        let jitem = jelly_show(show);
        let jeps = jitem.and_then(|i| jelly_eps.get(&i.id));
        for &(anilist, season, offset) in seasons {
            // Up to where the next part of the same season starts.
            let until = seasons
                .iter()
                .filter(|(_, s, o)| *s == season && *o > offset)
                .map(|(_, _, o)| *o)
                .min()
                .unwrap_or(u32::MAX);
            let mut eps = BTreeMap::new();
            let mut watched = BTreeMap::new();
            for e in episodes
                .iter()
                .filter(|e| e.season_number == season)
                .filter(|e| e.episode_number > offset && e.episode_number <= until)
            {
                let n = e.episode_number - offset;
                let file = e
                    .has_file
                    .then(|| files.get(&e.episode_file_id))
                    .flatten()
                    .map(|f| (file_name(&f.relative_path).to_owned(), f.size));
                let jep = jeps.and_then(|list| {
                    file.as_ref()
                        .and_then(|(name, _)| {
                            list.iter()
                                .find(|j| j.path.as_deref().map(file_name) == Some(name.as_str()))
                        })
                        .or_else(|| {
                            list.iter().find(|j| {
                                j.season == Some(season) && j.number == Some(e.episode_number)
                            })
                        })
                });
                if let Some(w) = jep.and_then(|j| j.watched) {
                    add_watch(&mut watched, n, w);
                }
                eps.insert(
                    n,
                    RemoteEpisode {
                        arr_id: Some(e.id),
                        monitored: e.monitored,
                        file_id: file.as_ref().map(|_| e.episode_file_id),
                        file,
                        jellyfin: jep.map(|j| j.id.clone()),
                    },
                );
            }
            let monitored = show.monitored
                && show
                    .seasons
                    .iter()
                    .any(|s| s.season_number == season && s.monitored);
            built.series.push(remote_series(
                anilist,
                show.title.clone(),
                season,
                Remote {
                    arr: ArrRef::Sonarr {
                        id: show.id,
                        tvdb: show.tvdb_id,
                        season,
                        offset,
                    },
                    path: show.path.clone(),
                    slug: show.title_slug.clone(),
                    monitored,
                    jellyfin: jitem.map(|i| i.id.clone()),
                    episodes: eps,
                },
                watched,
            ));
        }
    }
    for (anilist, m) in &movies {
        let file = m
            .movie_file
            .as_ref()
            .filter(|_| m.has_file)
            .map(|f| (file_name(&f.relative_path).to_owned(), f.size));
        let jitem = items
            .iter()
            .filter(|i| i.kind == "Movie")
            .find(|i| i.tmdb == Some(m.tmdb_id) || i.anilist == Some(*anilist))
            .or_else(|| {
                let name = file.as_ref()?.0.as_str();
                items
                    .iter()
                    .find(|i| i.path.as_deref().map(file_name) == Some(name))
            });
        let mut watched = BTreeMap::new();
        if let Some(w) = jitem.and_then(|i| i.watched) {
            add_watch(&mut watched, 1, w);
        }
        built.series.push(remote_series(
            *anilist,
            m.title.clone(),
            1,
            Remote {
                arr: ArrRef::Radarr {
                    id: m.id,
                    tmdb: m.tmdb_id,
                },
                path: m.path.clone(),
                slug: m.title_slug.clone(),
                monitored: m.monitored,
                jellyfin: jitem.map(|i| i.id.clone()),
                episodes: BTreeMap::from([(
                    1,
                    RemoteEpisode {
                        arr_id: None,
                        monitored: m.monitored,
                        file_id: file.as_ref().and(m.movie_file.as_ref()).map(|f| f.id),
                        file,
                        jellyfin: jitem.map(|i| i.id.clone()),
                    },
                )]),
            },
            watched,
        ));
    }

    // One entry an anime (Sonarr and Radarr could both have it).
    let mut seen = HashSet::new();
    built.series.retain(|s| seen.insert(s.anilist_id));

    // AniList's titles and covers for the anime not seen before.
    let new: Vec<u64> = built
        .series
        .iter()
        .map(|s| s.anilist_id)
        .filter(|id| !known.contains(id))
        .collect();
    if !new.is_empty() {
        match crate::browse::by_ids(client, &new).await {
            Ok(media) => {
                let media: HashMap<u64, crate::browse::Media> =
                    media.into_iter().map(|m| (m.id, m)).collect();
                for s in &mut built.series {
                    if let Some(m) = media.get(&s.anilist_id) {
                        s.english = m.english.clone();
                        s.romaji = m.romaji.clone();
                        s.cover = m.cover.clone();
                        s.format = m.format.clone();
                        s.year = m.year;
                        s.episodes = m.episodes;
                    }
                }
            }
            Err(e) => built.problems.push(format!("AniList: {e:#}")),
        }
    }
    built.queue = sonarr_queue
        .into_iter()
        .map(|q| (Kind::Sonarr, q))
        .chain(radarr_queue.into_iter().map(|q| (Kind::Radarr, q)))
        .collect();
    built.progress = progress(&built.series, &built.queue);
    built
}

/// Which anime and episode (AniList's numbering) a queue item is, if it's
/// one in the library.
pub fn queued_episode<'a>(
    series: &'a [Series],
    kind: Kind,
    q: &arr::Queued,
) -> Option<(&'a Series, u32)> {
    series.iter().find_map(|s| {
        let r = s.remote.as_ref()?;
        match (r.arr, kind) {
            (ArrRef::Sonarr { id, .. }, Kind::Sonarr) if id == q.owner => {
                let n = r
                    .episodes
                    .iter()
                    .find(|(_, e)| e.arr_id.is_some() && e.arr_id == q.episode)?
                    .0;
                Some((s, *n))
            }
            (ArrRef::Radarr { id, .. }, Kind::Radarr) if id == q.owner => Some((s, 1)),
            _ => None,
        }
    })
}

/// How far along each episode in the queues is, by AniList ID.
pub fn progress(
    series: &[Series],
    queue: &[(Kind, arr::Queued)],
) -> HashMap<u64, BTreeMap<u32, f32>> {
    let mut out: HashMap<u64, BTreeMap<u32, f32>> = HashMap::new();
    for (kind, q) in queue.iter().filter(|(_, q)| q.size > 0.0) {
        if let Some((s, n)) = queued_episode(series, *kind, q) {
            out.entry(s.anilist_id).or_default().insert(n, q.fraction());
        }
    }
    out
}

fn add_watch(watched: &mut BTreeMap<u32, Watch>, n: u32, w: jellyfin::Watched) {
    if w.played || w.position > 0.0 {
        watched.insert(
            n,
            Watch {
                position: w.position,
                duration: w.duration,
                done: w.played,
                at: w.at,
            },
        );
    }
}

/// A series as the library keeps it, titled (until AniList's titles are
/// in) as Sonarr or Radarr has it.
fn remote_series(
    anilist: u64,
    title: String,
    season: u32,
    remote: Remote,
    watched: BTreeMap<u32, Watch>,
) -> Series {
    let title = if season > 1 {
        format!("{title} (season {season})")
    } else {
        title
    };
    Series {
        anilist_id: anilist,
        english: Some(title),
        romaji: None,
        folder: Default::default(),
        added: chrono::Utc::now().timestamp(),
        cover: None,
        format: None,
        year: None,
        episodes: None,
        prefs: None,
        downloads: Default::default(),
        details: None,
        applied: Default::default(),
        watched,
        remote: Some(remote),
    }
}

/// The new list, keeping what's known of each series from before (AniList's
/// details and titles, when it was added, its languages, and progress not
/// in Jellyfin yet).
pub fn merge(old: &[Series], mut new: Vec<Series>, have_jellyfin: bool) -> Vec<Series> {
    let old: HashMap<u64, &Series> = old.iter().map(|s| (s.anilist_id, s)).collect();
    for s in &mut new {
        let Some(o) = old.get(&s.anilist_id) else {
            continue;
        };
        if s.romaji.is_none() && s.cover.is_none() {
            s.english = o.english.clone();
            s.romaji = o.romaji.clone();
            s.cover = o.cover.clone();
            s.format = o.format.clone();
            s.year = o.year;
            s.episodes = o.episodes;
        }
        s.added = o.added;
        s.details = o.details.clone();
        s.prefs = o.prefs.clone();
        for (n, w) in &o.watched {
            // Episodes it no longer has (they turned out to be another
            // anime's: a sequel's, say) take their marks with them.
            if s.remote
                .as_ref()
                .is_some_and(|r| !r.episodes.contains_key(n))
            {
                continue;
            }
            let jellyfin_knows = have_jellyfin
                && s.remote
                    .as_ref()
                    .is_some_and(|r| r.episodes.get(n).is_some_and(|e| e.jellyfin.is_some()));
            let newer = s.watched.get(n).is_none_or(|j| j.at < w.at);
            if !jellyfin_knows || newer && !s.watched.get(n).is_some_and(|j| j.done) {
                s.watched.entry(*n).or_insert_with(|| w.clone());
            }
        }
    }
    new
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::mock_http::{mock, reply};
    use crate::servers::{ArrAccount, JellyfinAccount};

    /// A Sonarr whose "Apothecary Diaries" has two seasons (two anime), a
    /// split-cour show (season 1 is two anime), a non-anime show; a Radarr
    /// film; a Jellyfin that has some of it watched.
    fn servers(req: &crate::mock_http::Req) -> crate::mock_http::Resp {
        let path = req.target.split('?').next().unwrap();
        let q = req.query();
        let ep = |id: u64, season: u32, n: u32, file: u64| {
            json!({"id": id, "seasonNumber": season, "episodeNumber": n,
                   "hasFile": file > 0, "monitored": true, "episodeFileId": file})
        };
        let v: Value = match path {
            "/sonarr/api/v3/series" => json!([
                {"id": 1, "title": "The Apothecary Diaries", "tvdbId": 431162, "titleSlug": "apothecary",
                 "path": "/tv/The Apothecary Diaries", "monitored": true,
                 "seasons": [{"seasonNumber": 1, "monitored": true}, {"seasonNumber": 2, "monitored": false}]},
                {"id": 2, "title": "Split", "tvdbId": 9000, "path": "/tv/Split", "monitored": true,
                 "seasons": [{"seasonNumber": 1, "monitored": true}]},
                {"id": 3, "title": "Not Anime", "tvdbId": 1, "path": "/tv/Not Anime"},
                {"id": 4, "title": "JoJo's Bizarre Adventure", "tvdbId": 262954,
                 "path": "/tv/JoJo", "monitored": true,
                 "seasons": [{"seasonNumber": 6, "monitored": true}]}]),
            "/sonarr/api/v3/episode" => match q["seriesId"].as_str() {
                // Season 6: Steel Ball Run, its 1st STAGE (one long episode)
                // then 2nd & 3rd STAGE (11).
                "4" => json!(
                    (1..=12)
                        .map(|n| ep(40 + u64::from(n), 6, n, 0))
                        .collect::<Vec<_>>()
                ),
                "1" => json!([
                    ep(10, 1, 1, 100),
                    ep(11, 1, 2, 0),
                    ep(20, 2, 1, 200),
                    ep(0, 0, 1, 0)
                ]),
                "2" => json!([
                    ep(30, 1, 1, 0),
                    ep(31, 1, 12, 0),
                    ep(32, 1, 13, 300),
                    ep(33, 1, 14, 0)
                ]),
                other => panic!("episodes of {other}"),
            },
            "/sonarr/api/v3/episodefile" => json!([
                {"id": 100, "relativePath": "Season 01/Apothecary - S01E01.mkv", "size": 1000},
                {"id": 200, "relativePath": "Season 02/Apothecary - S02E01.mkv", "size": 2000},
                {"id": 300, "relativePath": "Season 01/Split - S01E13.mkv", "size": 3000}]),
            "/sonarr/api/v3/queue" => json!({"records": [{"seriesId": 1, "episodeId": 11,
                "size": 100.0, "sizeleft": 40.0}]}),
            "/radarr/api/v3/movie" => json!([
                {"id": 5, "title": "Mugen Train", "tmdbId": 635302, "hasFile": true, "monitored": true,
                 "movieFile": {"id": 1, "relativePath": "Mugen Train (2020).mkv", "size": 5000}},
                {"id": 6, "title": "Some Film", "tmdbId": 42}]),
            "/radarr/api/v3/queue" => json!({"records": []}),
            "/jf/Items" => json!({"Items": [
                {"Id": "js1", "Type": "Series", "ProviderIds": {"Tmdb": "220542"},
                 "Path": "/media/anime/The Apothecary Diaries"},
                {"Id": "jm1", "Type": "Movie", "ProviderIds": {"Tmdb": "635302"},
                 "RunTimeTicks": 70_000_000_000_i64,
                 "UserData": {"Played": true, "PlaybackPositionTicks": 0}}]}),
            "/jf/Shows/js1/Episodes" => json!({"Items": [
                {"Id": "je1", "ParentIndexNumber": 1, "IndexNumber": 1,
                 "Path": "/media/anime/The Apothecary Diaries/Season 01/Apothecary - S01E01.mkv",
                 "RunTimeTicks": 14_400_000_000_i64,
                 "UserData": {"Played": false, "PlaybackPositionTicks": 3_000_000_000_i64,
                              "LastPlayedDate": "2026-11-20T10:00:00Z"}},
                {"Id": "je2", "ParentIndexNumber": 2, "IndexNumber": 1, "Path": "/elsewhere/x.mkv"}]}),
            "/anilist" => {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let ids = body["variables"]["ids"].as_array().unwrap().clone();
                // Lengths and sequels (AniList's, for Steel Ball Run).
                if body["query"].as_str().unwrap().contains("relations") {
                    if ANILIST_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
                        return reply(429, json!({"errors": [{"message": "Too Many Requests."}]}));
                    }
                    let part = |id: &Value| match id.as_u64() {
                        Some(190327) => {
                            json!({"id": 190327, "episodes": 1, "relations": {"edges": [
                            {"relationType": "PREQUEL", "node": {"id": 146722, "type": "ANIME", "format": "ONA"}},
                            {"relationType": "SEQUEL", "node": {"id": 210482, "type": "ANIME", "format": "ONA"}}]}})
                        }
                        Some(210482) => {
                            json!({"id": 210482, "episodes": 11, "relations": {"edges": []}})
                        }
                        _ => json!({"id": id, "episodes": null, "relations": {"edges": []}}),
                    };
                    return reply(
                        200,
                        json!({"data": {"Page": {"media": ids.iter().map(part).collect::<Vec<_>>()}}}),
                    );
                }
                json!({"data": {"Page": {"media": ids.iter().map(|id| json!({
                    "id": id, "title": {"romaji": format!("Anime {id}")},
                    "coverImage": {"large": format!("cover{id}")}, "format": "TV"})).collect::<Vec<_>>()}}})
            }
            other => panic!("unexpected {other}"),
        };
        reply(200, v)
    }

    /// AniList refuses to say more (as when it's been asked too often).
    static ANILIST_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn mapping() -> Mapping {
        Mapping::parse(
            br#"[
            {"type": "TV", "anilist_id": 161645, "tvdb_id": 431162, "season": {"tvdb": 1}},
            {"type": "TV", "anilist_id": 176301, "tvdb_id": 431162, "season": {"tvdb": 2}},
            {"type": "TV", "anilist_id": 501, "tvdb_id": 9000, "season": {"tvdb": 1}},
            {"type": "TV", "anilist_id": 502, "tvdb_id": 9000, "season": {"tvdb": 1},
             "episode_offset": {"tvdb": 12}},
            {"type": "MOVIE", "anilist_id": 112151, "themoviedb_id": {"movie": [635302]}},
            {"type": "ONA", "anilist_id": 190327, "tvdb_id": 262954, "season": {"tvdb": 6}},
            {"type": "ONA", "anilist_id": 210482}
        ]"#,
        )
        .unwrap()
    }

    #[test]
    fn builds_the_library_from_the_servers() {
        let _net = crate::mock_http::mocked();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let base = rt.block_on(mock(servers));
        *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(format!("{base}/anilist"));
        let arr = |p: &str| ArrAccount {
            url: format!("{base}/{p}"),
            api_key: "k".into(),
            ..Default::default()
        };
        let config = Config {
            mode: super::super::LibraryMode::Servers,
            sonarr: arr("sonarr"),
            radarr: arr("radarr"),
            jellyfin: JellyfinAccount {
                url: format!("{base}/jf"),
                token: Some("t".into()),
                user_id: Some("u".into()),
                ..Default::default()
            },
        };
        let built = rt.block_on(build(
            &reqwest::Client::new(),
            &config,
            &mapping(),
            &HashSet::from([501]),
            &Parts::default(),
        ));
        *crate::seadex::ANILIST_BASE.lock().unwrap() = None;
        assert!(built.problems.is_empty(), "{:?}", built.problems);
        let ids: Vec<u64> = built.series.iter().map(|s| s.anilist_id).collect();
        assert_eq!(
            ids,
            [161645, 176301, 501, 502, 190327, 210482, 112151],
            "not the non-anime"
        );
        // Steel Ball Run: 1st STAGE is season 6's first episode; the rest
        // is its sequel, 2nd & 3rd STAGE, which the mapping doesn't place.
        let stage = |id: u64| {
            let s = built.series.iter().find(|s| s.anilist_id == id).unwrap();
            let r = s.remote.as_ref().unwrap();
            let eps: Vec<(u32, Option<u64>)> =
                r.episodes.iter().map(|(n, e)| (*n, e.arr_id)).collect();
            (r.arr, eps)
        };
        let (_, first) = stage(190327);
        assert_eq!(first, [(1, Some(41))]);
        let (arr, second) = stage(210482);
        assert_eq!(
            arr,
            ArrRef::Sonarr {
                id: 4,
                tvdb: 262954,
                season: 6,
                offset: 1
            }
        );
        assert_eq!(second.len(), 11);
        assert_eq!(second[0], (1, Some(42)), "its episode 1 is S06E02");
        assert_eq!(second[10], (11, Some(52)));

        // Read again while AniList won't answer: what it said before does.
        ANILIST_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
        *crate::seadex::ANILIST_BASE.lock().unwrap() = Some(format!("{base}/anilist"));
        let known: HashSet<u64> = built.series.iter().map(|s| s.anilist_id).collect();
        let again = rt.block_on(build(
            &reqwest::Client::new(),
            &config,
            &mapping(),
            &known,
            &Parts {
                known: built.parts.clone(),
                fresh: HashSet::new(),
            },
        ));
        *crate::seadex::ANILIST_BASE.lock().unwrap() = None;
        ANILIST_DOWN.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(again.parts.is_empty(), "nothing new from AniList");
        let still = again.series.iter().find(|s| s.anilist_id == 210482);
        assert_eq!(
            still.map(|s| s.remote.as_ref().unwrap().episodes.len()),
            Some(11),
            "2nd & 3rd STAGE stays"
        );
        let get = |id| built.series.iter().find(|s| s.anilist_id == id).unwrap();
        let s1 = get(161645);
        assert_eq!(s1.romaji.as_deref(), Some("Anime 161645"));
        assert_eq!(s1.cover.as_deref(), Some("cover161645"));
        let r = s1.remote.as_ref().unwrap();
        assert!(r.monitored);
        assert_eq!(r.jellyfin.as_deref(), Some("js1"), "matched by its folder");
        assert_eq!(r.episodes.len(), 2);
        assert_eq!(
            r.episodes[&1].file,
            Some(("Apothecary - S01E01.mkv".into(), 1000))
        );
        assert_eq!(
            r.episodes[&1].jellyfin.as_deref(),
            Some("je1"),
            "by file name"
        );
        assert_eq!(r.episodes[&2].file, None);
        assert_eq!(s1.watched[&1].position, 300.0);
        assert!(!s1.watched[&1].done);
        assert_eq!(built.progress[&161645][&2], 0.6);
        let s2 = get(176301);
        assert!(!s2.remote.as_ref().unwrap().monitored);
        assert_eq!(
            s2.remote.as_ref().unwrap().episodes[&1].jellyfin.as_deref(),
            Some("je2")
        );
        // The split season: part one is episodes 1–12, part two 13 on (its 1).
        assert_eq!(
            get(501).english.as_deref(),
            Some("Split"),
            "known: not fetched"
        );
        let part1: Vec<u32> = get(501)
            .remote
            .as_ref()
            .unwrap()
            .episodes
            .keys()
            .copied()
            .collect();
        assert_eq!(part1, [1, 12]);
        let part2 = &get(502).remote.as_ref().unwrap().episodes;
        assert_eq!(part2.keys().copied().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(part2[&1].file, Some(("Split - S01E13.mkv".into(), 3000)));
        let film = get(112151);
        assert_eq!(
            film.remote.as_ref().unwrap().episodes[&1]
                .jellyfin
                .as_deref(),
            Some("jm1")
        );
        assert!(film.watched[&1].done);

        // Rebuilt: what was known stays.
        let mut old = built.series.clone();
        old[0].details = Some(Default::default());
        old[0].watched.insert(
            2,
            Watch {
                position: 10.0,
                duration: 20.0,
                done: false,
                at: 5,
            },
        );
        let mut fresh = built.series.clone();
        fresh[0].romaji = None;
        fresh[0].cover = None;
        fresh[0].english = Some("Sonarr's title".into());
        old[0].watched.insert(
            9,
            Watch {
                position: 1.0,
                duration: 2.0,
                done: true,
                at: 5,
            },
        );
        let merged = merge(&old, fresh, true);
        assert!(
            !merged[0].watched.contains_key(&9),
            "not one of its episodes now"
        );
        assert_eq!(merged[0].romaji.as_deref(), Some("Anime 161645"));
        assert!(merged[0].details.is_some());
        assert_eq!(
            merged[0].watched[&2].position, 10.0,
            "no Jellyfin episode: kept"
        );
    }

    #[test]
    fn without_servers_there_is_nothing() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let built = rt.block_on(build(
            &reqwest::Client::new(),
            &Config::default(),
            &mapping(),
            &HashSet::new(),
            &Parts::default(),
        ));
        assert!(built.series.is_empty() && built.problems.is_empty());
    }
}
