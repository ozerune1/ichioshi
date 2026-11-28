//! Searching Nyaa (and Sukebei, its 18+ sister site) and fetching .torrent
//! files from it.

use anyhow::{Context, Result, bail};

/// Which of Nyaa's sites a torrent is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Site {
    #[default]
    Nyaa,
    /// sukebei.nyaa.si: the same site for 18+ anime.
    Sukebei,
    /// A torrent TokyoTosho lists from another tracker (AniRena and the
    /// like); its ID is TokyoTosho's.
    TokyoTosho,
}

impl Site {
    pub fn name(self) -> &'static str {
        match self {
            Site::Nyaa => "Nyaa",
            Site::Sukebei => "Sukebei",
            Site::TokyoTosho => "TokyoTosho",
        }
    }

    fn host(self) -> &'static str {
        match self {
            Site::Nyaa => "nyaa.si",
            Site::Sukebei => "sukebei.nyaa.si",
            Site::TokyoTosho => "www.tokyotosho.info",
        }
    }

    /// What's searched: Nyaa's "Anime - English-translated"; Sukebei's
    /// "Art - Anime".
    fn category(self) -> &'static str {
        match self {
            Site::Nyaa => "1_2",
            Site::Sukebei => "1_1",
            Site::TokyoTosho => "",
        }
    }

    /// The trackers its magnet links carry.
    fn trackers(self) -> &'static [&'static str] {
        match self {
            Site::Nyaa => NYAA_TRACKERS,
            Site::Sukebei => SUKEBEI_TRACKERS,
            Site::TokyoTosho => &SUKEBEI_TRACKERS[1..],
        }
    }
}

/// A torrent on one of Nyaa's sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TorrentId {
    pub site: Site,
    pub id: u64,
}

impl TorrentId {
    pub fn nyaa(id: u64) -> Self {
        Self {
            site: Site::Nyaa,
            id,
        }
    }
}

/// One search result from Nyaa's (or Sukebei's) RSS feed.
#[derive(Debug, Clone)]
pub struct NyaaItem {
    pub site: Site,
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

impl NyaaItem {
    pub fn torrent(&self) -> TorrentId {
        TorrentId {
            site: self.site,
            id: self.id,
        }
    }

    pub fn magnet(&self) -> String {
        magnet_on(self.site, &self.info_hash)
    }

