//! Jellyfin (10.9 or later): signing in, what's there and what's been
//! watched, and streaming an episode with its tracks, chapters, subtitles
//! and fonts.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::JellyfinAccount;

/// Jellyfin counts time in ticks of 100 ns.
const TICKS: f64 = 10_000_000.0;

#[derive(Clone)]
pub struct Jellyfin {
    pub base: String,
    token: Option<String>,
    pub user: Option<String>,
    device_id: String,
    client: reqwest::Client,
}

/// A series or film in Jellyfin, with the IDs it's matched by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub id: String,
    pub name: String,
    /// "Series" or "Movie".
    pub kind: String,
    pub tvdb: Option<u64>,
    pub tmdb: Option<u64>,
    pub anilist: Option<u64>,
    /// The file (films).
    pub path: Option<String>,
    pub watched: Option<Watched>,
}

/// An episode in Jellyfin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Episode {
    pub id: String,
    pub season: Option<u32>,
    pub number: Option<u32>,
    pub path: Option<String>,
    pub watched: Option<Watched>,
}

/// How far it's been watched.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Watched {
    pub played: bool,
    /// Seconds.
    pub position: f64,
    pub duration: f64,
    /// When (Unix time).
    pub at: i64,
}

/// A stream in a file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stream {
    /// Its index among the file's streams (subtitles beside the file come
    /// after).
    pub index: u64,
    /// "Video", "Audio", "Subtitle".
    pub kind: String,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
    pub forced: bool,
    /// A subtitle file beside the video.
    pub external: bool,
    pub channels: Option<u32>,
}

/// What's needed to play an item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playable {
    pub item: String,
    pub source: String,
    pub streams: Vec<Stream>,
    /// Start (seconds) and name.
    pub chapters: Vec<(f64, String)>,
    /// Fonts and other attachments: index and file name.
    pub attachments: Vec<(u64, String)>,
    pub duration: f64,
}

impl Jellyfin {
    pub fn new(account: &JellyfinAccount, client: &reqwest::Client) -> Self {
        Self {
            base: super::base(&account.url),
            token: account.token.clone(),
            user: account.user_id.clone(),
            device_id: account.device_id.clone(),
            client: client.clone(),
        }
    }

