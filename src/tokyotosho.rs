//! Searching TokyoTosho's "Hentai (Anime)" category.
//!
//! TokyoTosho only lists torrents: the .torrent files are hosted elsewhere
//! (mostly sukebei.nyaa.si, also AniRena, hshare.net, Nyaa mirrors...). Its
//! search page is behind Cloudflare's browser check, its RSS feed isn't, so
//! we search through the feed. The feed has no seeder counts; an entry's
//! details page does ([`peers`]).
//!
//! The feed returns at most 150 entries, newest first. Its search words must
//! all match, but anywhere in the entry (the comment, and what looks like the
//! torrent's file names too), so a result's title can lack them.

use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;

/// TokyoTosho's category 12, "Hentai (Anime)".
const CATEGORY: &str = "12";

const BASE: &str = "https://www.tokyotosho.info";

/// One entry from TokyoTosho's RSS feed.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// TokyoTosho's own entry number (`details.php?id=`).
    pub tosho_id: u64,
    pub title: String,
    /// The .torrent, on whichever site it was submitted from.
    pub torrent_url: String,
    pub magnet: Option<String>,
    /// Lowercase hex (the feed's magnets carry it in base32).
    pub info_hash: Option<String>,
    /// Bytes; TokyoTosho rounds it to two decimals of its unit.
    pub size: Option<u64>,
    /// The sukebei.nyaa.si torrent, when the .torrent is hosted there.
    pub sukebei_id: Option<u64>,
}

/// Seeders and leechers TokyoTosho last saw for an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peers {
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
}

/// Where TokyoTosho requests go instead (tests).
#[cfg(test)]
pub static TOKYOTOSHO_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn base() -> String {
    #[cfg(test)]
    if let Some(base) = TOKYOTOSHO_BASE.lock().unwrap().as_ref() {
        return base.clone();
    }
    BASE.to_owned()
}

pub fn details_url(tosho_id: u64) -> String {
    format!("https://www.tokyotosho.info/details.php?id={tosho_id}")
}

/// Search the "Hentai (Anime)" category, newest first (at most 150 results).
pub async fn search(client: &reqwest::Client, query: &str) -> Result<Vec<Item>> {
    // Only these two parameters: unknown ones get Cloudflare's browser check.
    let body = get(
        client,
        &format!("{}/rss.php", base()),
        &[("filter", CATEGORY), ("terms", query)],
    )
    .await
    .context("TokyoTosho search failed")?;
    parse_rss(&body)
}

/// The seeders, leechers and downloads TokyoTosho shows for an entry (from
/// the trackers, when it last asked them); `None` if the page has none.
pub async fn peers(client: &reqwest::Client, tosho_id: u64) -> Result<Option<Peers>> {
    let id = tosho_id.to_string();
    let body = get(
        client,
        &format!("{}/details.php", base()),
        &[("id", id.as_str())],
    )
    .await
    .context("could not load the TokyoTosho entry")?;
    Ok(parse_peers(&body))
}

async fn get(client: &reqwest::Client, url: &str, query: &[(&str, &str)]) -> Result<String> {
    let resp = client
        .get(url)
        .query(query)
        .send()
        .await
        .context("could not reach TokyoTosho")?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .context("error reading TokyoTosho's answer")?;
    if !status.is_success() {
        if body.contains("challenge-platform") || body.contains("Just a moment") {
            bail!("TokyoTosho answered with Cloudflare's browser check (HTTP {status})");
        }
        bail!("TokyoTosho returned HTTP {status}");
    }
    Ok(body)
}

