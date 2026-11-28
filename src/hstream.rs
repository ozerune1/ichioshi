//! hstream.moe (18+): finding a show's episodes, and downloading one.
//!
//! Shows are found with the site's search (`/search?search=…`, a page of
//! episode cards rendered on the server) and listed from the show's own
//! page (`/hentai/<slug>`). An episode's page (`/hentai/<slug>-<n>`) holds
//! its ID; the player's API (`POST /player/api`, Laravel: the page's
//! XSRF-TOKEN cookie comes back as the X-XSRF-TOKEN header, with the session
//! cookie) answers with the stream's path and its CDN mirrors. Each
//! resolution is a DASH presentation, `<mirror>/<path>/<720|1080|2160>/
//! manifest.mpd` (720p H.264, 1080p and 2160p AV1; fMP4 segments, named
//! `.webp`), with video and audio apart; they're joined here into one MP4
//! (see [`super::fmp4`]). English subtitles are `<mirror>/<path>/eng.ass`.
//!
//! The 48 fps versions (frame-interpolated: `1080i`, `2160i`) aren't used,
//! and neither is the site's download button (it's captcha-gated). Nothing
//! here gets past bot checks: a Cloudflare challenge is reported as
//! [`Challenge`].

use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, COOKIE, HeaderMap, REFERER, SET_COOKIE};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use url::Url;

use super::dash::{self, Kind, Representation};
use super::fmp4::{self, Init, Muxer, Track};

const SITE: &str = "https://hstream.moe";
/// What a downloaded episode is.
pub const EXTENSION: &str = "mp4";
/// Highest first: resolution beats codec (1080p AV1 over 720p H.264).
const RESOLUTIONS: [u32; 3] = [2160, 1080, 720];
/// Segments downloading at once.
const PARALLEL: usize = 4;
/// Tries per mirror for a segment, before the next mirror.
const TRIES_PER_MIRROR: u32 = 2;
const SEGMENT_TIMEOUT: Duration = Duration::from_secs(90);
/// Searches at most per [`episodes`] call (the site is asked politely).
const MAX_SEARCHES: usize = 6;

/// An episode on hstream.moe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    /// The show's title on the site, e.g. "Sister Breeder".
    pub series: String,
    pub episode: u32,
    /// Its page, e.g. `https://hstream.moe/hentai/sister-breeder-6`.
    pub url: String,
    /// e.g. "Sister Breeder - 6".
    pub title: String,
    /// The best resolution the site lists for it (2160, 1080 or 720).
    pub resolution: Option<u32>,
}

/// The site answered with a Cloudflare check (a browser challenge), which
/// isn't something to get past.
#[derive(Debug)]
pub struct Challenge;

impl std::fmt::Display for Challenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "hstream.moe is showing a Cloudflare check (a browser challenge); try again later",
        )
    }
}

impl std::error::Error for Challenge {}

/// The download was cancelled (its partial file is removed).
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

// ---------------------------------------------------------------------------
// Finding episodes

/// Finds the show matching any of `names` (AniList's romaji / English /
/// native titles; compared lowercased, letters and digits only, "The
/// Animation" at the end optional) and lists its episodes, in order. None
/// if the site hasn't got it.
pub async fn episodes(client: &reqwest::Client, names: &[String]) -> Result<Vec<Episode>> {
    let wanted: Vec<String> = {
        let mut seen = HashSet::new();
        names
            .iter()
            .map(|n| normalize(n))
            .filter(|n| !n.is_empty() && seen.insert(n.clone()))
            .collect()
    };
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let mut candidates: Vec<String> = Vec::new();
    let mut answered = false;
    let mut failure = None;
    for query in queries(names).into_iter().take(MAX_SEARCHES) {
        let url = Url::parse_with_params(&format!("{SITE}/search"), [("search", query.as_str())])?;
        let page = match get_html(client, url.as_str()).await {
            Ok(Some(page)) => page,
            Ok(None) => continue,
            Err(e) if e.is::<Challenge>() => return Err(e),
            Err(e) => {
                tracing::warn!("hstream search for {query:?}: {e:#}");
                failure = Some(e);
                continue;
            }
        };
        answered = true;
        let cards = cards(&page.body);
        if let Some(card) = cards.iter().find(|c| {
            matches(&wanted, &normalize(&c.series_title())) || matches(&wanted, &normalize(&c.slug))
        }) {
            return list(client, &card.slug, Some(card.series_title())).await;
        }
        for c in cards {
            if !candidates.contains(&c.slug) {
                candidates.push(c.slug);
            }
        }
    }
    // A name the cards don't show (a Japanese title, say) can still be on
    // the show's page.
    for slug in candidates.iter().take(3) {
        let Some(page) = get_html(client, &format!("{SITE}/hentai/{slug}")).await? else {
            continue;
        };
        let show = show_page(&page.body, slug);
        if show.titles.iter().any(|t| matches(&wanted, &normalize(t))) {
            return Ok(show.episodes);
        }
    }
    // Last, the show's page where its name says it would be.
    for slug in slug_guesses(names) {
        match get_html(client, &format!("{SITE}/hentai/{slug}")).await {
            Ok(Some(page)) => {
                answered = true;
                let show = show_page(&page.body, &slug);
                if !show.episodes.is_empty() {
                    return Ok(show.episodes);
                }
            }
            Ok(None) => answered = true,
            Err(e) if e.is::<Challenge>() => return Err(e),
            Err(e) => failure = Some(e),
        }
    }
    match failure {
        Some(e) if !answered => Err(e.context("searching hstream.moe")),
        _ => Ok(Vec::new()),
    }
}

/// A show's episodes, from its page.
async fn list(client: &reqwest::Client, slug: &str, title: Option<String>) -> Result<Vec<Episode>> {
    let page = get_html(client, &format!("{SITE}/hentai/{slug}"))
        .await?
        .with_context(|| format!("hstream.moe has no page for {slug}"))?;
    let mut show = show_page(&page.body, slug);
    if show.titles.is_empty()
        && let Some(title) = title
    {
        for e in &mut show.episodes {
            e.series = title.clone();
        }
    }
    Ok(show.episodes)
}

