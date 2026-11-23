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
    #[serde(default)]
    pub release_group: Option<String>,
    /// The release's own name, when it was grabbed.
    #[serde(default)]
    pub scene_name: Option<String>,
    #[serde(default)]
    pub quality: Value,
    #[serde(default)]
    pub media_info: Value,
}

impl File {
    /// Its release: group, BD or WEB, dual audio, censored or not.
    pub fn release(&self) -> crate::library::EpisodeRelease {
        let file = self.relative_path.rsplit('/').next().unwrap_or_default();
        let mut names: Vec<&str> = Vec::new();
        names.extend(self.scene_name.as_deref());
        names.push(file.rsplit_once('.').map_or(file, |(s, _)| s));
        // Its audio, as read from the file ("jpn/eng").
        let audio: Option<Vec<String>> = self.media_info["audioLanguages"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.split(['/', ','])
                    .map(|l| crate::remux::normalize_language(l.trim()))
                    .collect()
            });
        let mut r = crate::library::EpisodeRelease::from_names(&names, audio.as_deref());
        if let Some(g) = self.release_group.clone().filter(|g| !g.is_empty()) {
            r.group = Some(g);
        }
        let source = self.quality["quality"]["source"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let source = match source.as_str() {
            s if s.starts_with("bluray") => Some("BD"),
            s if s.starts_with("web") => Some("WEB"),
            s if s.starts_with("tv") || s.starts_with("television") => Some("TV"),
            "dvd" => Some("DVD"),
            _ => None,
        };
        if let Some(s) = source {
            r.source = Some(s.to_owned());
        }
        r
    }
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

/// Something in the queue (what Sonarr's and Radarr's Queue tab shows):
/// for which series' episode (Sonarr) or film (Radarr), and how far along.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Queued {
    /// The queue item (to remove it by).
    pub id: u64,
    pub owner: u64,
    pub episode: Option<u64>,
    pub size: f64,
    pub left: f64,
    /// The release's name.
    pub title: String,
    /// The download client's ID for it (to import it by hand).
    pub download_id: Option<String>,
    /// downloading, queued, paused, completed, delay, warning, failed…
    pub status: String,
    /// downloading, importPending, importing, imported, failedPending,
    /// failed…
    pub state: String,
    /// What's wrong, when something is (import problems, failures).
    pub problem: Option<String>,
    /// "00:12:34".
    pub time_left: Option<String>,
    pub client: Option<String>,
    /// torrent or usenet.
    pub protocol: String,
    pub quality: Option<String>,
    /// The series' or film's title there.
    pub show: Option<String>,
    pub season: Option<u32>,
    pub number: Option<u32>,
    pub episode_title: Option<String>,
}

impl Queued {
    pub fn fraction(&self) -> f32 {
        if self.size > 0.0 {
            ((self.size - self.left) / self.size).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }
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
        let (owner_key, what) = match self.kind {
            Kind::Sonarr => ("seriesId", "series"),
            Kind::Radarr => ("movieId", "movie"),
        };
        let v: Value = self
            .get(
                "queue?page=1&pageSize=500&includeSeries=true&includeEpisode=true\
                 &includeMovie=true",
            )
            .await?;
        let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        Ok(v["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                // What's wrong: their own messages say it best ("Not an
                // upgrade for existing episode file(s)", …).
                let messages: Vec<String> = r["statusMessages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|m| {
                        let list: Vec<String> = m["messages"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(text)
                            .collect();
                        if list.is_empty() {
                            text(&m["title"]).into_iter().collect()
                        } else {
                            list
                        }
                    })
                    .collect();
                let ok = r["trackedDownloadStatus"]
                    .as_str()
                    .is_none_or(|s| s == "ok");
                let problem = text(&r["errorMessage"])
                    .or_else(|| (!ok && !messages.is_empty()).then(|| messages.join(" · ")));
                Some(Queued {
                    id: r["id"].as_u64().unwrap_or(0),
                    owner: r[owner_key].as_u64()?,
                    episode: r["episodeId"].as_u64(),
                    size: r["size"].as_f64().unwrap_or(0.0),
                    left: r["sizeleft"].as_f64().unwrap_or(0.0),
                    title: r["title"].as_str().unwrap_or_default().to_owned(),
                    download_id: text(&r["downloadId"]),
                    status: r["status"].as_str().unwrap_or_default().to_lowercase(),
                    state: r["trackedDownloadState"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    problem,
                    time_left: text(&r["timeleft"]),
                    client: text(&r["downloadClient"]),
                    protocol: r["protocol"].as_str().unwrap_or_default().to_owned(),
                    quality: text(&r["quality"]["quality"]["name"]),
                    show: text(&r[what]["title"]),
                    season: r["seasonNumber"]
                        .as_u64()
                        .or_else(|| r["episode"]["seasonNumber"].as_u64())
                        .map(|n| n as u32),
                    number: r["episode"]["episodeNumber"].as_u64().map(|n| n as u32),
                    episode_title: text(&r["episode"]["title"]),
                })
            })
            .collect())
    }

