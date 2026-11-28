//! hanime.tv.
//!
//! Its search, like its own search page's, loads the site's search
//! database (every video's name, slug and other titles, as JSON, from the
//! address its pages name as `search_hvs_url`) and looks for the show in
//! it; an episode is at `/videos/hentai/<slug>`. If the database can't be
//! had, episodes are looked for where the show's name says they'd be
//! (`/videos/hentai/<slug>-<n>`, from 1 up until one isn't there), each
//! page's heading ("Overflow 1") naming the show.
//!
//! An episode's streams come from the player's handshake
//! (`auth.hanime.tv/api/v11/handshake`): a token (JSON, AES-256-GCM with the
//! player's own key and header, base64), a signature (SHA-256 of the time
//! and the player's constants) and that time. The answer's `X-Token` header,
//! sealed the same way, lists HLS playlists; only the free ("normal") ones
//! are used.

use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, ORIGIN, REFERER};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::web::{self, Session, base, matches, normalize, py_object, slug, text, wanted};
use super::{Episode, Media, Resolved, Stream};

const SITE: &str = "https://hanime.tv";
const AUTH: &str = "https://auth.hanime.tv";
/// The search database, unless the site's pages name another.
const SEARCH_DB: &str = "https://guest.freeanimehentai.net/api/v11/search_hvs";
/// How long the search database is kept before it's fetched again.
const KEEP_DB: Duration = Duration::from_secs(3600);
/// The player's key and associated data for its tokens.
const KEY: [u8; 32] = [
    0x5d, 0x65, 0x7a, 0x4d, 0xcb, 0x0b, 0xad, 0x1c, 0x63, 0x7f, 0xf2, 0xe2, 0x21, 0x05, 0x9b, 0x10,
    0xff, 0x17, 0xae, 0x39, 0xfe, 0x85, 0x50, 0x03, 0xe8, 0x46, 0x91, 0x89, 0x41, 0xf4, 0xeb, 0xe3,
];
const HEADER: &[u8] = b"htv-insecure-v1";
/// Episodes looked for at most per show.
const MAX_EPISODES: u32 = 24;
/// Names tried at most.
const MAX_NAMES: usize = 3;

/// A video in the search database.
#[derive(Debug, Clone, PartialEq)]
struct Doc {
    /// e.g. "Overflow 1".
    name: String,
    slug: String,
    /// Its other titles.
    titles: Vec<String>,
}

/// The show's episodes, from the search database (or, without it, at the
/// addresses its names would have).
pub(super) async fn episodes(s: &Session, names: &[String]) -> Result<Vec<Episode>> {
    let wanted = wanted(names);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    match database(s).await {
        Ok(db) => return Ok(in_database(&db, &wanted)),
        Err(e) if e.is::<web::Challenge>() => return Err(e),
        Err(e) => tracing::warn!("hanime.tv's search database: {e:#}; guessing addresses"),
    }
    guessed(s, names, &wanted).await
}

