//! oppai.stream.
//!
//! An episode's page (`/watch/<Show>-<n>`) sets its player up with
//! variables: the storage server, the episode's folder on it, its layout
//! and resolutions. Its player (`watch.js`) then plays, as layout 2 (newer
//! episodes), a DASH presentation per resolution,
//! `<server>/<folder>/<4K|1080p|720p>/dash<nn>/manifest.mpd` (HLS beside it,
//! `master.m3u8`, below 4K); as layout 1, `<server>/<folder>/ep.webm` (or
//! `.mp4`), its DASH being WebM, which isn't joined here. All are fetched
//! with the site as the Referer. (The hanime-plugin's way, a
//! `'+startsource+'` address in the page, is gone.)
//!
//! A show is found with the site's search (`/api/search.php`, a form, as
//! its search page's script sends it), which answers with episode cards:
//! each a link to its page (`/watch/<Show>-<n>`), its number and its show's
//! title.

use std::sync::LazyLock;

use anyhow::{Context, Result};
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
/// What an episode's page sets its player up with.
#[derive(Debug, Clone, PartialEq)]
struct Player {
    /// e.g. `https://s2.myspacecat.pictures`.
    storage: String,
    /// e.g. `collaboration-works/harem-tou-e-youkoso` or `Overflow-1`.
    folder: String,
    /// 2 for newer episodes.
    layout: u32,
    /// Best first: "4k", "1080", "720".
    resolutions: Vec<String>,
    /// Each resolution's file format ("4k-2": 4K as WebM).
    formats: Vec<(String, String)>,
    episode: u32,
}

/// A variable the page's script sets (`var name = <JSON>;`).
fn variable(page: &str, name: &str) -> Option<serde_json::Value> {
    let re = Regex::new(&format!(r"\bvar\s+{}\s*=\s*([^;\n]+)", regex::escape(name))).ok()?;
    serde_json::from_str(re.captures(page)?[1].trim()).ok()
}

