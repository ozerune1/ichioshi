//! HLS streams (an `.m3u8` playlist of segments) downloaded into one file.
//!
//! A master playlist's best variant (the highest resolution, then the most
//! bits) is taken, with its audio if that comes apart. Segments may be
//! AES-128 encrypted; each key is fetched once. MPEG-TS segments in H.264
//! and AAC are remuxed into one MP4 ([`mp4frag`] makes each segment MP4
//! fragments, [`crate::fmp4::Muxer`] joins them with an index for seeking);
//! in other codecs they're kept as they come, one `.ts`. Fragmented MP4
//! segments are joined as DASH ones are. WebVTT subtitles are made into an
//! ASS file beside the video.

pub(crate) mod aes128;
#[cfg(test)]
mod e2e;
pub(crate) mod m3u8;
pub(crate) mod mp4frag;
pub(crate) mod mpegts;
#[cfg(test)]
mod tests;
mod vtt;

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::header::{RANGE, REFERER};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use url::Url;

use crate::fmp4::{Init, Muxer, Track};
use crate::sites::web::Session;
use m3u8::{KeyMethod, Playlist, RenditionKind};

/// Segments downloading at once.
const PARALLEL: usize = 4;
/// Tries per segment.
const TRIES: u32 = 3;
const SEGMENT_TIMEOUT: Duration = Duration::from_secs(90);

/// The download was cancelled (its partial file is removed).
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// What one request for a stream's file needs: the session (its cookies)
/// and the Referer to send.
#[derive(Clone)]
struct Fetcher {
    s: Session,
    referer: Option<String>,
    /// Keys by address, each fetched once.
    keys: Arc<Mutex<HashMap<Url, [u8; 16]>>>,
}

impl Fetcher {
    /// `url` (bytes `range` of it, if given).
    async fn bytes(&self, url: &Url, range: Option<&std::ops::Range<u64>>) -> Result<Vec<u8>> {
        let mut last = None;
        for attempt in 0..TRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(800 * u64::from(attempt))).await;
            }
            let mut req = self.s.client.get(url.as_str()).timeout(SEGMENT_TIMEOUT);
            if let Some(r) = &self.referer {
                req = req.header(REFERER, r);
            }
            if let Some(r) = range {
                ensure!(r.end > r.start, "an empty byte range of {url}");
                req = req.header(RANGE, format!("bytes={}-{}", r.start, r.end - 1));
            }
            let resp = match self.s.send(req).await {
                Ok(r) => r,
                Err(e) if e.is::<crate::sites::web::Challenge>() => return Err(e),
                Err(e) => {
                    last = Some(e);
                    continue;
                }
            };
            let status = resp.status();
            if matches!(status.as_u16(), 404 | 410 | 403) {
                bail!("{url} isn't there ({status})");
            }
            if !status.is_success() {
                last = Some(anyhow!("{url} answered {status}"));
                continue;
            }
            match resp.bytes().await {
                Ok(b) => {
                    let mut b = b.to_vec();
                    // A server that ignores the range sends it all.
                    if let Some(r) = range
                        && status == reqwest::StatusCode::OK
                        && b.len() as u64 >= r.end
                    {
                        b = b[r.start as usize..r.end as usize].to_vec();
                    }
                    return Ok(b);
                }
                Err(e) => last = Some(anyhow::Error::from(e).context(format!("downloading {url}"))),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("couldn't download {url}")))
    }

    async fn text(&self, url: &Url) -> Result<String> {
        let b = self.bytes(url, None).await?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    async fn playlist(&self, url: &Url) -> Result<Playlist> {
        let text = self.text(url).await?;
        m3u8::parse(&text, url).with_context(|| format!("reading the playlist {url}"))
    }

    async fn key(&self, url: &Url) -> Result<[u8; 16]> {
        if let Some(k) = self.keys.lock().unwrap().get(url) {
            return Ok(*k);
        }
        let b = self.bytes(url, None).await?;
        let key: [u8; 16] = b
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("the stream's key is {} bytes, not 16", b.len()))?;
        self.keys.lock().unwrap().insert(url.clone(), key);
        Ok(key)
    }

    /// A segment's bytes, decrypted.
    async fn segment(&self, seg: &m3u8::Segment) -> Result<Vec<u8>> {
        let data = self.bytes(&seg.url, seg.byte_range.as_ref()).await?;
        self.decrypt(seg, data).await
    }

    async fn decrypt(&self, seg: &m3u8::Segment, data: Vec<u8>) -> Result<Vec<u8>> {
        let Some(key) = &seg.key else {
            return Ok(data);
        };
        match &key.method {
            KeyMethod::None => Ok(data),
            KeyMethod::Aes128 => {
                let url = key
                    .url
                    .as_ref()
                    .context("an AES-128 key without an address")?;
                let k = self.key(url).await?;
                let iv = seg.iv().context("an encrypted segment without an IV")?;
                aes128::decrypt(&k, &iv, &data)
            }
            other => bail!("the stream is encrypted as {other:?}, which isn't supported"),
        }
    }
}

