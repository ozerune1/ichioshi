//! hentaihaven.com (WordPress, a "manga" theme: series are posts and
//! episodes their chapters).
//!
//! Its search (`/?s=…&post_type=wp-manga`) lists series cards; a series'
//! page (`/video/<slug>/`) has its title, its other names under it, and
//! its episodes (`/video/<slug>/episode-<n>`).
//!
//! An episode's page embeds the site's player (`…/player-logic/
//! player.php?…`), whose `x-secure-token` meta holds, base64 and ROT13
//! three times over, the player API's address and two values it's asked
//! with (a multipart form to `<address>/api.php`). Its answer has an HLS
//! playlist (H.264, sometimes at most 720p) and, for some episodes, a VP9
//! one beside it.

use std::sync::LazyLock;

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use reqwest::header::{ACCEPT, REFERER};
use serde::Deserialize;
use url::Url;

use super::web::{Session, absolute, base, meta, text};
use super::{Card, Media, Resolved, Show, Stream};

const SITE: &str = "https://hentaihaven.com";

pub(super) async fn search(s: &Session, query: &str) -> Result<Option<Vec<Card>>> {
    let url = Url::parse_with_params(
        &format!("{}/", base(SITE)),
        [("s", query), ("post_type", "wp-manga")],
    )?;
    Ok(s.page(url.as_str()).await?.map(|html| cards(&html)))
}

/// The series cards on a search page.
fn cards(html: &str) -> Vec<Card> {
    static CARD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<div class="item">\s*<a href="([^"]+)">(.*?)</a>"#).unwrap()
    });
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<div class="title">(.*?)</div>"#).unwrap());
    static ALT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\balt="([^"]*)""#).unwrap());
    let mut out: Vec<Card> = Vec::new();
    for c in CARD.captures_iter(html) {
        let Some(slug) = slug_of(&c[1]) else { continue };
        let mut titles = Vec::new();
        if let Some(t) = TITLE.captures(&c[2]) {
            titles.push(text(&t[1]));
        }
        if let Some(a) = ALT.captures(&c[2]) {
            let alt = text(&a[1]);
            titles.push(alt.strip_suffix(" cover").unwrap_or(&alt).to_owned());
        }
        titles.retain(|t| !t.is_empty());
        titles.dedup();
        if !out.iter().any(|o| o.slug == slug) {
            out.push(Card { slug, titles });
        }
    }
    out
}

/// `overflow` from `https://hentaihaven.com/video/overflow/` (a series',
/// not an episode's).
fn slug_of(href: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/video/([^/?#]+)/?(?:[?#]|$)").unwrap());
    Some(RE.captures(href)?[1].to_owned())
}

pub(super) async fn show(s: &Session, slug: &str) -> Result<Option<Show>> {
    let url = format!("{}/video/{slug}/", base(SITE));
    Ok(s.page(&url).await?.map(|html| show_page(&html, &url, slug)))
}

/// A series' page: its title, its other names ("Overflow - おーばーふろぉ"),
/// and its episodes.
fn show_page(html: &str, url: &str, slug: &str) -> Show {
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<h1 class="htitle">(.*?)</h1>"#).unwrap());
    static OTHER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<h1 class="htitle">.*?</h1>\s*<h3 class="h4">(.*?)</h3>"#).unwrap()
    });
    static EPISODE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<li class="hentai__chapter"[^>]*>\s*<a href="([^"]+)"(.*?)</a>"#).unwrap()
    });
    static LABEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<span class="title">(.*?)</span>"#).unwrap());
    let mut titles = Vec::new();
    if let Some(c) = TITLE.captures(html) {
        titles.push(text(&c[1]));
    }
    if let Some(c) = OTHER.captures(html) {
        let other = text(&c[1]);
        titles.extend(other.split(" - ").map(|t| t.trim().to_owned()));
        titles.push(other);
    }
    titles.retain(|t| !t.is_empty());
    titles.dedup();
    let show = titles
        .first()
        .cloned()
        .unwrap_or_else(|| slug.replace('-', " "));
    let links = EPISODE
        .captures_iter(html)
        .map(|c| {
            let label = LABEL
                .captures(&c[2])
                .map(|l| text(&l[1]))
                .unwrap_or_default();
            (c[1].to_owned(), label)
        })
        .collect();
    Show {
        titles,
        episodes: super::numbered(url, &show, links),
    }
}

