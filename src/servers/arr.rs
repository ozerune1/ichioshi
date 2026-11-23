//! Sonarr and Radarr (their v3 API, which Sonarr 4 and Radarr 5 speak):
//! what's there, adding anime, and handing them a release or a search.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ArrAccount;

/// Which one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Sonarr,
    Radarr,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Sonarr => "Sonarr",
            Kind::Radarr => "Radarr",
        }
    }
}

/// A connection to Sonarr or Radarr.
#[derive(Clone)]
pub struct Arr {
    pub kind: Kind,
    pub base: String,
    key: String,
    client: reqwest::Client,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Profile {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RootFolder {
    pub path: String,
    #[serde(rename = "freeSpace", default)]
    pub free: Option<u64>,
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

impl std::fmt::Display for RootFolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.path)?;
        if let Some(free) = self.free {
            write!(f, " ({} free)", crate::app::fmt_bytes(free))?;
        }
        Ok(())
    }
}

/// What a server says about itself, and what anime can be added with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct About {
    pub version: String,
    pub profiles: Vec<Profile>,
    pub folders: Vec<RootFolder>,
}

/// A series in Sonarr.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Show {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub tvdb_id: u64,
    #[serde(default)]
    pub title_slug: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub seasons: Vec<Season>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Season {
    pub season_number: u32,
    #[serde(default)]
    pub monitored: bool,
}

/// An episode in Sonarr.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: u64,
    pub season_number: u32,
    pub episode_number: u32,
    #[serde(default)]
    pub has_file: bool,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub episode_file_id: u64,
}

/// A file Sonarr or Radarr has.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub id: u64,
    #[serde(default)]
    pub relative_path: String,
    #[serde(default)]
    pub size: u64,
}

/// A film in Radarr.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Movie {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub tmdb_id: u64,
    #[serde(default)]
    pub title_slug: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub has_file: bool,
    #[serde(default)]
    pub movie_file: Option<File>,
}

/// Something downloading: for which series' episode (Sonarr) or film
/// (Radarr), and how far along.
#[derive(Clone, Debug, PartialEq)]
pub struct Queued {
    pub owner: u64,
    pub episode: Option<u64>,
    pub size: f64,
    pub left: f64,
}

/// What became of a release handed over.
#[derive(Clone, Debug, PartialEq)]
pub struct Pushed {
    pub approved: bool,
    pub rejections: Vec<String>,
}