    /// Take several things out of the queue at once (as `remove_queued`).
    pub async fn remove_queued_many(&self, ids: &[u64], blocklist: bool) -> Result<()> {
        self.send(
            self.client
                .delete(self.url(&format!(
                    "queue/bulk?removeFromClient=true&blocklist={blocklist}"
                )))
                .json(&json!({ "ids": ids })),
        )
        .await
        .map(drop)
    }

    /// Take something out of the queue (and the download client); with
    /// `blocklist`, its release isn't grabbed again.
    pub async fn remove_queued(&self, id: u64, blocklist: bool) -> Result<()> {
        self.send(self.client.delete(self.url(&format!(
            "queue/{id}?removeFromClient=true&blocklist={blocklist}"
        ))))
        .await
        .map(drop)
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
            "indexer": "Ichioshi",
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

    // ── Managing a series or film ─────────────────────────────────────

    /// "series" or "movie".
    fn owner_path(&self) -> &'static str {
        match self.kind {
            Kind::Sonarr => "series",
            Kind::Radarr => "movie",
        }
    }

    /// A series or film as it is there (all of it, to change and send
    /// back).
    pub async fn owner(&self, id: u64) -> Result<Value> {
        self.get(&format!("{}/{id}", self.owner_path())).await
    }

    /// Save a changed series or film; with `move_files`, files follow a
    /// new folder.
    pub async fn save_owner(&self, owner: &Value, move_files: bool) -> Result<Value> {
        let id = owner["id"].as_u64().context("no ID")?;
        self.send(
            self.client
                .put(self.url(&format!(
                    "{}/{id}?moveFiles={move_files}",
                    self.owner_path()
                )))
                .json(owner),
        )
        .await
    }

    /// Delete a series or film, maybe with its files; with `exclude`, lists
    /// don't add it back.
    pub async fn delete_owner(&self, id: u64, delete_files: bool, exclude: bool) -> Result<()> {
        let exclusion = match self.kind {
            Kind::Sonarr => "addImportListExclusion",
            Kind::Radarr => "addImportExclusion",
        };
        self.send(self.client.delete(self.url(&format!(
            "{}/{id}?deleteFiles={delete_files}&{exclusion}={exclude}",
            self.owner_path()
        ))))
        .await
        .map(drop)
    }

    /// An episode, as Sonarr has it (its synopsis, air date, …).
    pub async fn episode(&self, id: u64) -> Result<Value> {
        self.get(&format!("episode/{id}")).await
    }

