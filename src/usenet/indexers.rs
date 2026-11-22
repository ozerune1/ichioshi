//! The anime indexers that mirror torrents to Usenet: AnimeTosho
//! (feed.animetosho.net) and TsukiHime (api.tsukihime.org). Both know each
//! release by its torrent's info hash, the same release found on Nyaa, so a
//! release's NZBs from both are one entry.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Indexer {
    AnimeTosho,
    TsukiHime,
}

impl Indexer {
    pub fn name(self) -> &'static str {
        match self {
            Indexer::AnimeTosho => "AnimeTosho",
            Indexer::TsukiHime => "TsukiHime",
        }
    }
}

/// Where to get a release's NZB.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NzbLink {
    pub indexer: Indexer,
    pub url: String,
}

#[derive(Clone)]
pub struct Indexers {
    http: reqwest::Client,
    animetosho: String,
    tsukihime_api: String,
    tsukihime_storage: String,
}

impl Indexers {
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_bases(http, "https://feed.animetosho.net", "https://api.tsukihime.org", "https://storage.tsukihime.org")
    }

    pub fn with_bases(http: reqwest::Client, animetosho: &str, tsukihime_api: &str, tsukihime_storage: &str) -> Self {
        Self {
            http,
            animetosho: animetosho.trim_end_matches('/').to_owned(),
            tsukihime_api: tsukihime_api.trim_end_matches('/').to_owned(),
            tsukihime_storage: tsukihime_storage.trim_end_matches('/').to_owned(),
        }
    }

    /// The release's NZBs from every indexer that has a finished one, in the
    /// order to try them. An indexer that's down just has none.
    pub async fn lookup(&self, info_hash: &str, nyaa_id: Option<u64>) -> Vec<NzbLink> {
        let hash = info_hash.trim().to_ascii_lowercase();
        if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Vec::new();
        }
        let (at, tk) = tokio::join!(self.animetosho(&hash), self.tsukihime(&hash, nyaa_id));
        let mut links = Vec::new();
        for (indexer, found) in [(Indexer::AnimeTosho, at), (Indexer::TsukiHime, tk)] {
            match found {
                Ok(Some(url)) => links.push(NzbLink { indexer, url }),
                Ok(None) => {}
                Err(e) => tracing::warn!("{} lookup failed: {e:#}", indexer.name()),
            }
        }
        links
    }

    async fn get_json(&self, url: &str) -> Result<Option<Value>> {
        let resp = self.http.get(url).timeout(Duration::from_secs(20)).send().await.context("no answer")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("HTTP {}", resp.status());
        }
        Ok(Some(resp.json().await.context("unreadable answer")?))
    }

    async fn animetosho(&self, hash: &str) -> Result<Option<String>> {
        let Some(v) = self.get_json(&format!("{}/json?show=torrent&btih={hash}", self.animetosho)).await? else {
            return Ok(None);
        };
        // An unknown hash gets an empty object; a release still being
        // mirrored has status "processing".
        let complete = v["status"].as_str() == Some("complete");
        let same = v["info_hash"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(hash));
        Ok(v["nzb_url"].as_str().filter(|u| complete && same && !u.is_empty()).map(str::to_owned))
    }

    async fn tsukihime(&self, hash: &str, nyaa_id: Option<u64>) -> Result<Option<String>> {
        let mut found = self.get_json(&format!("{}/v1/torrents/btih/{hash}", self.tsukihime_api)).await?;
        if found.is_none()
            && let Some(id) = nyaa_id
        {
            found = self.get_json(&format!("{}/v1/torrents/nyaa/{id}", self.tsukihime_api)).await?;
        }
        let Some(v) = found else { return Ok(None) };
        let flag = |x: &Value| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok())) == Some(1);
        let id = v["id"].as_u64().map(|i| i.to_string()).or_else(|| v["id"].as_str().map(str::to_owned));
        let same = v["btih"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(hash));
        Ok(match id {
            Some(id) if flag(&v["has_nzb"]) && v["state"].as_str() == Some("completed") && same => {
                // Only the ID matters to the storage server; the name is cosmetic.
                Some(format!("{}/nzbs/{id}/{hash}.nzb.gz", self.tsukihime_storage))
            }
            _ => None,
        })
    }

    /// Download an NZB, unpacking it if it's gzipped.
    pub async fn fetch_nzb(&self, link: &NzbLink) -> Result<Vec<u8>> {
        let resp = self
            .http
            .get(&link.url)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .with_context(|| format!("could not reach {}", link.indexer.name()))?;
        if !resp.status().is_success() {
            bail!("{} answered HTTP {} for the NZB", link.indexer.name(), resp.status());
        }
        let bytes = resp.bytes().await?.to_vec();
        if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes.as_slice()).read_to_end(&mut out).context("could not unpack the NZB")?;
            return Ok(out);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;
    use crate::mock_http::{Resp, mock, reply};

    pub(crate) const HASH: &str = "419d5d20bae187555aa7ddceb016c789c15ea2e2";

    /// Both indexers on one mock server: AnimeTosho under /at, TsukiHime's
    /// API under /tk and its storage under /tks. `nzb` is served for both.
    pub(crate) async fn mock_indexers(at_status: &'static str, tk_has_nzb: bool, nzb: Vec<u8>) -> Indexers {
        let base = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let b = base.clone();
        let url = mock(move |req| {
            let path = req.target.split('?').next().unwrap().to_owned();
            let base = b.lock().unwrap().clone();
            match path.as_str() {
                "/at/json" => {
                    let q = req.query();
                    assert_eq!(q["show"], "torrent");
                    if q["btih"] == HASH {
                        reply(200, json!({"id": 706852, "info_hash": HASH, "status": at_status, "nzb_url": format!("{base}/at/nzb/706852")}))
                    } else {
                        reply(200, json!({}))
                    }
                }
                p if p == format!("/tk/v1/torrents/btih/{HASH}") => reply(
                    200,
                    json!({"id": 41407, "btih": HASH, "state": "completed", "has_nzb": if tk_has_nzb { 1 } else { 0 }, "nyaa_id": 2189234}),
                ),
                p if p.starts_with("/tk/v1/torrents/") => reply(404, json!({"detail": "Torrent not found"})),
                "/at/nzb/706852" => Resp::binary(200, nzb.clone()),
                p if p.starts_with("/tks/nzbs/41407/") => {
                    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                    std::io::Write::write_all(&mut gz, &nzb).unwrap();
                    Resp::binary(200, gz.finish().unwrap())
                }
                other => panic!("unexpected {other}"),
            }
        })
        .await;
        *base.lock().unwrap() = url.clone();
        Indexers::with_bases(reqwest::Client::new(), &format!("{url}/at"), &format!("{url}/tk"), &format!("{url}/tks"))
    }

    #[tokio::test]
    async fn finds_a_release_on_both_and_fetches_its_nzbs() {
        let idx = mock_indexers("complete", true, b"<nzb/>".to_vec()).await;
        let links = idx.lookup(&HASH.to_uppercase(), Some(2189234)).await;
        assert_eq!(links.iter().map(|l| l.indexer).collect::<Vec<_>>(), [Indexer::AnimeTosho, Indexer::TsukiHime]);
        assert!(links[1].url.ends_with(&format!("/tks/nzbs/41407/{HASH}.nzb.gz")));
        for link in &links {
            assert_eq!(idx.fetch_nzb(link).await.unwrap(), b"<nzb/>", "{link:?}");
        }
        // Unknown releases, and junk hashes, have none.
        assert!(idx.lookup(&"0".repeat(40), Some(1)).await.is_empty());
        assert!(idx.lookup("not-a-hash", None).await.is_empty());
    }

    #[tokio::test]
    async fn only_finished_nzbs_count() {
        let idx = mock_indexers("processing", false, Vec::new()).await;
        assert!(idx.lookup(HASH, None).await.is_empty());
    }

    #[tokio::test]
    async fn an_indexer_that_is_down_has_none() {
        let idx = Indexers::with_bases(reqwest::Client::new(), "http://127.0.0.1:9", "http://127.0.0.1:9", "http://127.0.0.1:9");
        assert!(idx.lookup(HASH, Some(1)).await.is_empty());
    }

    /// The real indexers, read-only: a release both have.
    #[tokio::test]
    #[ignore = "network"]
    async fn real_indexers() {
        let idx = Indexers::new(reqwest::Client::builder().user_agent("seadex-downloader-test").build().unwrap());
        let links = idx.lookup(HASH, Some(2189234)).await;
        println!("{links:?}");
        assert!(links.iter().any(|l| l.indexer == Indexer::TsukiHime));
        for link in &links {
            let files = crate::usenet::nzb::parse(&idx.fetch_nzb(link).await.unwrap()).unwrap();
            println!("{}: {:?}", link.indexer.name(), files.iter().map(|f| (&f.name, f.segments.len())).collect::<Vec<_>>());
            assert!(files.iter().any(|f| f.name.ends_with(".mkv")));
            assert!(files.iter().any(|f| f.is_par2()));
        }
    }
}