/// The show's episodes in the database: videos named it with a number
/// ("Sister Breeder 3"); else the videos whose other titles hold one of its
/// names ("Kuroinu 2" is also "Kuroinu: Kedakaki Seijo wa Hakudaku ni
/// Somaru"), those of the show most of them are. A whole season in one
/// video ("Overflow Season 1") isn't an episode, so isn't taken.
fn in_database(db: &[Doc], wanted: &[String]) -> Vec<Episode> {
    static SEASON: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bseason\s*\d+\s*$").unwrap());
    let videos: Vec<&Doc> = db.iter().filter(|d| !SEASON.is_match(&d.name)).collect();
    let show_of = |d: &Doc| web::without_episode(&d.name);
    let mut found: Vec<&Doc> = videos
        .iter()
        .copied()
        .filter(|d| matches(wanted, &normalize(&show_of(d))))
        .collect();
    if found.is_empty() {
        // Names long enough not to be in other shows' titles by chance.
        let long: Vec<&String> = wanted.iter().filter(|w| w.chars().count() >= 6).collect();
        let mut shows: Vec<(String, Vec<&Doc>)> = Vec::new();
        for d in videos {
            let titles = normalize(&d.titles.join(" "));
            if !long.iter().any(|w| titles.contains(w.as_str())) {
                continue;
            }
            let show = normalize(&show_of(d));
            match shows.iter_mut().find(|(s, _)| *s == show) {
                Some((_, docs)) => docs.push(d),
                None => shows.push((show, vec![d])),
            }
        }
        if let Some((_, docs)) = shows.into_iter().max_by_key(|(_, docs)| docs.len()) {
            found = docs;
        }
    }
    let site = base(SITE);
    let mut out: Vec<Episode> = Vec::new();
    for d in found {
        let n = web::episode_number(&d.name)
            .or_else(|| web::episode_number(&d.slug))
            .unwrap_or(1);
        if out.iter().any(|e| e.episode == n) {
            continue;
        }
        out.push(Episode {
            episode: n,
            url: format!("{site}/videos/hentai/{}", d.slug),
            title: super::episode_title(&show_of(d), n),
        });
    }
    out.sort_by_key(|e| e.episode);
    out
}

/// The search database as fetched: when, from where.
struct Kept {
    at: Instant,
    url: String,
    db: Arc<Vec<Doc>>,
}

/// The search database, fetched at most once an hour.
async fn database(s: &Session) -> Result<Arc<Vec<Doc>>> {
    static KEPT: Mutex<Option<Kept>> = Mutex::new(None);
    let url = database_url(s).await;
    if let Some(k) = KEPT.lock().unwrap().as_ref()
        && k.url == url
        && k.at.elapsed() < KEEP_DB
    {
        return Ok(k.db.clone());
    }
    let body = s
        .text(
            s.client
                .get(&url)
                .header(ACCEPT, "application/json")
                .header(ORIGIN, "https://hanime.tv")
                .header(REFERER, "https://hanime.tv/"),
        )
        .await?
        .context("hanime.tv's search database isn't there")?;
    let db = Arc::new(parse_database(&body)?);
    *KEPT.lock().unwrap() = Some(Kept {
        at: Instant::now(),
        url,
        db: db.clone(),
    });
    Ok(db)
}

/// Where the search database is: as the search page names it, else the
/// address it's had.
async fn database_url(s: &Session) -> String {
    let named = match s.page(&format!("{}/search", base(SITE))).await {
        Ok(Some(page)) => database_url_in(&page),
        _ => None,
    };
    named.unwrap_or_else(|| base(SEARCH_DB))
}