    /// Who's asking, as Jellyfin wants it said.
    fn authorization(&self) -> String {
        let mut a = format!(
            "MediaBrowser Client=\"SeaDex\", Device=\"SeaDex\", DeviceId=\"{}\", Version=\"{}\"",
            self.device_id,
            env!("CARGO_PKG_VERSION")
        );
        if let Some(t) = &self.token {
            a += &format!(", Token=\"{t}\"");
        }
        a
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = req
            .header("Authorization", self.authorization())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!(super::describe(&e, "Jellyfin")))?;
        let status = resp.status();
        if status.as_u16() == 401 {
            bail!("Jellyfin refused the sign-in (sign in again in Settings)");
        }
        if !status.is_success() {
            bail!("Jellyfin answered HTTP {status}");
        }
        Ok(resp)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let resp = self
            .send(self.client.get(format!("{}{path}", self.base)))
            .await?;
        resp.json()
            .await
            .context("Jellyfin answered with something else (is the address Jellyfin's?)")
    }

    fn user(&self) -> Result<&str> {
        self.user
            .as_deref()
            .context("sign in to Jellyfin in Settings first")
    }

    /// Sign in: the token, your user ID and the server's name.
    pub async fn sign_in(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(String, String, String)> {
        let resp = self
            .client
            .post(format!("{}/Users/AuthenticateByName", self.base))
            .header("Authorization", self.authorization())
            .json(&json!({ "Username": username, "Pw": password }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!(super::describe(&e, "Jellyfin")))?;
        let status = resp.status();
        if status.as_u16() == 401 {
            bail!("Jellyfin didn't accept that username and password");
        }
        if !status.is_success() {
            bail!("Jellyfin answered HTTP {status} (is the address Jellyfin's?)");
        }
        let v: Value = resp
            .json()
            .await
            .context("Jellyfin answered with something else (is the address Jellyfin's?)")?;
        let token = v["AccessToken"]
            .as_str()
            .context("no token from Jellyfin")?;
        let user = v["User"]["Id"].as_str().context("no user from Jellyfin")?;
        let server = match self.get("/System/Info/Public").await {
            Ok(info) => info["ServerName"].as_str().unwrap_or("Jellyfin").to_owned(),
            Err(_) => "Jellyfin".to_owned(),
        };
        Ok((token.to_owned(), user.to_owned(), server))
    }

    /// Sign out (the token stops working).
    pub async fn sign_out(&self) -> Result<()> {
        self.send(self.client.post(format!("{}/Sessions/Logout", self.base)))
            .await
            .map(drop)
    }

    /// Every series and film, with their IDs elsewhere.
    pub async fn items(&self) -> Result<Vec<Item>> {
        let user = self.user()?;
        let v = self
            .get(&format!(
                "/Items?userId={user}&IncludeItemTypes=Series,Movie&Recursive=true\
                 &Fields=ProviderIds,Path&EnableImages=false"
            ))
            .await?;
        Ok(v["Items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| {
                let id = |k: &str| {
                    i["ProviderIds"]
                        .as_object()?
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(k))?
                        .1
                        .as_str()?
                        .parse()
                        .ok()
                };
                Item {
                    id: i["Id"].as_str().unwrap_or_default().to_owned(),
                    name: i["Name"].as_str().unwrap_or_default().to_owned(),
                    kind: i["Type"].as_str().unwrap_or_default().to_owned(),
                    tvdb: id("Tvdb"),
                    tmdb: id("Tmdb"),
                    anilist: id("AniList"),
                    path: i["Path"].as_str().map(str::to_owned),
                    watched: watched(i),
                }
            })
            .collect())
    }

    /// A series' episodes, and how far you've watched each.
    pub async fn episodes(&self, series: &str) -> Result<Vec<Episode>> {
        let user = self.user()?;
        let v = self
            .get(&format!(
                "/Shows/{series}/Episodes?userId={user}&Fields=Path&EnableImages=false"
            ))
            .await?;
        Ok(v["Items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| Episode {
                id: e["Id"].as_str().unwrap_or_default().to_owned(),
                season: e["ParentIndexNumber"].as_u64().map(|n| n as u32),
                number: e["IndexNumber"].as_u64().map(|n| n as u32),
                path: e["Path"].as_str().map(str::to_owned),
                watched: watched(e),
            })
            .collect())
    }

    /// Its file's streams, chapters and attachments.
    pub async fn playable(&self, item: &str) -> Result<Playable> {
        let user = self.user()?;
        let v = self.get(&format!("/Items/{item}?userId={user}")).await?;
        let source = &v["MediaSources"][0];
        let source_id = source["Id"].as_str().unwrap_or(item).to_owned();
        let streams = source["MediaStreams"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| Stream {
                index: s["Index"].as_u64().unwrap_or(0),
                kind: s["Type"].as_str().unwrap_or_default().to_owned(),
                codec: s["Codec"].as_str().unwrap_or_default().to_ascii_lowercase(),
                language: s["Language"].as_str().map(str::to_owned),
                title: s["Title"].as_str().map(str::to_owned),
                default: s["IsDefault"] == true,
                forced: s["IsForced"] == true,
                external: s["IsExternal"] == true,
                channels: s["Channels"].as_u64().map(|c| c as u32),
            })
            .collect();
        let chapters = v["Chapters"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                (
                    c["StartPositionTicks"].as_f64().unwrap_or(0.0) / TICKS,
                    c["Name"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let attachments = source["MediaAttachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                Some((
                    a["Index"].as_u64()?,
                    a["FileName"].as_str().unwrap_or_default().to_owned(),
                ))
            })
            .collect();
        Ok(Playable {
            item: item.to_owned(),
            source: source_id,
            streams,
            chapters,
            attachments,
            duration: v["RunTimeTicks"].as_f64().unwrap_or(0.0) / TICKS,
        })
    }

    /// Where the file streams from, as it is (seekable).
    pub fn stream_url(&self, p: &Playable) -> String {
        let mut url = format!(
            "{}/Videos/{}/stream?static=true&mediaSourceId={}",
            self.base, p.item, p.source
        );
        // GStreamer can't send headers with the request; Jellyfin's own
        // players put the token in the address the same way.
        if let Some(t) = &self.token {
            url += &format!("&api_key={t}");
        }
        url
    }

    /// A subtitle stream as a whole file (ASS ones as ASS, others as SRT).
    pub async fn subtitle(&self, p: &Playable, s: &Stream) -> Result<Vec<u8>> {
        let format = if matches!(s.codec.as_str(), "ass" | "ssa") {
            "ass"
        } else {
            "srt"
        };
        let url = format!(
            "{}/Videos/{}/{}/Subtitles/{}/Stream.{format}",
            self.base, p.item, p.source, s.index
        );
        let resp = self.send(self.client.get(url)).await?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// An attachment (a font).
    pub async fn attachment(&self, p: &Playable, index: u64) -> Result<Vec<u8>> {
        let url = format!(
            "{}/Videos/{}/{}/Attachments/{index}",
            self.base, p.item, p.source
        );
        let resp = self.send(self.client.get(url)).await?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// Say how far it's been watched (and whether it's done).
    pub async fn set_watched(&self, item: &str, position: f64, played: bool) -> Result<()> {
        let user = self.user()?;
        self.send(
            self.client
                .post(format!(
                    "{}/UserItems/{item}/UserData?userId={user}",
                    self.base
                ))
                .json(&json!({
                    "PlaybackPositionTicks": if played { 0 } else { (position * TICKS) as i64 },
                    "Played": played,
                    "LastPlayedDate": chrono::Utc::now().to_rfc3339(),
                })),
        )
        .await
        .map(drop)
    }

    /// An item's page in Jellyfin's web interface.
    pub fn page(&self, item: &str) -> String {
        format!("{}/web/#/details?id={item}", self.base)
    }
}

fn watched(i: &Value) -> Option<Watched> {
    let u = i.get("UserData")?;
    let at = u["LastPlayedDate"]
        .as_str()
        .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        .map_or(0, |d| d.timestamp());
    Some(Watched {
        played: u["Played"] == true,
        position: u["PlaybackPositionTicks"].as_f64().unwrap_or(0.0) / TICKS,
        duration: i["RunTimeTicks"].as_f64().unwrap_or(0.0) / TICKS,
        at,
    })
}

/// A subtitle file Jellyfin sent, as the player's subtitles: an ASS file's
/// header and events, or SRT's cues as plain text events.
pub fn subtitle_track(codec: &str, bytes: &[u8]) -> crate::mkv::SubTrack {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let text = text.trim_start_matches('\u{feff}');
    if matches!(codec, "ass" | "ssa") {
        ass_track(text)
    } else {
        srt_track(text)
    }
}

fn ass_time(s: &str) -> Option<i64> {
    let mut parts = s.trim().split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let (sec, cs) = parts.next()?.split_once('.')?;
    let sec: i64 = sec.parse().ok()?;
    let cs: i64 = cs.parse().ok()?;
    Some(((h * 60 + m) * 60 + sec) * 1000 + cs * 10)
}

/// Matroska keeps ASS events as "ReadOrder, Layer, Style, Name, MarginL,
/// MarginR, MarginV, Effect, Text", timed by the block; the file's are
/// "Dialogue: Layer, Start, End, Style, …".
fn ass_track(text: &str) -> crate::mkv::SubTrack {
    let mut header = String::new();
    let mut events = Vec::new();
    let mut in_events = false;
    let mut format: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_events = t.eq_ignore_ascii_case("[events]");
        }
        if !in_events {
            header.push_str(line);
            header.push('\n');
            continue;
        }
        if let Some(f) = t.strip_prefix("Format:") {
            format = f
                .split(',')
                .map(|x| x.trim().to_ascii_lowercase())
                .collect();
            header.push_str(line);
            header.push('\n');
            continue;
        }
        if t.starts_with('[') {
            header.push_str(line);
            header.push('\n');
            continue;
        }
        let Some(rest) = t.strip_prefix("Dialogue:") else {
            continue;
        };
        if format.is_empty() {
            format = [
                "layer", "start", "end", "style", "name", "marginl", "marginr", "marginv",
                "effect", "text",
            ]
            .map(str::to_owned)
            .to_vec();
        }
        let fields: Vec<&str> = rest.trim_start().splitn(format.len(), ',').collect();
        if fields.len() < format.len() {
            continue;
        }
        let field: HashMap<&str, &str> = format
            .iter()
            .map(String::as_str)
            .zip(fields.iter().copied())
            .collect();
        let (Some(start), Some(end)) = (
            field.get("start").and_then(|s| ass_time(s)),
            field.get("end").and_then(|s| ass_time(s)),
        ) else {
            continue;
        };
        let get = |k: &str| field.get(k).copied().unwrap_or("").trim();
        let block = format!(
            "{},{},{},{},{},{},{},{},{}",
            events.len(),
            get("layer"),
            get("style"),
            get("name"),
            get("marginl"),
            get("marginr"),
            get("marginv"),
            get("effect"),
            field.get("text").copied().unwrap_or(""),
        );
        events.push(crate::mkv::SubEvent {
            start_ms: start,
            duration_ms: (end - start).max(0),
            data: block.into_bytes(),
        });
    }
    crate::mkv::SubTrack {
        codec_id: "S_TEXT/ASS".into(),
        codec_private: header.into_bytes(),
        events,
    }
}