/// What's downloaded of a stream: its video's media playlist (with its
/// audio, or that apart), its subtitles', and the variant's resolution.
struct Plan {
    video: m3u8::Media,
    audio: Option<m3u8::Media>,
    subtitles: Option<(Option<String>, m3u8::Media)>,
    resolution: Option<u32>,
}

/// 1280×720 is 720; 1920×800 is 1080.
fn resolution_of((w, h): (u32, u32)) -> u32 {
    crate::ohentai::resolution(w, h)
}

/// The stream at `url`'s best resolution (None for a variant's own
/// playlist, which doesn't say).
pub async fn resolution(s: &Session, url: &Url, referer: Option<&str>) -> Result<Option<u32>> {
    let f = fetcher(s, referer);
    match f.playlist(url).await? {
        Playlist::Media(_) => Ok(None),
        Playlist::Master(m) => Ok(m
            .best_variant(None)
            .context("the master playlist has no variants")?
            .resolution
            .map(resolution_of)),
    }
}

fn fetcher(s: &Session, referer: Option<&str>) -> Fetcher {
    Fetcher {
        s: s.clone(),
        referer: referer.map(str::to_owned),
        keys: Arc::default(),
    }
}

async fn plan(f: &Fetcher, url: &Url) -> Result<Plan> {
    let (video, audio, subtitles, resolution) = match f.playlist(url).await? {
        Playlist::Media(m) => (m, None, None, None),
        Playlist::Master(master) => {
            let v = master
                .best_variant(None)
                .context("the master playlist has no variants")?
                .clone();
            let pick = |kind: RenditionKind, group: &Option<String>| {
                let group = group.as_ref()?;
                let mut found: Vec<&m3u8::Rendition> = master
                    .renditions
                    .iter()
                    .filter(|r| r.kind == kind && &r.group_id == group && r.url.is_some())
                    .collect();
                found.sort_by_key(|r| {
                    let english = r
                        .language
                        .as_deref()
                        .is_some_and(|l| l.to_ascii_lowercase().starts_with("en"));
                    (!english, !r.default)
                });
                found.first().map(|r| (*r).clone())
            };
            let audio = pick(RenditionKind::Audio, &v.audio);
            let subs = pick(RenditionKind::Subtitles, &v.subtitles);
            let video = match f.playlist(&v.url).await? {
                Playlist::Media(m) => m,
                Playlist::Master(_) => bail!("a variant's playlist is a master playlist"),
            };
            let audio = match audio {
                Some(r) => match f.playlist(r.url.as_ref().unwrap()).await? {
                    Playlist::Media(m) => Some(m),
                    Playlist::Master(_) => None,
                },
                None => None,
            };
            let subtitles = match subs {
                Some(r) => match f.playlist(r.url.as_ref().unwrap()).await {
                    Ok(Playlist::Media(m)) => Some((r.language.clone(), m)),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!("{}: no subtitles ({e:#})", f.s.site);
                        None
                    }
                },
                None => None,
            };
            (video, audio, subtitles, v.resolution.map(resolution_of))
        }
    };
    ensure!(
        video.ended,
        "{}'s stream is live (its playlist has no end)",
        f.s.site
    );
    ensure!(!video.segments.is_empty(), "the stream's playlist is empty");
    for seg in video
        .segments
        .iter()
        .chain(audio.iter().flat_map(|a| &a.segments))
    {
        if let Some(k) = &seg.key {
            ensure!(
                matches!(k.method, KeyMethod::None | KeyMethod::Aes128),
                "the stream is encrypted as {:?}, which isn't supported",
                k.method
            );
        }
    }
    Ok(Plan {
        video,
        audio,
        subtitles,
        resolution,
    })
}