impl Arr {
    pub fn new(kind: Kind, account: &ArrAccount, client: &reqwest::Client) -> Self {
        Self {
            kind,
            base: super::base(&account.url),
            key: account.api_key.trim().to_owned(),
            client: client.clone(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v3/{path}", self.base)
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let name = self.kind.name();
        let resp = req
            .header("X-Api-Key", &self.key)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!(super::describe(&e, name)))?;
        let status = resp.status();
        if !status.is_success() {
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            // Validation errors come as a list of {errorMessage}.
            let why = body
                .as_array()
                .and_then(|a| a.first())
                .and_then(|e| e["errorMessage"].as_str())
                .or_else(|| body["message"].as_str())
                .map(str::to_owned);
            match (status.as_u16(), why) {
                (401, _) => bail!("{name} refused the API key (HTTP 401)"),
                (_, Some(why)) => bail!("{name}: {why}"),
                _ => bail!("{name} answered HTTP {status}"),
            }
        }
        let text = resp.text().await.unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| {
            format!("{name} answered with something else (is the address {name}'s?)")
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let v = self.send(self.client.get(self.url(path))).await?;
        serde_json::from_value(v)
            .with_context(|| format!("unexpected answer from {}", self.kind.name()))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(self.client.post(self.url(path)).json(body)).await
    }

    /// Its version, quality profiles and root folders; fails unless it's
    /// the right kind of server and the key works.
    pub async fn about(&self) -> Result<About> {
        let status: Value = self.get("system/status").await?;
        let app = status["appName"].as_str().unwrap_or_default();
        if !app.eq_ignore_ascii_case(self.kind.name()) {
            bail!(
                "that's {} not {}",
                if app.is_empty() {
                    "something else"
                } else {
                    app
                },
                self.kind.name()
            );
        }
        Ok(About {
            version: status["version"].as_str().unwrap_or_default().to_owned(),
            profiles: self.get("qualityprofile").await?,
            folders: self.get("rootfolder").await?,
        })
    }

    pub async fn shows(&self) -> Result<Vec<Show>> {
        self.get("series").await
    }

    pub async fn episodes(&self, show: u64) -> Result<Vec<Episode>> {
        self.get(&format!("episode?seriesId={show}")).await
    }

    pub async fn episode_files(&self, show: u64) -> Result<Vec<File>> {
        self.get(&format!("episodefile?seriesId={show}")).await
    }

    pub async fn movies(&self) -> Result<Vec<Movie>> {
        self.get("movie").await
    }

    /// What's downloading.
    pub async fn queue(&self) -> Result<Vec<Queued>> {
        let v: Value = self.get("queue?page=1&pageSize=500").await?;
        let owner_key = match self.kind {
            Kind::Sonarr => "seriesId",
            Kind::Radarr => "movieId",
        };
        Ok(v["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some(Queued {
                    owner: r[owner_key].as_u64()?,
                    episode: r["episodeId"].as_u64(),
                    size: r["size"].as_f64().unwrap_or(0.0),
                    left: r["sizeleft"].as_f64().unwrap_or(0.0),
                })
            })
            .collect())
    }

    /// Add a show (by TheTVDB ID) as anime, watching for `season` only.
    /// Its ID in Sonarr.
    pub async fn add_show(
        &self,
        tvdb: u64,
        season: u32,
        account: &ArrAccount,
        search: bool,
    ) -> Result<u64> {
        let found: Vec<Value> = self.get(&format!("series/lookup?term=tvdb:{tvdb}")).await?;
        let Some(mut show) = found.into_iter().next() else {
            bail!("Sonarr couldn't find it on TheTVDB ({tvdb})");
        };
        if let Some(id) = show["id"].as_u64().filter(|id| *id > 0) {
            return Ok(id);
        }
        let (profile, folder) = defaults(account, "Sonarr")?;
        if let Some(seasons) = show["seasons"].as_array_mut() {
            for s in seasons {
                let n = s["seasonNumber"].as_u64().unwrap_or(0);
                s["monitored"] = json!(n == u64::from(season));
            }
        }
        show["qualityProfileId"] = json!(profile);
        show["languageProfileId"] = json!(1);
        show["rootFolderPath"] = json!(folder);
        show["monitored"] = json!(true);
        show["seasonFolder"] = json!(true);
        show["seriesType"] = json!("anime");
        // The seasons' own flags say what's watched for.
        show["addOptions"] = json!({
            "monitor": "skip",
            "searchForMissingEpisodes": search,
            "searchForCutoffUnmetEpisodes": false,
        });
        let added = self.post("series", &show).await?;
        added["id"]
            .as_u64()
            .context("Sonarr didn't say what it added")
    }

    /// Add a film (by TheMovieDB ID). Its ID in Radarr.
    pub async fn add_movie(&self, tmdb: u64, account: &ArrAccount, search: bool) -> Result<u64> {
        let mut movie: Value = self
            .get(&format!("movie/lookup/tmdb?tmdbId={tmdb}"))
            .await?;
        if movie.is_array() {
            movie = movie[0].take();
        }
        if movie.is_null() {
            bail!("Radarr couldn't find it on TheMovieDB ({tmdb})");
        }
        if let Some(id) = movie["id"].as_u64().filter(|id| *id > 0) {
            return Ok(id);
        }
        let (profile, folder) = defaults(account, "Radarr")?;
        movie["qualityProfileId"] = json!(profile);
        movie["rootFolderPath"] = json!(folder);
        movie["monitored"] = json!(true);
        movie["minimumAvailability"] = json!("released");
        movie["addOptions"] = json!({ "monitor": "movieOnly", "searchForMovie": search });
        let added = self.post("movie", &movie).await?;
        added["id"]
            .as_u64()
            .context("Radarr didn't say what it added")
    }

    /// Hand over a release (a .torrent link or a magnet): it decides
    /// whether it's wanted, and if so its download client fetches it.
    pub async fn push(
        &self,
        title: &str,
        download: &str,
        magnet: &str,
        size: u64,
    ) -> Result<Pushed> {
        let mut body = json!({
            "title": title,
            "protocol": "torrent",
            "publishDate": chrono::Utc::now().to_rfc3339(),
            "indexer": "SeaDex",
        });
        if !download.is_empty() {
            body["downloadUrl"] = json!(download);
        }
        if !magnet.is_empty() {
            body["magnetUrl"] = json!(magnet);
        }
        if size > 0 {
            body["size"] = json!(size);
        }
        let v = self.post("release/push", &body).await?;
        let r = if v.is_array() { &v[0] } else { &v };
        Ok(Pushed {
            approved: r["approved"] == true,
            rejections: r["rejections"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().or_else(|| x["reason"].as_str()))
                .map(str::to_owned)
                .collect(),
        })
    }

    /// Search its indexers: some of a show's episodes, or a film.
    pub async fn search(&self, owner: u64, episodes: &[u64]) -> Result<()> {
        let body = match self.kind {
            Kind::Sonarr if episodes.is_empty() => {
                json!({ "name": "SeriesSearch", "seriesId": owner })
            }
            Kind::Sonarr => json!({ "name": "EpisodeSearch", "episodeIds": episodes }),
            Kind::Radarr => json!({ "name": "MoviesSearch", "movieIds": [owner] }),
        };
        self.post("command", &body).await.map(drop)
    }

    /// Watch for (or stop watching for) some episodes.
    pub async fn monitor(&self, episodes: &[u64], on: bool) -> Result<()> {
        self.send(
            self.client
                .put(self.url("episode/monitor"))
                .json(&json!({ "episodeIds": episodes, "monitored": on })),
        )
        .await
        .map(drop)
    }

    /// A series' or film's page in its web interface.
    pub fn page(&self, slug: &str) -> String {
        match self.kind {
            Kind::Sonarr => format!("{}/series/{slug}", self.base),
            Kind::Radarr => format!("{}/movie/{slug}", self.base),
        }
    }
}