/// What the player's token holds.
#[derive(Debug, Deserialize, PartialEq)]
struct Token {
    en: String,
    iv: String,
    uri: String,
}

#[derive(Debug, Deserialize)]
struct Answer {
    status: bool,
    #[serde(default)]
    data: Option<Data>,
}

#[derive(Debug, Deserialize)]
struct Data {
    #[serde(default)]
    sources: Vec<Source>,
    #[serde(default, rename = "isOctopus")]
    is_octopus: bool,
}

#[derive(Debug, Deserialize)]
struct Source {
    src: String,
}

pub(super) async fn resolve(s: &Session, url: &str) -> Result<Resolved> {
    let page = s.need(url).await?;
    let title = episode_title(&page);
    let embed = embed_url(&page).context("no player on hentaihaven's episode page")?;
    let embed = absolute(url, &embed)?;
    let player = s.need(embed.as_str()).await?;
    let secret = meta(&player, "x-secure-token").context("hentaihaven's player has no token")?;
    let token = decipher(&secret)?;
    // Its address may leave out the scheme (`//host/…`): the page's, as
    // yt-dlp does.
    let api = api_url(absolute(embed.as_str(), &token.uri)?.as_str())?;
    let form = reqwest::multipart::Form::new()
        .text("action", "zarat_get_data_player_ajax")
        .text("a", token.en)
        .text("b", token.iv);
    let body = s
        .text(
            s.client
                .post(api.as_str())
                .header(ACCEPT, "application/json")
                .header(REFERER, embed.as_str())
                .multipart(form),
        )
        .await?
        .context("hentaihaven's player API isn't there")?;
    let answer: Answer = serde_json::from_str(&body).with_context(|| {
        format!(
            "hentaihaven's player API answered something else: {}",
            body.chars().take(200).collect::<String>()
        )
    })?;
    let Some(data) = answer.data.filter(|_| answer.status) else {
        bail!("hentaihaven's player API has no video for this episode");
    };
    let source = data
        .sources
        .first()
        .context("hentaihaven's player API listed no video")?;
    let h264 = Url::parse(source.src.trim())
        .with_context(|| format!("not a playlist address: {}", source.src))?;
    let mut streams = vec![Stream {
        media: Media::Hls(h264.clone()),
        referer: None,
        resolution: None,
        label: "H.264".into(),
    }];
    if data.is_octopus {
        streams.push(Stream {
            media: Media::Hls(h264.join("./playlist_vp9.m3u8")?),
            referer: None,
            resolution: None,
            label: "VP9".into(),
        });
    }
    Ok(Resolved { title, streams })
}

fn episode_title(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"chapter-heading" class="h3">([^<]+)"#).unwrap());
    Some(text(&RE.captures(page)?[1])).filter(|t| !t.is_empty())
}

/// The player embedded in an episode's page.
fn embed_url(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"([^"]+/wp-content/plugins/player-logic/player\.php[^"]+)"#).unwrap()
    });
    Some(RE.captures(page)?[1].to_owned())
}

/// The player's token: three rounds of ROT13, then base64.
fn decipher(secret: &str) -> Result<Token> {
    let attempt = |s: &str| -> Result<Token> {
        let mut b = s.trim().as_bytes().to_vec();
        for _ in 0..3 {
            let rotated: Vec<u8> = b.iter().map(|&c| rot13(c)).collect();
            b = super::web::b64(&String::from_utf8_lossy(&rotated))
                .context("the player's token isn't base64")?;
        }
        serde_json::from_slice(&b).context("the player's token isn't what it was")
    };
    // The plugin strips any of "sha512-"'s letters from the start (Python's
    // lstrip); a token that only starts with the prefix comes first.
    let exact = secret.strip_prefix("sha512-").unwrap_or(secret);
    let stripped = secret.trim_start_matches(|c| "sha512-".contains(c));
    attempt(exact).or_else(|e| {
        if stripped == exact {
            Err(e)
        } else {
            attempt(stripped)
        }
    })
}