fn database_url_in(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""search_hvs_url"\s*:\s*"([^"]+)""#).unwrap());
    let url = RE.captures(page)?[1].replace("\\/", "/");
    Url::parse(&url).ok().map(|_| url)
}

/// The search database's videos: a list, or one under `data` or `hits`.
fn parse_database(json: &str) -> Result<Vec<Doc>> {
    let v: Value = serde_json::from_str(json).context("hanime.tv's search database isn't JSON")?;
    let list = v
        .as_array()
        .or_else(|| v.get("data").and_then(Value::as_array))
        .or_else(|| v.get("hits").and_then(Value::as_array))
        .context("hanime.tv's search database lists no videos")?;
    let text = |d: &Value, k: &str| {
        d.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    Ok(list
        .iter()
        .map(|d| Doc {
            name: text(d, "name"),
            slug: text(d, "slug"),
            titles: match d.get("search_titles") {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                Some(Value::String(t)) => vec![t.clone()],
                _ => Vec::new(),
            },
        })
        .filter(|d| !d.name.is_empty() && !d.slug.is_empty())
        .collect())
}

/// The show's episodes at the addresses its names would have.
async fn guessed(s: &Session, names: &[String], wanted: &[String]) -> Result<Vec<Episode>> {
    let mut slugs: Vec<String> = Vec::new();
    for n in names.iter().map(|n| slug(n)) {
        if !n.is_empty() && !slugs.contains(&n) {
            slugs.push(n);
        }
    }
    for show in slugs.iter().take(MAX_NAMES) {
        let mut found = Vec::new();
        // Its addresses have been both ways: the first that's there.
        let mut form = None;
        for n in 1..=MAX_EPISODES {
            let mut page = None;
            for (i, url) in episode_urls(show, n).into_iter().enumerate() {
                if form.is_some_and(|f| f != i) {
                    continue;
                }
                if let Some(p) = s.page(&url).await? {
                    form = Some(i);
                    page = Some((url, p));
                    break;
                }
            }
            let Some((url, page)) = page else {
                break;
            };
            let Some(title) = heading(&page) else {
                break;
            };
            if !matches(wanted, &normalize(&web::without_episode(&title))) {
                break;
            }
            found.push(Episode {
                episode: n,
                url,
                title: super::episode_title(&web::without_episode(&title), n),
            });
        }
        if !found.is_empty() {
            return Ok(found);
        }
    }
    Ok(Vec::new())
}

/// Where episode `n` of the show `slug` would be.
fn episode_urls(slug: &str, n: u32) -> [String; 2] {
    let site = base(SITE);
    [
        format!("{site}/videos/hentai/{slug}-{n}"),
        format!("{site}/hentai/video/{slug}-{n}"),
    ]
}

/// The page's first heading: the episode's title.
fn heading(page: &str) -> Option<String> {
    static H1: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<h1[^>]+?>([^<]+)").unwrap());
    Some(text(&H1.captures(page)?[1])).filter(|t| !t.is_empty())
}

/// The episode's slug: `overflow-1` from `/videos/hentai/overflow-1`.
fn video_id(url: &str) -> Result<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"/(?:videos/hentai|hentai/video|playlists/[0-9a-z]+/video)/([0-9a-z-]+)")
            .unwrap()
    });
    Ok(RE
        .captures(url)
        .with_context(|| format!("not a hanime.tv episode: {url}"))?[1]
        .to_owned())
}

fn key() -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &KEY).expect("a 32-byte key"))
}

/// `payload` sealed as the player seals its tokens, with `iv`.
fn seal(payload: &str, iv: [u8; 12]) -> Result<String> {
    let mut data = payload.as_bytes().to_vec();
    let tag = key()
        .seal_in_place_separate_tag(
            Nonce::assume_unique_for_key(iv),
            Aad::from(HEADER),
            &mut data,
        )
        .map_err(|_| anyhow!("couldn't seal the player's token"))?;
    let envelope = py_object(&[
        ("v", json!(1)),
        ("alg", json!("AES-256-GCM")),
        ("iv", json!(URL_SAFE_NO_PAD.encode(iv))),
        ("tag", json!(URL_SAFE_NO_PAD.encode(tag.as_ref()))),
        ("data", json!(URL_SAFE_NO_PAD.encode(&data))),
    ]);
    Ok(URL_SAFE_NO_PAD.encode(envelope))
}

#[derive(Debug, Deserialize)]
struct Envelope {
    iv: String,
    tag: String,
    data: String,
}

/// A token sealed by the player, opened.
fn open(token: &str) -> Result<Value> {
    let envelope: Envelope =
        serde_json::from_slice(&web::b64(token)?).context("hanime.tv's token isn't what it was")?;
    let iv: [u8; 12] = web::b64(&envelope.iv)?
        .try_into()
        .map_err(|_| anyhow!("hanime.tv's token has a {}-byte IV", envelope.iv.len()))?;
    let mut sealed = web::b64(&envelope.data)?;
    sealed.extend_from_slice(&web::b64(&envelope.tag)?);
    let plain = key()
        .open_in_place(
            Nonce::assume_unique_for_key(iv),
            Aad::from(HEADER),
            &mut sealed,
        )
        .map_err(|_| anyhow!("hanime.tv's token doesn't open with the player's key"))?;
    serde_json::from_slice(plain).context("hanime.tv's token holds something else")
}