fn defaults(account: &ArrAccount, name: &str) -> Result<(u64, String)> {
    match (account.quality_profile, account.root_folder.clone()) {
        (Some(p), Some(f)) => Ok((p, f)),
        _ => bail!("choose {name}'s quality profile and root folder in Settings first"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::mock_http::{Req, mock, reply};

    fn account(base: &str) -> ArrAccount {
        ArrAccount {
            url: base.into(),
            api_key: "key".into(),
            quality_profile: Some(4),
            root_folder: Some("/tv/anime".into()),
        }
    }

    #[test]
    fn checks_and_lists() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let base = rt.block_on(mock(|req| {
            assert_eq!(
                req.headers.get("x-api-key").map(String::as_str),
                Some("key")
            );
            match req.target.as_str() {
                "/api/v3/system/status" => {
                    reply(200, json!({"appName": "Sonarr", "version": "4.0.15"}))
                }
                "/api/v3/qualityprofile" => reply(200, json!([{"id": 4, "name": "HD-1080p"}])),
                "/api/v3/rootfolder" => reply(200, json!([{"path": "/tv/anime", "freeSpace": 5}])),
                "/api/v3/series" => reply(
                    200,
                    json!([{"id": 1, "title": "Frieren", "tvdbId": 424536,
                    "titleSlug": "frieren", "path": "/tv/anime/Frieren", "monitored": true,
                    "seasons": [{"seasonNumber": 1, "monitored": true}]}]),
                ),
                "/api/v3/queue?page=1&pageSize=500" => reply(
                    200,
                    json!({"records": [
                    {"seriesId": 1, "episodeId": 11, "size": 100.0, "sizeleft": 25.0}]}),
                ),
                other => reply(404, json!({"message": format!("no {other}")})),
            }
        }));
        let sonarr = Arr::new(Kind::Sonarr, &account(&base), &reqwest::Client::new());
        let about = rt.block_on(sonarr.about()).unwrap();
        assert_eq!(about.version, "4.0.15");
        assert_eq!(about.profiles[0].name, "HD-1080p");
        let shows = rt.block_on(sonarr.shows()).unwrap();
        assert_eq!(
            (shows[0].tvdb_id, shows[0].seasons[0].season_number),
            (424536, 1)
        );
        let q = rt.block_on(sonarr.queue()).unwrap();
        assert_eq!((q[0].owner, q[0].episode, q[0].left), (1, Some(11), 25.0));
        // Radarr's address given as Sonarr's.
        let radarr = Arr::new(Kind::Radarr, &account(&base), &reqwest::Client::new());
        let e = rt.block_on(radarr.about()).unwrap_err().to_string();
        assert!(e.contains("that's Sonarr not Radarr"), "{e}");
    }

    #[test]
    fn bad_keys_and_wrong_servers_say_so() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let base = rt.block_on(mock(|req| match req.target.as_str() {
            "/api/v3/system/status" if req.headers["x-api-key"] == "key" => {
                crate::mock_http::Resp::html(200, "<html>a router</html>")
            }
            _ => reply(401, json!({})),
        }));
        let client = reqwest::Client::new();
        let mut bad = account(&base);
        bad.api_key = "nope".into();
        let e = rt
            .block_on(Arr::new(Kind::Sonarr, &bad, &client).about())
            .unwrap_err();
        assert!(e.to_string().contains("refused the API key"), "{e}");
        let e = rt
            .block_on(Arr::new(Kind::Sonarr, &account(&base), &client).about())
            .unwrap_err();
        assert!(format!("{e:#}").contains("something else"), "{e:#}");
        let e = rt
            .block_on(Arr::new(Kind::Sonarr, &account("127.0.0.1:9"), &client).about())
            .unwrap_err();
        assert!(e.to_string().contains("couldn't reach Sonarr"), "{e}");
    }

    /// Added as anime, watching for the one season, with the chosen
    /// profile and folder; a release handed over says why it was refused.
    #[test]
    fn adds_anime_and_pushes_releases() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let seen: Arc<Mutex<Vec<Req>>> = Arc::default();
        let log = seen.clone();
        let base = rt.block_on(mock(move |req| {
            log.lock().unwrap().push(req.clone());
            match (req.method.as_str(), req.target.as_str()) {
                ("GET", "/api/v3/series/lookup?term=tvdb:431162") => reply(
                    200,
                    json!([{
                    "title": "The Apothecary Diaries", "tvdbId": 431162,
                    "seasons": [{"seasonNumber": 0, "monitored": false},
                                {"seasonNumber": 1, "monitored": false},
                                {"seasonNumber": 2, "monitored": false}]}]),
                ),
                ("POST", "/api/v3/series") => reply(201, json!({"id": 77})),
                ("GET", "/api/v3/movie/lookup/tmdb?tmdbId=635302") => reply(
                    200,
                    json!({"title": "Mugen Train", "tmdbId": 635302, "id": 5}),
                ),
                ("POST", "/api/v3/release/push") => reply(
                    200,
                    json!([{
                    "approved": false, "rejections": ["Unknown Series"]}]),
                ),
                ("POST", "/api/v3/command") => reply(201, json!({"id": 1})),
                _ => reply(404, json!({})),
            }
        }));
        let sonarr = Arr::new(Kind::Sonarr, &account(&base), &reqwest::Client::new());
        assert_eq!(
            rt.block_on(sonarr.add_show(431162, 2, &account(&base), true))
                .unwrap(),
            77
        );
        let added: Value = serde_json::from_slice(
            &seen
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.method == "POST")
                .unwrap()
                .body,
        )
        .unwrap();
        assert_eq!(added["seriesType"], "anime");
        assert_eq!(added["qualityProfileId"], 4);
        assert_eq!(added["rootFolderPath"], "/tv/anime");
        let monitored: Vec<bool> = added["seasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["monitored"] == true)
            .collect();
        assert_eq!(monitored, [false, false, true], "only season 2");
        assert_eq!(added["addOptions"]["searchForMissingEpisodes"], true);

        let radarr = Arr::new(Kind::Radarr, &account(&base), &reqwest::Client::new());
        assert_eq!(
            rt.block_on(radarr.add_movie(635302, &account(&base), false))
                .unwrap(),
            5,
            "already in Radarr: nothing added"
        );

        let pushed = rt
            .block_on(sonarr.push(
                "[G] Show - S01",
                "https://nyaa.si/download/1.torrent",
                "",
                9,
            ))
            .unwrap();
        assert_eq!(
            pushed,
            Pushed {
                approved: false,
                rejections: vec!["Unknown Series".into()]
            }
        );
        let push = seen
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|r| r.target.ends_with("push"))
            .cloned()
            .unwrap();
        let push: Value = serde_json::from_slice(&push.body).unwrap();
        assert_eq!(push["protocol"], "torrent");
        assert!(push.get("magnetUrl").is_none());
        rt.block_on(sonarr.search(77, &[3, 4])).unwrap();
        let cmd = seen.lock().unwrap().last().cloned().unwrap();
        let cmd: Value = serde_json::from_slice(&cmd.body).unwrap();
        assert_eq!(cmd, json!({"name": "EpisodeSearch", "episodeIds": [3, 4]}));

        let mut unset = account(&base);
        unset.root_folder = None;
        let e = rt
            .block_on(sonarr.add_show(431162, 2, &unset, false))
            .unwrap_err();
        assert!(e.to_string().contains("root folder in Settings"), "{e}");
    }
}