#[cfg(test)]
/// Whether the stream at `url` can be downloaded here, from its playlists
/// alone (no segment is fetched): a whole (not live) stream, encrypted at
/// most with AES-128, its segments MPEG-TS or fragmented MP4.
pub async fn check(s: &Session, url: &Url, referer: Option<&str>) -> Result<()> {
    let f = fetcher(s, referer);
    let p = plan(&f, url).await?;
    if p.audio.is_some() && p.video.segments[0].map.is_none() {
        bail!("the stream's audio comes apart from its MPEG-TS video, which isn't supported");
    }
    Ok(())
}

/// Segment downloads under way, in the order they're needed; stopped if
/// the download is.
struct Queue(VecDeque<JoinHandle<Result<Vec<u8>>>>);

impl Drop for Queue {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// A playlist's segments, downloaded [`PARALLEL`] at a time and handed out
/// in order.
struct Segments {
    f: Fetcher,
    list: Vec<m3u8::Segment>,
    next: usize,
    queue: Queue,
}

impl Segments {
    fn new(f: &Fetcher, list: &[m3u8::Segment]) -> Self {
        Segments {
            f: f.clone(),
            list: list.to_vec(),
            next: 0,
            queue: Queue(VecDeque::new()),
        }
    }

    /// The next segment's bytes, decrypted (None after the last).
    async fn next(&mut self, cancel: &AtomicBool) -> Result<Option<Vec<u8>>> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        while self.queue.0.len() < PARALLEL && self.next < self.list.len() {
            let (f, seg) = (self.f.clone(), self.list[self.next].clone());
            self.queue
                .0
                .push_back(tokio::spawn(async move { f.segment(&seg).await }));
            self.next += 1;
        }
        let Some(mut task) = self.queue.0.pop_front() else {
            return Ok(None);
        };
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                r = &mut task => return Ok(Some(r.context("a segment download stopped")??)),
                _ = tick.tick() => {
                    if cancel.load(Ordering::Relaxed) {
                        task.abort();
                        return Err(Cancelled.into());
                    }
                }
            }
        }
    }
}