/// What to search for: each name, then without punctuation (the site
/// drops apostrophes: "Hon'i" is "Honi"), then each name's first words.
fn queries(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |q: String| {
        let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
        if !q.is_empty() && !out.contains(&q) {
            out.push(q);
        }
    };
    for name in names {
        push(name.clone());
        push(plain(name));
    }
    for name in names {
        let plain = plain(name);
        let words: Vec<&str> = plain.split_whitespace().collect();
        if words.len() > 3 {
            push(words[..3].join(" "));
        }
    }
    out
}

/// `s` with only letters, digits, spaces and hyphens; apostrophes dropped,
/// other punctuation made spaces.
fn plain(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\'' | '’' | '`'))
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect()
}

/// Where a show called `name` would be: `/hentai/<slug>`.
fn slug_guesses(names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        let mut slug = String::new();
        for c in name.chars().flat_map(char::to_lowercase) {
            if c.is_ascii_alphanumeric() {
                slug.push(c);
            } else if !matches!(c, '\'' | '’' | '`') && !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        let slug = slug.trim_end_matches('-').to_owned();
        if slug.is_empty() {
            continue;
        }
        let mut guesses = vec![slug.clone()];
        if !slug.ends_with("-the-animation") {
            guesses.push(format!("{slug}-the-animation"));
        }
        for s in guesses {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out.truncate(4);
    out
}

/// Lowercase letters and digits only.
fn normalize(s: &str) -> String {
    unescape(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether `found` (normalized) is one of `wanted` (normalized), give or
/// take a closing "The Animation".
fn matches(wanted: &[String], found: &str) -> bool {
    if found.is_empty() {
        return false;
    }
    let bare = |s: &str| s.strip_suffix("theanimation").unwrap_or(s).to_owned();
    let found = bare(found);
    wanted.iter().any(|w| !w.is_empty() && bare(w) == found)
}

/// An episode card on a search or show page.
#[derive(Debug, Clone, PartialEq)]
struct Card {
    /// The show's slug: `sister-breeder` for `/hentai/sister-breeder-6`.
    slug: String,
    episode: u32,
    /// e.g. "Sister Breeder - 6".
    title: String,
    resolution: Option<u32>,
}

impl Card {
    /// The show's title: the card's without " - 6".
    fn series_title(&self) -> String {
        static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*-\s*\d+\s*$").unwrap());
        let t = NUMBER.replace(&self.title, "").trim().to_owned();
        if t.is_empty() {
            self.slug.replace('-', " ")
        } else {
            t
        }
    }

    fn into_episode(self, series: &str) -> Episode {
        Episode {
            url: format!("{SITE}/hentai/{}-{}", self.slug, self.episode),
            series: series.to_owned(),
            episode: self.episode,
            title: if self.title.is_empty() {
                format!("{series} - {}", self.episode)
            } else {
                self.title
            },
            resolution: self.resolution,
        }
    }
}

/// The episode cards in a page, once each, in order.
fn cards(html: &str) -> Vec<Card> {
    static LINK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"<a\s[^>]*?href="(?:https?://hstream\.moe)?/hentai/([a-z0-9][a-z0-9-]*?)-(\d+)/?""#,
        )
        .unwrap()
    });
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<h3\b[^>]*>(.*?)</h3>").unwrap());
    static ALT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\balt="([^"]*)""#).unwrap());
    let mut out: Vec<Card> = Vec::new();
    for m in LINK.captures_iter(html) {
        let whole = m.get(0).unwrap();
        let rest = &html[whole.end()..];
        let card = &rest[..rest.find("</a>").unwrap_or(rest.len())];
        let Ok(episode) = m[2].parse::<u32>() else {
            continue;
        };
        let title = TITLE
            .captures(card)
            .map(|c| text(&c[1]))
            .filter(|t| !t.is_empty())
            .or_else(|| {
                ALT.captures(card)
                    .map(|c| unescape(&c[1]).trim().to_owned())
            })
            .unwrap_or_default();
        let card = Card {
            slug: m[1].to_owned(),
            episode,
            title,
            resolution: badge_resolution(card),
        };
        match out
            .iter_mut()
            .find(|c| c.slug == card.slug && c.episode == card.episode)
        {
            Some(seen) => {
                if seen.title.is_empty() {
                    seen.title = card.title;
                }
                seen.resolution = seen.resolution.max(card.resolution);
            }
            None => out.push(card),
        }
    }
    out
}

/// The best resolution a card's badge ("4k | FHD 48fps") lists.
fn badge_resolution(card: &str) -> Option<u32> {
    static TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r">([^<>]+)<").unwrap());
    static RES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(4k|uhd|2160p|fhd|1080p|hd|720p)\b").unwrap());
    let mut best = None;
    for t in TEXT.captures_iter(card) {
        for r in RES.captures_iter(&t[1]) {
            let res = match r[1].to_ascii_lowercase().as_str() {
                "4k" | "uhd" | "2160p" => 2160,
                "fhd" | "1080p" => 1080,
                _ => 720,
            };
            best = best.max(Some(res));
        }
    }
    best
}

struct Show {
    /// Its title and other names on its page.
    titles: Vec<String>,
    episodes: Vec<Episode>,
}

/// A show's page: its titles (the heading, and the Japanese one under it)
/// and its episodes (not the other shows' it recommends).
fn show_page(html: &str, slug: &str) -> Show {
    static H1: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<h1\b[^>]*>(.*?)</h1>(?:\s*<p\b[^>]*>(.*?)</p>)?").unwrap()
    });
    let mut titles = Vec::new();
    if let Some(c) = H1.captures(html) {
        for t in [c.get(1), c.get(2)].into_iter().flatten() {
            let t = text(t.as_str());
            if !t.is_empty() {
                titles.push(t);
            }
        }
    }
    let mut cards: Vec<Card> = cards(html).into_iter().filter(|c| c.slug == slug).collect();
    cards.sort_by_key(|c| c.episode);
    let series = titles
        .first()
        .cloned()
        .or_else(|| cards.first().map(Card::series_title))
        .unwrap_or_else(|| slug.replace('-', " "));
    Show {
        titles,
        episodes: cards.into_iter().map(|c| c.into_episode(&series)).collect(),
    }
}