/// The player's signature for `time` (seconds since 1970).
fn signature(time: u64) -> String {
    let d = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{time},Xkdi29,https://hanime.tv,mn2,{time}").as_bytes(),
    );
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn random_iv() -> Result<[u8; 12]> {
    let mut iv = [0u8; 12];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut iv)
        .map_err(|_| anyhow!("no random numbers"))?;
    Ok(iv)
}

/// The handshake's request body: `{"token": …}` for `slug` at `time`.
fn handshake_body(slug: &str, time: u64, iv: [u8; 12]) -> Result<String> {
    let payload = py_object(&[
        ("timestamp_unix", json!(time)),
        ("directive", json!("htv_player_handshake")),
        ("slug", json!(slug)),
    ]);
    Ok(py_object(&[("token", json!(seal(&payload, iv)?))]))
}

pub(super) async fn resolve(s: &Session, url: &str) -> Result<Resolved> {
    let id = video_id(url)?;
    let page = s.need(url).await?;
    let title = heading(&page);
    let time = now();
    let body = handshake_body(&id, time, random_iv()?)?;
    let resp = s
        .send(
            s.client
                .post(format!("{}/api/v11/handshake", base(AUTH)))
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .header(ORIGIN, "https://hanime.tv")
                .header(REFERER, "https://hanime.tv/")
                .header("X-Csrf-Token", "null")
                .header("X-Signature", signature(time))
                .header("X-Time", time.to_string())
                .header("X-Signature-Version", "web2")
                .body(body),
        )
        .await?;
    let status = resp.status();
    ensure!(
        status.is_success(),
        "hanime.tv's player handshake answered {status}"
    );
    let token = resp
        .headers()
        .get("x-token")
        .and_then(|v| v.to_str().ok())
        .context("hanime.tv's player handshake sent no token")?
        .to_owned();
    let manifest = open(&token)?;
    Ok(Resolved {
        title,
        streams: streams(&manifest)?,
    })
}

/// The free streams a handshake's manifest lists.
fn streams(manifest: &Value) -> Result<Vec<Stream>> {
    let sources = manifest
        .get("sources")
        .and_then(Value::as_array)
        .context("hanime.tv's manifest lists no streams")?;
    let site = Url::parse(&base(SITE))?;
    let mut out = Vec::new();
    for source in sources {
        let field = |k: &str| source.get(k).and_then(Value::as_str).unwrap_or_default();
        // Premium streams need an account.
        if field("kind") != "normal" || field("src").is_empty() {
            continue;
        }
        let Ok(url) = site.join(field("src")) else {
            continue;
        };
        let label = field("label");
        out.push(Stream {
            media: Media::Hls(url),
            referer: None,
            resolution: crate::ohentai::label_resolution(label)
                .or_else(|| label.trim_end_matches(['p', 'P']).parse().ok()),
            label: if label.is_empty() { "HLS" } else { label }.to_owned(),
        });
    }
    Ok(out)
}

/// What's at the first address `name` would have, for the live test:
/// nothing, a page without a heading, or the heading.
#[cfg(test)]
pub(super) async fn diagnose(s: &Session, name: &str) -> String {
    let mut out = Vec::new();
    for url in episode_urls(&slug(name), 1) {
        let what = match s.page(&url).await {
            Ok(None) => "no page".into(),
            Ok(Some(page)) => heading(&page).map_or("a page without a heading".into(), |h| {
                format!("titled {h:?}")
            }),
            Err(e) => format!("{e:#}"),
        };
        let path = url.split("hanime.tv").nth(1).unwrap_or(&url).to_owned();
        out.push(format!("{path}: {what}"));
    }
    out.join("; ")
}

#[cfg(test)]
pub(super) fn seal_for_test(manifest: &Value) -> String {
    seal(&manifest.to_string(), [7; 12]).unwrap()
}

#[cfg(test)]
pub(super) fn open_for_test(token: &str) -> Value {
    open(token).unwrap()
}

