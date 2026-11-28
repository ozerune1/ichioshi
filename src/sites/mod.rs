//! 18+ anime from more streaming sites: hanime.tv, oppai.stream,
//! hentaihaven.com, hentaimama.io and hanime.red. How each site's player
//! gets its video follows the hanime-plugin extractors for yt-dlp (PyPI
//! `hanime-plugin` 2026.9.27), ported here so the app needs no Python;
//! finding a show on a site uses its own search and series pages.
//!
//! An episode's page is read only when it's downloaded (the addresses its
//! player gets are signed and expire), or once per show to learn the best
//! resolution it comes in. Video comes as HLS (joined into one MP4, see
//! [`crate::hls`]), DASH (as hstream.moe's), or one MP4.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use url::Url;

use crate::streams::StreamSite;
use web::{Challenge, Session, matches, normalize, queries, slug, wanted};

mod hanimered;
mod hanimetv;
mod hentaihaven;
mod hentaimama;
mod oppai;
pub mod web;

/// Searches at most per show and site (each site is asked politely).
const MAX_SEARCHES: usize = 4;

/// An episode a site lists for a show.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub episode: u32,
    /// Its page.
    pub url: String,
    /// e.g. "Overflow - Episode 3".
    pub title: String,
}

/// How an episode's player gets its video.
#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    /// An HLS playlist (a master playlist or one variant's).
    Hls(Url),
    /// DASH: an MPD per resolution, (resolution, URL).
    Dash(Vec<(u32, Url)>),
    /// One video file.
    File(Url),
}

/// One way an episode plays (a site may have a few: codecs, qualities).
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub media: Media,
    /// Sent as the Referer when fetching it.
    pub referer: Option<String>,
    /// Its resolution, if the site says.
    pub resolution: Option<u32>,
    /// For the log: "h264", "720p".
    pub label: String,
}

/// What an episode's page plays.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub title: Option<String>,
    /// At a tie in resolution, the earlier is taken.
    pub streams: Vec<Stream>,
}

/// A show as a site's search lists it: its address on the site and its
/// names there.
#[derive(Debug, Clone, PartialEq)]
struct Card {
    slug: String,
    titles: Vec<String>,
}

/// A show's page: its names and episodes.
#[derive(Debug, Clone, PartialEq)]
struct Show {
    titles: Vec<String>,
    episodes: Vec<Episode>,
}

fn site_name(site: StreamSite) -> &'static str {
    site.name()
}

/// The show called any of `names` on `site`, its episodes in order; none
/// if the site hasn't got it.
pub async fn episodes(
    client: &reqwest::Client,
    site: StreamSite,
    names: &[String],
) -> Result<Vec<Episode>> {
    let s = Session::new(client, site_name(site));
    let mut eps = match site {
        StreamSite::HanimeTv => hanimetv::episodes(&s, names).await?,
        StreamSite::OppaiStream => oppai::episodes(&s, names).await?,
        _ => find_show(&s, site, names).await?,
    };
    eps.sort_by_key(|e| e.episode);
    eps.dedup_by_key(|e| e.episode);
    Ok(eps)
}

/// A site's search for `query`: the shows it lists (None: no such page).
async fn search(s: &Session, site: StreamSite, query: &str) -> Result<Option<Vec<Card>>> {
    match site {
        StreamSite::HentaiHaven => hentaihaven::search(s, query).await,
        StreamSite::Hentaimama => hentaimama::search(s, query).await,
        StreamSite::HanimeRed => hanimered::search(s, query).await,
        _ => Ok(None),
    }
}

/// The show at `slug` on a site (None: it hasn't got one there).
async fn show(s: &Session, site: StreamSite, slug: &str) -> Result<Option<Show>> {
    match site {
        StreamSite::HentaiHaven => hentaihaven::show(s, slug).await,
        StreamSite::Hentaimama => hentaimama::show(s, slug).await,
        StreamSite::HanimeRed => hanimered::show(s, slug).await,
        _ => Ok(None),
    }
}

