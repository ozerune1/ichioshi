//! Streaming episodes from ohentai.org (MP4s).
//!
//! The site lists all its series on one page (`series.php`, about 460 KB for
//! some 4200 series), and each series' page lists its episodes. Its search
//! (`search.php?k=`) matches any one of the words, so we don't use it.
//!
//! An episode page (`detail.php?vid=` and its number in base64) sets up a
//! JW Player with the MP4: a signed URL, good for about an hour, so it's
//! resolved just before it's played. Old episodes may have no video any more.
//! Their subtitles are burnt in (the files are `..._en.mp4`).

use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::Value;

const BASE: &str = "https://ohentai.org";

/// The site's video server wants it as the Referer.
pub const REFERER: &str = "https://ohentai.org/";

/// One episode of a series on ohentai.org.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub series: String,
    pub episode: u32,
    /// The episode's page.
    pub url: String,
    pub title: String,
    /// From the series' tags ("1080p"), which few have.
    pub resolution: Option<u32>,
}

/// An episode's video file.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    /// Signed; good for about an hour.
    pub url: String,
    pub len: Option<u64>,
    /// Send as the Referer when fetching `url`.
    pub referer: String,
    /// The picture's height (its short side, or the width's 16:9 equivalent
    /// for wider pictures: 1920×800 is 1080).
    pub resolution: Option<u32>,
    /// A subtitle file, if the player lists one.
    pub subtitles: Option<String>,
}

/// Where ohentai.org requests go instead (tests).
#[cfg(test)]
pub static OHENTAI_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn base() -> String {
    #[cfg(test)]
    if let Some(base) = OHENTAI_BASE.lock().unwrap().as_ref() {
        return base.clone();
    }
    BASE.to_owned()
}

/// A link on the site (`detail.php?vid=...`) as a full URL.
fn site_url(href: &str) -> Result<String> {
    let base = url::Url::parse(&format!("{}/", base())).context("bad base URL")?;
    Ok(base.join(href).context("bad link")?.to_string())
}

/// Find the show matching any of these names (AniList romaji / English
/// titles; compared normalized) and list its episodes, in order. Empty if
/// the site hasn't got it.
pub async fn episodes(client: &reqwest::Client, names: &[String]) -> Result<Vec<Episode>> {
    let wanted: Vec<String> = names
        .iter()
        .map(|n| normalize(n))
        .filter(|n| !n.is_empty())
        .collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let index = get_page(client, &site_url("series.php")?)
        .await
        .context("could not load ohentai's series list")?;
    let series: Vec<(String, String, String)> = parse_series_index(&index)
        .into_iter()
        .map(|(name, href)| (normalize(&name), name, href))
        .collect();
    let found = wanted
        .iter()
        .find_map(|w| series.iter().find(|(norm, ..)| norm == w))
        .map(|(_, name, href)| (name, href));
    let Some((name, href)) = found else {
        tracing::debug!(?names, "not on ohentai ({} series)", series.len());
        return Ok(Vec::new());
    };
    let page = get_page(client, &site_url(href)?)
        .await
        .with_context(|| format!("could not load ohentai's page for {name}"))?;
    parse_series_page(&page, name)
}

/// Resolve an episode page to its MP4 (the highest resolution if there are
/// several), with its size and picture size read from the file's first few
/// KiB.
pub async fn video(client: &reqwest::Client, episode_url: &str) -> Result<Video> {
    let page = get_page(client, episode_url)
        .await
        .context("could not load the ohentai episode")?;
    let page_url = url::Url::parse(episode_url).context("bad episode URL")?;
    let (source, subtitles) = player_media(&page)?;
    let url = page_url
        .join(&source.file)
        .context("bad video URL")?
        .to_string();
    let subtitles = subtitles
        .and_then(|s| page_url.join(&s).ok())
        .map(|u| u.to_string());
    let (len, dims) = probe(client, &url).await?;
    Ok(Video {
        resolution: dims.map(|(w, h)| resolution(w, h)).or(source.resolution),
        url,
        len,
        referer: REFERER.to_owned(),
        subtitles,
    })
}

async fn get_page(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .header(reqwest::header::REFERER, REFERER)
        .send()
        .await
        .context("could not reach ohentai.org")?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .context("error reading ohentai.org's page")?;
    if !status.is_success() {
        if body.contains("challenge-platform") || body.contains("Just a moment") {
            bail!("ohentai.org answered with Cloudflare's browser check (HTTP {status})");
        }
        bail!("ohentai.org returned HTTP {status}");
    }
    Ok(body)
}

/// Lowercase letters and digits only, macrons and long vowels folded
/// ("Kyōiku", "Kyouiku" and "Kyoiku" are the same), "×" read as "x".
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in decode_entities(s).to_lowercase().chars() {
        let c = match c {
            'ā' | 'â' => 'a',
            'ē' | 'ê' => 'e',
            'ī' | 'î' => 'i',
            'ō' | 'ô' => 'o',
            'ū' | 'û' => 'u',
            '×' => 'x',
            c => c,
        };
        if c.is_alphanumeric() {
            out.push(c);
        }
    }
    out.replace("ou", "o").replace("oo", "o").replace("uu", "u")
}