#[cfg(test)]
pub(super) fn signature_for_test(time: u64) -> String {
    signature(time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_opens_tokens_as_the_player() {
        let body = handshake_body("overflow-1", 1_700_000_000, [1; 12]).unwrap();
        let token: Value = serde_json::from_str(&body).unwrap();
        assert!(body.starts_with(r#"{"token": ""#));
        let inner = open(token["token"].as_str().unwrap()).unwrap();
        assert_eq!(
            inner,
            json!({"timestamp_unix": 1_700_000_000u64, "directive": "htv_player_handshake", "slug": "overflow-1"})
        );
        // The envelope is Python's json.dumps of the dict, in order.
        let envelope =
            String::from_utf8(web::b64(token["token"].as_str().unwrap()).unwrap()).unwrap();
        assert!(
            envelope.starts_with(r#"{"v": 1, "alg": "AES-256-GCM", "iv": ""#),
            "{envelope}"
        );
        // Tampered: it doesn't open.
        let mut bad = token["token"].as_str().unwrap().to_owned();
        bad.insert(40, 'A');
        assert!(open(&bad).is_err());
    }

    #[test]
    fn finds_shows_in_the_search_database() {
        let doc = |name: &str, slug: &str, titles: &str| Doc {
            name: name.into(),
            slug: slug.into(),
            titles: vec![titles.into()],
        };
        let db = [
            doc(
                "Overflow Season 1",
                "overflow-season-1",
                "Overflow Season 1 Overflow おーばーふろぉ",
            ),
            doc(
                "Kuroinu 2",
                "kuroinu-2",
                "Kuroinu 2 Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru 黒獣",
            ),
            doc(
                "Kuroinu 1",
                "kuroinu-1",
                "Kuroinu 1 Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru 黒獣",
            ),
            doc(
                "Kuroinu 2 Episode 1",
                "kuroinu-2-episode-1",
                "Kuroinu 2 Episode 1 Kuroinu II The Animation",
            ),
            doc(
                "Sister Breeder 1",
                "sister-breeder-1",
                "Sister Breeder 1 OVA Sister Breeder OVA",
            ),
        ];
        let find = |names: &[&str]| {
            let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
            in_database(&db, &wanted(&names))
                .into_iter()
                .map(|e| (e.episode, e.url.rsplit('/').next().unwrap().to_owned()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            find(&["Sister Breeder"]),
            [(1, "sister-breeder-1".to_owned())]
        );
        assert_eq!(
            find(&["Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru"]),
            [(1, "kuroinu-1".to_owned()), (2, "kuroinu-2".to_owned())],
            "by its other title; not the sequel"
        );
        assert!(
            find(&["Overflow"]).is_empty(),
            "a whole season isn't episode 1"
        );
    }

    #[test]
    fn signs_like_the_player() {
        // SHA-256 of "1700000000,Xkdi29,https://hanime.tv,mn2,1700000000".
        let expected: String = ring::digest::digest(
            &ring::digest::SHA256,
            b"1700000000,Xkdi29,https://hanime.tv,mn2,1700000000",
        )
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
        assert_eq!(signature(1_700_000_000), expected);
        assert_eq!(signature(1).len(), 64);
    }

    #[test]
    fn reads_manifests_and_pages() {
        let manifest = json!({"sources": [
            {"kind": "premium", "src": "/p/1080.m3u8", "label": "1080p"},
            {"kind": "normal", "src": "/api/v8/s/720.m3u8", "label": "720p"},
            {"kind": "normal", "src": "https://cdn.example/480/index.m3u8", "label": "480"},
        ]});
        let s = streams(&manifest).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0].media,
            Media::Hls("https://hanime.tv/api/v8/s/720.m3u8".parse().unwrap())
        );
        assert_eq!(s[0].resolution, Some(720));
        assert_eq!(s[1].resolution, Some(480));
        assert_eq!(
            video_id("https://hanime.tv/videos/hentai/overflow-1").unwrap(),
            "overflow-1"
        );
        assert!(video_id("https://hanime.tv/browse").is_err());
        assert_eq!(
            heading(r#"<div><h1 class="tv-title">Overflow 1</h1></div>"#).as_deref(),
            Some("Overflow 1")
        );
    }
}