fn parse_rss(xml: &str) -> Result<Vec<Item>> {
    let doc = roxmltree::Document::parse(xml).context("TokyoTosho returned malformed RSS")?;
    static ID_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"details\.php\?id=(\d+)").unwrap());
    static MAGNET_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"href="(magnet:\?[^"]+)""#).unwrap());
    static SIZE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Size:\s*([0-9.]+)\s*([KMGT]?B)").unwrap());
    let mut items = Vec::new();
    for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
        let text = |name: &str| {
            item.children()
                .find(|c| c.tag_name().name() == name)
                .and_then(|c| c.text())
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let description = text("description");
        let id = ID_RE
            .captures(&text("guid"))
            .or_else(|| ID_RE.captures(&description))
            .and_then(|c| c[1].parse().ok());
        let torrent_url = text("link");
        let Some(tosho_id) = id else { continue };
        if torrent_url.is_empty() {
            continue;
        }
        let magnet = MAGNET_RE
            .captures(&description)
            .map(|c| c[1].replace("&amp;", "&"));
        let size = SIZE_RE
            .captures(&description)
            .and_then(|c| parse_size(&c[1], &c[2]));
        items.push(Item {
            tosho_id,
            title: text("title"),
            info_hash: magnet.as_deref().and_then(magnet_info_hash),
            sukebei_id: sukebei_id(&torrent_url),
            torrent_url,
            magnet,
            size,
        });
    }
    Ok(items)
}

/// "793.89" "MB" -> bytes. TokyoTosho's MB and GB are MiB and GiB.
fn parse_size(num: &str, unit: &str) -> Option<u64> {
    let num: f64 = num.parse().ok()?;
    let mult = match unit {
        "B" => 1u64,
        "KB" => 1 << 10,
        "MB" => 1 << 20,
        "GB" => 1 << 30,
        "TB" => 1 << 40,
        _ => return None,
    };
    Some((num * mult as f64).round() as u64)
}

/// A magnet's BitTorrent v1 info hash, as lowercase hex (it may be base32).
fn magnet_info_hash(magnet: &str) -> Option<String> {
    let start = magnet.find("urn:btih:")? + "urn:btih:".len();
    let hash = magnet[start..].split('&').next()?;
    if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(hash.to_ascii_lowercase());
    }
    if hash.len() == 32 {
        let bytes = base32(hash)?;
        return Some(bytes.iter().map(|b| format!("{b:02x}")).collect());
    }
    None
}

/// RFC 4648 base32 (no padding) -> bytes.
fn base32(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = (acc << 5) | u32::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `https://sukebei.nyaa.si/download/3971269.torrent` (or `/view/3971269`)
/// -> 3971269. Its mirrors (sukebei.nyaa.net, .pt) name torrents by hash
/// instead.
fn sukebei_id(url: &str) -> Option<u64> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    let rest = rest.strip_prefix("sukebei.nyaa.si/")?;
    let rest = rest
        .strip_prefix("download/")
        .or_else(|| rest.strip_prefix("view/"))?;
    let digits = rest.split(['.', '/', '?', '#']).next()?;
    digits.parse().ok()
}