/// `series.php`: (name, `sery_video.php?seryid=...`) for each series.
fn parse_series_index(html: &str) -> Vec<(String, String)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"<a href='(sery_video\.php\?seryid=[^']+)'\s*>([^<]*)</a>").unwrap()
    });
    RE.captures_iter(html)
        .map(|c| (decode_entities(c[2].trim()), c[1].to_owned()))
        .collect()
}

/// A series' page: its episodes, in order.
fn parse_series_page(html: &str, index_name: &str) -> Result<Vec<Episode>> {
    static H1: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<h1 class='title'>(.*?)</h1>").unwrap());
    let series = H1
        .captures(html)
        .map(|c| decode_entities(c[1].trim()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| index_name.to_owned());
    // The series' own tags (the sidebar has a "1080p" one too).
    let resolution = html.find("<div class='serydetailsection'>").and_then(|i| {
        let rest = &html[i..];
        let start = rest.find("<div class='tagcontainer'>")?;
        let end = rest[start..].find("</div>")? + start;
        tags_resolution(&rest[start..end])
    });
    static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?s)<div class='videotitle'[^>]*><a href='(detail\.php\?vid=[^']+)'[^>]*>([^<]*)</a>",
        )
        .unwrap()
    });
    static IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/Episode-(\d+)/").unwrap());
    let mut episodes = Vec::new();
    for (i, brick) in html.split("<div class='videobrick'>").skip(1).enumerate() {
        // Adverts link elsewhere.
        let Some(c) = TITLE_RE.captures(brick) else {
            continue;
        };
        let title = decode_entities(c[2].trim());
        let episode = episode_number(&title)
            .or_else(|| IMAGE_RE.captures(brick).and_then(|c| c[1].parse().ok()))
            .unwrap_or(i as u32 + 1);
        episodes.push(Episode {
            series: series.clone(),
            episode,
            url: site_url(&c[1])?,
            title,
            resolution,
        });
    }
    episodes.sort_by_key(|e| e.episode);
    Ok(episodes)
}

/// "... - Episode 3" -> 3.
fn episode_number(title: &str) -> Option<u32> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bEp(?:isode)?\.?\s*(\d+)\s*$").unwrap());
    RE.captures(title)?[1].parse().ok()
}

/// "1080p" (or "4K") among a series' tags.
fn tags_resolution(tags: &str) -> Option<u32> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)tag=(?:(\d{3,4})p|(4k))'").unwrap());
    RE.captures_iter(tags)
        .filter_map(|c| match c.get(1) {
            Some(n) => n.as_str().parse().ok(),
            None => Some(2160),
        })
        .max()
}

/// A video file the player lists.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Source {
    pub file: String,
    pub resolution: Option<u32>,
    pub mp4: bool,
}

/// The video (highest resolution, MP4s first) and subtitle file the page's
/// player is set up with.
fn player_media(html: &str) -> Result<(Source, Option<String>)> {
    let mut configs = 0;
    for config in setup_objects(html) {
        configs += 1;
        let json: Value = serde_json::from_str(&js_to_json(config))
            .context("could not read ohentai's player setup")?;
        let (sources, subtitles) = media(&json);
        let best = sources.into_iter().max_by_key(|s| (s.mp4, s.resolution));
        if let Some(best) = best {
            return Ok((best, subtitles));
        }
    }
    if configs == 0 && !html.contains("<h1 class='title'>") {
        bail!("this isn't an ohentai episode page");
    }
    bail!("ohentai has no video for this episode");
}

/// The `{...}` passed to each `SendPlay.setup(...)` (a JavaScript object).
fn setup_objects(html: &str) -> Vec<&str> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"SendPlay\s*\.\s*setup\s*\(\s*\{").unwrap());
    RE.find_iter(html)
        .filter_map(|m| {
            let start = m.end() - 1;
            let bytes = html.as_bytes();
            let (mut depth, mut quote, mut i) = (0usize, None::<u8>, start);
            while i < bytes.len() {
                let c = bytes[i];
                match quote {
                    Some(_) if c == b'\\' => i += 1,
                    Some(q) if c == q => quote = None,
                    Some(_) => {}
                    None => match c {
                        b'"' | b'\'' => quote = Some(c),
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(&html[start..=i]);
                            }
                        }
                        _ => {}
                    },
                }
                i += 1;
            }
            None
        })
        .collect()
}

