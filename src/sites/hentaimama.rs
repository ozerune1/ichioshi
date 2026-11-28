//! hentaimama.io (WordPress).
//!
//! Its search (`/?s=…`) lists series cards, each with a title and an
//! alternative name; a series' page (`/tvshows/<slug>/`) lists its
//! episodes (`/episodes/<slug>-episode-<n>/`).
//!
//! An episode's page sets up `var data = {…}` for the site's player call
//! (`/wp-admin/admin-ajax.php`, as a form); the answer is a list whose
//! first item embeds the player's page, which sets up a JW Player with the
//! video (an MP4: the site serves the same files for downloads).

use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow, bail, ensure};
use regex::Regex;
use reqwest::header::{ACCEPT, REFERER};
use serde_json::Value;

use super::web::{self, Session, absolute, attr, base, text};
use super::{Card, Media, Resolved, Show, Stream};

const SITE: &str = "https://hentaimama.io";

pub(super) async fn search(s: &Session, query: &str) -> Result<Option<Vec<Card>>> {
    let url = url::Url::parse_with_params(&format!("{}/", base(SITE)), [("s", query)])?;
    Ok(s.page(url.as_str()).await?.map(|html| cards(&html)))
}

/// The series cards on a search page.
fn cards(html: &str) -> Vec<Card> {
    static TITLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<h3 class="sc-title">\s*<a\b[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap()
    });
    static ALT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<div class="sc-alt">(.*?)</div>"#).unwrap());
    let mut out: Vec<Card> = Vec::new();
    for card in html.split(r#"<article class="series-card""#).skip(1) {
        let Some(c) = TITLE.captures(card) else {
            continue;
        };
        let Some(slug) = slug_of(&c[1]) else { continue };
        let mut titles = vec![text(&c[2])];
        if let Some(alt) = ALT.captures(card) {
            titles.push(text(&alt[1]));
        }
        titles.retain(|t| !t.is_empty());
        if !out.iter().any(|o| o.slug == slug) {
            out.push(Card { slug, titles });
        }
    }
    out
}

/// `overflow` from `https://hentaimama.io/tvshows/overflow/`.
fn slug_of(href: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/tvshows/([^/?#]+)/?(?:[?#]|$)").unwrap());
    Some(RE.captures(href)?[1].to_owned())
}

pub(super) async fn show(s: &Session, slug: &str) -> Result<Option<Show>> {
    let url = format!("{}/tvshows/{slug}/", base(SITE));
    Ok(s.page(&url).await?.map(|html| show_page(&html, &url, slug)))
}

/// A series' page: its title and alternative name, and its episodes.
fn show_page(html: &str, url: &str, slug: &str) -> Show {
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<h1 class="dsc-title">(.*?)</h1>"#).unwrap());
    static ALT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<div class="dsc-alt">(.*?)</div>"#).unwrap());
    static EPISODE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<a class="dt-se-item" href="([^"]+)"(.*?)</a>"#).unwrap()
    });
    static LABEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)class="dt-se-title">(.*?)<"#).unwrap());
    let mut titles: Vec<String> = [&TITLE, &ALT]
        .iter()
        .filter_map(|re| re.captures(html).map(|c| text(&c[1])))
        .filter(|t| !t.is_empty())
        .collect();
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

pub(super) async fn resolve(s: &Session, url: &str) -> Result<Resolved> {
    let page = s.need(url).await?;
    let title = episode_title(&page);
    let post = post_id(&page).context("hentaimama's episode page")?;
    let mut streams = Vec::new();
    let mut failure = None;
    // Each of its player's mirrors, in the page's order.
    for n in options(&page) {
        match option_streams(s, url, &post, n).await {
            Ok(found) => streams.extend(found),
            Err(e) if e.is::<web::Challenge>() => return Err(e),
            Err(e) => {
                tracing::debug!("hentaimama mirror {n}: {e:#}");
                failure = Some(e);
            }
        }
    }
    if streams.is_empty() {
        return Err(failure.unwrap_or_else(|| anyhow!("hentaimama's player has no video")));
    }
    Ok(Resolved { title, streams })
}

/// The episode's post ID, which its player's mirrors are asked by: the
/// page's `{action: 'get_player_contents', a: '<id>', i: n}` (or, as the
/// plugin read it, `var data = {…}`'s `a`).
fn post_id(page: &str) -> Result<String> {
    static INLINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"get_player_contents['"]?\s*,\s*a\s*:\s*['"]?(\d+)"#).unwrap()
    });
    if let Some(c) = INLINE.captures(page) {
        return Ok(c[1].to_owned());
    }
    let data = ajax_data(page)?;
    data.into_iter()
        .find(|(k, _)| k == "a")
        .map(|(_, v)| v)
        .context("the player's data has no post")
}

