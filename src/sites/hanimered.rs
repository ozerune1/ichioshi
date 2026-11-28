//! hanime.red.
//!
//! Its search (`/search?q=…`) lists the series it finds as links to their
//! pages (`/serie/<slug>`), which list their episodes (`/<slug>-episode-<n>`).
//!
//! An episode's page embeds a player page, which names the real player on
//! nhplayer.com. That page holds the player's parameters (`window._pV`)
//! and five challenge parts, hidden in elements its script names; the
//! script holds two server tokens. The video's address comes from
//! `get-video-url-v2.php`, asked with all of these, a proof of work over
//! the parts (a SHA-256 starting with two zero bytes) and a plain browser
//! fingerprint (a 1920×1080 screen, no mouse or key events), as the
//! hanime-plugin sends them.

use std::sync::LazyLock;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use reqwest::header::ACCEPT;
use serde_json::Value;
use url::Url;

use super::web::{self, Session, absolute, base, text};
use super::{Card, Media, Resolved, Show, Stream};

const SITE: &str = "https://hanime.red";
const PLAYER: &str = "https://nhplayer.com";
/// The proof of work gives up after this many tries (it takes about 65 536).
const MAX_WORK: u32 = 10_000_000;

pub(super) async fn search(s: &Session, query: &str) -> Result<Option<Vec<Card>>> {
    let url = Url::parse_with_params(&format!("{}/search", base(SITE)), [("q", query)])?;
    Ok(s.page(url.as_str()).await?.map(|html| cards(&html)))
}

/// The series a search page links to.
fn cards(html: &str) -> Vec<Card> {
    static CHIP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<a class="chip" href="(?:https?://[^/"]+)?/serie/([^"/?#]+)"[^>]*>(.*?)(?:<span|</a>)"#)
            .unwrap()
    });
    let mut out: Vec<Card> = Vec::new();
    for c in CHIP.captures_iter(html) {
        let slug = c[1].to_owned();
        let title = text(&c[2]);
        if !out.iter().any(|o| o.slug == slug) {
            out.push(Card {
                slug,
                titles: vec![title].into_iter().filter(|t| !t.is_empty()).collect(),
            });
        }
    }
    out
}

pub(super) async fn show(s: &Session, slug: &str) -> Result<Option<Show>> {
    let url = format!("{}/serie/{slug}", base(SITE));
    Ok(s.page(&url).await?.map(|html| show_page(&html, &url, slug)))
}

/// A series' page: its title and its episodes' cards.
fn show_page(html: &str, url: &str, slug: &str) -> Show {
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<h1\b[^>]*>(.*?)</h1>").unwrap());
    static CARD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<a class="card[^"]*" title="([^"]*)" href="([^"]+)""#).unwrap()
    });
    let titles: Vec<String> = TITLE
        .captures(html)
        .map(|c| text(&c[1]))
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect();
    let show = titles
        .first()
        .cloned()
        .unwrap_or_else(|| slug.replace('-', " "));
    let links = CARD
        .captures_iter(html)
        .map(|c| (c[2].to_owned(), web::unescape(&c[1])))
        .collect();
    Show {
        titles,
        episodes: super::numbered(url, &show, links),
    }
}

/// What the real player's page and script give.
#[derive(Debug, Clone, PartialEq)]
struct Challenge {
    /// `window._pV`: the player's `vid`, `ct`, `pid` and `st`.
    params: Value,
    parts: Vec<String>,
    tokens: (String, String),
}