/// A JavaScript object literal as JSON: keys quoted, single-quoted strings
/// double-quoted, comments and trailing commas dropped, other names
/// (variables) null.
pub(crate) fn js_to_json(js: &str) -> String {
    let b: Vec<char> = js.chars().collect();
    let mut out = String::with_capacity(js.len() + 64);
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            '"' | '\'' => {
                out.push('"');
                i += 1;
                while i < b.len() && b[i] != c {
                    match b[i] {
                        '\\' if i + 1 < b.len() => {
                            match b[i + 1] {
                                '\'' => out.push('\''),
                                '\n' => {}
                                'x' if i + 3 < b.len() => {
                                    out.push_str("\\u00");
                                    out.push(b[i + 2]);
                                    out.push(b[i + 3]);
                                    i += 2;
                                }
                                e => {
                                    out.push('\\');
                                    out.push(e);
                                }
                            }
                            i += 2;
                            continue;
                        }
                        '"' => out.push_str("\\\""),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        ch => out.push(ch),
                    }
                    i += 1;
                }
                out.push('"');
                i += 1;
            }
            '/' if b.get(i + 1) == Some(&'/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if b.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ',' => {
                let next = b[i + 1..].iter().find(|c| !c.is_whitespace());
                if !matches!(next, Some('}' | ']')) {
                    out.push(',');
                }
                i += 1;
            }
            c if c.is_ascii_digit() => {
                while i < b.len() && (ident(b[i]) || b[i] == '.') {
                    out.push(b[i]);
                    i += 1;
                }
            }
            c if ident(c) => {
                let start = i;
                while i < b.len() && (ident(b[i]) || b[i] == '.') {
                    i += 1;
                }
                let word: String = b[start..i].iter().collect();
                let next = b[i..].iter().find(|c| !c.is_whitespace());
                if next == Some(&':') {
                    out.push('"');
                    out.push_str(&word);
                    out.push('"');
                } else if matches!(word.as_str(), "true" | "false" | "null") {
                    out.push_str(&word);
                } else {
                    out.push_str("null");
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// A JW Player setup's video files and its first subtitle file.
pub(crate) fn media(config: &Value) -> (Vec<Source>, Option<String>) {
    let item = match config.get("playlist").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => &list[0],
        _ => config,
    };
    let mut sources = Vec::new();
    let mut add = |src: &Value| {
        let Some(file) = src.get("file").and_then(Value::as_str).map(str::trim) else {
            return;
        };
        if file.is_empty() {
            return;
        }
        let text = |k: &str| src.get(k).and_then(Value::as_str).unwrap_or_default();
        let number = |k: &str| {
            src.get(k).and_then(|v| {
                v.as_u64().map(|n| n as u32).or_else(|| {
                    v.as_str()
                        .and_then(|s| s.trim_end_matches(['p', 'P']).parse().ok())
                })
            })
        };
        let resolution = label_resolution(text("label"))
            .or_else(|| number("height"))
            .or_else(|| number("res"))
            .or_else(|| label_resolution(file.rsplit('/').next().unwrap_or_default()));
        let kind = text("type").to_ascii_lowercase();
        let path = file
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        sources.push(Source {
            file: file.to_owned(),
            resolution,
            mp4: kind.contains("mp4") || path.ends_with(".mp4") || path.ends_with(".m4v"),
        });
    };
    if let Some(list) = item.get("sources").and_then(Value::as_array) {
        list.iter().for_each(&mut add);
    }
    add(item);
    let tracks = item
        .get("tracks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let subtitle = |t: &&Value| {
        let file = t.get("file").and_then(Value::as_str).unwrap_or_default();
        let kind = t.get("kind").and_then(Value::as_str).unwrap_or("captions");
        !file.is_empty() && matches!(kind, "captions" | "subtitles")
    };
    let subtitles = tracks
        .iter()
        .filter(subtitle)
        .max_by_key(|t| t.get("default").and_then(Value::as_bool).unwrap_or(false))
        .and_then(|t| t.get("file").and_then(Value::as_str))
        .map(str::to_owned);
    (sources, subtitles)
}

/// "720p", "1080P HD", "4K", "x_1080p.mp4" -> the resolution.
pub(crate) fn label_resolution(label: &str) -> Option<u32> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:^|[^0-9a-z])(?:(\d{3,4})p|(4k|2160))(?:$|[^0-9a-z])").unwrap()
    });
    let c = RE.captures(label)?;
    match c.get(1) {
        Some(n) => n.as_str().parse().ok(),
        None => Some(2160),
    }
}

/// 1280×720 -> 720; 1920×800 -> 1080; 1080×1920 -> 1080.
pub(crate) fn resolution(w: u32, h: u32) -> u32 {
    let (long, short) = (w.max(h), w.min(h));
    short.max(long * 9 / 16)
}

/// The first request reads this much of the MP4; later ones (to reach a
/// box further on) this much.
const FIRST_READ: usize = 4096;
const LATER_READ: usize = 512;
const MAX_READS: usize = 6;

/// An MP4's size (from its first bytes' Content-Range) and the picture size
/// of its video track (its `tkhd`), read with a few small Range requests.
pub(crate) async fn probe(
    client: &reqwest::Client,
    url: &str,
) -> Result<(Option<u64>, Option<(u32, u32)>)> {
    probe_with(client, url, Some(REFERER)).await
}

/// [`probe`], sending `referer` (if any) as the Referer.
pub(crate) async fn probe_with(
    client: &reqwest::Client,
    url: &str,
    referer: Option<&str>,
) -> Result<(Option<u64>, Option<(u32, u32)>)> {
    let (total, first) = read_range(client, url, referer, 0, FIRST_READ).await?;
    let Some(mut buf) = first else {
        return Ok((total, None));
    };
    let (mut off, mut walk, mut reads) = (0u64, Walk::Top(0), 1);
    loop {
        match step(walk, off, &buf) {
            Step::Next(w) => walk = w,
            Step::Dims(w, h) => return Ok((total, Some((w, h)))),
            Step::Done => return Ok((total, None)),
            Step::Need(at) => {
                let past_end = total.is_some_and(|t| at >= t);
                if reads == MAX_READS || past_end || at == off {
                    return Ok((total, None));
                }
                reads += 1;
                let (_, data) = read_range(client, url, referer, at, LATER_READ).await?;
                let Some(data) = data else {
                    return Ok((total, None));
                };
                (off, buf) = (at, data);
            }
        }
    }
}

