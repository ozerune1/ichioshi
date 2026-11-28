//! oppai.stream.
//!
//! An episode's page (`/watch?e=…`) builds its stream's address in its
//! script: `'<base>'+startsource+'/<manifest>'`, where `startsource` is a
//! resolution (`720`, `1080` or `4k`). Each is a DASH presentation, fetched
//! with the site as the Referer.
//!
//! A show is found with the site's search (`/api/search.php`, a form, as
//! its search page's script sends it), which answers with episode cards:
//! each a link to its page (`/watch/<Show>-<n>`), its number and its show's
//! title.

use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::header::{ACCEPT, REFERER as REFERER_HEADER};
use url::Url;

use super::web::{self, Challenge, Session, base, matches, normalize, queries, text, wanted};
use super::{Episode, Media, Resolved, Stream};

const SITE: &str = "https://oppai.stream";
const REFERER: &str = "https://oppai.stream/";
/// Episodes asked for a search page at a time.
const PER_PAGE: usize = 40;
/// Search pages read at most per query.
const MAX_PAGES: usize = 3;

/// The show called any of `names`, its episodes (none if it isn't there).
pub(super) async fn episodes(s: &Session, names: &[String]) -> Result<Vec<Episode>> {
    let wanted = wanted(names);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let mut failure = None;
    let mut answered = false;
    for query in queries(names).into_iter().take(super::MAX_SEARCHES) {
        let mut found: Vec<Card> = Vec::new();
        for page in 1..=MAX_PAGES {
            let (cards, total) = match search(s, &query, page).await {
                Ok(r) => r,
                Err(e) if e.is::<Challenge>() => return Err(e),
                Err(e) => {
                    tracing::warn!("oppai.stream search for {query:?}: {e:#}");
                    failure = Some(e);
                    break;
                }
            };
            answered = true;
            let n = cards.len();
            found.extend(cards);
            if n < PER_PAGE || total.is_some_and(|t| found.len() >= t) {
                break;
            }
        }
        let mut eps: Vec<Episode> = Vec::new();
        for c in found {
            if matches(&wanted, &normalize(&c.show)) && !eps.iter().any(|e| e.episode == c.episode)
            {
                eps.push(Episode {
                    episode: c.episode,
                    title: super::episode_title(&c.show, c.episode),
                    url: c.url,
                });
            }
        }
        if !eps.is_empty() {
            return Ok(eps);
        }
    }
    match failure {
        Some(e) if !answered => Err(e.context("searching oppai.stream")),
        _ => Ok(Vec::new()),
    }
}

/// An episode card the search answers with.
#[derive(Debug, Clone, PartialEq)]
struct Card {
    /// Its page, e.g. `https://oppai.stream/watch/Overflow-2`.
    url: String,
    episode: u32,
    /// Its show's title, e.g. "Overflow".
    show: String,
}

/// Page `page` of the search for `query`: its cards, and how many there are
/// in all (if it says).
async fn search(s: &Session, query: &str, page: usize) -> Result<(Vec<Card>, Option<usize>)> {
    let site = base(SITE);
    let per_page = PER_PAGE.to_string();
    let page = page.to_string();
    let form = [
        ("text", query),
        ("tags", ""),
        ("blacklist", ""),
        ("studios", ""),
        ("order", "recent"),
        ("limit", per_page.as_str()),
        ("page", page.as_str()),
        ("classes", "smaller-episode-gap"),
        ("character_filters", "{}"),
    ];
    let html = s
        .text(
            s.client
                .post(format!("{site}/api/search.php"))
                .header(ACCEPT, "*/*")
                .header(REFERER_HEADER, format!("{site}/search"))
                .header("X-Requested-With", "XMLHttpRequest")
                .form(&form),
        )
        .await?
        .context("oppai.stream's search isn't there")?;
    Ok((cards(&html, &site), total(&html)))
}