pub(super) async fn resolve(s: &Session, url: &str) -> Result<Resolved> {
    let page = s.need(url).await?;
    let title = episode_title(&page);
    let wrapper = iframe(&page).context("no player on hanime.red's episode page")?;
    let wrapper = absolute(url, &wrapper)?;
    let wrapper_page = s.need(wrapper.as_str()).await?;
    let id = player_id(&wrapper_page).context("hanime.red's player names no player")?;
    let player_url = format!("{}/{}", base(PLAYER), id.trim_start_matches('/'));
    let player = s.need(&player_url).await?;
    let script = script_name(&player).context("nhplayer's page has no player script")?;
    let script = s
        .need(&format!(
            "{}/{}",
            base(PLAYER),
            script.trim_start_matches('/')
        ))
        .await?;
    let started = monotonic_ms();
    let challenge = challenge(&player, &script)?;
    let joined = challenge.parts.concat();
    let work = tokio::task::spawn_blocking(move || proof_of_work(&joined))
        .await?
        .context("couldn't do nhplayer's proof of work")?;
    let fingerprint = fingerprint(started + 2100);
    // The plugin reads only the first token (yt-dlp's `_search_regex`
    // gives one group) and sends its first two letters: the tokens
    // themselves go first, that after.
    let (a, b) = &challenge.tokens;
    let first_two: Vec<String> = a.chars().take(2).map(String::from).collect();
    let mut tries = vec![(a.clone(), b.clone())];
    if first_two.len() == 2 {
        tries.push((first_two[0].clone(), first_two[1].clone()));
    }
    let mut last = None;
    for (sc, rid) in tries {
        let query = video_query(&challenge, &sc, &rid, &fingerprint, &work);
        let answer = s
            .text(
                s.client
                    .get(format!("{}/get-video-url-v2.php", base(PLAYER)))
                    .query(&query)
                    .header(ACCEPT, "application/json, text/javascript, */*")
                    .header("X-Requested-With", "XMLHttpRequest"),
            )
            .await;
        match answer.map(|a| a.map(|t| serde_json::from_str::<Value>(&t))) {
            Ok(Some(Ok(json))) => match json.get("url").and_then(Value::as_str) {
                Some(video) if !video.trim().is_empty() => {
                    let video = absolute(&player_url, video)?;
                    let hls = video.path().ends_with(".m3u8") || is_playlist(s, &video).await;
                    return Ok(Resolved {
                        title,
                        streams: vec![Stream {
                            media: if hls {
                                Media::Hls(video)
                            } else {
                                Media::File(video)
                            },
                            referer: None,
                            resolution: None,
                            label: if hls { "HLS" } else { "MP4" }.into(),
                        }],
                    });
                }
                _ => {
                    last = Some(anyhow::anyhow!(
                        "nhplayer gave no video: {}",
                        json.to_string().chars().take(200).collect::<String>()
                    ))
                }
            },
            Ok(Some(Err(e))) => {
                last = Some(anyhow::Error::from(e).context("nhplayer's answer isn't JSON"))
            }
            Ok(None) => last = Some(anyhow::anyhow!("nhplayer's video API isn't there")),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("nhplayer gave no video")))
}

/// Whether `url` is an HLS playlist whatever its name (its first bytes
/// say), not a video file.
async fn is_playlist(s: &Session, url: &Url) -> bool {
    let head = s
        .send(
            s.client
                .get(url.as_str())
                .header(reqwest::header::RANGE, "bytes=0-15"),
        )
        .await;
    let Ok(resp) = head else { return false };
    let kind = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if kind.contains("mpegurl") {
        return true;
    }
    if kind.starts_with("video/") || resp.status() == reqwest::StatusCode::OK {
        // A video, or a server sending all of it: not read further.
        return false;
    }
    resp.bytes()
        .await
        .is_ok_and(|b| b.trim_ascii_start().starts_with(b"#EXTM3U"))
}

fn episode_title(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<h1 duration="" .+>([^<]+)</h1>"#).unwrap());
    Some(text(&RE.captures(page)?[1])).filter(|t| !t.is_empty())
}

fn iframe(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<iframe src="([^"]+)""#).unwrap());
    Some(RE.captures(page)?[1].to_owned())
}

fn player_id(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"li data-id="([^"]+)""#).unwrap());
    Some(web::unescape(&RE.captures(page)?[1]))
}

fn script_name(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"script src="(player-core[^"]+)""#).unwrap());
    Some(web::unescape(&RE.captures(page)?[1]))
}

/// The real player's parameters, challenge parts and server tokens.
fn challenge(player: &str, script: &str) -> Result<Challenge> {
    static PARAMS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"window\._pV\s*=\s*").unwrap());
    static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"//.+challenge token.+\n?var \w+='([^']+)';\n?var \w+='([^']+)';\n").unwrap()
    });
    static SELECTOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"d.getElementById\('(x[0-9a-f]+)'\)").unwrap());
    const PATTERNS: [&str; 5] = [
        r#"<span id="{}" data-[0-9a-f]{4}="([^"]+)""#,
        r#"<input type="hidden" id="{}" value="([^"]+)""#,
        r#"<div id="{}" data-[0-9a-f]{4}="([^"]+)""#,
        r#"<template id="{}"><p>([^<]+)"#,
        r#"<span id="{}" data-[0-9a-f]{4}="([^"]+)""#,
    ];
    let js = web::object_after(player, &PARAMS).context("nhplayer's page has no parameters")?;
    let params: Value = serde_json::from_str(&crate::ohentai::js_to_json(js))
        .context("nhplayer's parameters aren't readable")?;
    ensure!(params.is_object(), "nhplayer's parameters aren't an object");
    let tokens = TOKENS
        .captures(script)
        .context("nhplayer's script has no challenge tokens")?;
    let selectors: Vec<&str> = SELECTOR
        .captures_iter(script)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();
    ensure!(
        selectors.len() >= PATTERNS.len(),
        "nhplayer's script names {} challenge parts, not {}",
        selectors.len(),
        PATTERNS.len()
    );
    let mut parts = Vec::new();
    for (i, (sel, pattern)) in selectors.iter().zip(PATTERNS).enumerate() {
        let re = Regex::new(&pattern.replacen("{}", &regex::escape(sel), 1))?;
        let part = re
            .captures(player)
            .with_context(|| format!("nhplayer's page lacks challenge part {i}"))?;
        parts.push(part[1].to_owned());
    }
    Ok(Challenge {
        params,
        parts,
        tokens: (tokens[1].to_owned(), tokens[2].to_owned()),
    })
}