/// Removes a partial file unless it's been kept.
struct Partial {
    path: PathBuf,
    keep: bool,
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn with_suffix(stem: &Path, suffix: &str) -> PathBuf {
    let mut s: OsString = stem.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Bytes written so far, and an estimate of the whole from the seconds of
/// the stream they hold.
struct Progress<F> {
    report: F,
    done: u64,
    seconds: f64,
    total_seconds: f64,
}

impl<F: FnMut(u64, Option<u64>)> Progress<F> {
    fn add(&mut self, bytes: usize, seconds: f64) {
        self.done += bytes as u64;
        self.seconds += seconds;
        let total = (self.seconds > 0.0)
            .then(|| (self.done as f64 / self.seconds * self.total_seconds) as u64)
            .map(|t| t.max(self.done));
        (self.report)(self.done, total);
    }
}

/// Downloads the stream at `url` (a master or media playlist) into
/// `<stem>.mp4` (`<stem>.ts` if it's MPEG-TS in codecs the MP4 isn't made
/// of here), and its subtitles beside it as `<stem>.<language>.ass`.
/// Writes `<file>.part` and renames it when done; removes it on failure or
/// when `cancel` is set. `progress(done, total)` in bytes.
pub async fn download(
    s: &Session,
    url: &Url,
    referer: Option<&str>,
    stem: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<PathBuf> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    let f = fetcher(s, referer);
    let plan = plan(&f, url).await?;
    if let Some(dir) = stem.parent().filter(|d| !d.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut progress = Progress {
        report: progress,
        done: 0,
        seconds: 0.0,
        total_seconds: plan.video.duration(),
    };
    tracing::info!(
        "{}: HLS, {} segments{}{}",
        s.site,
        plan.video.segments.len(),
        plan.resolution.map_or(String::new(), |r| format!(", {r}p")),
        if plan.audio.is_some() {
            ", audio apart"
        } else {
            ""
        },
    );
    let (file, zero) = if plan.video.segments[0].map.is_some() {
        (
            fmp4_stream(&f, &plan, stem, cancel, &mut progress).await?,
            0.0,
        )
    } else {
        ensure!(
            plan.audio.is_none(),
            "the stream's audio comes apart from its MPEG-TS video, which isn't supported"
        );
        match ts_stream(&f, &plan.video, stem, cancel, &mut progress).await {
            Err(e) if e.is::<RemuxFailed>() => {
                tracing::warn!("{}: {e:#}; keeping the MPEG-TS as it is", s.site);
                progress.done = 0;
                progress.seconds = 0.0;
                let segs = Segments::new(&f, &plan.video.segments);
                let durations: Vec<f64> = plan.video.segments.iter().map(|s| s.duration).collect();
                let file =
                    keep_ts(segs, Vec::new(), &durations, stem, cancel, &mut progress).await?;
                (file, 0.0)
            }
            other => other?,
        }
    };
    if let Some((language, subs)) = &plan.subtitles
        && let Err(e) = subtitles(&f, language.as_deref(), subs, zero, stem).await
    {
        tracing::warn!("{}: couldn't get the subtitles: {e:#}", s.site);
    }
    Ok(file)
}

/// Remuxing failed part way (what's in the stream isn't what [`mp4frag`]
/// reads): it's downloaded again as one `.ts`.
#[derive(Debug)]
struct RemuxFailed(anyhow::Error);

impl std::fmt::Display for RemuxFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "couldn't remux the stream into an MP4: {:#}", self.0)
    }
}

impl std::error::Error for RemuxFailed {}

fn remux_failed(e: anyhow::Error) -> anyhow::Error {
    RemuxFailed(e).into()
}