/// Finds the show on a site with a search and series pages: a search
/// result named like it (each name, then plainer spellings); else a result
/// whose own page has one of its names (a Japanese one, say); else the page
/// where its name says it would be.
async fn find_show(s: &Session, site: StreamSite, names: &[String]) -> Result<Vec<Episode>> {
    let wanted = wanted(names);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let named = |titles: &[String]| titles.iter().any(|t| matches(&wanted, &normalize(t)));
    let mut candidates: Vec<String> = Vec::new();
    let mut answered = false;
    let mut failure = None;
    for query in queries(names).into_iter().take(MAX_SEARCHES) {
        let cards = match search(s, site, &query).await {
            Ok(Some(cards)) => cards,
            Ok(None) => continue,
            Err(e) if e.is::<Challenge>() => return Err(e),
            Err(e) => {
                tracing::warn!("{} search for {query:?}: {e:#}", s.site);
                failure = Some(e);
                continue;
            }
        };
        answered = true;
        if let Some(card) = cards.iter().find(|c| named(&c.titles))
            && let Some(show) = show(s, site, &card.slug).await?
            && !show.episodes.is_empty()
        {
            return Ok(show.episodes);
        }
        for c in cards {
            if !candidates.contains(&c.slug) {
                candidates.push(c.slug);
            }
        }
    }
    for slug in candidates.iter().take(3) {
        if let Some(show) = show(s, site, slug).await?
            && named(&show.titles)
        {
            return Ok(show.episodes);
        }
    }
    let mut guesses: Vec<String> = Vec::new();
    for g in names.iter().map(|n| slug(n)) {
        if !g.is_empty() && !guesses.contains(&g) && !candidates.contains(&g) {
            guesses.push(g);
        }
    }
    for g in guesses.iter().take(3) {
        match show(s, site, g).await {
            Ok(Some(show)) => {
                answered = true;
                if !show.episodes.is_empty() && (show.titles.is_empty() || named(&show.titles)) {
                    return Ok(show.episodes);
                }
            }
            Ok(None) => answered = true,
            Err(e) if e.is::<Challenge>() => return Err(e),
            Err(e) => failure = Some(e),
        }
    }
    match failure {
        Some(e) if !answered => Err(e.context(format!("searching {}", s.site))),
        _ => Ok(Vec::new()),
    }
}

/// What the episode page at `url` plays.
async fn resolve(s: &Session, site: StreamSite, url: &str) -> Result<Resolved> {
    let resolved = match site {
        StreamSite::HanimeTv => hanimetv::resolve(s, url).await,
        StreamSite::OppaiStream => oppai::resolve(s, url).await,
        StreamSite::HentaiHaven => hentaihaven::resolve(s, url).await,
        StreamSite::Hentaimama => hentaimama::resolve(s, url).await,
        StreamSite::HanimeRed => hanimered::resolve(s, url).await,
        _ => bail!("{} isn't one of these sites", site.name()),
    }?;
    if resolved.streams.is_empty() {
        bail!("{} has no video for this episode", s.site);
    }
    Ok(resolved)
}

/// `stream`'s resolution: as the site says, else from its playlist or
/// file. None if neither says.
async fn stream_resolution(s: &Session, stream: &Stream) -> Result<Option<u32>> {
    if stream.resolution.is_some() {
        return Ok(stream.resolution);
    }
    match &stream.media {
        Media::Hls(url) => crate::hls::resolution(s, url, stream.referer.as_deref()).await,
        Media::Dash(mpds) => {
            let dash = crate::hstream::Dash::at(s.site, referer_of(stream), mpds)?;
            crate::hstream::dash_resolution(&s.client, &dash)
                .await
                .map(Some)
        }
        Media::File(url) => {
            let (_, dims) =
                crate::ohentai::probe_with(&s.client, url.as_str(), stream.referer.as_deref())
                    .await?;
            Ok(dims.map(|(w, h)| crate::ohentai::resolution(w, h)))
        }
    }
}

fn referer_of(stream: &Stream) -> &str {
    stream.referer.as_deref().unwrap_or_default()
}

/// The best of `resolved`'s streams (the highest resolution; the first at
/// a tie) and its resolution. A stream that can't be read is passed over.
async fn best(s: &Session, resolved: &Resolved) -> Result<(Stream, Option<u32>)> {
    let mut best: Option<(Stream, Option<u32>)> = None;
    let mut failure = None;
    for stream in &resolved.streams {
        let res = match stream_resolution(s, stream).await {
            Ok(r) => r,
            Err(e) if e.is::<Challenge>() => return Err(e),
            Err(e) => {
                tracing::warn!("{} {}: {e:#}", s.site, stream.label);
                failure = Some(e);
                continue;
            }
        };
        if best.as_ref().is_none_or(|(_, b)| res > *b) {
            best = Some((stream.clone(), res));
        }
    }
    best.ok_or_else(|| failure.unwrap_or_else(|| anyhow!("{} has no video", s.site)))
}