/// The episode cards in a search's answer.
fn cards(html: &str, site: &str) -> Vec<Card> {
    static LINK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<a href="([^"]*/watch/[^"]+)"[^>]*\bclass="episode\b[^"]*"[^>]*>"#).unwrap()
    });
    static EPID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\bepid="(\d+)""#).unwrap());
    // The class itself, not the `episode-title-wrap` around it.
    static TITLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)class="(?:[^"]*\s)?episode-title(?:\s[^"]*)?"[^>]*>(.*?)<"#).unwrap()
    });
    let starts: Vec<_> = LINK.captures_iter(html).collect();
    let mut out: Vec<Card> = Vec::new();
    for (i, c) in starts.iter().enumerate() {
        let tag = c.get(0).unwrap();
        let end = starts
            .get(i + 1)
            .map_or(html.len(), |n| n.get(0).unwrap().start());
        let body = &html[tag.end()..end];
        let href = web::unescape(&c[1]);
        // Its page, without where it was found from (`…/from/search`).
        let page = href.split("/from/").next().unwrap_or(&href);
        let Ok(url) = web::absolute(&format!("{site}/"), page) else {
            continue;
        };
        let episode = EPID
            .captures(tag.as_str())
            .and_then(|e| e[1].parse().ok())
            .or_else(|| web::episode_number(url.path()));
        let show = TITLE
            .captures(body)
            .map(|t| text(&t[1]))
            .unwrap_or_default();
        let (Some(episode), false) = (episode, show.is_empty()) else {
            continue;
        };
        if !out.iter().any(|o| o.url == url.as_str()) {
            out.push(Card {
                url: url.to_string(),
                episode,
                show,
            });
        }
    }
    out
}

/// How many episodes the search found in all (`results-num`).
fn total(html: &str) -> Option<usize> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"results-num'?"?[^>]*>\s*(\d+)"#).unwrap());
    RE.captures(html)?[1].parse().ok()
}
/// The resolutions the player picks from, as its addresses name them.
const RESOLUTIONS: [(u32, &str); 3] = [(720, "720"), (1080, "1080"), (2160, "4k")];

pub(super) async fn resolve(s: &Session, url: &str) -> Result<Resolved> {
    // Its pages have been `/watch?e=<id>` (as the plugin reads them) and
    // `/watch/<id>`: the other if this one isn't there or has no stream.
    let first = s.page(url).await?;
    let page = match first {
        Some(p) if stream_base(&p).is_some() => p,
        first => {
            let other = match other_address(url) {
                Some(o) => s.page(&o).await.ok().flatten(),
                None => None,
            };
            match (other, first) {
                (Some(p), _) if stream_base(&p).is_some() => p,
                (_, Some(p)) | (Some(p), None) => p,
                (None, None) => bail!("oppai.stream has no page at {url}"),
            }
        }
    };
    let (stream_base, manifest) = stream_base(&page).with_context(|| {
        format!(
            "no stream on oppai.stream's episode page (it has {})",
            clues(&page)
        )
    })?;
    let mut mpds = Vec::new();
    for (res, name) in RESOLUTIONS {
        mpds.push((res, join_path(&stream_base, name, &manifest)?));
    }
    Ok(Resolved {
        title: episode_title(&page),
        streams: vec![Stream {
            media: Media::Dash(mpds),
            referer: Some(REFERER.to_owned()),
            resolution: None,
            label: "DASH".into(),
        }],
    })
}

/// The stream's base address and its manifest's name.
fn stream_base(page: &str) -> Option<(String, String)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"'(https?://[^']+)'\s*\+\s*startsource\s*\+\s*'/([^']+)'").unwrap()
    });
    let c = RE.captures(page)?;
    Some((c[1].to_owned(), c[2].to_owned()))
}

/// What of the stream's makings a page has, for the error when its
/// address can't be read (its text isn't shown).
fn clues(page: &str) -> String {
    let has = |t: &str| page.contains(t);
    let mut found: Vec<&str> = [
        ("startsource", "startsource"),
        (".mpd", ".mpd"),
        (".m3u8", ".m3u8"),
        ("myspacecat", "its stream server's name"),
    ]
    .into_iter()
    .filter(|(t, _)| has(t))
    .map(|(_, say)| say)
    .collect();
    for (joined, say) in [
        ("'+startsource+'", "'+startsource+'"),
        ("\"+startsource+\"", "\"+startsource+\""),
        ("${startsource}", "${startsource}"),
    ] {
        if has(joined) {
            found.push(say);
        }
    }
    if found.is_empty() {
        "none of startsource, .mpd, .m3u8 or its stream server's name".into()
    } else {
        found.join(", ")
    }
}