fn srt_time(s: &str) -> Option<i64> {
    let (hms, ms) = s.trim().split_once([',', '.'])?;
    let mut p = hms.split(':');
    let h: i64 = p.next()?.trim().parse().ok()?;
    let m: i64 = p.next()?.parse().ok()?;
    let sec: i64 = p.next()?.parse().ok()?;
    let ms: i64 = ms.trim().get(..3).unwrap_or(ms.trim()).parse().ok()?;
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

fn srt_track(text: &str) -> crate::mkv::SubTrack {
    let mut events = Vec::new();
    for cue in text.split("\n\n") {
        let mut lines = cue.lines().skip_while(|l| !l.contains("-->"));
        let Some(timing) = lines.next() else { continue };
        let Some((a, b)) = timing.split_once("-->") else {
            continue;
        };
        let b = b.split_whitespace().next().unwrap_or("");
        let (Some(start), Some(end)) = (srt_time(a), srt_time(b)) else {
            continue;
        };
        let body = lines.collect::<Vec<_>>().join("\n");
        if body.trim().is_empty() {
            continue;
        }
        events.push(crate::mkv::SubEvent {
            start_ms: start,
            duration_ms: (end - start).max(0),
            data: body.into_bytes(),
        });
    }
    crate::mkv::SubTrack {
        codec_id: "S_TEXT/UTF8".into(),
        codec_private: Vec::new(),
        events,
    }
}

/// Matroska's name for a codec as Jellyfin names it (subtitles matter:
/// text ones are drawn by libass, images by GStreamer).
fn codec_id(kind: &str, codec: &str) -> String {
    match (kind, codec) {
        ("Subtitle", "ass") => "S_TEXT/ASS".into(),
        ("Subtitle", "ssa") => "S_TEXT/SSA".into(),
        ("Subtitle", "subrip" | "srt" | "mov_text" | "text") => "S_TEXT/UTF8".into(),
        ("Subtitle", "webvtt" | "vtt") => "S_TEXT/WEBVTT".into(),
        ("Subtitle", "pgssub" | "hdmv_pgs_subtitle" | "pgs") => "S_HDMV/PGS".into(),
        ("Subtitle", "dvdsub" | "dvd_subtitle" | "vobsub") => "S_VOBSUB".into(),
        ("Subtitle", other) => format!("S_{}", other.to_ascii_uppercase()),
        ("Audio", c) => format!("A_{}", c.to_ascii_uppercase()),
        (_, c) => format!("V_{}", c.to_ascii_uppercase()),
    }
}

/// The file's tracks and chapters, as the player knows a file's: each
/// track numbered by its stream index. Subtitle files beside the video
/// come last (GStreamer doesn't see them, so only text ones, which are
/// drawn here).
pub fn track_info(p: &Playable) -> crate::mkv::MkvInfo {
    use crate::mkv::{TrackInfo, TrackKind};
    let mut streams: Vec<&Stream> = p.streams.iter().collect();
    streams.sort_by_key(|s| (s.external, s.index));
    let tracks = streams
        .into_iter()
        .filter_map(|s| {
            let kind = match s.kind.as_str() {
                "Video" => TrackKind::Video,
                "Audio" => TrackKind::Audio,
                "Subtitle" => TrackKind::Subtitles,
                _ => return None,
            };
            let codec_id = codec_id(&s.kind, &s.codec);
            if s.external && !codec_id.starts_with("S_TEXT/") {
                return None;
            }
            Some(TrackInfo {
                number: s.index,
                uid: s.index,
                kind,
                codec_id,
                language: s.language.clone().unwrap_or_else(|| "und".into()),
                name: s.title.clone().unwrap_or_default(),
                default: s.default,
                forced: s.forced,
                hearing_impaired: false,
                commentary: false,
                channels: s.channels.map(u64::from),
            })
        })
        .collect();
    crate::mkv::MkvInfo {
        title: None,
        tracks,
        chapters: p.chapters.clone(),
    }
}

impl Jellyfin {
    /// Every text subtitle track (by stream index) and the fonts attached
    /// to the file. What can't be read is left out.
    pub async fn text_subtitles(&self, p: &Playable) -> crate::mkv::TextSubtitles {
        let mut out = crate::mkv::TextSubtitles::default();
        for s in p.streams.iter().filter(|s| s.kind == "Subtitle") {
            if !codec_id(&s.kind, &s.codec).starts_with("S_TEXT/") {
                continue;
            }
            match self.subtitle(p, s).await {
                Ok(bytes) => {
                    out.tracks.insert(s.index, subtitle_track(&s.codec, &bytes));
                }
                Err(e) => tracing::warn!("subtitles {} from Jellyfin: {e:#}", s.index),
            }
        }
        for (index, name) in p.attachments.iter().filter(|(_, n)| is_font(n)) {
            match self.attachment(p, *index).await {
                Ok(bytes) => out.fonts.push((name.clone(), bytes)),
                Err(e) => tracing::warn!("font {name} from Jellyfin: {e:#}"),
            }
        }
        out
    }
}

/// Whether an attachment is a font (by its name).
pub fn is_font(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2"]
        .iter()
        .any(|e| n.ends_with(e))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::mock_http::{Req, mock, reply};

    fn account(base: &str) -> JellyfinAccount {
        JellyfinAccount {
            url: base.into(),
            username: "me".into(),
            token: Some("tok".into()),
            user_id: Some("u1".into()),
            server_name: None,
            device_id: "dev".into(),
        }
    }

    #[test]
    fn signs_in_lists_and_reports() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let seen: Arc<Mutex<Vec<Req>>> = Arc::default();
        let log = seen.clone();
        let base = rt.block_on(mock(move |req| {
            log.lock().unwrap().push(req.clone());
            let auth = req.headers.get("authorization").cloned().unwrap_or_default();
            assert!(auth.contains("DeviceId=\"dev\""), "{auth}");
            let path = req.target.split('?').next().unwrap();
            match (req.method.as_str(), path) {
                ("POST", "/Users/AuthenticateByName") => {
                    let b: Value = serde_json::from_slice(&req.body).unwrap();
                    if b["Pw"] == "right" {
                        reply(200, json!({"AccessToken": "tok", "User": {"Id": "u1"}}))
                    } else {
                        reply(401, json!({}))
                    }
                }
                ("GET", "/System/Info/Public") => reply(200, json!({"ServerName": "nas"})),
                ("GET", "/Items") => {
                    assert!(auth.contains("Token=\"tok\""));
                    assert_eq!(req.query()["userId"], "u1");
                    reply(200, json!({"Items": [
                        {"Id": "s1", "Name": "Frieren", "Type": "Series",
                         "ProviderIds": {"Tvdb": "424536", "Tmdb": "209867"}},
                        {"Id": "m1", "Name": "Mugen Train", "Type": "Movie",
                         "ProviderIds": {"Tmdb": "635302", "AniList": "112151"},
                         "RunTimeTicks": 70_000_000_000_i64,
                         "UserData": {"Played": false, "PlaybackPositionTicks": 6_000_000_000_i64}}]}))
                }
                ("GET", "/Shows/s1/Episodes") => reply(200, json!({"Items": [
                    {"Id": "e1", "ParentIndexNumber": 1, "IndexNumber": 1,
                     "Path": "/media/Frieren/Season 01/Frieren - S01E01.mkv",
                     "RunTimeTicks": 14_400_000_000_i64,
                     "UserData": {"Played": true, "PlaybackPositionTicks": 0,
                                  "LastPlayedDate": "2026-11-20T10:00:00Z"}}]})),
                ("POST", "/UserItems/e2/UserData") => reply(200, json!({})),
                _ => reply(404, json!({})),
            }
        }));
        let client = reqwest::Client::new();
        let mut signed_out = account(&base);
        signed_out.token = None;
        let j = Jellyfin::new(&signed_out, &client);
        let e = rt.block_on(j.sign_in("me", "wrong")).unwrap_err();
        assert!(e.to_string().contains("didn't accept"), "{e}");
        let (token, user, server) = rt.block_on(j.sign_in("me", "right")).unwrap();
        assert_eq!(
            (token.as_str(), user.as_str(), server.as_str()),
            ("tok", "u1", "nas")
        );

        let j = Jellyfin::new(&account(&base), &client);
        let items = rt.block_on(j.items()).unwrap();
        assert_eq!((items[0].tvdb, items[0].tmdb), (Some(424536), Some(209867)));
        assert_eq!(items[1].anilist, Some(112151));
        let w = items[1].watched.unwrap();
        assert_eq!((w.played, w.position, w.duration), (false, 600.0, 7000.0));
        let eps = rt.block_on(j.episodes("s1")).unwrap();
        assert_eq!((eps[0].season, eps[0].number), (Some(1), Some(1)));
        assert!(eps[0].watched.unwrap().played);
        assert!(eps[0].watched.unwrap().at > 0);

        rt.block_on(j.set_watched("e2", 754.5, false)).unwrap();
        let last = seen.lock().unwrap().last().cloned().unwrap();
        let b: Value = serde_json::from_slice(&last.body).unwrap();
        assert_eq!(b["PlaybackPositionTicks"], 7_545_000_000_i64);
        assert_eq!(b["Played"], false);
        assert_eq!(last.query()["userId"], "u1");
    }

    #[test]
    fn reads_what_plays() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let base = rt.block_on(mock(|req| match req.target.split('?').next().unwrap() {
            "/Items/e1" => reply(
                200,
                json!({
                "RunTimeTicks": 14_400_000_000_i64,
                "Chapters": [{"StartPositionTicks": 0, "Name": "Prologue"},
                             {"StartPositionTicks": 900_000_000, "Name": "Opening"}],
                "MediaSources": [{"Id": "src1",
                    "MediaStreams": [
                        {"Index": 0, "Type": "Video", "Codec": "hevc"},
                        {"Index": 1, "Type": "Audio", "Codec": "flac", "Language": "jpn",
                         "IsDefault": true, "Channels": 2},
                        {"Index": 2, "Type": "Subtitle", "Codec": "ASS", "Language": "eng",
                         "Title": "Full", "IsDefault": true}],
                    "MediaAttachments": [{"Index": 3, "FileName": "Font.TTF"},
                                         {"Index": 4, "FileName": "cover.jpg"}]}]}),
            ),
            "/Videos/e1/src1/Subtitles/2/Stream.ass" => crate::mock_http::Resp::html(200, ASS),
            "/Videos/e1/src1/Attachments/3" => {
                crate::mock_http::Resp::binary(200, b"font".to_vec())
            }
            _ => reply(404, json!({})),
        }));
        let j = Jellyfin::new(&account(&base), &reqwest::Client::new());
        let p = rt.block_on(j.playable("e1")).unwrap();
        assert_eq!(p.source, "src1");
        assert_eq!(p.duration, 1440.0);
        assert_eq!(p.chapters[1], (90.0, "Opening".to_owned()));
        assert_eq!(p.streams[2].codec, "ass");
        assert!(p.streams[1].default);
        assert_eq!(
            j.stream_url(&p),
            format!("{base}/Videos/e1/stream?static=true&mediaSourceId=src1&api_key=tok")
        );
        let info = track_info(&p);
        let kinds: Vec<(u64, &str)> = info
            .tracks
            .iter()
            .map(|t| (t.number, t.codec_id.as_str()))
            .collect();
        assert_eq!(kinds, [(0, "V_HEVC"), (1, "A_FLAC"), (2, "S_TEXT/ASS")]);
        assert_eq!(info.tracks[1].language, "jpn");
        assert_eq!(info.chapters.len(), 2);
        let subs = rt.block_on(j.text_subtitles(&p));
        assert_eq!(subs.tracks[&2].events.len(), 2);
        assert_eq!(subs.fonts, [("Font.TTF".to_owned(), b"font".to_vec())]);
        let fonts: Vec<_> = p.attachments.iter().filter(|(_, n)| is_font(n)).collect();
        assert_eq!(fonts, [&(3, "Font.TTF".to_owned())]);
        assert_eq!(rt.block_on(j.attachment(&p, 3)).unwrap(), b"font");
        let bytes = rt.block_on(j.subtitle(&p, &p.streams[2])).unwrap();
        let t = subtitle_track("ass", &bytes);
        assert!(String::from_utf8_lossy(&t.codec_private).contains("[V4+ Styles]"));
        assert!(String::from_utf8_lossy(&t.codec_private).ends_with("Effect, Text\n"));
        assert_eq!(t.events.len(), 2);
        assert_eq!(
            (t.events[0].start_ms, t.events[0].duration_ms),
            (1500, 2250)
        );
        assert_eq!(
            String::from_utf8_lossy(&t.events[0].data),
            "0,0,Default,,0,0,0,,Hello, {\\i1}world{\\i0}"
        );
        assert_eq!(
            String::from_utf8_lossy(&t.events[1].data),
            "1,1,Sign,Board,10,10,20,,Shop"
        );
    }

    const ASS: &str = "\u{feff}[Script Info]\r\nScriptType: v4.00+\r\n\r\n[V4+ Styles]\r\n\
        Format: Name, Fontname\r\nStyle: Default,Arial\r\n\r\n[Events]\r\n\
        Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\r\n\
        Dialogue: 0,0:00:01.50,0:00:03.75,Default,,0,0,0,,Hello, {\\i1}world{\\i0}\r\n\
        Comment: 0,0:00:01.50,0:00:03.75,Default,,0,0,0,,not shown\r\n\
        Dialogue: 1,0:01:00.00,0:01:02.00,Sign,Board,10,10,20,,Shop\r\n";

    #[test]
    fn reads_srt() {
        let t = subtitle_track(
            "subrip",
            b"1\n00:00:01,000 --> 00:00:02,500\nHi\nthere\n\n2\n00:01:00,000 --> 00:01:01,000 X:1\n<i>Bye</i>\n",
        );
        assert_eq!(t.codec_id, "S_TEXT/UTF8");
        assert_eq!(t.events.len(), 2);
        assert_eq!(
            (t.events[0].start_ms, t.events[0].duration_ms),
            (1000, 1500)
        );
        assert_eq!(t.events[0].data, b"Hi\nthere");
        assert_eq!(t.events[1].start_ms, 60_000);
    }
}