/// The best resolution the episode at `url` comes in (to rank the site's
/// release against others).
pub async fn resolution(
    client: &reqwest::Client,
    site: StreamSite,
    url: &str,
) -> Result<Option<u32>> {
    let s = Session::new(client, site_name(site));
    let resolved = resolve(&s, site, url).await?;
    Ok(best(&s, &resolved).await?.1)
}

/// `stem` (a path without an extension) with `ext` added (`stem` may hold
/// dots: "Show 2.5 - 01").
fn with_ext(stem: &Path, ext: &str) -> PathBuf {
    let mut s: OsString = stem.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Downloads the episode at `url` to `<stem>.mp4` (or `<stem>.ts`, for HLS
/// in codecs an MP4 isn't made of here), with any subtitles beside it.
/// `progress(done, total)` in bytes; returns the video's path.
pub async fn download(
    client: &reqwest::Client,
    site: StreamSite,
    url: &str,
    stem: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<PathBuf> {
    let s = Session::new(client, site_name(site));
    let resolved = resolve(&s, site, url).await?;
    let (stream, res) = best(&s, &resolved).await?;
    tracing::info!(
        "{}: {} as {} ({})",
        s.site,
        resolved.title.as_deref().unwrap_or(url),
        stream.label,
        res.map_or("resolution unknown".to_owned(), |r| format!("{r}p")),
    );
    match &stream.media {
        Media::Hls(playlist) => {
            crate::hls::download(
                &s,
                playlist,
                stream.referer.as_deref(),
                stem,
                cancel,
                progress,
            )
            .await
        }
        Media::Dash(mpds) => {
            let mut dash = crate::hstream::Dash::at(s.site, referer_of(&stream), mpds)?;
            dash.title = resolved.title.clone();
            crate::hstream::download_dash(
                client,
                &dash,
                &with_ext(stem, "mp4"),
                &Default::default(),
                None,
                cancel,
                progress,
            )
            .await
        }
        Media::File(file) => {
            let http = file_client(stream.referer.as_deref())?;
            let (len, _) =
                crate::ohentai::probe_with(&http, file.as_str(), stream.referer.as_deref()).await?;
            let len = len.with_context(|| format!("{} didn't say how big the video is", s.site))?;
            let ext = Path::new(file.path())
                .extension()
                .and_then(|e| e.to_str())
                .filter(|e| {
                    ["mp4", "m4v", "mkv", "webm"].contains(&e.to_ascii_lowercase().as_str())
                })
                .unwrap_or("mp4")
                .to_ascii_lowercase();
            let dest = with_ext(stem, &ext);
            crate::debrid::jobs::fetch(&http, file.as_str(), &dest, len, cancel, |have, _| {
                progress(have, Some(len))
            })
            .await?;
            Ok(dest)
        }
    }
}

/// A client for one file, sending `referer` with each request.
fn file_client(referer: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(r) = referer {
        headers.insert(reqwest::header::REFERER, r.parse()?);
    }
    Ok(reqwest::Client::builder()
        .user_agent(concat!("ichioshi/", env!("CARGO_PKG_VERSION")))
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(90))
        .build()?)
}

/// "Overflow - Episode 3".
fn episode_title(show: &str, n: u32) -> String {
    format!("{show} - Episode {n}")
}

/// A show's episodes from its page's links (href, its text or title),
/// numbered from either; numbers seen twice are kept once.
fn numbered(page: &str, show: &str, links: Vec<(String, String)>) -> Vec<Episode> {
    let mut out: Vec<Episode> = Vec::new();
    for (href, label) in links {
        let n = web::episode_number(&label).or_else(|| {
            let path = href.split(['?', '#']).next().unwrap_or_default();
            web::episode_number(path.trim_end_matches('/'))
        });
        let (Some(n), Ok(url)) = (n, web::absolute(page, &href)) else {
            continue;
        };
        if out.iter().any(|e| e.episode == n) {
            continue;
        }
        out.push(Episode {
            episode: n,
            url: url.to_string(),
            title: episode_title(show, n),
        });
    }
    out.sort_by_key(|e| e.episode);
    out
}

#[cfg(test)]
mod tests;