/// `len` bytes of the file from `start`, and its total size. No bytes (just
/// the size) if the server sends the whole file instead: that isn't read.
async fn read_range(
    client: &reqwest::Client,
    url: &str,
    referer: Option<&str>,
    start: u64,
    len: usize,
) -> Result<(Option<u64>, Option<Vec<u8>>)> {
    use reqwest::StatusCode;
    use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE};
    let mut req = client
        .get(url)
        .header(RANGE, format!("bytes={start}-{}", start + len as u64 - 1));
    if let Some(r) = referer {
        req = req.header(reqwest::header::REFERER, r);
    }
    let mut resp = req
        .send()
        .await
        .context("could not reach the video server")?;
    let status = resp.status();
    let header = |name| {
        resp.headers()
            .get(name)
            .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    // "bytes 0-4095/300853758"
    let total = header(CONTENT_RANGE)
        .rsplit('/')
        .next()
        .and_then(|t| t.trim().parse().ok());
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok((total, Some(Vec::new())));
    }
    if resp.headers().contains_key("cf-mitigated") {
        return Err(crate::sites::web::Challenge("the video's server").into());
    }
    if !status.is_success() {
        bail!("the video's server returned HTTP {status}");
    }
    if header(CONTENT_TYPE).starts_with("text/html") {
        bail!("the video's server returned a web page, not the video");
    }
    if status != StatusCode::PARTIAL_CONTENT {
        return Ok((resp.content_length(), None));
    }
    let mut data = Vec::with_capacity(len);
    while data.len() < len {
        match resp.chunk().await.context("error reading the video")? {
            Some(chunk) => data.extend_from_slice(&chunk),
            None => break,
        }
    }
    data.truncate(len);
    Ok((total, Some(data)))
}

/// Where the walk through an MP4's boxes is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Walk {
    /// At a top-level box.
    Top(u64),
    /// At a box inside `moov`, which ends at `end`.
    Moov { at: u64, end: u64 },
}

#[derive(Debug, PartialEq)]
enum Step {
    Next(Walk),
    /// The bytes from this offset are needed.
    Need(u64),
    /// A video track's width and height.
    Dims(u32, u32),
    /// No video track found.
    Done,
}

/// One step through the boxes, with the file's bytes from `off` in `buf`.
fn step(walk: Walk, off: u64, buf: &[u8]) -> Step {
    let bytes = |at: u64, len: usize| -> Option<&[u8]> {
        let start = usize::try_from(at.checked_sub(off)?).ok()?;
        buf.get(start..start.checked_add(len)?)
    };
    let at = match walk {
        Walk::Top(at) | Walk::Moov { at, .. } => at,
    };
    if let Walk::Moov { end, .. } = walk
        && at.saturating_add(8) > end
    {
        return Step::Done;
    }
    let Some(head) = bytes(at, 8) else {
        return Step::Need(at);
    };
    let kind: [u8; 4] = head[4..8].try_into().unwrap();
    const TOP: &[&[u8; 4]] = &[
        b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pdin", b"uuid", b"meta", b"styp",
        b"sidx", b"moof", b"mfra",
    ];
    if matches!(walk, Walk::Top(_)) && !TOP.contains(&&kind) {
        return Step::Done;
    }
    let (mut size, mut header) = (
        u64::from(u32::from_be_bytes(head[..4].try_into().unwrap())),
        8,
    );
    if size == 1 {
        let Some(large) = bytes(at + 8, 8) else {
            return Step::Need(at);
        };
        size = u64::from_be_bytes(large.try_into().unwrap());
        header = 16;
    }
    // Size 0: to the end of the file (or of moov).
    let end = match (size, walk) {
        (0, Walk::Moov { end, .. }) => end,
        (0, Walk::Top(_)) if &kind == b"moov" => u64::MAX,
        (0, Walk::Top(_)) => return Step::Done,
        (s, _) if s < header => return Step::Done,
        (s, _) => match at.checked_add(s) {
            Some(end) => end,
            None => return Step::Done,
        },
    };
    match walk {
        Walk::Top(_) if &kind == b"moov" => Step::Next(Walk::Moov {
            at: at + header,
            end,
        }),
        Walk::Top(_) => Step::Next(Walk::Top(end)),
        Walk::Moov { end: moov_end, .. } => {
            let next = Step::Next(Walk::Moov {
                at: end,
                end: moov_end,
            });
            if &kind != b"trak" {
                return next;
            }
            // A trak's first box is its tkhd; a video track's has the
            // picture's size (16.16 fixed point) at its end.
            let tkhd = at + header;
            let Some(th) = bytes(tkhd, 9) else {
                return Step::Need(at);
            };
            if &th[4..8] != b"tkhd" {
                return next;
            }
            let wh_at = tkhd + 8 + if th[8] == 1 { 88 } else { 76 };
            let Some(wh) = bytes(wh_at, 8) else {
                return Step::Need(at);
            };
            let w = u32::from_be_bytes(wh[..4].try_into().unwrap()) >> 16;
            let h = u32::from_be_bytes(wh[4..].try_into().unwrap()) >> 16;
            if w > 0 && h > 0 {
                Step::Dims(w, h)
            } else {
                next
            }
        }
    }
}