/// An HTML fragment's text, entities decoded and spaces collapsed.
fn text(html: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
    unescape(&TAG.replace_all(html, " "))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());
    ENTITY
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = e.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match e {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => None,
                }
            };
            ch.map_or_else(|| c[0].to_owned(), String::from)
        })
        .into_owned()
}

struct Page {
    body: String,
    cookies: Vec<(String, String)>,
}

/// A page of the site; None if it hasn't got one (404).
async fn get_html(client: &reqwest::Client, url: &str) -> Result<Option<Page>> {
    let resp = client
        .get(url)
        .header(ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await
        .with_context(|| format!("couldn't reach hstream.moe ({url})"))?;
    let status = resp.status();
    let challenged = resp.headers().get("cf-mitigated").is_some();
    let cookies = cookies(resp.headers());
    let body = resp
        .text()
        .await
        .with_context(|| format!("reading {url}"))?;
    if challenged || (matches!(status.as_u16(), 403 | 429 | 503) && looks_like_challenge(&body)) {
        return Err(Challenge.into());
    }
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    ensure!(
        status.is_success(),
        "hstream.moe answered {status} for {url}"
    );
    Ok(Some(Page { body, cookies }))
}

fn looks_like_challenge(body: &str) -> bool {
    body.contains("challenge-platform")
        || body.contains("<title>Just a moment")
        || body.contains("cf-turnstile")
}

/// The cookies a response sets, as (name, value).
fn cookies(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| {
            let pair = v.split(';').next()?;
            let (name, value) = pair.split_once('=')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Downloading

/// Choices for [`download_with`].
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Highest resolution to take (e.g. 1080 for phones that can't decode
    /// 4K AV1 smoothly); the best there is if None.
    pub max_height: Option<u32>,
}

/// Downloads an episode (its page's URL): the best video (highest
/// resolution) and the best audio, joined into one MP4 at `dest` (`.mp4`
/// added unless it ends so), and English subtitles beside it as
/// `<dest without .mp4>.en.ass` if there are any. Writes `<file>.mp4.part`
/// and renames it when done; removes it on failure or when `cancel` is
/// set (checked while downloading; the error is then [`Cancelled`]).
/// `progress(done, total)`: bytes downloaded and the expected total.
/// Returns the MP4's path.
pub async fn download(
    client: &reqwest::Client,
    episode_url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<PathBuf> {
    download_with(
        client,
        episode_url,
        dest,
        &Options::default(),
        cancel,
        progress,
    )
    .await
}

/// [`download`], with [`Options`].
pub async fn download_with(
    client: &reqwest::Client,
    episode_url: &str,
    dest: &Path,
    options: &Options,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<PathBuf> {
    fetch(client, episode_url, dest, options, None, cancel, progress).await
}

/// What the player's API says about an episode.
#[derive(Debug, Deserialize)]
struct Stream {
    #[serde(default)]
    title: Option<String>,
    stream_url: String,
    stream_domains: Vec<String>,
    #[serde(default)]
    asia_stream_domains: Vec<String>,
}

/// The CDN mirrors serving an episode, the one that last worked first.
#[derive(Debug)]
struct Mirrors {
    /// e.g. `https://imoto-str.ane-h.xyz`, without a trailing slash.
    domains: Vec<String>,
    preferred: AtomicUsize,
}

/// A file on the mirrors: a path (from `/`) any of them serves, or (if an
/// MPD points elsewhere) one URL.
#[derive(Debug, Clone)]
enum Target {
    Path(String),
    Url(String),
}

impl Mirrors {
    fn new(domains: Vec<String>) -> Result<Self> {
        let mut out: Vec<String> = Vec::new();
        for d in domains {
            let d = d.trim().trim_end_matches('/').to_owned();
            let ok = Url::parse(&d)
                .is_ok_and(|u| matches!(u.scheme(), "https" | "http") && u.host().is_some());
            if ok && !out.contains(&d) {
                out.push(d);
            }
        }
        ensure!(!out.is_empty(), "hstream.moe named no stream servers");
        Ok(Mirrors {
            domains: out,
            preferred: AtomicUsize::new(0),
        })
    }

    fn order(&self) -> impl Iterator<Item = usize> + '_ {
        let start = self.preferred.load(Ordering::Relaxed);
        let n = self.domains.len();
        (0..n).map(move |k| (start + k) % n)
    }

    fn url(&self, mirror: usize, target: &Target) -> String {
        match target {
            Target::Path(p) => format!("{}{p}", self.domains[mirror]),
            Target::Url(u) => u.clone(),
        }
    }

    /// `url` (on mirror `mirror`) as a target any mirror serves.
    fn target(&self, mirror: usize, url: &Url) -> Target {
        match url.as_str().strip_prefix(self.domains[mirror].as_str()) {
            Some(path) if path.starts_with('/') => Target::Path(path.to_owned()),
            _ => Target::Url(url.to_string()),
        }
    }
}

enum Fetched {
    Bytes(Vec<u8>),
    Missing,
}

/// One GET of a file on the CDN.
async fn get_bytes(client: &reqwest::Client, url: &str, timeout: Duration) -> Result<Fetched> {
    let resp = client
        .get(url)
        .header(REFERER, format!("{SITE}/"))
        .timeout(timeout)
        .send()
        .await
        .with_context(|| format!("couldn't reach {url}"))?;
    let status = resp.status();
    if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
        return Ok(Fetched::Missing);
    }
    if resp.headers().get("cf-mitigated").is_some() {
        return Err(Challenge.into());
    }
    ensure!(status.is_success(), "{url} answered {status}");
    let body = resp
        .bytes()
        .await
        .with_context(|| format!("downloading {url}"))?;
    Ok(Fetched::Bytes(body.to_vec()))
}

/// `target` from the mirrors, trying each (twice) in turn; the body must
/// pass `check`. None if the mirrors agree it isn't there.
async fn fetch_from(
    client: &reqwest::Client,
    mirrors: &Mirrors,
    target: &Target,
    timeout: Duration,
    check: fn(&[u8]) -> Result<()>,
) -> Result<Option<Vec<u8>>> {
    let mut last: Option<anyhow::Error> = None;
    let mut missing = 0;
    let order: Vec<usize> = match target {
        Target::Url(_) => vec![0],
        Target::Path(_) => mirrors.order().collect(),
    };
    for &mirror in &order {
        let url = mirrors.url(mirror, target);
        for attempt in 0..TRIES_PER_MIRROR {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(700 * u64::from(attempt))).await;
            }
            match get_bytes(client, &url, timeout).await {
                Ok(Fetched::Bytes(body)) => match check(&body) {
                    Ok(()) => {
                        mirrors.preferred.store(mirror, Ordering::Relaxed);
                        return Ok(Some(body));
                    }
                    Err(e) => last = Some(e.context(format!("{url} sent something else"))),
                },
                Ok(Fetched::Missing) => {
                    missing += 1;
                    last = Some(anyhow::anyhow!("{url} isn't there (404)"));
                    break;
                }
                Err(e) => {
                    tracing::debug!("{e:#}");
                    last = Some(e);
                }
            }
        }
        // Two mirrors without it: it isn't there.
        if missing >= 2.min(order.len()) {
            return Ok(None);
        }
        if order.len() > 1 {
            tracing::warn!(
                "{} failed ({:#}); trying another mirror",
                mirrors.domains[mirror],
                last.as_ref().unwrap()
            );
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no mirrors")))
}

fn any_bytes(_: &[u8]) -> Result<()> {
    Ok(())
}

fn media_segment(bytes: &[u8]) -> Result<()> {
    fmp4::check_media_segment(bytes)
}

fn init_segment(bytes: &[u8]) -> Result<()> {
    Init::parse(bytes.to_vec()).map(|_| ())
}

fn subtitles(bytes: &[u8]) -> Result<()> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
    ensure!(head.contains("[Script Info]"), "not ASS subtitles");
    Ok(())
}

/// The episode's page URL, checked to be hstream's.
fn episode_page(url: &str) -> Result<Url> {
    let mut u = Url::parse(url.trim()).with_context(|| format!("not a URL: {url}"))?;
    ensure!(
        matches!(u.host_str(), Some("hstream.moe" | "www.hstream.moe"))
            && u.path().starts_with("/hentai/"),
        "not an hstream.moe episode: {url}"
    );
    if u.scheme() == "http" {
        u.set_scheme("https").ok();
    }
    Ok(u)
}

/// The episode's ID from its page.
fn episode_id(html: &str) -> Option<String> {
    static INPUT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<input\b[^>]*\bid\s*=\s*["']e_id["'][^>]*>"#).unwrap());
    static VALUE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\bvalue\s*=\s*["']([^"']*)["']"#).unwrap());
    let input = INPUT.find(html)?;
    let id = VALUE.captures(input.as_str())?[1].trim().to_owned();
    (!id.is_empty()).then_some(id)
}

/// Asks the player's API about the episode at `page`.
async fn stream(client: &reqwest::Client, page: &Url) -> Result<Stream> {
    let html = get_html(client, page.as_str())
        .await?
        .with_context(|| format!("hstream.moe has no episode at {page}"))?;
    let id = episode_id(&html.body).context("no episode ID on the episode's page")?;
    let cookie = html
        .cookies
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let xsrf = html
        .cookies
        .iter()
        .find(|(n, _)| n == "XSRF-TOKEN")
        .map(|(_, v)| percent_decode(v))
        .context("the episode's page set no XSRF-TOKEN cookie")?;
    let resp = client
        .post(format!("{SITE}/player/api"))
        .header(ACCEPT, "application/json")
        .header(REFERER, page.as_str())
        .header("X-Requested-With", "XMLHttpRequest")
        .header("X-XSRF-TOKEN", xsrf)
        .header(COOKIE, cookie)
        .json(&serde_json::json!({ "episode_id": id }))
        .send()
        .await
        .context("couldn't reach hstream.moe's player")?;
    let status = resp.status();
    let challenged = resp.headers().get("cf-mitigated").is_some();
    let body = resp
        .text()
        .await
        .context("reading hstream.moe's player's answer")?;
    if challenged || (matches!(status.as_u16(), 403 | 429 | 503) && looks_like_challenge(&body)) {
        return Err(Challenge.into());
    }
    ensure!(
        status.is_success(),
        "hstream.moe's player answered {status}"
    );
    let stream: Stream = serde_json::from_str(&body).with_context(|| {
        format!(
            "hstream.moe's player answered something unexpected: {}",
            body.chars().take(200).collect::<String>()
        )
    })?;
    ensure!(
        !stream.stream_url.trim().is_empty(),
        "hstream.moe's player gave no stream"
    );
    Ok(stream)
}

/// `<path>/` of the episode's files on the mirrors, e.g.
/// `/2025/Sister.Breeder/E06/`.
fn stream_path(stream_url: &str) -> String {
    let p = stream_url.replace('\\', "/");
    let p = p.trim_matches('/');
    format!("/{p}/")
}

/// The output's paths: the MP4, its partial file, the subtitles.
fn output_paths(dest: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let name = dest
        .file_name()
        .context("the destination has no file name")?;
    let stem: OsString = match name.to_str() {
        Some(n) if n.len() > 4 && n[n.len() - 4..].eq_ignore_ascii_case(".mp4") => {
            n[..n.len() - 4].into()
        }
        _ => name.to_owned(),
    };
    let with = |suffix: &str| {
        let mut s = stem.clone();
        s.push(suffix);
        dest.with_file_name(s)
    };
    Ok((with(".mp4"), with(".mp4.part"), with(".en.ass")))
}

/// Removes a partial file unless it's been kept.
struct Partial {
    path: PathBuf,
    keep: bool,
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Segment downloads under way, in the order they're needed; stopped if
/// the download is.
struct Queue(VecDeque<(usize, u64, JoinHandle<Result<Option<Vec<u8>>>>)>);

impl Drop for Queue {
    fn drop(&mut self) {
        for (_, _, task) in &self.0 {
            task.abort();
        }
    }
}

/// Picks the best presentation: the highest resolution there is (at most
/// `max_height`), its best video and audio.
async fn presentation(
    client: &reqwest::Client,
    mirrors: &Mirrors,
    path: &str,
    max_height: Option<u32>,
) -> Result<(u32, Representation, Representation)> {
    let mut failures = Vec::new();
    for res in RESOLUTIONS
        .into_iter()
        .filter(|&r| max_height.is_none_or(|m| r <= m))
    {
        let target = Target::Path(format!("{path}{res}/manifest.mpd"));
        let xml =
            match fetch_from(client, mirrors, &target, Duration::from_secs(30), any_bytes).await {
                Ok(Some(xml)) => xml,
                Ok(None) => {
                    tracing::debug!("hstream: no {res}p");
                    continue;
                }
                Err(e) if e.is::<Challenge>() => return Err(e),
                Err(e) => {
                    failures.push(format!("{res}p: {e:#}"));
                    continue;
                }
            };
        let mirror = mirrors.preferred.load(Ordering::Relaxed);
        let mpd_url = Url::parse(&mirrors.url(mirror, &target))?;
        let mpd = match dash::parse(&String::from_utf8_lossy(&xml), &mpd_url) {
            Ok(mpd) => mpd,
            Err(e) => {
                failures.push(format!("{res}p: {e:#}"));
                continue;
            }
        };
        let best = |kind: Kind| {
            mpd.representations
                .iter()
                .filter(|r| r.kind == kind && r.init.is_some())
                .max_by_key(|r| (r.height.unwrap_or(0), r.bandwidth))
                .cloned()
        };
        match (best(Kind::Video), best(Kind::Audio)) {
            (Some(v), Some(a)) => return Ok((res, v, a)),
            _ => failures.push(format!("{res}p: no video and audio in its MPD")),
        }
    }
    if failures.is_empty() {
        bail!("hstream.moe has no stream for this episode");
    }
    bail!(
        "couldn't get the episode's stream ({})",
        failures.join("; ")
    )
}

async fn fetch(
    client: &reqwest::Client,
    episode_url: &str,
    dest: &Path,
    options: &Options,
    limit: Option<usize>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<PathBuf> {
    let page = episode_page(episode_url)?;
    let (file, part, subs) = output_paths(dest)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    let info = stream(client, &page).await?;
    let mut domains = info.stream_domains.clone();
    domains.extend(info.asia_stream_domains.iter().cloned());
    let mirrors = Arc::new(Mirrors::new(domains)?);
    let path = stream_path(&info.stream_url);
    let (res, mut video, mut audio) =
        presentation(client, &mirrors, &path, options.max_height).await?;
    tracing::info!(
        "hstream: {} at {res}p: video {} {}x{} {} kb/s, audio {} {} kb/s",
        info.title.as_deref().unwrap_or(page.as_str()),
        video.codecs,
        video.width.unwrap_or(0),
        video.height.unwrap_or(0),
        video.bandwidth / 1000,
        audio.codecs,
        audio.bandwidth / 1000,
    );
    if let Some(n) = limit {
        video.segments.truncate(n);
        audio.segments.truncate(n);
    }
    let reps = [&video, &audio];
    let mirror = mirrors.preferred.load(Ordering::Relaxed);

    let mut tracks = Vec::new();
    for rep in reps {
        let target = mirrors.target(mirror, rep.init.as_ref().unwrap());
        let bytes = fetch_from(client, &mirrors, &target, SEGMENT_TIMEOUT, init_segment)
            .await?
            .with_context(|| format!("the {} init segment isn't on the mirrors", rep.mime_type))?;
        let init = Init::parse(bytes)?;
        // In the track's timescale (the MPD's is normally the same).
        let ts = u64::from(init.timescale());
        let duration =
            u64::try_from(u128::from(rep.duration()) * u128::from(ts) / u128::from(rep.timescale))?;
        tracks.push(Track { init, duration });
    }
    ensure!(
        tracks[0].init.handler() == *b"vide",
        "the video track isn't video"
    );
    ensure!(
        tracks[1].init.handler() == *b"soun",
        "the audio track isn't audio"
    );
    let (mut muxer, header) = Muxer::new(&tracks)?;

    // Both tracks' segments, in the order they play (video first at a tie).
    let mut schedule: Vec<(usize, &dash::Segment)> = reps
        .iter()
        .enumerate()
        .flat_map(|(t, rep)| rep.segments.iter().map(move |s| (t, s)))
        .collect();
    schedule.sort_by(|a, b| {
        let at = |(t, s): &(usize, &dash::Segment)| reps[*t].seconds(s.time);
        at(a).total_cmp(&at(b)).then(a.0.cmp(&b.0))
    });
    let seconds: Vec<f64> = reps.iter().map(|r| r.seconds(r.duration())).collect();
    let mut estimate: Vec<f64> = reps
        .iter()
        .zip(&seconds)
        .map(|(r, s)| r.bandwidth as f64 / 8.0 * s)
        .collect();
    let mut got_bytes = [0u64; 2];
    let mut got_seconds = [0f64; 2];

    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    // (Before the file, so it's closed before it's removed.)
    let mut partial = Partial {
        path: part.clone(),
        keep: false,
    };
    let mut out = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("creating {}", part.display()))?;
    out.write_all(&header).await.context("writing the video")?;
    let mut done = 0u64;
    progress(0, Some(estimate.iter().sum::<f64>() as u64));

    let mut queue = Queue(VecDeque::new());
    let mut next = 0;
    loop {
        while queue.0.len() < PARALLEL && next < schedule.len() {
            let (track, segment) = schedule[next];
            let target = mirrors.target(mirror, &segment.url);
            let (client, mirrors) = (client.clone(), mirrors.clone());
            let task = tokio::spawn(async move {
                fetch_from(&client, &mirrors, &target, SEGMENT_TIMEOUT, media_segment).await
            });
            queue.0.push_back((track, segment.duration, task));
            next += 1;
        }
        let Some((track, duration, task)) = queue.0.pop_front() else {
            break;
        };
        let bytes = until_cancelled(task, cancel)
            .await?
            .context("a segment download stopped")??
            .context("a segment isn't on the mirrors")?;
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let fragments = muxer.fragments(track, &bytes)?;
        out.write_all(&fragments)
            .await
            .context("writing the video")?;
        done += bytes.len() as u64;
        got_bytes[track] += bytes.len() as u64;
        got_seconds[track] += reps[track].seconds(duration);
        if got_seconds[track] > 0.0 {
            estimate[track] = got_bytes[track] as f64 / got_seconds[track] * seconds[track];
        }
        let total = (estimate.iter().sum::<f64>() as u64).max(done);
        progress(done, Some(total));
    }
    out.write_all(&muxer.finish())
        .await
        .context("writing the video")?;
    out.flush().await.context("writing the video")?;
    out.sync_all().await.context("writing the video")?;
    drop(out);
    tokio::fs::rename(&part, &file)
        .await
        .with_context(|| format!("renaming {} to {}", part.display(), file.display()))?;
    partial.keep = true;

    // Subtitles: their absence (or a failure to get them) isn't the
    // download's.
    let target = Target::Path(format!("{path}eng.ass"));
    match fetch_from(
        client,
        &mirrors,
        &target,
        Duration::from_secs(30),
        subtitles,
    )
    .await
    {
        Ok(Some(ass)) => {
            let mut tmp = subs.clone().into_os_string();
            tmp.push(".part");
            let tmp = PathBuf::from(tmp);
            let saved = async {
                tokio::fs::write(&tmp, &ass).await?;
                tokio::fs::rename(&tmp, &subs).await
            }
            .await;
            if let Err(e) = saved {
                let _ = tokio::fs::remove_file(&tmp).await;
                tracing::warn!(
                    "hstream: couldn't save the subtitles to {}: {e}",
                    subs.display()
                );
            }
        }
        Ok(None) => tracing::info!("hstream: no English subtitles"),
        Err(e) => tracing::warn!("hstream: couldn't get the subtitles: {e:#}"),
    }
    Ok(file)
}

/// The task's result, or [`Cancelled`] as soon as `cancel` is set.
async fn until_cancelled<T>(
    mut task: JoinHandle<T>,
    cancel: &AtomicBool,
) -> Result<Result<T, tokio::task::JoinError>> {
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            r = &mut task => return Ok(r),
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    task.abort();
                    return Err(Cancelled.into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = r#"
    <div wire:key="episode-2061">
    <div class="group w-full p-1">
    <a
        href="https://hstream.moe/hentai/sister-breeder-6"
        class="block overflow-hidden rounded-2xl"
    >
        <div class="relative overflow-hidden">
            <!--[if BLOCK]><![endif]-->                                <img
                    src="/images/hentai/sister-breeder/gallery-ep-6-0-thumbnail.webp"
                    alt="Sister Breeder - 6"
                    loading="lazy"
                    data-gallery='["\/images\/hentai\/sister-breeder\/gallery-ep-6-0-thumbnail.webp"]'
                >
            <div class="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-start justify-between p-3">
                <div class="ml-auto rounded-full bg-black/70 px-2 py-1 text-[11px] font-semibold tracking-wide text-white ring-1 ring-white/10">
                    4k | FHD 48fps
                </div>
            </div>
            <div class="pointer-events-none absolute inset-x-0 bottom-0 z-20 p-4">
                <h3 class=" text-sm font-semibold leading-snug text-white md:text-base">
                    Sister Breeder - 6
                </h3>
                <span class="flex items-center gap-1">
                    <i class="fa-regular fa-heart text-neutral-200 font-bold"></i>
                    138
                </span>
            </div>
        </div>
    </a>
    </div></div>
    <div wire:key="episode-1500"><div class="group w-full p-1">
    <a href="https://hstream.moe/hentai/mujikaku-na-osananajimi-to-kyoumi-honi-de-yatte-mitara-the-animation-2" class="block">
        <img src="/x.webp" alt="Mujikaku na Osananajimi to Kyoumi Honi de Yatte Mitara The Animation - 2">
        <div class="ml-auto">FHD</div>
        <h3 class="text-sm">Mujikaku na Osananajimi to Kyoumi Honi de Yatte Mitara The Animation - 2</h3>
    </a></div></div>
    <a href="https://hstream.moe/search?order=recently-uploaded&amp;tags%5B0%5D=x-ray">X-Ray</a>
    <a href="https://hstream.moe/hentai/old-show-1"><img alt="Old Show &amp; Friends - 1"></a>
    "#;

    #[test]
    fn reads_episode_cards() {
        let cards = cards(SEARCH);
        assert_eq!(cards.len(), 3, "{cards:#?}");
        assert_eq!(
            cards[0],
            Card {
                slug: "sister-breeder".into(),
                episode: 6,
                title: "Sister Breeder - 6".into(),
                resolution: Some(2160),
            }
        );
        assert_eq!(cards[0].series_title(), "Sister Breeder");
        assert_eq!(
            cards[1].slug,
            "mujikaku-na-osananajimi-to-kyoumi-honi-de-yatte-mitara-the-animation"
        );
        assert_eq!(cards[1].resolution, Some(1080));
        assert_eq!(cards[2].title, "Old Show & Friends - 1");
        assert_eq!(cards[2].resolution, None);
    }

    #[test]
    fn reads_show_pages() {
        let html = format!(
            r#"<h1 class="x">
                <span class="bg-linear-to-r">
                    Sister Breeder
                </span>
            </h1>
            <p class="mt-2 text-sm">
                シスターブリーダー
            </p>
            <a class="text-lg font-bold">Episodes (2)</a>
            {}
            <a href="https://hstream.moe/hentai/sister-breeder-1"><h3>Sister Breeder - 1</h3><div>4k</div></a>
            <a href="https://hstream.moe/hentai/sister-breeder-6"><h3>Sister Breeder - 6</h3></a>
            <a href="https://hstream.moe/hentai/harem-tou-e-youkoso-2"><h3>Harem Tou e Youkoso - 2</h3></a>"#,
            SEARCH
        );
        let show = show_page(&html, "sister-breeder");
        assert_eq!(show.titles, ["Sister Breeder", "シスターブリーダー"]);
        let eps: Vec<(u32, &str, Option<u32>)> = show
            .episodes
            .iter()
            .map(|e| (e.episode, e.url.as_str(), e.resolution))
            .collect();
        assert_eq!(
            eps,
            [
                (1, "https://hstream.moe/hentai/sister-breeder-1", Some(2160)),
                (6, "https://hstream.moe/hentai/sister-breeder-6", Some(2160)),
            ]
        );
        assert_eq!(show.episodes[1].series, "Sister Breeder");
        assert_eq!(show.episodes[1].title, "Sister Breeder - 6");
    }

    #[test]
    fn matches_names_loosely() {
        let wanted = vec![normalize(
            "Mujikaku na Osananajimi to Kyoumi Hon'i de Yatte Mitara",
        )];
        assert!(matches(
            &wanted,
            &normalize("Mujikaku na Osananajimi to Kyoumi Honi de Yatte Mitara The Animation")
        ));
        assert!(!matches(&wanted, &normalize("Mujikaku na Osananajimi")));
        let wanted = vec![normalize("Netorareta Bakunyuu Tsuma-tachi")];
        assert!(matches(
            &wanted,
            &normalize("Netorareta Bakunyuu Tsumatachi")
        ));
        assert!(!matches(
            &wanted,
            &normalize("Netorareta Bakunyuu Tsuma-tachi S2")
        ));
        assert!(matches(
            &[normalize("シスターブリーダー")],
            &normalize(" シスター・ブリーダー ")
        ));
        assert!(!matches(&[String::new()], ""));
        assert_eq!(normalize("Old Show &amp; Friends!"), "oldshowfriends");
    }

    #[test]
    fn searches_the_way_the_site_spells() {
        let names = vec![
            "Mujikaku na Osananajimi to Kyoumi Hon'i de Yatte Mitara".to_owned(),
            "Sister Breeder".to_owned(),
        ];
        assert_eq!(
            queries(&names),
            [
                "Mujikaku na Osananajimi to Kyoumi Hon'i de Yatte Mitara",
                "Mujikaku na Osananajimi to Kyoumi Honi de Yatte Mitara",
                "Sister Breeder",
                "Mujikaku na Osananajimi",
            ]
        );
        assert_eq!(
            slug_guesses(&names),
            [
                "mujikaku-na-osananajimi-to-kyoumi-honi-de-yatte-mitara",
                "mujikaku-na-osananajimi-to-kyoumi-honi-de-yatte-mitara-the-animation",
                "sister-breeder",
                "sister-breeder-the-animation",
            ]
        );
        assert_eq!(
            slug_guesses(&["Aneki: My Sweet Elder Sister The Animation".into()]),
            ["aneki-my-sweet-elder-sister-the-animation"]
        );
        assert!(slug_guesses(&["シスターブリーダー".into()]).is_empty());
    }

    #[test]
    fn reads_the_episode_page() {
        let html = r#"<div><input id="e_id" type="hidden" value="2061" /> <script>e_id: document.getElementById('e_id').value</script>"#;
        assert_eq!(episode_id(html).as_deref(), Some("2061"));
        assert_eq!(
            episode_id(r#"<input type="hidden" value="7" id="e_id">"#).as_deref(),
            Some("7")
        );
        assert_eq!(episode_id("<input id=\"x\" value=\"1\">"), None);

        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, "XSRF-TOKEN=eyJpdiI6Ik%3D%3D; expires=Mon, 28 Dec 2026 03:37:26 GMT; Max-Age=2592000; path=/; secure; samesite=lax".parse().unwrap());
        headers.append(
            SET_COOKIE,
            "hstream_session=abc%3D; path=/; secure; httponly"
                .parse()
                .unwrap(),
        );
        let c = cookies(&headers);
        assert_eq!(
            c,
            [
                ("XSRF-TOKEN".to_owned(), "eyJpdiI6Ik%3D%3D".to_owned()),
                ("hstream_session".to_owned(), "abc%3D".to_owned())
            ]
        );
        assert_eq!(percent_decode(&c[0].1), "eyJpdiI6Ik==");
        assert_eq!(percent_decode("a%2Bb%2fc%"), "a+b/c%");

        assert!(episode_page("https://hstream.moe/hentai/sister-breeder-6").is_ok());
        assert_eq!(
            episode_page("http://hstream.moe/hentai/x-1")
                .unwrap()
                .scheme(),
            "https"
        );
        assert!(episode_page("https://example.com/hentai/x-1").is_err());
        assert!(episode_page("https://hstream.moe/search?search=x").is_err());
    }

    #[test]
    fn reads_the_players_answer() {
        let s: Stream = serde_json::from_str(
            r#"{"title":"Sister Breeder - 6","poster":"/images/x.webp","interpolated":1,"interpolated_uhd":0,
            "stream_url":"2025\\Sister.Breeder\\E06","stream_domains":["https://imoto-str.ane-h.xyz/","https://chibi-str.imoto-h.xyz"],
            "asia_stream_domains":["https://komako-b-str.musume-h.xyz"],"extra_subtitles":[]}"#,
        )
        .unwrap();
        assert_eq!(stream_path(&s.stream_url), "/2025/Sister.Breeder/E06/");
        assert_eq!(stream_path("/2025/X/E01/"), "/2025/X/E01/");
        let mut domains = s.stream_domains.clone();
        domains.extend(s.asia_stream_domains);
        domains.push("javascript:alert(1)".into());
        domains.push("https://chibi-str.imoto-h.xyz/".into());
        let m = Mirrors::new(domains).unwrap();
        assert_eq!(
            m.domains,
            [
                "https://imoto-str.ane-h.xyz",
                "https://chibi-str.imoto-h.xyz",
                "https://komako-b-str.musume-h.xyz"
            ]
        );
        let url =
            Url::parse("https://imoto-str.ane-h.xyz/2025/X/E01/1080/chunks/init-stream0.webp")
                .unwrap();
        let t = m.target(0, &url);
        assert_eq!(
            m.url(1, &t),
            "https://chibi-str.imoto-h.xyz/2025/X/E01/1080/chunks/init-stream0.webp"
        );
        m.preferred.store(2, Ordering::Relaxed);
        assert_eq!(m.order().collect::<Vec<_>>(), [2, 0, 1]);
        let elsewhere = Url::parse("https://other.example/a.mp4").unwrap();
        assert_eq!(
            m.url(1, &m.target(0, &elsewhere)),
            "https://other.example/a.mp4"
        );
        assert!(Mirrors::new(vec![]).is_err());
    }

    #[test]
    fn names_the_output() {
        let (file, part, subs) = output_paths(Path::new("/lib/Dr. Show - 01")).unwrap();
        assert_eq!(file, Path::new("/lib/Dr. Show - 01.mp4"));
        assert_eq!(part, Path::new("/lib/Dr. Show - 01.mp4.part"));
        assert_eq!(subs, Path::new("/lib/Dr. Show - 01.en.ass"));
        let (file, _, subs) = output_paths(Path::new("ep.MP4")).unwrap();
        assert_eq!(file, Path::new("ep.mp4"));
        assert_eq!(subs, Path::new("ep.en.ass"));
        assert!(output_paths(Path::new("/")).is_err());
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            unescape("a &amp; b &#039;c&#x27; &lt;d&gt; &bogus;"),
            "a & b 'c' <d> &bogus;"
        );
        assert_eq!(
            text("<span>\n  Sister   <b>Breeder</b>\n</span>"),
            "Sister Breeder"
        );
    }

    #[test]
    fn spots_challenges() {
        assert!(looks_like_challenge(
            "<html><head><title>Just a moment...</title>"
        ));
        assert!(!looks_like_challenge("<html><title>hstream.moe</title>"));
    }

    /// The download runs on the app's runtime, across threads.
    #[allow(dead_code)]
    fn download_is_send(client: &reqwest::Client, cancel: &AtomicBool) {
        fn send<T: Send>(_: T) {}
        send(download(client, "", Path::new(""), cancel, |_, _| {}));
        send(episodes(client, &[]));
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent("ichioshi-tests")
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
    }

    fn test_dir() -> PathBuf {
        std::env::var_os("HSTREAM_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("hstream-test"))
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn live_finds_episodes() {
        let client = client();
        let eps = episodes(&client, &["Sister Breeder".into()]).await.unwrap();
        assert_eq!(
            eps.iter().map(|e| e.episode).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6],
            "{eps:#?}"
        );
        assert_eq!(eps[5].url, "https://hstream.moe/hentai/sister-breeder-6");
        assert_eq!(eps[5].series, "Sister Breeder");
        assert_eq!(eps[5].title, "Sister Breeder - 6");
        assert_eq!(eps[5].resolution, Some(2160));

        // Spelled as AniList does (an apostrophe, no "The Animation").
        let eps = episodes(
            &client,
            &["Mujikaku na Osananajimi to Kyoumi Hon'i de Yatte Mitara".into()],
        )
        .await
        .unwrap();
        assert!(eps.len() >= 3, "{eps:#?}");
        assert!(eps[0].url.ends_with("-the-animation-1"), "{eps:#?}");

        // Only its Japanese title: the show's page has it.
        let eps = episodes(&client, &["シスターブリーダー".into()])
            .await
            .unwrap();
        assert_eq!(eps.len(), 6, "{eps:#?}");

        let eps = episodes(&client, &["Zzyzx Qwerty Nonexistent".into()])
            .await
            .unwrap();
        assert!(eps.is_empty(), "{eps:#?}");
    }

    async fn sample(url: &str, name: &str, options: Options, segments: usize) -> PathBuf {
        let dir = test_dir();
        let cancel = AtomicBool::new(false);
        let mut calls = Vec::new();
        let file = fetch(
            &client(),
            url,
            &dir.join(name),
            &options,
            Some(segments),
            &cancel,
            |d, t| calls.push((d, t)),
        )
        .await
        .unwrap();
        assert_eq!(file, dir.join(format!("{name}.mp4")));
        assert!(!dir.join(format!("{name}.mp4.part")).exists());
        assert!(dir.join(format!("{name}.en.ass")).exists());
        let (done, total) = *calls.last().unwrap();
        assert_eq!(calls.len(), 2 * segments + 1);
        assert!(total.unwrap() >= done && done > 0, "{calls:?}");
        eprintln!("{}: {done} bytes; progress {calls:?}", file.display());
        file
    }

    /// The first few segments of an episode, at 2160p (AV1) and at 720p
    /// (H.264): check them with ffprobe, gst-discoverer-1.0, mpv.
    #[tokio::test]
    #[ignore = "network: downloads ~20 MB into $HSTREAM_TEST_DIR"]
    async fn live_downloads_samples() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("hstream=debug")
            .try_init();
        let url = "https://hstream.moe/hentai/sister-breeder-6";
        sample(url, "sample-2160", Options::default(), 3).await;
        sample(
            url,
            "sample-720",
            Options {
                max_height: Some(720),
            },
            3,
        )
        .await;
    }

    /// Cancelling stops the download and leaves no partial file.
    #[tokio::test]
    #[ignore = "network"]
    async fn live_cancels() {
        let dir = test_dir();
        let cancel = AtomicBool::new(false);
        let options = Options {
            max_height: Some(720),
        };
        let err = fetch(
            &client(),
            "https://hstream.moe/hentai/sister-breeder-6",
            &dir.join("cancelled"),
            &options,
            None,
            &cancel,
            |done, _| {
                if done > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .await
        .unwrap_err();
        assert!(err.is::<Cancelled>(), "{err:#}");
        assert!(!dir.join("cancelled.mp4.part").exists());
        assert!(!dir.join("cancelled.mp4").exists());
    }
}