    /// What could be imported by hand: a download's files, or a folder's
    /// (on the server), for a series or film.
    pub async fn import_candidates(&self, from: &ImportFrom, owner: u64) -> Result<Vec<Candidate>> {
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        let mut query = match from {
            ImportFrom::Folder(f) => format!("folder={}", enc(f)),
            ImportFrom::Download(d) => format!("downloadId={}", enc(d)),
        };
        query += &match self.kind {
            Kind::Sonarr => format!("&seriesId={owner}&filterExistingFiles=true"),
            Kind::Radarr => format!("&movieId={owner}&filterExistingFiles=true"),
        };
        let v: Value = self.get(&format!("manualimport?{query}")).await?;
        let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .map(|c| Candidate {
                path: c["path"].as_str().unwrap_or_default().to_owned(),
                name: text(&c["name"])
                    .or_else(|| text(&c["relativePath"]))
                    .unwrap_or_default(),
                size: c["size"].as_u64().unwrap_or(0),
                episodes: c["episodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e["id"].as_u64())
                    .collect(),
                labels: c["episodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        format!(
                            "S{:02}E{:02}",
                            e["seasonNumber"].as_u64().unwrap_or(0),
                            e["episodeNumber"].as_u64().unwrap_or(0)
                        )
                    })
                    .collect(),
                quality: text(&c["quality"]["quality"]["name"]),
                group: text(&c["releaseGroup"]),
                rejections: c["rejections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| text(x).or_else(|| text(&x["reason"])))
                    .collect(),
                raw: c.clone(),
            })
            .collect())
    }

    /// Import files by hand: each candidate as the given episodes (Sonarr)
    /// or as the film; moved, or copied.
    pub async fn import(
        &self,
        owner: u64,
        files: &[(Candidate, Vec<u64>)],
        copy: bool,
    ) -> Result<()> {
        let files: Vec<Value> = files
            .iter()
            .map(|(c, episodes)| {
                let r = &c.raw;
                let mut f = json!({
                    "path": c.path,
                    "quality": r["quality"],
                    "languages": r["languages"],
                    "releaseGroup": r["releaseGroup"],
                    "indexerFlags": r["indexerFlags"].as_i64().unwrap_or(0),
                });
                if let Some(d) = r["downloadId"].as_str() {
                    f["downloadId"] = json!(d);
                }
                match self.kind {
                    Kind::Sonarr => {
                        f["seriesId"] = json!(owner);
                        f["episodeIds"] = json!(episodes);
                        f["releaseType"] = r["releaseType"].clone();
                    }
                    Kind::Radarr => f["movieId"] = json!(owner),
                }
                f
            })
            .collect();
        self.command(json!({
            "name": "ManualImport",
            "files": files,
            "importMode": if copy { "copy" } else { "move" },
        }))
        .await
    }

    /// Its tags.
    pub async fn tags(&self) -> Result<Vec<Tag>> {
        self.get("tag").await
    }

    /// Make a tag.
    pub async fn add_tag(&self, label: &str) -> Result<Tag> {
        let v = self.post("tag", &json!({ "label": label })).await?;
        serde_json::from_value(v).context("no tag in the answer")
    }

    /// Run a command ("RefreshSeries", "RenameFiles", …).
    pub async fn command(&self, body: Value) -> Result<()> {
        self.post("command", &body).await.map(drop)
    }

    /// What its indexers have for an episode, a season or a film.
    pub async fn releases(&self, scope: Scope) -> Result<Vec<Release>> {
        let query = match scope {
            Scope::Episode(id) => format!("episodeId={id}"),
            Scope::Season { series, season } => {
                format!("seriesId={series}&seasonNumber={season}")
            }
            Scope::Movie(id) => format!("movieId={id}"),
        };
        let v: Value = self.get(&format!("release?{query}")).await?;
        let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let mut out: Vec<Release> = v
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| Release {
                guid: r["guid"].as_str().unwrap_or_default().to_owned(),
                indexer_id: r["indexerId"].as_u64().unwrap_or(0),
                title: r["title"].as_str().unwrap_or_default().to_owned(),
                indexer: text(&r["indexer"]),
                size: r["size"].as_u64().unwrap_or(0),
                age_hours: r["ageHours"].as_f64(),
                seeders: r["seeders"].as_u64(),
                leechers: r["leechers"].as_u64(),
                protocol: r["protocol"].as_str().unwrap_or_default().to_owned(),
                quality: text(&r["quality"]["quality"]["name"]),
                languages: r["languages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| text(&l["name"]))
                    .collect(),
                score: r["customFormatScore"].as_i64(),
                approved: r["approved"] == true,
                rejections: r["rejections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| text(x).or_else(|| text(&x["reason"])))
                    .collect(),
            })
            .collect();
        // As Sonarr sorts them: what it would take first.
        out.sort_by(|a, b| {
            b.approved
                .cmp(&a.approved)
                .then(b.score.unwrap_or(0).cmp(&a.score.unwrap_or(0)))
                .then(b.seeders.unwrap_or(0).cmp(&a.seeders.unwrap_or(0)))
        });
        Ok(out)
    }

    /// Download a release its indexers listed (it's sent to the download
    /// client, rejected or not).
    pub async fn grab(&self, r: &Release) -> Result<()> {
        self.post(
            "release",
            &json!({ "guid": r.guid, "indexerId": r.indexer_id }),
        )
        .await
        .map(drop)
    }

    /// A series' (or film's) files, with what's in them.
    pub async fn files(&self, owner: u64) -> Result<Vec<FileInfo>> {
        let path = match self.kind {
            Kind::Sonarr => format!("episodefile?seriesId={owner}"),
            Kind::Radarr => format!("moviefile?movieId={owner}"),
        };
        let v: Value = self.get(&path).await?;
        let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                let m = &f["mediaInfo"];
                FileInfo {
                    id: f["id"].as_u64().unwrap_or(0),
                    path: text(&f["relativePath"]).unwrap_or_default(),
                    size: f["size"].as_u64().unwrap_or(0),
                    quality: text(&f["quality"]["quality"]["name"]),
                    group: text(&f["releaseGroup"]),
                    languages: f["languages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|l| text(&l["name"]))
                        .collect(),
                    video: text(&m["videoCodec"]),
                    resolution: text(&m["resolution"]),
                    audio: text(&m["audioCodec"]),
                    audio_languages: text(&m["audioLanguages"]),
                    subtitles: text(&m["subtitles"]),
                }
            })
            .collect())
    }

    /// Delete files (from disk).
    pub async fn delete_files(&self, ids: &[u64]) -> Result<()> {
        let (path, key) = match self.kind {
            Kind::Sonarr => ("episodefile/bulk", "episodeFileIds"),
            Kind::Radarr => ("moviefile/bulk", "movieFileIds"),
        };
        self.send(
            self.client
                .delete(self.url(path))
                .json(&json!({ key: ids })),
        )
        .await
        .map(drop)
    }

    /// How files would be renamed (by its naming settings).
    pub async fn rename_preview(&self, owner: u64, season: Option<u32>) -> Result<Vec<Rename>> {
        let mut path = match self.kind {
            Kind::Sonarr => format!("rename?seriesId={owner}"),
            Kind::Radarr => format!("rename?movieId={owner}"),
        };
        if let Some(s) = season {
            path += &format!("&seasonNumber={s}");
        }
        let v: Value = self.get(&path).await?;
        let file_key = match self.kind {
            Kind::Sonarr => "episodeFileId",
            Kind::Radarr => "movieFileId",
        };
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .map(|r| Rename {
                file: r[file_key].as_u64().unwrap_or(0),
                from: r["existingPath"].as_str().unwrap_or_default().to_owned(),
                to: r["newPath"].as_str().unwrap_or_default().to_owned(),
            })
            .collect())
    }

    /// Rename files as previewed.
    pub async fn rename(&self, owner: u64, files: &[u64]) -> Result<()> {
        let body = match self.kind {
            Kind::Sonarr => json!({ "name": "RenameFiles", "seriesId": owner, "files": files }),
            Kind::Radarr => json!({ "name": "RenameFiles", "movieId": owner, "files": files }),
        };
        self.command(body).await
    }

    /// What happened to a series (one season of it) or film: grabs,
    /// imports, failures, deletions.
    pub async fn history(&self, owner: u64, season: Option<u32>) -> Result<Vec<Event>> {
        let mut path = match self.kind {
            Kind::Sonarr => format!("history/series?seriesId={owner}&includeEpisode=true"),
            Kind::Radarr => format!("history/movie?movieId={owner}"),
        };
        if let Some(s) = season {
            path += &format!("&seasonNumber={s}");
        }
        let v: Value = self.get(&path).await?;
        let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let mut out: Vec<Event> = v
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| Event {
                kind: e["eventType"].as_str().unwrap_or_default().to_owned(),
                title: e["sourceTitle"].as_str().unwrap_or_default().to_owned(),
                at: e["date"]
                    .as_str()
                    .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                    .map_or(0, |d| d.timestamp()),
                episode: e["episodeId"].as_u64().filter(|id| *id > 0),
                quality: text(&e["quality"]["quality"]["name"]),
                detail: text(&e["data"]["message"])
                    .or_else(|| text(&e["data"]["reason"]))
                    .or_else(|| text(&e["data"]["indexer"]).map(|i| format!("from {i}"))),
            })
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.at));
        Ok(out)
    }
}