/// `&amp;`, `&#39;`, `&#x1F4A6;`... -> the characters.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());
    RE.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        } else if let Some(dec) = e.strip_prefix('#') {
            dec.parse().ok().and_then(char::from_u32)
        } else {
            match e {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => None,
            }
        };
        ch.map_or_else(|| c[0].to_owned(), String::from)
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed `series.php`.
    const INDEX: &str = r#"<div class='serylist'>
<a class='alpahticlabel' name='num'>#</a><div class='sererytitle'><a href='sery_video.php?seryid=MA==' >3Ping Lovers: Ippu Nisai no Sekai e Youkoso</a></div>
<a class='alpahticlabel' name='a'>A</a><div class='sererytitle'><a href='sery_video.php?seryid=MzI1Mg==' >Agriculture Story ~Chlore & Alka's Erotic Struggles</a></div>
<a class='alpahticlabel' name='k'>K</a><div class='sererytitle'><a href='sery_video.php?seryid=OTY=' >Kanojo x Kanojo x Kanojo</a></div><div class='sererytitle'><a href='sery_video.php?seryid=MzAzMg==' >Kuroinu 2</a></div><div class='sererytitle'><a href='sery_video.php?seryid=MjE1' >Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru</a></div>
</div>"#;

    /// A trimmed `sery_video.php?seryid=MjE1` (the sidebar's tags, an
    /// advert, a title split over two lines, and an episode shown as raw).
    const SERIES: &str = r#"<div class='tagcontainer'><a href='tagsearch.php?tag=Uncensored'>Uncensored</a><a href='tagsearch.php?tag=1080p'>1080p</a></div>
	<div class='serylistcontainer'>
		<img src="video_data/Kuroinu Kedakaki Seijo wa Hakudaku ni Somaru/cover.png" alt="Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru" >
		<div class='serydetailsection'>
			<h1 class='title'>Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru</h1>
			<div class='tagcontainer'>
				<a href='tagsearch.php?tag=Big Boobs'>Big Boobs</a><a href='tagsearch.php?tag=Rape'>Rape</a>			</div>
			<div class='longdes'><span>Episode Included: </span>3</div>
		</div>
	</div>
		<div class='videobrickwrap'>
		<div class='videobrick'>
				<a href='detail.php?vid=NDYy' class='interlink' ><div class='videocontentblock'>
				<div class='videosubcontainer'></div>
					<img src="video_data/Kuroinu Kedakaki Seijo wa Hakudaku ni Somaru/Episode-2/Episode-2.png" alt='Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 2' />
				</div></a>
				<div class='videotitle' title='Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 2'><a href='detail.php?vid=NDYy' class='interlink' >Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 2</a></div>
				<div class='videointro'>1 | 2</div></div><div class='videobrick'>
				<a href='detail.php?vid=NDYx' class='interlink' ><div class='videocontentblock'>
				<div class='videosubcontainer'><div class='subtag'>Raw</div></div>
					<img src="video_data/Kuroinu Kedakaki Seijo wa Hakudaku ni Somaru/Episode-1/Episode-1.png" alt='Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 1
' />
				</div></a>
				<div class='videotitle' title='Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 1
'><a href='detail.php?vid=NDYx' class='interlink' >Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 1
</a></div>
				<div class='videointro'>1 | 2</div></div><div class='videobrick'><div class='videocontentblock'>
								<a href='https://a.example/ad' class='gabuisvelink' target='_blank'><div class='videosubcontainer'><div class='adtag'>Sponsored</div></div>
				</div></a>
								<div class='videotitle' title='Adult Advertising'><a href='https://a.example/ad' class='gabuisvelink'  target='_blank'>Create your AI (free)</a></div>
				<div class='videoadintro'>Advert.</div></div><div class='videobrick'>
				<a href='detail.php?vid=MTMwNQ==' class='interlink' ><div class='videocontentblock'>
				<div class='videosubcontainer'></div>
					<img src="video_data/Kuroinu Kedakaki Seijo wa Hakudaku ni Somaru/Episode-3/Episode-3.png" alt='x' />
				</div></a>
				<div class='videotitle' title='Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru &amp; Extra'><a href='detail.php?vid=MTMwNQ==' class='interlink' >Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru &amp; Extra</a></div>
				<div class='videointro'>1 | 2</div></div>
		</div>"#;

    /// The player setup of `detail.php?vid=NDYx` (the advert's tag cut).
    const DETAIL: &str = r#"<div class='video'><video id="my-video"></video>