    /// Its ID on Nyaa (Usenet indexers know Nyaa's, not Sukebei's).
    pub fn nyaa_id(&self) -> Option<u64> {
        (self.site == Site::Nyaa).then_some(self.id)
    }
}

/// Public trackers Nyaa embeds in its magnet links.
const NYAA_TRACKERS: &[&str] = &[
    "http://nyaa.tracker.wf:7777/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

/// Sukebei's own tracker, then the public ones.
const SUKEBEI_TRACKERS: &[&str] = &[
    "http://sukebei.tracker.wf:8888/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

/// Extract the torrent ID from a Nyaa view URL such as `https://nyaa.si/view/1961373`.
pub fn view_id(url: &str) -> Option<u64> {
    view_on(url).filter(|t| t.site == Site::Nyaa).map(|t| t.id)
}

/// The torrent a Nyaa or Sukebei view URL is for.
pub fn view_on(url: &str) -> Option<TorrentId> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let (site, rest) = if let Some(r) = rest.strip_prefix("sukebei.nyaa.si/view/") {
        (Site::Sukebei, r)
    } else {
        (Site::Nyaa, rest.strip_prefix("nyaa.si/view/")?)
    };
    let digits = rest.split(['/', '?', '#']).next()?;
    Some(TorrentId {
        site,
        id: digits.parse().ok()?,
    })
}

pub fn view_url(t: TorrentId) -> String {
    match t.site {
        Site::TokyoTosho => crate::tokyotosho::details_url(t.id),
        _ => format!("https://{}/view/{}", t.site.host(), t.id),
    }
}

/// The .torrent links of TokyoTosho's entries (on other trackers), by its
/// IDs, as its searches found them.
static TOSHO_LINKS: std::sync::Mutex<std::collections::BTreeMap<u64, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Remember where a TokyoTosho entry's .torrent is.
pub fn remember_tosho_link(tosho_id: u64, url: &str) {
    TOSHO_LINKS.lock().unwrap().insert(tosho_id, url.to_owned());
}

/// Where .torrent downloads go instead of Nyaa (tests).
#[cfg(test)]
pub static DOWNLOAD_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn torrent_url(t: TorrentId) -> String {
    #[cfg(test)]
    if let Some(base) = DOWNLOAD_BASE.lock().unwrap().as_ref() {
        return format!("{base}/download/{}.torrent", t.id);
    }
    match t.site {
        Site::TokyoTosho => TOSHO_LINKS
            .lock()
            .unwrap()
            .get(&t.id)
            .cloned()
            .unwrap_or_else(|| crate::tokyotosho::details_url(t.id)),
        _ => format!("https://{}/download/{}.torrent", t.site.host(), t.id),
    }
}

/// A magnet link with Nyaa's trackers, used if the .torrent download fails.
pub fn magnet(info_hash_hex: &str) -> String {
    magnet_on(Site::Nyaa, info_hash_hex)
}

/// A magnet link with the trackers of the site the torrent is on.
pub fn magnet_on(site: Site, info_hash_hex: &str) -> String {
    let mut m = format!("magnet:?xt=urn:btih:{info_hash_hex}");
    for tr in site.trackers() {
        m.push_str("&tr=");
        m.push_str(tr);
    }
    m
}

/// Where searches go instead of the sites (tests).
#[cfg(test)]
pub static SEARCH_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn search_url(site: Site) -> String {
    #[cfg(test)]
    if let Some(base) = SEARCH_BASE.lock().unwrap().as_ref() {
        return format!("{base}/{}/", site.name().to_lowercase());
    }
    format!("https://{}/", site.host())
}

/// Search a site's anime, most-seeded first (Nyaa's English-translated
/// releases; Sukebei's anime). Each returns at most 75 results per query.
pub async fn search(client: &reqwest::Client, site: Site, query: &str) -> Result<Vec<NyaaItem>> {
    let name = site.name();
    let resp = client
        .get(search_url(site))
        .query(&[
            ("page", "rss"),
            ("q", query),
            ("c", site.category()),
            ("f", "0"),
            ("s", "seeders"),
            ("o", "desc"),
        ])
        .send()
        .await
        .with_context(|| format!("could not reach {name}"))?
        .error_for_status()
        .with_context(|| format!("{name} search failed"))?;
    let body = resp
        .text()
        .await
        .with_context(|| format!("error reading {name} search results"))?;
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
        let Some(TorrentId { site, id }) = view_on(&text("guid")) else {
            continue;
        };
        items.push(NyaaItem {
            site,
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

/// Download a torrent's .torrent file from its site.
pub async fn fetch_torrent(client: &reqwest::Client, t: TorrentId) -> Result<Vec<u8>> {
    let url = torrent_url(t);
    let name = t.site.name();
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("{name} returned HTTP {status} for {url}");
    }
    let body = resp
        .bytes()
        .await
        .with_context(|| format!("error reading .torrent from {name}"))?;
    // A bencoded torrent is a dictionary, so it starts with 'd'. Anything else
    // is most likely an HTML error or DDoS-protection page.
    if body.first() != Some(&b'd') {
        bail!("{name} did not return a .torrent file for {url}");
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
        assert_eq!(
            view_on("https://sukebei.nyaa.si/view/4734397"),
            Some(TorrentId {
                site: Site::Sukebei,
                id: 4734397
            })
        );
        assert_eq!(
            view_on("https://nyaa.si/view/12"),
            Some(TorrentId::nyaa(12))
        );
    }

    #[test]
    fn sukebei_links_and_magnets() {
        let t = TorrentId {
            site: Site::Sukebei,
            id: 4734397,
        };
        assert_eq!(view_url(t), "https://sukebei.nyaa.si/view/4734397");
        assert_eq!(
            torrent_url(t),
            "https://sukebei.nyaa.si/download/4734397.torrent"
        );
        let m = magnet_on(Site::Sukebei, "bd637fb379e6fd9c14ddb0567b9fb9a46aeb1149");
        assert!(m.contains("sukebei.tracker.wf"), "{m}");
        assert!(!magnet("ab").contains("sukebei"));
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
        assert_eq!(items[0].site, Site::Nyaa);
    }

    #[test]
    fn parses_sukebei_rss() {
        // As Sukebei sends it (its own namespace URL).
        let xml = r#"<rss xmlns:atom="http://www.w3.org/2005/Atom" xmlns:nyaa="https://sukebei.nyaa.si/xmlns/nyaa" version="2.0"><channel>
            <item>
              <title>[Doomdos] - Overflow - 08 [Uncensored] [Dub] [1080p OV WEB-DL]</title>
              <link>https://sukebei.nyaa.si/download/4734397.torrent</link>
              <guid isPermaLink="true">https://sukebei.nyaa.si/view/4734397</guid>
              <nyaa:seeders>15</nyaa:seeders><nyaa:leechers>1</nyaa:leechers>
              <nyaa:infoHash>bd637fb379e6fd9c14ddb0567b9fb9a46aeb1149</nyaa:infoHash>
              <nyaa:categoryId>1_1</nyaa:categoryId><nyaa:size>422.9 MiB</nyaa:size>
              <nyaa:trusted>No</nyaa:trusted><nyaa:remake>No</nyaa:remake>
            </item>
        </channel></rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        let i = &items[0];
        assert_eq!(i.site, Site::Sukebei);
        assert_eq!(i.id, 4734397);
        assert_eq!(i.seeders, 15);
        assert_eq!(i.nyaa_id(), None);
        assert!(i.magnet().contains("sukebei.tracker.wf"));
        assert_eq!(
            torrent_url(i.torrent()),
            "https://sukebei.nyaa.si/download/4734397.torrent"
        );
    }

    #[test]
    fn builds_urls() {
        let _net = crate::mock_http::real();
        assert_eq!(
            torrent_url(TorrentId::nyaa(42)),
            "https://nyaa.si/download/42.torrent"
        );
        let m = magnet("143ed15e5e3df072ae91adaeb149973a887590dd");
        assert!(m.starts_with("magnet:?xt=urn:btih:143ed15e5e3df072ae91adaeb149973a887590dd&tr="));
    }
}