/// An episode's page's other address: `/watch?e=<id>` for `/watch/<id>`
/// and the other way round.
fn other_address(url: &str) -> Option<String> {
    let mut u = Url::parse(url).ok()?;
    if let Some(id) = u.path().strip_prefix("/watch/").map(str::to_owned) {
        u.set_path("/watch");
        u.set_query(Some(&format!("e={id}")));
        return Some(u.to_string());
    }
    let id = u.query_pairs().find(|(k, _)| k == "e")?.1.into_owned();
    u.set_query(None);
    u.set_path(&format!("/watch/{id}"));
    Some(u.to_string())
}

/// `base` with `res` and `manifest` added to its path (yarl's `joinpath`).
fn join_path(base: &str, res: &str, manifest: &str) -> Result<Url> {
    let mut u = Url::parse(base).with_context(|| format!("not an address: {base}"))?;
    let path = format!(
        "{}/{res}/{}",
        u.path().trim_end_matches('/'),
        manifest.trim_start_matches('/')
    );
    u.set_path(&path);
    Ok(u)
}

fn episode_title(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<h1.*line-2">(.*)</h1>"#).unwrap());
    Some(text(&RE.captures(page)?[1])).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_episode_page() {
        let page = r#"<h1 class="title line-2">Overflow 1</h1>
        <script>var startsource = '1080'; player.src('https://s2.myspacecat.pictures/v/over-1/'+startsource+'/manifest.mpd');</script>"#;
        let (b, m) = stream_base(page).unwrap();
        assert_eq!(b, "https://s2.myspacecat.pictures/v/over-1/");
        assert_eq!(m, "manifest.mpd");
        assert_eq!(
            join_path(&b, "4k", &m).unwrap().as_str(),
            "https://s2.myspacecat.pictures/v/over-1/4k/manifest.mpd"
        );
        assert_eq!(
            join_path("https://h/x", "720", "/m.mpd").unwrap().as_str(),
            "https://h/x/720/m.mpd"
        );
        assert_eq!(episode_title(page).as_deref(), Some("Overflow 1"));
        assert!(stream_base("<p>nothing</p>").is_none());
        assert_eq!(
            other_address("https://oppai.stream/watch/Overflow-2").as_deref(),
            Some("https://oppai.stream/watch?e=Overflow-2")
        );
        assert_eq!(
            other_address("https://oppai.stream/watch?e=Overflow-2").as_deref(),
            Some("https://oppai.stream/watch/Overflow-2")
        );
    }

    pub(crate) const RESULTS: &str = r#"<div class='results-num' style='display: none;'>3</div>
    <a href="https://oppai.stream/watch/Overflow-1/from/search" class="episode " epid="1" data-layout="1" draggable="false"><div class="cover-wrap"><img class="cover" alt="Overflow Episode 1 thumbnail"></div><div class="info"><div class="episode-title-wrap">\n    <p class="s4 white bold episode-title">Overflow</p></div><p class="s6 white">52K</p></div></a>
    <a href="https://oppai.stream/watch/Overflow-2/from/search" class="episode " epid="2" draggable="false"><p class="s4 white bold episode-title">Overflow</p></a>
    <a href="https://oppai.stream/watch/Overflow-Season-2-1/from/search" class="episode " epid="1"><p class="s4 white bold episode-title">Overflow Season 2</p></a>"#;

    #[test]
    fn reads_search_results() {
        let c = cards(RESULTS, "https://oppai.stream");
        assert_eq!(c.len(), 3);
        assert_eq!(
            c[1],
            Card {
                url: "https://oppai.stream/watch/Overflow-2".into(),
                episode: 2,
                show: "Overflow".into()
            }
        );
        assert_eq!(c[2].show, "Overflow Season 2");
        assert_eq!(total(RESULTS), Some(3));
    }
}