<script>
	SendPlay.setup({
			sources: [{"file":"https://videos.ohentai.org/videos/storage.bunnycdn.com/storage.bunnycdn.com/Old Anime/Kuroinu/kuroinu_ep1_en.mp4?token=f95d0edc62fd06207aa8ccb0dc76bbda&expires=1795840950"}],
			preload: "none",
			primary: "html5",
			image: "video_data/Kuroinu Kedakaki Seijo wa Hakudaku ni Somaru/Episode-1/Episode-1.png",
			width: "100%",
			height:"100%",
	    advertising: {
            client: "vast",
            tag: "https://go.example/api/vast/?campaignId=7177&duration=00%3A00%3A30",
	    skipoffset: 5
        }
});
</script>
		</div>
		  <h1 class='title'>
		  	Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 1
		  </h1>"#;

    /// An episode whose video is gone: no setup, only `load`s with no file.
    const DETAIL_GONE: &str = r#"<div class='video'><img src='img/videoempty.png' class='emptyimage' /></div>
		  <h1 class='title'>
		  	Kanojo x Kanojo x Kanojo - Episode 2
		  </h1>
<script>
 	SendPlay.load({
	 sources: [{"file":""}],
	 image: "video_data/Kanojo-x-Kanojo-x-Kanojo/Episode-2/Episode-2.png"});