/// MPEG-TS segments, remuxed into one MP4 if [`mp4frag`] can, else kept as
/// one `.ts`. Returns the file and where the video starts on the stream's
/// clock (seconds), for the subtitles.
async fn ts_stream(
    f: &Fetcher,
    video: &m3u8::Media,
    stem: &Path,
    cancel: &AtomicBool,
    progress: &mut Progress<impl FnMut(u64, Option<u64>)>,
) -> Result<(PathBuf, f64)> {
    let mut segs = Segments::new(f, &video.segments);
    let durations: Vec<f64> = video.segments.iter().map(|s| s.duration).collect();
    let mut remux = mp4frag::Remuxer::new();
    // What's read before the tracks are known, kept in case they're kept as TS.
    let mut early: Vec<Vec<u8>> = Vec::new();
    let starts_over = |n: usize| n > 0 && video.segments.get(n).is_some_and(|s| s.discontinuity);
    let tracks = loop {
        let Some(bytes) = segs.next(cancel).await? else {
            match remux.tracks() {
                Some(t) => break t,
                None => bail!("the stream has no video"),
            }
        };
        let pushed = if starts_over(early.len()) {
            remux.discontinuity().and_then(|()| remux.push(&bytes))
        } else {
            remux.push(&bytes)
        };
        early.push(bytes);
        if let Err(e) = pushed {
            tracing::info!("{}: keeping the MPEG-TS as it is ({e:#})", f.s.site);
            return keep_ts(segs, early, &durations, stem, cancel, progress)
                .await
                .map(|file| (file, 0.0));
        }
        if let Some(t) = remux.tracks() {
            break t;
        }
        ensure!(
            early.len() < 8,
            "the stream's first segments don't say what's in it"
        );
    };
    let infos = match tracks {
        Ok(infos) => infos,
        Err(unsupported) => {
            tracing::info!("{}: keeping the MPEG-TS as it is ({unsupported})", f.s.site);
            return keep_ts(segs, early, &durations, stem, cancel, progress)
                .await
                .map(|file| (file, 0.0));
        }
    };
    let total = video.duration();
    let tracks: Vec<Track> = infos
        .iter()
        .map(|i| {
            Ok(Track {
                init: Init::parse(i.init.clone())?,
                duration: (total * f64::from(i.timescale)).ceil() as u64,
            })
        })
        .collect::<Result<_>>()?;
    let (mut muxer, header) = Muxer::new(&tracks)?;
    let (file, part) = (with_suffix(stem, ".mp4"), with_suffix(stem, ".mp4.part"));
    let mut partial = Partial {
        path: part.clone(),
        keep: false,
    };
    let mut out = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("creating {}", part.display()))?;
    out.write_all(&header).await.context("writing the video")?;
    let mut write = async |frags: Vec<(usize, Vec<u8>)>, out: &mut tokio::fs::File| -> Result<()> {
        for (t, frag) in frags {
            let joined = muxer.fragments(t, &frag).map_err(remux_failed)?;
            out.write_all(&joined).await.context("writing the video")?;
        }
        Ok(())
    };
    write(remux.take_fragments().map_err(remux_failed)?, &mut out).await?;
    for (i, bytes) in early.iter().enumerate() {
        progress.add(bytes.len(), durations[i]);
    }
    let mut n = early.len();
    drop(early);
    while let Some(bytes) = segs.next(cancel).await? {
        if starts_over(n) {
            remux.discontinuity().map_err(remux_failed)?;
        }
        remux.push(&bytes).map_err(remux_failed)?;
        write(remux.take_fragments().map_err(remux_failed)?, &mut out).await?;
        progress.add(bytes.len(), durations.get(n).copied().unwrap_or(0.0));
        n += 1;
    }
    let zero = remux.zero_seconds();
    write(remux.finish().map_err(remux_failed)?, &mut out).await?;
    out.write_all(&muxer.finish())
        .await
        .context("writing the video")?;
    out.flush().await.context("writing the video")?;
    out.sync_all().await.context("writing the video")?;
    drop(out);
    tokio::fs::rename(&part, &file)
        .await
        .with_context(|| format!("renaming {} to {}", part.display(), file.display()))?;
    partial.keep = true;
    Ok((file, zero))
}

/// The MPEG-TS segments as they are, one after another (that's a whole
/// TS file too).
async fn keep_ts(
    mut segs: Segments,
    early: Vec<Vec<u8>>,
    durations: &[f64],
    stem: &Path,
    cancel: &AtomicBool,
    progress: &mut Progress<impl FnMut(u64, Option<u64>)>,
) -> Result<PathBuf> {
    let (file, part) = (with_suffix(stem, ".ts"), with_suffix(stem, ".ts.part"));
    let mut partial = Partial {
        path: part.clone(),
        keep: false,
    };
    let mut out = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("creating {}", part.display()))?;
    let mut n = 0;
    for bytes in early {
        out.write_all(&bytes).await.context("writing the video")?;
        progress.add(bytes.len(), durations[n]);
        n += 1;
    }
    while let Some(bytes) = segs.next(cancel).await? {
        out.write_all(&bytes).await.context("writing the video")?;
        progress.add(bytes.len(), durations.get(n).copied().unwrap_or(0.0));
        n += 1;
    }
    out.flush().await.context("writing the video")?;
    out.sync_all().await.context("writing the video")?;
    drop(out);
    tokio::fs::rename(&part, &file)
        .await
        .with_context(|| format!("renaming {} to {}", part.display(), file.display()))?;
    partial.keep = true;
    Ok(file)
}