/// The first `n` (in hex) whose SHA-256 of `s` + `n` starts with two zero
/// bytes.
fn proof_of_work(s: &str) -> Option<String> {
    let mut buf = String::with_capacity(s.len() + 8);
    for n in 0..MAX_WORK {
        buf.clear();
        buf.push_str(s);
        buf.push_str(&format!("{n:x}"));
        let h = ring::digest::digest(&ring::digest::SHA256, buf.as_bytes());
        if h.as_ref()[..2] == [0, 0] {
            return Some(format!("{n:x}"));
        }
    }
    None
}

/// The fingerprint the plugin sends: base64 of this JSON (as Python writes
/// it), `t` a time in milliseconds.
fn fingerprint(t: u64) -> String {
    use base64::Engine as _;
    let json = format!(
        r#"{{"t": {t}, "mm": [], "tm": [], "cl": [], "kp": [], "sc": [], "i": 0, "mc": 0, "tc": 0, "cc": 0, "kc": 0, "b": {{"sw": 1920, "sh": 1080, "aw": 1920, "ah": 1080, "cd": 24, "pd": 24, "tz": 0, "hc": 8, "dm": 8, "pl": "", "lang": "en", "langs": "en", "dpr": 1, "ww": 1920, "wh": 1080, "touch": false, "pdf": true, "fonts": 1}}}}"#
    );
    base64::engine::general_purpose::STANDARD.encode(json)
}

/// Milliseconds on a clock like Python's `time.monotonic()`, which the
/// plugin's fingerprint time is from: the system's uptime where it's known.
fn monotonic_ms() -> u64 {
    static START: LazyLock<(Instant, u64)> = LazyLock::new(|| {
        let uptime = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .map_or(3_600_000, |s| (s * 1000.0) as u64);
        (Instant::now(), uptime)
    });
    START.1 + START.0.elapsed().as_millis() as u64
}

/// `get-video-url-v2.php`'s query, in the plugin's order (its empty `df`
/// list isn't sent: Python's `urlencode` leaves empty lists out).
fn video_query(
    c: &Challenge,
    sc: &str,
    rid: &str,
    fingerprint: &str,
    work: &str,
) -> Vec<(&'static str, String)> {
    let p = |k: &str| {
        c.params
            .get(k)
            .map(web::python_str)
            .unwrap_or_else(|| "None".into())
    };
    vec![
        ("vid", p("vid")),
        ("c", p("ct")),
        ("p1", c.parts[0].clone()),
        ("p2", c.parts[1].clone()),
        ("p3", c.parts[2].clone()),
        ("p4", c.parts[3].clone()),
        ("t", c.parts[4].clone()),
        ("sc", sc.to_owned()),
        ("rid", rid.to_owned()),
        ("fp", fingerprint.to_owned()),
        ("pow", work.to_owned()),
        ("pid", p("pid")),
        ("st", p("st")),
    ]
}

#[cfg(test)]
pub(super) const PLAYER_PAGE: &str =
    r#"<html><body><ul><li data-id="v/abc123">Server 1</li></ul></body></html>"#;

#[cfg(test)]
pub(super) const REAL_PLAYER: &str = r#"<html><head><script>window._pV={vid: 4821, ct: 'c7f', pid: "p9", st: true};</script>
<script src="player-core.min.js?v=3"></script></head><body>
<span id="x1a" data-3f2e="part-one"></span>
<input type="hidden" id="x2b" value="part-two">
<div id="x3c" data-00af="part-three"></div>
<template id="x4d"><p>part-four</p></template>
<span id="x5e" data-beef="tee"></span>
</body></html>"#;

