//! Searching Nyaa and fetching .torrent files from it.

use anyhow::{Context, Result, bail};

/// One search result from Nyaa's RSS feed.
#[derive(Debug, Clone)]
pub struct NyaaItem {
    pub id: u64,
    pub title: String,
    pub seeders: u32,
    pub size: u64,
    pub info_hash: String,
    /// Uploaded by a Nyaa "trusted" user.
    pub trusted: bool,
    /// Nyaa marks re-uploads of other people's releases as remakes.
    pub remake: bool,
}

/// Public trackers Nyaa embeds in its magnet links.
const NYAA_TRACKERS: &[&str] = &[
    "http://nyaa.tracker.wf:7777/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

/// Extract the torrent ID from a Nyaa view URL such as `https://nyaa.si/view/1961373`.
pub fn view_id(url: &str) -> Option<u64> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.strip_prefix("nyaa.si/view/")?;
    let digits = rest.split(['/', '?', '#']).next()?;
    digits.parse().ok()
}

pub fn view_url(id: u64) -> String {
    format!("https://nyaa.si/view/{id}")
}

/// Where .torrent downloads go instead of Nyaa (tests).
#[cfg(test)]
pub static DOWNLOAD_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn torrent_url(id: u64) -> String {
    #[cfg(test)]
    if let Some(base) = DOWNLOAD_BASE.lock().unwrap().as_ref() {
        return format!("{base}/download/{id}.torrent");
    }
    format!("https://nyaa.si/download/{id}.torrent")
}

/// A magnet link, used if the .torrent download fails.
pub fn magnet(info_hash_hex: &str) -> String {
    let mut m = format!("magnet:?xt=urn:btih:{info_hash_hex}");
    for tr in NYAA_TRACKERS {
        m.push_str("&tr=");
        m.push_str(tr);
    }
    m
}

/// Search Nyaa's "Anime - English-translated" category, most-seeded first.
/// Nyaa returns at most 75 results per query.
pub async fn search(client: &reqwest::Client, query: &str) -> Result<Vec<NyaaItem>> {
    let resp = client
        .get("https://nyaa.si/")
        .query(&[
            ("page", "rss"),
            ("q", query),
            ("c", "1_2"),
            ("f", "0"),
            ("s", "seeders"),
            ("o", "desc"),
        ])
        .send()
        .await
        .context("could not reach Nyaa")?
        .error_for_status()
        .context("Nyaa search failed")?;
    let body = resp
        .text()
        .await
        .context("error reading Nyaa search results")?;
    parse_rss(&body)
}

fn parse_rss(xml: &str) -> Result<Vec<NyaaItem>> {
    let doc = roxmltree::Document::parse(xml).context("Nyaa returned malformed RSS")?;
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
        let Some(id) = view_id(&text("guid")) else {
            continue;
        };
        items.push(NyaaItem {
            id,
            title: text("title"),
            seeders: text("seeders").parse().unwrap_or(0),
            size: parse_size(&text("size")).unwrap_or(0),
            info_hash: text("infoHash"),
            trusted: text("trusted") == "Yes",
            remake: text("remake") == "Yes",
        });
    }
    Ok(items)
}

/// "6.6 GiB" -> bytes.
fn parse_size(s: &str) -> Option<u64> {
    let (num, unit) = s.split_once(' ')?;
    let num: f64 = num.parse().ok()?;
    let mult = match unit {
        "Bytes" | "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((num * mult) as u64)
}

/// Download the .torrent file for a Nyaa torrent ID.
pub async fn fetch_torrent(client: &reqwest::Client, id: u64) -> Result<Vec<u8>> {
    let url = torrent_url(id);
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("Nyaa returned HTTP {status} for {url}");
    }
    let body = resp
        .bytes()
        .await
        .context("error reading .torrent from Nyaa")?;
    // A bencoded torrent is a dictionary, so it starts with 'd'. Anything else
    // is most likely an HTML error or DDoS-protection page.
    if body.first() != Some(&b'd') {
        bail!("Nyaa did not return a .torrent file for {url}");
    }
    Ok(body.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_view_urls() {
        assert_eq!(view_id("https://nyaa.si/view/1961373"), Some(1961373));
        assert_eq!(view_id("http://www.nyaa.si/view/12/"), Some(12));
        assert_eq!(view_id("https://nyaa.si/view/5?foo=bar"), Some(5));
        assert_eq!(view_id("/torrents.php?id=86576&torrentid=1167293"), None);
        assert_eq!(view_id("https://sukebei.nyaa.si/view/5"), None);
        assert_eq!(view_id("https://nyaa.si/view/"), None);
    }

    #[test]
    fn parses_rss() {
        let xml = r#"<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa" version="2.0"><channel>
            <item>
              <title>[SubsPlease] Sousou no Frieren S2 (01-10) (1080p) [Batch]</title>
              <link>https://nyaa.si/download/2100000.torrent</link>
              <guid isPermaLink="true">https://nyaa.si/view/2100000</guid>
              <nyaa:seeders>47</nyaa:seeders><nyaa:leechers>7</nyaa:leechers>
              <nyaa:infoHash>ef3e7ad1b12bdd9fc341691d8866cd1fa8374a4b</nyaa:infoHash>
              <nyaa:size>6.6 GiB</nyaa:size><nyaa:trusted>Yes</nyaa:trusted><nyaa:remake>No</nyaa:remake>
            </item>
            <item><title>Frieren: Beyond Journey&#39;s End</title><guid>https://nyaa.si/view/7</guid>
              <nyaa:seeders>0</nyaa:seeders><nyaa:size>512 MiB</nyaa:size><nyaa:remake>Yes</nyaa:remake></item>
        </channel></rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 2100000);
        assert_eq!(items[0].seeders, 47);
        assert_eq!(items[0].size, (6.6 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert!(items[0].trusted && !items[0].remake);
        assert_eq!(items[1].title, "Frieren: Beyond Journey's End");
        assert!(items[1].remake);
        assert_eq!(items[1].size, 512 * 1024 * 1024);
    }

    #[test]
    fn builds_urls() {
        let _net = crate::mock_http::real();
        assert_eq!(torrent_url(42), "https://nyaa.si/download/42.torrent");
        let m = magnet("143ed15e5e3df072ae91adaeb149973a887590dd");
        assert!(m.starts_with("magnet:?xt=urn:btih:143ed15e5e3df072ae91adaeb149973a887590dd&tr="));
    }
}