/// Where to import files by hand from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportFrom {
    /// A folder on the server.
    Folder(String),
    /// What the download client downloaded (by its ID).
    Download(String),
}

/// A file that could be imported by hand, and what Sonarr made of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Candidate {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// The episodes Sonarr takes it for (their IDs), and as "S01E05".
    pub episodes: Vec<u64>,
    pub labels: Vec<String>,
    pub quality: Option<String>,
    pub group: Option<String>,
    /// Why Sonarr wouldn't import it itself.
    pub rejections: Vec<String>,
    /// All of it, as Sonarr sent it (sent back to import it).
    pub raw: Value,
}

/// A tag (Sonarr's and Radarr's own, for sorting series and choosing
/// indexers, download clients and the like for them).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Tag {
    pub id: u64,
    pub label: String,
}

/// What to look for releases for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Episode(u64),
    Season { series: u64, season: u32 },
    Movie(u64),
}

/// A release an indexer has (Sonarr's interactive search).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    pub guid: String,
    pub indexer_id: u64,
    pub title: String,
    pub indexer: Option<String>,
    pub size: u64,
    pub age_hours: Option<f64>,
    pub seeders: Option<u64>,
    pub leechers: Option<u64>,
    pub protocol: String,
    pub quality: Option<String>,
    pub languages: Vec<String>,
    pub score: Option<i64>,
    pub approved: bool,
    /// Why it wouldn't take it itself.
    pub rejections: Vec<String>,
}