#[cfg(test)]
pub(super) const SCRIPT: &str = "(function(d){\n// the challenge token for this load\nvar a='tok-one';\nvar b='tok-two';\nvar p=[d.getElementById('x1a'),d.getElementById('x2b'),d.getElementById('x3c'),d.getElementById('x4d'),d.getElementById('x5e')];\n})(document);\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_search_and_series_pages() {
        let search = r#"<h1 class="display text-[24px]">3 results<!-- --> <span>for “overflow”</span></h1></div><div class="mt-4 flex flex-wrap gap-2"><a class="chip" href="/serie/overflow"> <!-- -->Overflow<!-- --> <span class="tnum text-txt-3">1</span></a></div><div class="grid-cards"><a class="card group block " title="Overflow Season Episode 1" href="/overflow-season-episode-1">"#;
        assert_eq!(
            cards(search),
            [Card {
                slug: "overflow".into(),
                titles: vec!["Overflow".into()]
            }]
        );
        let page = r#"<nav><a href="/series">Series</a></nav><h1 class="display mt-1.5 text-[30px] ">Sister Breeder</h1><p class="tnum">6<!-- --> episodes</p><div class="grid-wide"><a class="card group block " title="Sister Breeder Episode 2" href="/sister-breeder-episode-2"><img alt="x"></a><a class="card group block " title="Sister Breeder Episode 1" href="/sister-breeder-episode-1"></a></div>"#;
        let show = show_page(
            page,
            "https://hanime.red/serie/sister-breeder",
            "sister-breeder",
        );
        assert_eq!(show.titles, ["Sister Breeder"]);
        let eps: Vec<_> = show
            .episodes
            .iter()
            .map(|e| (e.episode, e.url.as_str()))
            .collect();
        assert_eq!(
            eps,
            [
                (1, "https://hanime.red/sister-breeder-episode-1"),
                (2, "https://hanime.red/sister-breeder-episode-2")
            ]
        );
    }

    #[test]
    fn reads_the_players_challenge() {
        assert_eq!(player_id(PLAYER_PAGE).as_deref(), Some("v/abc123"));
        assert_eq!(
            script_name(REAL_PLAYER).as_deref(),
            Some("player-core.min.js?v=3")
        );
        let c = challenge(REAL_PLAYER, SCRIPT).unwrap();
        assert_eq!(
            c.parts,
            ["part-one", "part-two", "part-three", "part-four", "tee"]
        );
        assert_eq!(c.tokens, ("tok-one".to_owned(), "tok-two".to_owned()));
        let q = video_query(&c, "tok-one", "tok-two", "fp", "1f");
        let get = |k: &str| q.iter().find(|(n, _)| *n == k).unwrap().1.as_str();
        assert_eq!(get("vid"), "4821");
        assert_eq!(get("c"), "c7f");
        assert_eq!(get("st"), "True");
        assert_eq!(get("t"), "tee");
        assert!(!q.iter().any(|(n, _)| *n == "df"));
        assert!(challenge(REAL_PLAYER, "no tokens").is_err());
    }

    #[test]
    fn does_the_proof_of_work() {
        let n = proof_of_work("part-onepart-two").unwrap();
        let h = ring::digest::digest(
            &ring::digest::SHA256,
            format!("part-onepart-two{n}").as_bytes(),
        );
        assert_eq!(&h.as_ref()[..2], &[0, 0]);
        // It's the first one that does.
        let first = u32::from_str_radix(&n, 16).unwrap();
        for m in 0..first.min(2000) {
            let h = ring::digest::digest(
                &ring::digest::SHA256,
                format!("part-onepart-two{m:x}").as_bytes(),
            );
            assert_ne!(&h.as_ref()[..2], &[0, 0]);
        }
    }

    #[test]
    fn writes_the_fingerprint_as_python_does() {
        use base64::Engine as _;
        let fp = base64::engine::general_purpose::STANDARD
            .decode(fingerprint(12345))
            .unwrap();
        let json: Value = serde_json::from_slice(&fp).unwrap();
        assert_eq!(json["t"], 12345);
        assert_eq!(json["b"]["sw"], 1920);
        assert_eq!(json["b"]["touch"], false);
        assert!(
            String::from_utf8(fp)
                .unwrap()
                .starts_with(r#"{"t": 12345, "mm": [], "#)
        );
        assert!(monotonic_ms() > 0);
    }
}