fn player(page: &str) -> Option<Player> {
    let text = |n: &str| variable(page, n).and_then(|v| v.as_str().map(str::to_owned));
    let folder = text("folder").filter(|f| !f.is_empty())?;
    let storage = text("storageBase")
        .filter(|b| b.starts_with("http"))
        .unwrap_or_else(|| "https://s1.myspacecat.pictures".to_owned());
    let layout = variable(page, "layout")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;
    // "720-1,1080-1,4k-2": each resolution and its format (1 MP4, 2 WebM).
    let formats: Vec<(String, String)> = text("res")
        .unwrap_or_default()
        .split(',')
        .filter_map(|part| {
            let (res, f) = part.trim().split_once('-').unwrap_or((part.trim(), "1"));
            (!res.is_empty()).then(|| {
                let f = if f.trim() == "2" { "webm" } else { "mp4" };
                (res.to_owned(), f.to_owned())
            })
        })
        .collect();
    let mut resolutions: Vec<String> = variable(page, "resList")
        .and_then(|v| v.as_array().cloned())
        .map(|l| {
            l.iter()
                .filter_map(|r| {
                    r.as_str()
                        .map(str::to_owned)
                        .or_else(|| r.as_u64().map(|n| n.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    if resolutions.is_empty() {
        resolutions = formats.iter().map(|(r, _)| r.clone()).collect();
    }
    resolutions.sort_by_key(|r| std::cmp::Reverse(height(r)));
    let episode = text("ep")
        .and_then(|e| e.trim().parse().ok())
        .or_else(|| {
            variable(page, "ep")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
        })
        .unwrap_or(1);
    Some(Player {
        storage: storage.trim_end_matches('/').to_owned(),
        folder,
        layout,
        resolutions,
        formats,
        episode,
    })
}

/// "4k" is 2160; "1080" is 1080.
fn height(res: &str) -> u32 {
    if res.eq_ignore_ascii_case("4k") {
        2160
    } else {
        res.trim_end_matches(['p', 'P']).parse().unwrap_or(0)
    }
}

impl Player {
    /// `<server>/<folder>/<rest…>`, each part of the folder escaped.
    fn address(&self, rest: &[&str]) -> Result<Url> {
        let mut u = Url::parse(&self.storage)
            .with_context(|| format!("not an address: {}", self.storage))?;
        u.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("not a web address: {}", self.storage))?
            .pop_if_empty()
            .extend(self.folder.split('/'))
            .extend(rest);
        Ok(u)
    }

    /// What its player would play, best first.
    fn streams(&self) -> Result<Vec<Stream>> {
        let stream = |media, resolution, label: String| Stream {
            media,
            referer: Some(REFERER.to_owned()),
            resolution,
            label,
        };
        if self.layout == 2 {
            let nn = format!("dash{:02}", self.episode);
            let folder = |r: &str| {
                if r.eq_ignore_ascii_case("4k") {
                    "4K".to_owned()
                } else {
                    format!("{r}p")
                }
            };
            let mut mpds = Vec::new();
            let mut out = Vec::new();
            for r in &self.resolutions {
                mpds.push((height(r), self.address(&[&folder(r), &nn, "manifest.mpd"])?));
            }
            out.push(stream(Media::Dash(mpds), None, "DASH".into()));
            // HLS, as its player falls back to (none for 4K).
            for r in self.resolutions.iter().filter(|r| height(r) < 2160) {
                out.push(stream(
                    Media::Hls(self.address(&[&folder(r), &nn, "master.m3u8"])?),
                    Some(height(r)),
                    format!("HLS {r}"),
                ));
            }
            return Ok(out);
        }
        // Its file: MP4 if its 4K one is, else WebM (as its player says).
        let format = |r: &str| {
            self.formats
                .iter()
                .find(|(x, _)| x == r)
                .map(|(_, f)| f.as_str())
        };
        let ext = if format("4k") == Some("mp4") {
            "mp4"
        } else {
            "webm"
        };
        let best = self
            .resolutions
            .first()
            .map(|r| height(r))
            .filter(|h| *h > 0);
        Ok(vec![stream(
            Media::File(self.address(&[&format!("ep.{ext}")])?),
            best,
            format!("file ({ext})"),
        )])
    }
}

pub(super) async fn resolve(s: &Session, url: &str) -> Result<Resolved> {
    let page = s.need(url).await?;
    let p = player(&page).context("oppai.stream's episode page sets up no player")?;
    Ok(Resolved {
        title: episode_title(&page),
        streams: p.streams()?,
    })
}

fn episode_title(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<h1\b[^>]*\bid="episode-title"[^>]*>(.*?)</h1>"#).unwrap()
    });
    Some(text(&RE.captures(page)?[1])).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_episode_pages_player() {
        let new = r#"<h1 id="episode-title" class="white bebas extra-title center">Harem-tou e Youkoso! Episode 1</h1>
        <script>var folder = "collaboration-works\/harem-tou-e-youkoso"; var storageBase = "https:\/\/s2.myspacecat.pictures";
        var layout = 2;
        var resList = ["4k","1080","720"];
        var ep = "1";
        var res = "720-1,1080-1,4k-1";</script>"#;
        let p = player(new).unwrap();
        assert_eq!(p.layout, 2);
        assert_eq!(p.resolutions, ["4k", "1080", "720"]);
        let streams = p.streams().unwrap();
        let base = "https://s2.myspacecat.pictures/collaboration-works/harem-tou-e-youkoso";
        assert_eq!(
            streams[0].media,
            Media::Dash(vec![
                (
                    2160,
                    format!("{base}/4K/dash01/manifest.mpd").parse().unwrap()
                ),
                (
                    1080,
                    format!("{base}/1080p/dash01/manifest.mpd").parse().unwrap()
                ),
                (
                    720,
                    format!("{base}/720p/dash01/manifest.mpd").parse().unwrap()
                ),
            ])
        );
        assert_eq!(
            streams[1].media,
            Media::Hls(format!("{base}/1080p/dash01/master.m3u8").parse().unwrap()),
            "HLS below 4K"
        );
        assert_eq!(streams.len(), 3);
        assert!(
            streams
                .iter()
                .all(|s| s.referer.as_deref() == Some(REFERER))
        );
        assert_eq!(
            episode_title(new).as_deref(),
            Some("Harem-tou e Youkoso! Episode 1")
        );

        let old = r#"<script>var storageBase = "https:\/\/s1.myspacecat.pictures"
        var folder = "Overflow-1"
        var layout = 1
        var resList = ["4k"]
        var res = "4k-2"
        var ep = "1"</script>"#;
        let streams = player(old).unwrap().streams().unwrap();
        assert_eq!(
            streams,
            [Stream {
                media: Media::File(
                    "https://s1.myspacecat.pictures/Overflow-1/ep.webm"
                        .parse()
                        .unwrap()
                ),
                referer: Some(REFERER.into()),
                resolution: Some(2160),
                label: "file (webm)".into(),
            }]
        );
        let mp4 = old.replace("4k-2", "4k-1");
        assert!(matches!(
            &player(&mp4).unwrap().streams().unwrap()[0].media,
            Media::File(u) if u.path().ends_with("/ep.mp4")
        ));
        assert!(player("<p>nothing</p>").is_none());
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