</script>"#;

    #[test]
    fn normalizes_names() {
        assert_eq!(
            normalize("Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru"),
            normalize("Kuroinu ~Kedakaki Seijo wa Hakudaku ni Somaru~")
        );
        assert_eq!(
            normalize("Toshoshitsu no Kanojo: Seiso na Kimi ga Ochiru made"),
            normalize("Toshoshitsu no Kanojo Seiso na Kimi ga Ochiru Made")
        );
        assert_eq!(normalize("Kyōiku"), normalize("Kyouiku"));
        assert_eq!(
            normalize("Kanojo × Kanojo × Kanojo"),
            "kanojoxkanojoxkanojo"
        );
        assert_eq!(normalize("Chlore &amp; Alka's"), "chlorealkas");
        assert_ne!(normalize("Kuroinu"), normalize("Kuroinu 2"));
        assert_eq!(normalize(" ~ "), "");
    }

    #[test]
    fn parses_the_series_index() {
        let series = parse_series_index(INDEX);
        assert_eq!(series.len(), 5);
        assert_eq!(
            series[1],
            (
                "Agriculture Story ~Chlore & Alka's Erotic Struggles".to_owned(),
                "sery_video.php?seryid=MzI1Mg==".to_owned()
            )
        );
        assert_eq!(series[4].1, "sery_video.php?seryid=MjE1");
    }

    #[test]
    fn parses_a_series_page() {
        let _net = crate::mock_http::real();
        let eps = parse_series_page(SERIES, "index name").unwrap();
        let got: Vec<(u32, &str, &str)> = eps
            .iter()
            .map(|e| (e.episode, e.title.as_str(), e.url.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    1,
                    "Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 1",
                    "https://ohentai.org/detail.php?vid=NDYx"
                ),
                (
                    2,
                    "Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru - Episode 2",
                    "https://ohentai.org/detail.php?vid=NDYy"
                ),
                // Numbered by its thumbnail.
                (
                    3,
                    "Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru & Extra",
                    "https://ohentai.org/detail.php?vid=MTMwNQ=="
                ),
            ]
        );
        assert!(
            eps.iter()
                .all(|e| e.series == "Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru")
        );
        assert!(
            eps.iter().all(|e| e.resolution.is_none()),
            "the sidebar's 1080p isn't the series'"
        );

        let tagged = SERIES.replace(
            "tag=Rape'>Rape</a>",
            "tag=Rape'>Rape</a><a href='tagsearch.php?tag=1080p'>1080p</a>",
        );
        let eps = parse_series_page(&tagged, "x").unwrap();
        assert!(eps.iter().all(|e| e.resolution == Some(1080)));
    }

    #[test]
    fn numbers_episodes() {
        assert_eq!(episode_number("Bible Black - Episode 6"), Some(6));
        assert_eq!(episode_number("Foo - Ep. 12 "), Some(12));
        assert_eq!(
            episode_number("District 13 Housewife Animation - Flipside 6"),
            None
        );
        assert_eq!(episode_number("Summer Days"), None);
    }

    #[test]
    fn reads_the_player_setup() {
        let (source, subs) = player_media(DETAIL).unwrap();
        assert_eq!(
            source.file,
            "https://videos.ohentai.org/videos/storage.bunnycdn.com/storage.bunnycdn.com/Old Anime/Kuroinu/kuroinu_ep1_en.mp4?token=f95d0edc62fd06207aa8ccb0dc76bbda&expires=1795840950"
        );
        assert_eq!((source.resolution, source.mp4), (None, true));
        assert_eq!(subs, None);

        let err = format!("{:#}", player_media(DETAIL_GONE).unwrap_err());
        assert!(err.contains("no video"), "{err}");
    }

    #[test]
    fn picks_the_best_source() {
        let page = r#"<script>
        SendPlay.setup({
            // several qualities
            sources: [
                {file: 'v/ep1_480.mp4', label: '480p'},
                {file: "v/ep1.m3u8", label: "1080p", type: "hls"},
                {'file': 'v/ep1_hd.mp4', 'label': '720p HD', },
            ],
            tracks: [{file: "v/thumbs.vtt", kind: "thumbnails"}, {file: "v/ep1.en.vtt", label: "English", kind: "captions", default: true}],
            image: thumbnail, /* a variable */
            autostart: false,
            skipoffset: 5,
        });</script><h1 class='title'>x</h1>"#;
        let (source, subs) = player_media(page).unwrap();
        assert_eq!(source.file, "v/ep1_hd.mp4");
        assert_eq!(source.resolution, Some(720));
        assert_eq!(subs.as_deref(), Some("v/ep1.en.vtt"));
    }

    #[test]
    fn converts_javascript_objects() {
        let js = r#"{a: 'it\'s "x"', b: [1, 2.5, 1e3,], 'c': true, d: someVar, e: {f: null}, // note
            g: "a,}b"}"#;
        let v: Value = serde_json::from_str(&js_to_json(js)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"a": "it's \"x\"", "b": [1, 2.5, 1000.0], "c": true, "d": null, "e": {"f": null}, "g": "a,}b"})
        );
    }

    #[test]
    fn reads_resolutions() {
        assert_eq!(label_resolution("720p"), Some(720));
        assert_eq!(label_resolution("1080P HD"), Some(1080));
        assert_eq!(label_resolution("4K"), Some(2160));
        assert_eq!(label_resolution("ep1_1080p.mp4"), Some(1080));
        assert_eq!(label_resolution("kuroinu_ep1_en.mp4"), None);
        assert_eq!(label_resolution("x264"), None);
        assert_eq!(resolution(1280, 720), 720);
        assert_eq!(resolution(1024, 576), 576);
        assert_eq!(resolution(1920, 800), 1080);
        assert_eq!(resolution(1080, 1920), 1080);
        assert_eq!(resolution(1440, 1080), 1080);
    }

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    fn tkhd(w: u32, h: u32) -> Vec<u8> {
        let mut body = vec![0u8; 84];
        body[76..80].copy_from_slice(&(w << 16).to_be_bytes());
        body[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        mp4_box(b"tkhd", &body)
    }

    /// ftyp, then moov with an audio track big enough to push the video
    /// track past the first read, then mdat.
    fn faststart_mp4() -> Vec<u8> {
        let audio = mp4_box(
            b"trak",
            &[tkhd(0, 0), mp4_box(b"mdia", &[0; 6000])].concat(),
        );
        let video = mp4_box(
            b"trak",
            &[tkhd(1280, 720), mp4_box(b"mdia", &[0; 100])].concat(),
        );
        let moov = mp4_box(
            b"moov",
            &[mp4_box(b"mvhd", &[0; 100]), audio, video].concat(),
        );
        [
            mp4_box(b"ftyp", b"isomiso2avc1mp41"),
            moov,
            mp4_box(b"mdat", &[0; 3000]),
        ]
        .concat()
    }

    fn walk_all(file: &[u8], read: usize) -> (Step, usize) {
        let (mut off, mut walk, mut reads) = (0u64, Walk::Top(0), 1);
        let window =
            |at: u64, n: usize| file[at as usize..(at as usize + n).min(file.len())].to_vec();
        let mut buf = window(0, read);
        loop {
            match step(walk, off, &buf) {
                Step::Next(w) => walk = w,
                Step::Need(at) if reads < 10 && (at as usize) < file.len() && at != off => {
                    reads += 1;
                    (off, buf) = (at, window(at, LATER_READ));
                }
                other => return (other, reads),
            }
        }
    }

    #[test]
    fn finds_the_video_track() {
        let file = faststart_mp4();
        assert_eq!(walk_all(&file, FIRST_READ), (Step::Dims(1280, 720), 2));
        assert_eq!(walk_all(&file, file.len()), (Step::Dims(1280, 720), 1));

        // moov at the end, a 64-bit mdat size, a version 1 tkhd.
        let mut v1 = vec![0u8; 96];
        v1[0] = 1;
        v1[88..92].copy_from_slice(&(1920u32 << 16).to_be_bytes());
        v1[92..96].copy_from_slice(&(1080u32 << 16).to_be_bytes());
        let trak = mp4_box(b"trak", &mp4_box(b"tkhd", &v1));
        let mut mdat = 1u32.to_be_bytes().to_vec();
        mdat.extend_from_slice(b"mdat");
        mdat.extend_from_slice(&(16u64 + 9000).to_be_bytes());
        mdat.extend_from_slice(&[0; 9000]);
        let file = [mp4_box(b"ftyp", b"isom"), mdat, mp4_box(b"moov", &trak)].concat();
        assert_eq!(walk_all(&file, FIRST_READ), (Step::Dims(1920, 1080), 2));

        // Audio only.
        let file = [
            mp4_box(b"ftyp", b"M4A "),
            mp4_box(b"moov", &mp4_box(b"trak", &tkhd(0, 0))),
        ]
        .concat();
        assert_eq!(walk_all(&file, FIRST_READ), (Step::Done, 1));
        // Not an MP4.
        assert_eq!(walk_all(b"<html></html>", FIRST_READ).0, Step::Done);
    }

    /// Serves `file` at /v/ep 1.mp4 honouring Range, or (`ranges` false)
    /// ignoring it.
    fn serve(file: Vec<u8>, ranges: bool, req: &crate::mock_http::Req) -> crate::mock_http::Resp {
        assert_eq!(req.target, "/v/ep%201.mp4?token=t&expires=1");
        assert_eq!(
            req.headers.get("referer").map(String::as_str),
            Some(REFERER)
        );
        let range = req
            .headers
            .get("range")
            .and_then(|r| r.strip_prefix("bytes="));
        let headers = |extra: Option<String>| {
            let mut h = vec![("Content-Type".to_owned(), "video/mp4".to_owned())];
            h.extend(extra.map(|v| ("Content-Range".to_owned(), v)));
            h
        };
        match range {
            Some(r) if ranges => {
                let (a, b) = r.split_once('-').unwrap();
                let a: usize = a.parse().unwrap();
                let b = b.parse::<usize>().unwrap().min(file.len() - 1);
                crate::mock_http::Resp {
                    status: 206,
                    headers: headers(Some(format!("bytes {a}-{b}/{}", file.len()))),
                    body: file[a..=b].to_vec(),
                }
            }
            _ => crate::mock_http::Resp {
                status: 200,
                headers: headers(None),
                body: file,
            },
        }
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn lists_and_resolves_episodes() {
        let file = faststart_mp4();
        let len = file.len() as u64;
        let ranges = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = ranges.clone();
        let base = crate::mock_http::mock(move |req| {
            let page = |s: &str| crate::mock_http::Resp::html(200, s);
            match req.target.as_str() {
                "/series.php" => page(INDEX),
                "/sery_video.php?seryid=MjE1" => page(SERIES),
                "/detail.php?vid=NDYx" => page(&DETAIL.replace(
                    "https://videos.ohentai.org/videos/storage.bunnycdn.com/storage.bunnycdn.com/Old Anime/Kuroinu/kuroinu_ep1_en.mp4?token=f95d0edc62fd06207aa8ccb0dc76bbda&expires=1795840950",
                    "/v/ep 1.mp4?token=t&expires=1",
                )),
                "/detail.php?vid=NDYy" => page(DETAIL_GONE),
                t if t.starts_with("/v/") => {
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    serve(file.clone(), true, req)
                }
                t => panic!("unexpected request {t}"),
            }
        })
        .await;
        let _net = crate::mock_http::mocked();
        *OHENTAI_BASE.lock().unwrap() = Some(base.clone());
        let client = reqwest::Client::new();
        let names = [
            "Black Dogs".to_owned(),
            "Kuroinu ~Kedakaki Seijo wa Hakudaku ni Somaru~".to_owned(),
        ];
        let eps = episodes(&client, &names).await;
        let missing = episodes(&client, &["Overflow".to_owned()]).await;
        let video_ok = video(&client, &format!("{base}/detail.php?vid=NDYx")).await;
        let gone = video(&client, &format!("{base}/detail.php?vid=NDYy")).await;
        *OHENTAI_BASE.lock().unwrap() = None;

        let eps = eps.unwrap();
        assert_eq!(eps.len(), 3);
        assert_eq!(eps[0].url, format!("{base}/detail.php?vid=NDYx"));
        assert!(missing.unwrap().is_empty());
        let v = video_ok.unwrap();
        assert_eq!(
            v,
            Video {
                url: format!("{base}/v/ep%201.mp4?token=t&expires=1"),
                len: Some(len),
                referer: REFERER.to_owned(),
                resolution: Some(720),
                subtitles: None,
            }
        );
        assert_eq!(ranges.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(format!("{:#}", gone.unwrap_err()).contains("no video"));
    }

    #[tokio::test]
    async fn does_not_read_a_whole_file() {
        let file = faststart_mp4();
        let len = file.len() as u64;
        let base = crate::mock_http::mock(move |req| serve(file.clone(), false, req)).await;
        let url = format!("{base}/v/ep%201.mp4?token=t&expires=1");
        let got = probe(&reqwest::Client::new(), &url).await.unwrap();
        assert_eq!(got, (Some(len), None));
    }

    #[tokio::test]
    #[ignore = "uses the network"]
    #[allow(
        clippy::await_holding_lock,
        reason = "the lock orders whole tests, each on its own runtime"
    )]
    async fn live_episodes_and_video() {
        let _net = crate::mock_http::real();
        let client = reqwest::Client::builder()
            .user_agent("ichioshi-tests")
            .build()
            .unwrap();
        let names = [
            "Kuroinu: Kedakaki Seijo wa Hakudaku ni Somaru".to_owned(),
            "Kuroinu ~ Kedakaki Seijo wa Hakudaku ni Somaru".to_owned(),
        ];
        let eps = episodes(&client, &names).await.unwrap();
        for e in &eps {
            println!(
                "{} | {:?} | {} | {}",
                e.episode, e.resolution, e.title, e.url
            );
        }
        assert_eq!(eps.len(), 6);
        assert_eq!(
            eps.iter().map(|e| e.episode).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6]
        );
        for e in &eps[..2] {
            let v = video(&client, &e.url).await.unwrap();
            println!("{v:?}");
            assert!(v.len.unwrap() > 10_000_000);
            assert!(v.resolution.is_some());
        }
        let gone = episodes(&client, &["Kanojo x Kanojo x Kanojo".to_owned()])
            .await
            .unwrap();
        println!("{gone:?}");
        let err = video(&client, &gone[1].url).await.unwrap_err();
        println!("{err:#}");
        assert!(
            episodes(&client, &["Overflow".to_owned()])
                .await
                .unwrap()
                .is_empty()
        );
    }
}