/// The player's mirrors (`#option-<n>`), in the page's order; the first if
/// it lists none.
fn options(page: &str) -> Vec<u32> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r##"href=['"]#option-(\d+)['"]"##).unwrap());
    let mut out: Vec<u32> = Vec::new();
    for c in RE.captures_iter(page) {
        if let Ok(n) = c[1].parse::<u32>()
            && n > 0
            && !out.contains(&n)
        {
            out.push(n);
        }
    }
    if out.is_empty() {
        out.push(1);
    }
    out
}

/// Mirror `n`'s video: the player page its answer embeds, and the JW
/// Player there.
async fn option_streams(s: &Session, page_url: &str, post: &str, n: u32) -> Result<Vec<Stream>> {
    let i = n.to_string();
    let answer: Value = serde_json::from_str(
        &s.text(
            s.client
                .post(format!("{}/wp-admin/admin-ajax.php", base(SITE)))
                .header(ACCEPT, "application/json, text/javascript, */*")
                .header(REFERER, page_url)
                .header("X-Requested-With", "XMLHttpRequest")
                .form(&[("action", "get_player_contents"), ("a", post), ("i", &i)]),
        )
        .await?
        .context("hentaimama's player isn't there")?,
    )
    .context("hentaimama's player answered something else")?;
    // A list with mirror n's at n - 1 (the plugin's answer had it first).
    let list = answer.as_array().cloned().unwrap_or_default();
    let html = list
        .get(n as usize - 1)
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .or_else(|| {
            list.iter()
                .filter_map(Value::as_str)
                .find(|h| !h.is_empty())
        })
        .context("the mirror sent no player")?;
    static SRC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"src=["']([^"']+)["']"#).unwrap());
    let embed = SRC
        .captures(html)
        .map(|c| c[1].to_owned())
        .context("the mirror named no player page")?;
    let player_url = absolute(page_url, &embed)?;
    let player = s.need(player_url.as_str()).await?;
    jw_streams(&player, player_url.as_str())
}

/// The episode's title (the page's hidden `title` field).
fn episode_title(page: &str) -> Option<String> {
    static INPUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<input\b[^>]*>").unwrap());
    INPUT.find_iter(page).find_map(|m| {
        (attr(m.as_str(), "name").as_deref() == Some("title"))
            .then(|| attr(m.as_str(), "value"))
            .flatten()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    })
}

/// The form the page's player sends: the first `var data = {…}`'s keys
/// and values.
fn ajax_data(page: &str) -> Result<Vec<(String, String)>> {
    static DATA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"var data\s*=\s*").unwrap());
    let starts: Vec<usize> = DATA.find_iter(page).map(|m| m.end()).collect();
    ensure!(!starts.is_empty(), "no `var data` for the player");
    let mut failure = None;
    let mut set_to = Vec::new();
    for at in starts {
        let rest = page[at..].trim_start();
        if !rest.starts_with('{') {
            // What it's set to instead, roughly (for the error).
            set_to.push(match rest.chars().next() {
                Some('[') => "a list",
                Some('"' | '\'' | '`') => "a string",
                Some(c) if c.is_ascii_digit() => "a number",
                Some(c) if c.is_alphabetic() || c == '$' || c == '_' => "an expression",
                _ => "something else",
            });
            continue;
        }
        let Some(js) = web::balanced(rest) else {
            failure = Some(anyhow!("the player's data doesn't end"));
            continue;
        };
        match web::ordered_fields(&crate::ohentai::js_to_json(js)) {
            Ok(fields) => {
                return Ok(fields
                    .iter()
                    .map(|(k, v)| (k.clone(), web::python_str(v)))
                    .collect());
            }
            Err(e) => failure = Some(e.context("the player's data isn't readable")),
        }
    }
    Err(failure.unwrap_or_else(|| {
        anyhow!(
            "no `var data = {{…}}` for the player (it's set to {})",
            set_to.join(", ")
        )
    }))
}

/// The video files a page's JW Player is set up with (MP4s first, then the
/// highest resolution).
pub(super) fn jw_streams(page: &str, page_url: &str) -> Result<Vec<Stream>> {
    static SETUP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"jwplayer\s*\(\s*(?:"[^"]*"|'[^']*'|[\w$.]*)\s*\)[\s\S]{0,200}?\.setup\s*\("#)
            .unwrap()
    });
    let mut sources = Vec::new();
    for m in SETUP.find_iter(page) {
        let Some(js) = web::balanced(page[m.end()..].trim_start()) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&crate::ohentai::js_to_json(js)) else {
            continue;
        };
        sources.extend(crate::ohentai::media(&json).0);
    }
    if sources.is_empty() {
        bail!("the player page has no video");
    }
    sources.sort_by_key(|s| std::cmp::Reverse((s.mp4, s.resolution)));
    let mut out = Vec::new();
    for src in sources {
        let Ok(url) = absolute(page_url, &src.file) else {
            continue;
        };
        let hls = url.path().ends_with(".m3u8");
        out.push(Stream {
            label: src
                .resolution
                .map_or_else(|| "video".to_owned(), |r| format!("{r}p")),
            media: if hls {
                Media::Hls(url)
            } else {
                Media::File(url)
            },
            referer: None,
            resolution: src.resolution,
        });
    }
    Ok(out)
}