fn parse_peers(html: &str) -> Option<Peers> {
    static FIELD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<li class="detailsleft[^"]*">(\w+):</li>\s*<li class="detailsright[^"]*">\s*(\d+)\s*</li>"#)
            .unwrap()
    });
    let (mut seeders, mut leechers, mut completed) = (None, None, 0);
    for c in FIELD.captures_iter(html) {
        let n = c[2].parse().ok();
        match &c[1] {
            "Seeders" => seeders = n,
            "Leechers" => leechers = n,
            "Completed" => completed = n.unwrap_or(0),
            _ => {}
        }
    }
    Some(Peers {
        seeders: seeders?,
        leechers: leechers?,
        completed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three entries of `rss.php?filter=12&terms=overflow` (trackers cut).
    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom"><channel>
<title>overflow - Tokyo Toshokan</title><link>https://www.tokyotosho.info</link>
<ttl>15</ttl><item>
<category>Hentai (Anime)</category>
<title>[18+] [おーばーふろぉ] Overflow S01 UNCEN 1080p WEB-DL AAC2.0 x265-TBMovies</title>
<link><![CDATA[https://www.anirena.com/dl/198262]]></link>
<description><![CDATA[<a href="https://www.anirena.com/dl/198262">Torrent Link</a><br />
<a href="magnet:?xt=urn:btih:IQQ3MCBMDVLG4AUW6L4I6LQN62U4APFP&tr=http://tracker.anirena.com:80/announce">Magnet Link</a><br />
<a href="https://www.tokyotosho.info/details.php?id=2001122">Tokyo Tosho</a><br />
Size: 793.89MB<br />
Authorized: N/A<br />
Submitter: TBMovies<br />
Comment: PLEASE SEED THIS TORRENT AFTER DOWNLOADING.]]></description>
<guid><![CDATA[https://www.tokyotosho.info/details.php?id=2001122]]>
</guid>
<pubDate>Mon, 21 Jul 2025 06:59:44 GMT</pubDate>
</item>
<item>
<category>Hentai (Anime)</category>
<title>[SakuraCircle] Overflow - 08 (WEB 1280x720 h264 AAC) [606B0A4A].mkv</title>
<link><![CDATA[https://sukebei.nyaa.si/download/2948207.torrent]]></link>
<description><![CDATA[<a href="https://sukebei.nyaa.si/download/2948207.torrent">Torrent Link</a><br />
<a href="magnet:?xt=urn:btih:HYWH3Q6KS7FO52U4QHQPZYIXCOM4D47I&tr=http://sukebei.tracker.wf:8888/announce">Magnet Link</a><br />
<a href="https://www.tokyotosho.info/details.php?id=1428994">Tokyo Tosho</a><br />
Size: 164.61MB<br />
Authorized: No<br />
Submitter: Tennouji<br />
Comment: A SakuraCircle project.]]></description>
<guid><![CDATA[https://www.tokyotosho.info/details.php?id=1428994]]>
</guid>
</item>
<item>
<category>Hentai (Anime)</category>
<title>Rare, Classic &amp; Uncensored Hentai Collection</title>
<link><![CDATA[https://sukebei.nyaa.net/download/ac70294f74e87d59318c7b2a4c37f1fadb4fa7f7]]></link>
<description><![CDATA[<a href="https://sukebei.nyaa.net/download/ac70294f74e87d59318c7b2a4c37f1fadb4fa7f7">Torrent Link</a><br />
<a href="https://www.tokyotosho.info/details.php?id=1429479">Tokyo Tosho</a><br />
Size: 3.56GB<br />
Authorized: N/A<br />]]></description>
<guid><![CDATA[https://www.tokyotosho.info/details.php?id=1429479]]>
</guid>
</item>
</channel></rss>"#;

    #[test]
    fn parses_rss() {
        let items = parse_rss(RSS).unwrap();
        assert_eq!(items.len(), 3);
        let first = &items[0];
        assert_eq!(first.tosho_id, 2001122);
        assert!(
            first
                .title
                .starts_with("[18+] [おーばーふろぉ] Overflow S01")
        );
        assert_eq!(first.torrent_url, "https://www.anirena.com/dl/198262");
        assert_eq!(
            first.magnet.as_deref(),
            Some(
                "magnet:?xt=urn:btih:IQQ3MCBMDVLG4AUW6L4I6LQN62U4APFP&tr=http://tracker.anirena.com:80/announce"
            )
        );
        assert_eq!(
            first.info_hash.as_deref(),
            Some("4421b6082c1d566e0296f2f88f2e0df6a9c03caf")
        );
        assert_eq!(first.size, Some((793.89f64 * 1048576.0).round() as u64));
        assert_eq!(first.sukebei_id, None);

        let second = &items[1];
        assert_eq!(second.sukebei_id, Some(2948207));
        // The .torrent's own info hash.
        assert_eq!(
            second.info_hash.as_deref(),
            Some("3e2c7dc3ca97caeeea9c81e0fce1171399c1f3e8")
        );
        // The .torrent says 172603645 bytes.
        assert!(second.size.unwrap().abs_diff(172_603_645) < 10_000);

        let third = &items[2];
        assert_eq!(third.title, "Rare, Classic & Uncensored Hentai Collection");
        assert_eq!(
            (third.magnet.as_ref(), third.info_hash.as_ref()),
            (None, None)
        );
        assert_eq!(third.sukebei_id, None, "a mirror's hash-named download");
        assert_eq!(third.size, Some((3.56f64 * 1073741824.0).round() as u64));
    }

    #[test]
    fn reads_info_hashes() {
        assert_eq!(
            magnet_info_hash("magnet:?xt=urn:btih:VRYCST3U5B6VSMMMPMVEYN7R7LNU7J7X&tr=x")
                .as_deref(),
            Some("ac70294f74e87d59318c7b2a4c37f1fadb4fa7f7")
        );
        assert_eq!(
            magnet_info_hash("magnet:?xt=urn:btih:AC70294F74E87D59318C7B2A4C37F1FADB4FA7F7")
                .as_deref(),
            Some("ac70294f74e87d59318c7b2a4c37f1fadb4fa7f7")
        );
        assert_eq!(magnet_info_hash("magnet:?xt=urn:btih:short"), None);
        assert_eq!(magnet_info_hash("magnet:?dn=x"), None);
    }

    #[test]
    fn reads_sukebei_ids() {
        assert_eq!(
            sukebei_id("https://sukebei.nyaa.si/download/3971269.torrent"),
            Some(3971269)
        );
        assert_eq!(sukebei_id("http://sukebei.nyaa.si/view/12"), Some(12));
        assert_eq!(sukebei_id("https://nyaa.si/download/5.torrent"), None);
        assert_eq!(
            sukebei_id("https://sukebei.nyaa.pt/download/5fe00a025be9629e68042a57a66d66f6c1ff1785"),
            None
        );
        assert_eq!(sukebei_id("https://www.anirena.com/dl/198262"), None);
    }

    #[test]
    fn parses_peers() {
        let html = r#"<li class="detailsleft">BT Info Hash (SHA):</li>
<li class="detailsright"><a href="magnet:?xt=urn:btih:HYWH">Magnet Link</a> 3e2c7dc3</li>
<li class="detailsleft shade">Seeders:</li>
<li class="detailsright shade">3</li>
<li class="detailsleft">Leechers:</li>
<li class="detailsright">0</li>
<li class="detailsleft shade">Completed:</li>
<li class="detailsright shade">18924</li>"#;
        assert_eq!(
            parse_peers(html),
            Some(Peers {
                seeders: 3,
                leechers: 0,
                completed: 18924
            })
        );
        assert_eq!(parse_peers("<html></html>"), None);
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn searches_the_hentai_category() {
        let base = crate::mock_http::mock(|req| {
            assert!(req.target.starts_with("/rss.php?"), "{}", req.target);
            let q = req.query();
            assert_eq!(q.get("filter").map(String::as_str), Some("12"));
            assert_eq!(q.get("terms").map(String::as_str), Some("overflow & more"));
            crate::mock_http::Resp {
                status: 200,
                headers: vec![(
                    "Content-Type".into(),
                    "application/rss+xml; charset=utf-8".into(),
                )],
                body: RSS.as_bytes().to_vec(),
            }
        })
        .await;
        let _net = crate::mock_http::mocked();
        *TOKYOTOSHO_BASE.lock().unwrap() = Some(base);
        let items = search(&reqwest::Client::new(), "overflow & more").await;
        *TOKYOTOSHO_BASE.lock().unwrap() = None;
        assert_eq!(items.unwrap().len(), 3);
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn reports_cloudflare_checks() {
        let base = crate::mock_http::mock(|_| {
            crate::mock_http::Resp::html(
                403,
                "<title>Just a moment...</title><script>window._cf_chl_opt={}</script>",
            )
        })
        .await;
        let _net = crate::mock_http::mocked();
        *TOKYOTOSHO_BASE.lock().unwrap() = Some(base);
        let err = search(&reqwest::Client::new(), "x").await;
        *TOKYOTOSHO_BASE.lock().unwrap() = None;
        let err = format!("{:#}", err.unwrap_err());
        assert!(err.contains("Cloudflare"), "{err}");
    }

    #[tokio::test]
    #[ignore = "uses the network"]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn live_search() {
        let _net = crate::mock_http::real();
        let client = reqwest::Client::builder()
            .user_agent("ichioshi-tests")
            .build()
            .unwrap();
        let items = search(&client, "overflow").await.unwrap();
        for i in &items {
            println!(
                "{} | {:?} MiB | sukebei {:?} | {:?} | {}",
                i.tosho_id,
                i.size.map(|s| s / 1048576),
                i.sukebei_id,
                i.info_hash,
                i.title
            );
        }
        assert!(items.iter().any(|i| i.title.contains("Overflow")));
        assert!(
            items
                .iter()
                .all(|i| i.info_hash.as_ref().is_none_or(|h| h.len() == 40))
        );
        let first = items.iter().find(|i| i.sukebei_id.is_some()).unwrap();
        let peers = peers(&client, first.tosho_id).await.unwrap();
        println!("{}: {peers:?}", first.title);
        assert!(peers.is_some());
    }
}