/// Fragmented MP4 segments (and their audio's, if apart), joined into one
/// MP4 with an index; or, if an init segment has more than one track, put
/// one after another as they come (a fragmented MP4 too).
async fn fmp4_stream(
    f: &Fetcher,
    plan: &Plan,
    stem: &Path,
    cancel: &AtomicBool,
    progress: &mut Progress<impl FnMut(u64, Option<u64>)>,
) -> Result<PathBuf> {
    let lists: Vec<&m3u8::Media> = std::iter::once(&plan.video)
        .chain(plan.audio.as_ref())
        .collect();
    let mut inits = Vec::new();
    for list in &lists {
        let map = list.segments[0]
            .map
            .as_ref()
            .context("an fMP4 stream without its init segment")?;
        inits.push(f.bytes(&map.url, map.byte_range.as_ref()).await?);
    }
    let (file, part) = (with_suffix(stem, ".mp4"), with_suffix(stem, ".mp4.part"));
    let mut partial = Partial {
        path: part.clone(),
        keep: false,
    };
    let mut out = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("creating {}", part.display()))?;
    let parsed: Result<Vec<Init>> = inits.iter().map(|b| Init::parse(b.clone())).collect();
    match parsed {
        Ok(parsed) => {
            let mut tracks = Vec::new();
            for (init, list) in parsed.into_iter().zip(&lists) {
                let duration = (list.duration() * f64::from(init.timescale())).ceil() as u64;
                tracks.push(Track { init, duration });
            }
            let (mut muxer, header) = Muxer::new(&tracks)?;
            out.write_all(&header).await.context("writing the video")?;
            // Both lists' segments in the order they play (video first at a tie).
            let mut order: Vec<(f64, usize, usize)> = Vec::new();
            for (t, list) in lists.iter().enumerate() {
                let mut at = 0.0;
                for (i, seg) in list.segments.iter().enumerate() {
                    order.push((at, t, i));
                    at += seg.duration;
                }
            }
            order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let mut segs: Vec<Segments> = lists
                .iter()
                .map(|l| Segments::new(f, &l.segments))
                .collect();
            for (_, t, i) in order {
                let bytes = segs[t]
                    .next(cancel)
                    .await?
                    .context("a segment went missing")?;
                out.write_all(&muxer.fragments(t, &bytes)?)
                    .await
                    .context("writing the video")?;
                let seconds = if t == 0 {
                    lists[0].segments[i].duration
                } else {
                    0.0
                };
                progress.add(bytes.len(), seconds);
            }
            out.write_all(&muxer.finish())
                .await
                .context("writing the video")?;
        }
        Err(e) => {
            ensure!(
                lists.len() == 1,
                "the stream's init segment can't be joined with its audio's ({e:#})"
            );
            tracing::info!("{}: keeping the fMP4 as it comes ({e:#})", f.s.site);
            out.write_all(&inits[0])
                .await
                .context("writing the video")?;
            let mut segs = Segments::new(f, &plan.video.segments);
            let mut n = 0;
            while let Some(bytes) = segs.next(cancel).await? {
                out.write_all(&bytes).await.context("writing the video")?;
                progress.add(bytes.len(), plan.video.segments[n].duration);
                n += 1;
            }
        }
    }
    out.flush().await.context("writing the video")?;
    out.sync_all().await.context("writing the video")?;
    drop(out);
    tokio::fs::rename(&part, &file)
        .await
        .with_context(|| format!("renaming {} to {}", part.display(), file.display()))?;
    partial.keep = true;
    Ok(file)
}

/// The subtitle playlist's WebVTT, as one ASS file beside the video.
async fn subtitles(
    f: &Fetcher,
    language: Option<&str>,
    list: &m3u8::Media,
    zero: f64,
    stem: &Path,
) -> Result<()> {
    let mut cues = Vec::new();
    for seg in &list.segments {
        let bytes = f.segment(seg).await?;
        cues.extend(vtt::cues(&String::from_utf8_lossy(&bytes), zero));
    }
    if cues.is_empty() {
        return Ok(());
    }
    let lang: String = language
        .unwrap_or("en")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let lang = if lang.is_empty() {
        "en".to_owned()
    } else {
        lang
    };
    let path = with_suffix(stem, &format!(".{lang}.ass"));
    let tmp = with_suffix(&path, ".part");
    tokio::fs::write(&tmp, vtt::ass(&cues)).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}