#[cfg(test)]
pub(super) const SEARCH: &str = r#"<div id="dt-search-grid" class="dt-series-cards"><article class="series-card"><div class="sc-left"><a class="sc-poster" href="https://hentaimama.io/tvshows/overflow/"><img src="x.webp" alt="Overflow"></a></div><div class="sc-body"><h3 class="sc-title"><a href="https://hentaimama.io/tvshows/overflow/">Overflow</a></h3><div class="sc-alt">Overflow</div><div class="sc-meta"><span class="sc-tag">8 Episodes</span></div></div></article><article class="series-card"><div class="sc-body"><h3 class="sc-title"><a href="https://hentaimama.io/tvshows/kuroinu-kedakaki/">Kuroinu &amp; co</a></h3><div class="sc-alt">Black Dogs</div></div></article></div>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_search_cards() {
        let cards = cards(SEARCH);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].slug, "overflow");
        assert_eq!(cards[1].slug, "kuroinu-kedakaki");
        assert_eq!(cards[1].titles, ["Kuroinu & co", "Black Dogs"]);
    }

    #[test]
    fn reads_series_pages() {
        let html = r#"<h1 class="dsc-title">Overflow</h1><div class="dsc-alt">オーバーフロー</div>
        <a class="dt-se-item" href="https://hentaimama.io/episodes/overflow-episode-2/"><b class="dt-se-title">Episode 2</b></a>
        <a class="dt-se-item" href="https://hentaimama.io/episodes/overflow-episode-1/"><span class="dt-se-thumb"></span><b class="dt-se-title">Episode 1</b></a>
        <a href="https://hentaimama.io/episodes/other-episode-9/">not one</a>"#;
        let show = show_page(html, "https://hentaimama.io/tvshows/overflow/", "overflow");
        assert_eq!(show.titles, ["Overflow", "オーバーフロー"]);
        let eps: Vec<_> = show
            .episodes
            .iter()
            .map(|e| (e.episode, e.url.as_str()))
            .collect();
        assert_eq!(
            eps,
            [
                (1, "https://hentaimama.io/episodes/overflow-episode-1/"),
                (2, "https://hentaimama.io/episodes/overflow-episode-2/"),
            ]
        );
        assert_eq!(show.episodes[0].title, "Overflow - Episode 1");
    }

    #[test]
    fn reads_the_players_mirrors() {
        let page = r##"<ul><li><a href="#option-5"><b class="icon-play_arrow"></b> mi-1</a></li>
        <li><a href="#option-3">mi-2</a></li><li><a href="#option-1">mi-3</a></li></ul>
        <script>function dtLoadOption(n) {$.post("https://hentaimama.io/wp-admin/admin-ajax.php", {action: 'get_player_contents',a: '7360',i: n}, function(response) {});}</script>"##;
        assert_eq!(post_id(page).unwrap(), "7360");
        assert_eq!(options(page), [5, 3, 1]);
        assert_eq!(options("<p>none</p>"), [1]);
        let old = "<script>var data = { action: 'get_player_contents', a: '42' };</script>";
        assert_eq!(post_id(old).unwrap(), "42");
        assert!(post_id("<p>nothing</p>").is_err());
    }

    #[test]
    fn reads_the_episode_page() {
        let page = r#"<form><input type="hidden" name="title" value="Overflow Episode 1"></form>
        <script>var data = { action: 'get_player_contents', a: "7353", b: 2, c: true };</script>"#;
        assert_eq!(episode_title(page).as_deref(), Some("Overflow Episode 1"));
        assert_eq!(
            ajax_data(page).unwrap(),
            [
                ("action".to_owned(), "get_player_contents".to_owned()),
                ("a".to_owned(), "7353".to_owned()),
                ("b".to_owned(), "2".to_owned()),
                ("c".to_owned(), "True".to_owned()),
            ]
        );
    }

    #[test]
    fn reads_jw_players() {
        let page = r#"<script>var p = jwplayer("player").setup({
            sources: [{file: "/v/ep1-720.mp4", label: "720p", type: "video/mp4"},
                      {file: 'https://cdn.example/ep1-1080.mp4', label: '1080p'}],
            image: "x.jpg",
        });</script>"#;
        let streams = jw_streams(page, "https://player.example/embed/1").unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(
            streams[0].media,
            Media::File("https://cdn.example/ep1-1080.mp4".parse().unwrap())
        );
        assert_eq!(streams[0].resolution, Some(1080));
        assert_eq!(
            streams[1].media,
            Media::File("https://player.example/v/ep1-720.mp4".parse().unwrap())
        );
        assert!(jw_streams("<p>nothing</p>", "https://x/").is_err());
    }
}