/// A file, and what's in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileInfo {
    pub id: u64,
    pub path: String,
    pub size: u64,
    pub quality: Option<String>,
    pub group: Option<String>,
    pub languages: Vec<String>,
    pub video: Option<String>,
    pub resolution: Option<String>,
    pub audio: Option<String>,
    pub audio_languages: Option<String>,
    pub subtitles: Option<String>,
}

/// A file's new name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rename {
    pub file: u64,
    pub from: String,
    pub to: String,
}

/// Something that happened (history).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    /// grabbed, downloadFolderImported, downloadFailed,
    /// episodeFileDeleted, episodeFileRenamed, downloadIgnored…
    pub kind: String,
    pub title: String,
    /// Unix time.
    pub at: i64,
    pub episode: Option<u64>,
    pub quality: Option<String>,
    pub detail: Option<String>,
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
                p if p.starts_with("/api/v3/queue?") => {
                    assert!(p.contains("includeEpisode=true"), "{p}");
                    reply(200, json!({"records": [
                        {"id": 7, "seriesId": 1, "episodeId": 11, "size": 100.0, "sizeleft": 25.0,
                         "title": "[SubsPlease] Frieren - 05 (1080p)", "status": "downloading",
                         "trackedDownloadStatus": "ok", "trackedDownloadState": "downloading",
                         "timeleft": "00:01:30", "downloadClient": "qBittorrent", "protocol": "torrent",
                         "quality": {"quality": {"name": "WEBDL-1080p"}},
                         "series": {"title": "Frieren"},
                         "episode": {"seasonNumber": 1, "episodeNumber": 5, "title": "Phantoms of the Dead"}},
                        {"id": 8, "seriesId": 1, "episodeId": 12, "size": 10.0, "sizeleft": 0.0,
                         "title": "Frieren - 06", "status": "completed",
                         "trackedDownloadStatus": "warning", "trackedDownloadState": "importPending",
                         "statusMessages": [{"title": "Frieren - 06.mkv",
                            "messages": ["Not an upgrade for existing episode file(s)"]}]}]}))
                }
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
        assert_eq!(q[0].fraction(), 0.75);
        assert_eq!((q[0].id, q[0].quality.as_deref()), (7, Some("WEBDL-1080p")));
        assert_eq!((q[0].season, q[0].number), (Some(1), Some(5)));
        assert_eq!(q[0].episode_title.as_deref(), Some("Phantoms of the Dead"));
        assert_eq!(q[0].problem, None);
        assert_eq!(q[1].state, "importPending");
        assert_eq!(
            q[1].problem.as_deref(),
            Some("Not an upgrade for existing episode file(s)")
        );
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

    /// A file's release: its group, BD or WEB, dual audio, uncensored.
    #[test]
    fn reads_files_releases() {
        let f: File = serde_json::from_value(json!({
            "id": 1, "relativePath": "Season 01/Show - S01E01.mkv", "size": 9,
            "releaseGroup": "Group", "sceneName": "[Group] Show - 01 (BD 1080p) [Uncensored]",
            "quality": {"quality": {"name": "Bluray-1080p", "source": "bluray"}},
            "mediaInfo": {"audioLanguages": "jpn/eng"}}))
        .unwrap();
        let r = f.release();
        assert_eq!(r.group.as_deref(), Some("Group"));
        assert_eq!(r.source.as_deref(), Some("BD"));
        assert_eq!((r.dual_audio, r.censored), (Some(true), Some(false)));
        let web: File = serde_json::from_value(json!({"id": 2,
            "quality": {"quality": {"source": "webRip"}}, "mediaInfo": {"audioLanguages": "Japanese"}}))
        .unwrap();
        let r = web.release();
        assert_eq!(
            (r.source.as_deref(), r.dual_audio),
            (Some("WEB"), Some(false))
        );
    }

    /// Managing a series: interactive search and grabbing, files, renames,
    /// history, changing and deleting it.
    #[test]
    fn manages_a_series() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let seen: Arc<Mutex<Vec<Req>>> = Arc::default();
        let log = seen.clone();
        let base = rt.block_on(mock(move |req| {
            log.lock().unwrap().push(req.clone());
            match (req.method.as_str(), req.target.as_str()) {
                ("GET", "/api/v3/release?seriesId=1&seasonNumber=6") => reply(200, json!([
                    {"guid": "g1", "indexerId": 3, "title": "Worse", "indexer": "Nyaa", "size": 10,
                     "ageHours": 5.0, "seeders": 50, "approved": false,
                     "rejections": [{"reason": "Not an upgrade"}], "customFormatScore": 0,
                     "quality": {"quality": {"name": "WEBDL-1080p"}}, "protocol": "torrent"},
                    {"guid": "g2", "indexerId": 3, "title": "Best", "indexer": "Nyaa", "size": 20,
                     "seeders": 5, "approved": true, "customFormatScore": 1500,
                     "languages": [{"name": "Japanese"}], "protocol": "torrent"}])),
                ("POST", "/api/v3/release") => reply(200, json!({})),
                ("GET", "/api/v3/episodefile?seriesId=1") => reply(200, json!([
                    {"id": 9, "relativePath": "Season 06/SBR - S06E01.mkv", "size": 3000,
                     "releaseGroup": "Group", "quality": {"quality": {"name": "Bluray-1080p"}},
                     "languages": [{"name": "Japanese"}],
                     "mediaInfo": {"videoCodec": "x265", "resolution": "1920x1080",
                                   "audioCodec": "FLAC", "audioLanguages": "jpn/eng",
                                   "subtitles": "eng/spa"}}])),
                ("DELETE", "/api/v3/episodefile/bulk") => reply(200, json!({})),
                ("GET", "/api/v3/rename?seriesId=1&seasonNumber=6") => reply(200, json!([
                    {"episodeFileId": 9, "existingPath": "Season 06/old.mkv",
                     "newPath": "Season 06/SBR - S06E01.mkv"}])),
                ("POST", "/api/v3/command") => reply(201, json!({"id": 1})),
                ("GET", "/api/v3/history/series?seriesId=1&includeEpisode=true&seasonNumber=6") => {
                    reply(200, json!([
                        {"eventType": "grabbed", "sourceTitle": "SBR - 01", "date": "2026-11-20T10:00:00Z",
                         "episodeId": 41, "data": {"indexer": "Nyaa"}},
                        {"eventType": "downloadFolderImported", "sourceTitle": "SBR - 01",
                         "date": "2026-11-20T11:00:00Z", "episodeId": 41}]))
                }
                ("GET", "/api/v3/series/1") => reply(200, json!({"id": 1, "monitored": true,
                    "qualityProfileId": 4, "path": "/tv/JoJo"})),
                ("PUT", "/api/v3/series/1?moveFiles=true") => reply(202, json!({"id": 1})),
                ("DELETE", "/api/v3/series/1?deleteFiles=false&addImportListExclusion=true") => {
                    reply(200, json!({}))
                }
                other => reply(404, json!({"message": format!("no {other:?}")})),
            }
        }));
        let sonarr = Arr::new(Kind::Sonarr, &account(&base), &reqwest::Client::new());
        let found = rt
            .block_on(sonarr.releases(Scope::Season {
                series: 1,
                season: 6,
            }))
            .unwrap();
        assert_eq!(found[0].title, "Best", "what Sonarr would take first");
        assert_eq!(found[1].rejections, ["Not an upgrade"]);
        rt.block_on(sonarr.grab(&found[1])).unwrap();
        let last_body = |m: &str| -> Value {
            let r = seen
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|r| r.method == m)
                .cloned()
                .unwrap();
            serde_json::from_slice(&r.body).unwrap_or(Value::Null)
        };
        assert_eq!(last_body("POST"), json!({"guid": "g1", "indexerId": 3}));

        let files = rt.block_on(sonarr.files(1)).unwrap();
        assert_eq!(files[0].quality.as_deref(), Some("Bluray-1080p"));
        assert_eq!(files[0].audio_languages.as_deref(), Some("jpn/eng"));
        rt.block_on(sonarr.delete_files(&[9])).unwrap();
        assert_eq!(last_body("DELETE"), json!({"episodeFileIds": [9]}));

        let renames = rt.block_on(sonarr.rename_preview(1, Some(6))).unwrap();
        assert_eq!(renames[0].file, 9);
        rt.block_on(sonarr.rename(1, &[9])).unwrap();
        assert_eq!(
            last_body("POST"),
            json!({"name": "RenameFiles", "seriesId": 1, "files": [9]})
        );

        let history = rt.block_on(sonarr.history(1, Some(6))).unwrap();
        assert_eq!(history[0].kind, "downloadFolderImported", "newest first");
        assert_eq!(history[1].detail.as_deref(), Some("from Nyaa"));

        let mut series = rt.block_on(sonarr.owner(1)).unwrap();
        series["path"] = json!("/tv2/JoJo");
        rt.block_on(sonarr.save_owner(&series, true)).unwrap();
        assert_eq!(last_body("PUT")["path"], "/tv2/JoJo");
        rt.block_on(sonarr.delete_owner(1, false, true)).unwrap();
    }
}