fn rot13(c: u8) -> u8 {
    match c {
        b'a'..=b'z' => (c - b'a' + 13) % 26 + b'a',
        b'A'..=b'Z' => (c - b'A' + 13) % 26 + b'A',
        _ => c,
    }
}

/// `<uri>/api.php` (`api.php` added to the path, as yarl's `joinpath`).
fn api_url(uri: &str) -> Result<Url> {
    let mut u = Url::parse(uri.trim()).with_context(|| format!("not an address: {uri}"))?;
    ensure!(
        matches!(u.scheme(), "https" | "http"),
        "not a web address: {uri}"
    );
    let path = format!("{}/api.php", u.path().trim_end_matches('/'));
    u.set_path(&path);
    u.set_query(None);
    u.set_fragment(None);
    Ok(u)
}

#[cfg(test)]
pub(super) fn encipher(json: &str) -> String {
    let mut s = json.as_bytes().to_vec();
    for _ in 0..3 {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &s);
        s = b64.bytes().map(rot13).collect();
    }
    format!("sha512-{}", String::from_utf8(s).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_search_cards() {
        let html = r#"<div class="item"> <a href="https://hentaihaven.com/video/overflow-hentai/"> <div class="cover"> <img src="x.jpg" alt="Overflow Hentai cover"> </div> <div class="title">Overflow Hentai</div> </a> </div>
        <div class="item"> <a href="https://hentaihaven.com/video/overflow/"> <div class="cover"> <img src="y.jpg" alt="Overflow cover" width="204"> <span class="hov"><svg></svg></span> </div> <div class="title">Overflow</div> </a> </div>"#;
        let cards = cards(html);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[1].slug, "overflow");
        assert_eq!(cards[1].titles, ["Overflow"]);
    }

    #[test]
    fn reads_series_pages() {
        let html = r#"<h1 class="htitle"> Overflow </h1> <h3 class="h4">Overflow - おーばーふろぉ</h3>
        <ul><li class="hentai__chapter" data-chapter="304"> <a href="https://hentaihaven.com/video/overflow/episode-2"> <img src="a.jpg"> <div class="chapter_info"> <span class="title">Episode 2</span> </div> </a> </li>
        <li class="hentai__chapter" data-chapter="303"> <a href="https://hentaihaven.com/video/overflow/episode-1"> <div class="chapter_info"> <span class="title">Episode 1</span> </div> </a> </li></ul>"#;
        let show = show_page(html, "https://hentaihaven.com/video/overflow/", "overflow");
        assert_eq!(
            show.titles,
            ["Overflow", "おーばーふろぉ", "Overflow - おーばーふろぉ"]
        );
        assert_eq!(show.episodes.len(), 2);
        assert_eq!(show.episodes[0].episode, 1);
        assert_eq!(
            show.episodes[0].url,
            "https://hentaihaven.com/video/overflow/episode-1"
        );
    }

    #[test]
    fn deciphers_the_players_token() {
        let json = r#"{"en":"abc","iv":"def","uri":"https://player.example/x/"}"#;
        let token = decipher(&encipher(json)).unwrap();
        assert_eq!(
            token,
            Token {
                en: "abc".into(),
                iv: "def".into(),
                uri: "https://player.example/x/".into()
            }
        );
        assert!(decipher("sha512-nonsense").is_err());
        assert_eq!(
            api_url("https://player.example/x/").unwrap().as_str(),
            "https://player.example/x/api.php"
        );
        assert_eq!(
            api_url("https://player.example/x").unwrap().as_str(),
            "https://player.example/x/api.php"
        );
    }

    #[test]
    fn reads_the_episode_page() {
        let page = r#"<h1 id="chapter-heading" class="h3">Overflow Episode 1</h1>
        <iframe src="https://hentaihaven.com/wp-content/plugins/player-logic/player.php?data=xyz&amp;n=1"></iframe>"#;
        assert_eq!(episode_title(page).as_deref(), Some("Overflow Episode 1"));
        assert_eq!(
            embed_url(page).as_deref(),
            Some(
                "https://hentaihaven.com/wp-content/plugins/player-logic/player.php?data=xyz&amp;n=1"
            )
        );
    }
}
