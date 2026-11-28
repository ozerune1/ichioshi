//! Just enough HLS (RFC 8216) to download a video on demand: a master
//! playlist's variants and renditions, and a media playlist's segments with
//! their byte ranges, keys and init sections (`EXT-X-MAP`).
//!
//! Attribute lists are read properly (a quoted string may hold commas), and
//! every URI is resolved against the playlist's own URL. A byte order mark,
//! CRLF line ends and blank lines are fine. Tags not needed here are
//! skipped, and so are I-frame playlists (`EXT-X-I-FRAME-STREAM-INF`). A
//! live playlist parses too, but nothing here reloads it: see
//! [`Media::ended`].

use std::ops::Range;

use anyhow::{Context, Result, bail, ensure};
use url::Url;

#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    Master(Master),
    Media(Media),
}

/// A list of the same video at different qualities, and the audio and
/// subtitle tracks that go with them.
#[derive(Debug, Clone, PartialEq)]
pub struct Master {
    pub variants: Vec<Variant>,
    pub renditions: Vec<Rendition>,
}

/// One quality: a media playlist of its own (`EXT-X-STREAM-INF`).
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub url: Url,
    /// Peak bits per second, as the playlist says (0 if it doesn't).
    pub bandwidth: u64,
    /// Width and height, in pixels.
    pub resolution: Option<(u32, u32)>,
    /// RFC 6381 codecs, like `avc1.64001f,mp4a.40.2`.
    pub codecs: Option<String>,
    /// The `group_id` of the audio renditions that go with it.
    pub audio: Option<String>,
    /// The `group_id` of its subtitle renditions.
    pub subtitles: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenditionKind {
    Audio,
    Video,
    Subtitles,
    ClosedCaptions,
}

/// An alternative track (`EXT-X-MEDIA`): another language's audio, or
/// subtitles.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    pub kind: RenditionKind,
    pub group_id: String,
    pub name: String,
    pub language: Option<String>,
    pub default: bool,
    /// Its media playlist; none if it's carried in the variant's own
    /// stream (closed captions always are).
    pub url: Option<Url>,
}

/// A list of segments to play one after another.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub segments: Vec<Segment>,
    /// Whether the list is complete (`EXT-X-ENDLIST`, or a `VOD` playlist
    /// type). A live list that isn't would need reloading for more.
    pub ended: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub url: Url,
    /// Seconds, as `EXTINF` says.
    pub duration: f64,
    /// The bytes of `url` that are the segment, if not all of them.
    pub byte_range: Option<Range<u64>>,
    /// How it's encrypted; none if it isn't.
    pub key: Option<Key>,
    /// Its init section (fragmented MP4 segments have one; TS normally not).
    pub map: Option<Map>,
    /// Its media sequence number.
    pub sequence: u64,
    /// Whether it follows an `EXT-X-DISCONTINUITY`: its timestamps may
    /// start over, or its encoding change.
    pub discontinuity: bool,
}

/// An init section (`EXT-X-MAP`).
#[derive(Debug, Clone, PartialEq)]
pub struct Map {
    pub url: Url,
    pub byte_range: Option<Range<u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyMethod {
    /// Not encrypted. Segments carry no key for this: their `key` is none.
    #[allow(dead_code, reason = "named for completeness: METHOD=NONE is no key")]
    None,
    /// The whole segment, AES-128 in CBC mode with PKCS#7 padding.
    Aes128,
    /// Samples inside the segment (Apple's FairPlay scheme and others).
    SampleAes,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub method: KeyMethod,
    /// Where the key is.
    pub url: Option<Url>,
    pub iv: Option<[u8; 16]>,
}

impl Key {
    /// The IV for the segment numbered `sequence`: the playlist's, or else
    /// the media sequence number as a 128-bit big-endian number.
    pub fn iv_for(&self, sequence: u64) -> [u8; 16] {
        self.iv
            .unwrap_or_else(|| u128::from(sequence).to_be_bytes())
    }
}

impl Segment {
    /// The IV to decrypt this segment with, if it's encrypted.
    pub fn iv(&self) -> Option<[u8; 16]> {
        self.key.as_ref().map(|k| k.iv_for(self.sequence))
    }
}

impl Media {
    /// The segments' total duration, in seconds.
    pub fn duration(&self) -> f64 {
        self.segments.iter().map(|s| s.duration).sum()
    }
}

impl Master {
    /// The variant to download: the highest resolution no taller than
    /// `max_height` (all of them if none), then the highest bandwidth.
    /// Variants that don't say their resolution count as the smallest. If
    /// every variant is too tall, the smallest one.
    pub fn best_variant(&self, max_height: Option<u32>) -> Option<&Variant> {
        let rank = |v: &&Variant| {
            let (w, h) = v.resolution.unwrap_or((0, 0));
            (h, w, v.bandwidth)
        };
        let fits = |v: &&Variant| match (max_height, v.resolution) {
            (Some(max), Some((_, h))) => h <= max,
            _ => true,
        };
        self.variants
            .iter()
            .filter(fits)
            .max_by_key(rank)
            .or_else(|| self.variants.iter().min_by_key(rank))
    }
}

/// Reads the playlist at `base` (relative URIs in it are resolved against
/// it).
pub fn parse(text: &str, base: &Url) -> Result<Playlist> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Old Mac line ends, if there are no others.
    let sep = if text.contains('\n') { '\n' } else { '\r' };
    let lines: Vec<(usize, &str)> = text
        .split(sep)
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty())
        .collect();
    ensure!(
        lines.first().is_some_and(|(_, l)| l.starts_with("#EXTM3U")),
        "not an HLS playlist (no #EXTM3U at the start)"
    );
    let lines = &lines[1..];
    let has = |tag: &str| lines.iter().any(|(_, l)| l.starts_with(tag));
    let master = has("#EXT-X-STREAM-INF:");
    let media = has("#EXTINF:");
    ensure!(
        !(master && media),
        "the playlist has both variants and segments"
    );
    if master || (has("#EXT-X-MEDIA:") && !media) {
        Ok(Playlist::Master(parse_master(lines, base)?))
    } else {
        Ok(Playlist::Media(parse_media(lines, base)?))
    }
}

fn parse_master(lines: &[(usize, &str)], base: &Url) -> Result<Master> {
    let mut variants = Vec::new();
    let mut renditions = Vec::new();
    // A variant's attributes, waiting for its URI on a following line.
    let mut pending: Option<(usize, Variant)> = None;
    for &(n, line) in lines {
        let at = || format!("line {n}");
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = Attrs::parse(rest).with_context(at)?;
            let bandwidth = match a.get("BANDWIDTH").or(a.get("AVERAGE-BANDWIDTH")) {
                Some(v) => v.integer().with_context(at)?,
                None => 0,
            };
            let resolution = match a.get("RESOLUTION") {
                Some(v) => Some(v.resolution().with_context(at)?),
                None => None,
            };
            let variant = Variant {
                // Replaced by the URI line's.
                url: base.clone(),
                bandwidth,
                resolution,
                codecs: a.string("CODECS"),
                audio: a.string("AUDIO"),
                subtitles: a.string("SUBTITLES"),
            };
            pending = Some((n, variant));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = Attrs::parse(rest).with_context(at)?;
            let kind = match a.get("TYPE").map(|v| v.text) {
                Some("AUDIO") => RenditionKind::Audio,
                Some("VIDEO") => RenditionKind::Video,
                Some("SUBTITLES") => RenditionKind::Subtitles,
                Some("CLOSED-CAPTIONS") => RenditionKind::ClosedCaptions,
                // Unknown to this version of the spec: not one to use.
                _ => continue,
            };
            let url = match a.get("URI") {
                Some(v) if kind != RenditionKind::ClosedCaptions => {
                    Some(join(base, v.text).with_context(at)?)
                }
                _ => None,
            };
            renditions.push(Rendition {
                kind,
                group_id: a.string("GROUP-ID").unwrap_or_default(),
                name: a.string("NAME").unwrap_or_default(),
                language: a.string("LANGUAGE"),
                default: a.get("DEFAULT").is_some_and(|v| v.text == "YES"),
                url,
            });
        } else if line.starts_with('#') {
            continue;
        } else if let Some((_, mut variant)) = pending.take() {
            variant.url = join(base, line).with_context(at)?;
            variants.push(variant);
        }
    }
    if let Some((n, _)) = pending {
        bail!("line {n}: a variant without a URI");
    }
    Ok(Master {
        variants,
        renditions,
    })
}

fn parse_media(lines: &[(usize, &str)], base: &Url) -> Result<Media> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut ended = false;
    let mut media_sequence = 0u64;
    let mut duration = None;
    let mut byte_range: Option<(u64, Option<u64>)> = None;
    let mut key: Option<Key> = None;
    // Whether an identity-format key was given since the last segment:
    // other formats beside it (DRM systems' keys for the same segments)
    // don't replace it.
    let mut identity_key = false;
    let mut map: Option<Map> = None;
    let mut discontinuity = false;
    for &(n, line) in lines {
        let at = || format!("line {n}");
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let secs = rest.split(',').next().unwrap_or("").trim();
            let secs: f64 = secs
                .parse()
                .ok()
                .filter(|s: &f64| s.is_finite() && *s >= 0.0)
                .with_context(|| format!("line {n}: bad segment duration '{secs}'"))?;
            duration = Some(secs);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            byte_range = Some(range_spec(rest).with_context(at)?);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let a = Attrs::parse(rest).with_context(at)?;
            let identity = a.get("KEYFORMAT").is_none_or(|v| v.text == "identity");
            if identity || !identity_key {
                key = parse_key(&a, base).with_context(at)?;
                identity_key = identity;
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let a = Attrs::parse(rest).with_context(at)?;
            let uri = a
                .get("URI")
                .with_context(|| format!("line {n}: EXT-X-MAP without a URI"))?;
            let byte_range = match a.get("BYTERANGE") {
                Some(v) => {
                    let (len, offset) = range_spec(v.text).with_context(at)?;
                    Some(span(offset.unwrap_or(0), len).with_context(at)?)
                }
                None => None,
            };
            map = Some(Map {
                url: join(base, uri.text).with_context(at)?,
                byte_range,
            });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = rest
                .trim()
                .parse()
                .with_context(|| format!("line {n}: bad media sequence '{rest}'"))?;
        } else if line == "#EXT-X-DISCONTINUITY" {
            discontinuity = true;
        } else if line == "#EXT-X-ENDLIST"
            || line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") == Some("VOD")
        {
            ended = true;
        } else if line.starts_with('#') {
            continue;
        } else {
            let url = join(base, line).with_context(at)?;
            let duration = duration
                .take()
                .with_context(|| format!("line {n}: a segment without #EXTINF"))?;
            let byte_range = match byte_range.take() {
                Some((len, offset)) => {
                    // No offset: right after the previous segment's bytes,
                    // if that was a range of the same file.
                    let follows = segments.last().and_then(|s| match &s.byte_range {
                        Some(r) if s.url == url => Some(r.end),
                        _ => None,
                    });
                    let start = offset.or(follows).unwrap_or(0);
                    Some(span(start, len).with_context(at)?)
                }
                None => None,
            };
            segments.push(Segment {
                url,
                duration,
                byte_range,
                key: key.clone(),
                map: map.clone(),
                sequence: media_sequence.saturating_add(segments.len() as u64),
                discontinuity,
            });
            discontinuity = false;
            identity_key = false;
        }
    }
    Ok(Media { segments, ended })
}

/// An `EXT-X-KEY`'s key; none for `METHOD=NONE`.
fn parse_key(a: &Attrs, base: &Url) -> Result<Option<Key>> {
    let method = match a.get("METHOD").map(|v| v.text) {
        Some("NONE") => return Ok(None),
        Some("AES-128") => KeyMethod::Aes128,
        Some("SAMPLE-AES") => KeyMethod::SampleAes,
        Some(other) => KeyMethod::Other(other.to_string()),
        None => bail!("EXT-X-KEY without a METHOD"),
    };
    let url = match a.get("URI") {
        Some(v) => Some(join(base, v.text)?),
        None => None,
    };
    let iv = match a.get("IV") {
        Some(v) => Some(v.hex_iv()?),
        None => None,
    };
    Ok(Some(Key { method, url, iv }))
}

/// `<length>[@<offset>]`.
fn range_spec(s: &str) -> Result<(u64, Option<u64>)> {
    let s = s.trim();
    let (len, offset) = match s.split_once('@') {
        Some((len, offset)) => (len, Some(offset)),
        None => (s, None),
    };
    let len = len
        .trim()
        .parse()
        .with_context(|| format!("bad byte range '{s}'"))?;
    let offset = match offset {
        Some(o) => Some(
            o.trim()
                .parse()
                .with_context(|| format!("bad byte range '{s}'"))?,
        ),
        None => None,
    };
    Ok((len, offset))
}

fn span(start: u64, len: u64) -> Result<Range<u64>> {
    let end = start
        .checked_add(len)
        .context("byte range past the largest file")?;
    Ok(start..end)
}

fn join(base: &Url, uri: &str) -> Result<Url> {
    base.join(uri.trim())
        .with_context(|| format!("bad URI '{uri}'"))
}

/// An attribute list: `NAME=value,NAME="quoted, maybe with commas",…`.
struct Attrs<'a>(Vec<(&'a str, Value<'a>)>);

#[derive(Debug, Clone, Copy)]
struct Value<'a> {
    text: &'a str,
}

impl<'a> Attrs<'a> {
    fn parse(mut rest: &'a str) -> Result<Self> {
        let mut out = Vec::new();
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            // A name without a value (never valid) is passed over.
            let Some(eq) = rest.find(['=', ',']) else {
                break;
            };
            if rest.as_bytes()[eq] == b',' {
                rest = &rest[eq + 1..];
                continue;
            }
            let name = rest[..eq].trim();
            rest = rest[eq + 1..].trim_start();
            let text;
            if let Some(quoted) = rest.strip_prefix('"') {
                let end = quoted
                    .find('"')
                    .with_context(|| format!("unterminated quoted string in {name}"))?;
                text = &quoted[..end];
                rest = &quoted[end + 1..];
                // Anything up to the next comma is junk.
                rest = rest.find(',').map_or("", |c| &rest[c + 1..]);
            } else {
                let end = rest.find(',').unwrap_or(rest.len());
                text = rest[..end].trim();
                rest = rest.get(end + 1..).unwrap_or("");
            }
            out.push((name, Value { text }));
        }
        Ok(Attrs(out))
    }

    fn get(&self, name: &str) -> Option<Value<'a>> {
        self.0.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }

    fn string(&self, name: &str) -> Option<String> {
        self.get(name).map(|v| v.text.to_string())
    }
}

impl Value<'_> {
    fn integer(&self) -> Result<u64> {
        self.text
            .parse()
            .with_context(|| format!("bad number '{}'", self.text))
    }

    /// `<width>x<height>`.
    fn resolution(&self) -> Result<(u32, u32)> {
        let bad = || format!("bad resolution '{}'", self.text);
        let (w, h) = self.text.split_once(['x', 'X']).with_context(bad)?;
        Ok((
            w.trim().parse().with_context(bad)?,
            h.trim().parse().with_context(bad)?,
        ))
    }

    /// `0x` and up to 32 hex digits: a 128-bit big-endian number.
    fn hex_iv(&self) -> Result<[u8; 16]> {
        let bad = || format!("bad IV '{}'", self.text);
        let digits = self
            .text
            .strip_prefix("0x")
            .or_else(|| self.text.strip_prefix("0X"))
            .with_context(bad)?;
        ensure!(
            !digits.is_empty()
                && digits.len() <= 32
                && digits.bytes().all(|b| b.is_ascii_hexdigit()),
            "bad IV '{}'",
            self.text
        );
        let n = u128::from_str_radix(digits, 16).with_context(bad)?;
        Ok(n.to_be_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.com/show/ep1/master.m3u8?token=abc").unwrap()
    }

    fn master(text: &str) -> Master {
        match parse(text, &base()).unwrap() {
            Playlist::Master(m) => m,
            other => panic!("not a master playlist: {other:?}"),
        }
    }

    fn media(text: &str) -> Media {
        match parse(text, &base()).unwrap() {
            Playlist::Media(m) => m,
            other => panic!("not a media playlist: {other:?}"),
        }
    }

    const MASTER: &str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-INDEPENDENT-SEGMENTS
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="Japanese, original",LANGUAGE="ja",DEFAULT=YES,AUTOSELECT=YES,URI="audio/ja.m3u8"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",LANGUAGE="en",DEFAULT=NO,URI="https://other.example.net/en.m3u8?sig=1,2"
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",NAME="English",LANGUAGE="en",FORCED=NO,URI="subs/en.m3u8"
#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID="cc",NAME="CC1",INSTREAM-ID="CC1"
#EXT-X-MEDIA:TYPE=FUTURE-THING,GROUP-ID="x",NAME="?"

#EXT-X-STREAM-INF:BANDWIDTH=2500000,AVERAGE-BANDWIDTH=2000000,RESOLUTION=1280x720,CODECS="avc1.64001f,mp4a.40.2",FRAME-RATE=23.976,AUDIO="aud",SUBTITLES="subs",CLOSED-CAPTIONS="cc"
720/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=6000000,RESOLUTION=1920x1080,CODECS="avc1.640028,mp4a.40.2",AUDIO="aud"
# a comment between the tag and its URI
/abs/1080/index.m3u8?x=1
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080
1080b.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS="mp4a.40.5"
https://audio.example.org/only.m3u8
#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=90000,URI="iframes.m3u8"
"#;

    #[test]
    fn reads_a_master_playlist() {
        let m = master(MASTER);
        assert_eq!(m.variants.len(), 4);
        let v = &m.variants[0];
        assert_eq!(
            v.url.as_str(),
            "https://cdn.example.com/show/ep1/720/index.m3u8"
        );
        assert_eq!(v.bandwidth, 2_500_000);
        assert_eq!(v.resolution, Some((1280, 720)));
        assert_eq!(v.codecs.as_deref(), Some("avc1.64001f,mp4a.40.2"));
        assert_eq!(v.audio.as_deref(), Some("aud"));
        assert_eq!(v.subtitles.as_deref(), Some("subs"));
        assert_eq!(
            m.variants[1].url.as_str(),
            "https://cdn.example.com/abs/1080/index.m3u8?x=1"
        );
        assert_eq!(m.variants[1].subtitles, None);
        assert_eq!(
            m.variants[3].url.as_str(),
            "https://audio.example.org/only.m3u8"
        );
        assert_eq!(m.variants[3].resolution, None);

        // The unknown TYPE is skipped; closed captions have no URI.
        assert_eq!(m.renditions.len(), 4);
        let ja = &m.renditions[0];
        assert_eq!(ja.kind, RenditionKind::Audio);
        assert_eq!(ja.group_id, "aud");
        assert_eq!(ja.name, "Japanese, original");
        assert_eq!(ja.language.as_deref(), Some("ja"));
        assert!(ja.default);
        assert_eq!(
            ja.url.as_ref().unwrap().as_str(),
            "https://cdn.example.com/show/ep1/audio/ja.m3u8"
        );
        let en = &m.renditions[1];
        assert!(!en.default);
        assert_eq!(
            en.url.as_ref().unwrap().as_str(),
            "https://other.example.net/en.m3u8?sig=1,2"
        );
        assert_eq!(m.renditions[2].kind, RenditionKind::Subtitles);
        assert_eq!(m.renditions[2].group_id, "subs");
        assert_eq!(m.renditions[3].kind, RenditionKind::ClosedCaptions);
        assert_eq!(m.renditions[3].url, None);
    }

    #[test]
    fn picks_the_best_variant() {
        let m = master(MASTER);
        // 1080p twice: the higher bandwidth wins.
        let best = m.best_variant(None).unwrap();
        assert_eq!(best.bandwidth, 6_000_000);
        assert_eq!(m.best_variant(Some(1080)).unwrap().bandwidth, 6_000_000);
        assert_eq!(
            m.best_variant(Some(1000)).unwrap().resolution,
            Some((1280, 720))
        );
        // Nothing is short enough but the audio-only one (no resolution).
        assert_eq!(m.best_variant(Some(240)).unwrap().bandwidth, 64_000);
        // All too tall: the smallest.
        let tall = Master {
            variants: m.variants[..3].to_vec(),
            renditions: vec![],
        };
        assert_eq!(
            tall.best_variant(Some(240)).unwrap().resolution,
            Some((1280, 720))
        );
        let empty = Master {
            variants: vec![],
            renditions: vec![],
        };
        assert!(empty.best_variant(None).is_none());
    }

    const MEDIA: &str = "\u{feff}#EXTM3U\r
#EXT-X-VERSION:4\r
#EXT-X-TARGETDURATION:6\r
#EXT-X-MEDIA-SEQUENCE:100\r
#EXT-X-PLAYLIST-TYPE:EVENT\r
#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720@0\"\r
#EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example.com/k?id=7&x=y\",IV=0x000102030405060708090A0B0C0D0E0F\r
#EXTINF:5.005,\r
#EXT-X-BYTERANGE:1000@720\r
media.ts\r
#EXTINF:5.005,title, with comma\r
#EXT-X-BYTERANGE:500\r
media.ts\r
\r
#EXT-X-KEY:METHOD=AES-128,URI=\"../keys/k2.bin\"\r
#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://fairplay\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\r
#EXTINF:4\r
#EXT-X-BYTERANGE:300\r
other.ts?part=3\r
#EXT-X-DISCONTINUITY\r
#EXT-X-KEY:METHOD=NONE\r
#EXTINF:2.5,\r
https://ads.example.com/ad.ts\r
#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://only\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\r
#EXTINF:1.0,\r
last.ts\r
#EXT-X-ENDLIST\r
";

    #[test]
    fn reads_a_media_playlist() {
        let m = media(MEDIA);
        assert!(m.ended);
        assert_eq!(m.segments.len(), 5);
        assert!((m.duration() - 17.51).abs() < 1e-9);
        let s: Vec<&Segment> = m.segments.iter().collect();

        assert_eq!(
            s[0].url.as_str(),
            "https://cdn.example.com/show/ep1/media.ts"
        );
        assert_eq!(s[0].duration, 5.005);
        assert_eq!(s[0].sequence, 100);
        assert_eq!(s[0].byte_range, Some(720..1720));
        // Continues the same file's range.
        assert_eq!(s[1].byte_range, Some(1720..2220));
        assert_eq!(s[1].sequence, 101);
        // Another file, no offset: from its start.
        assert_eq!(s[2].byte_range, Some(0..300));
        assert_eq!(
            s[2].url.as_str(),
            "https://cdn.example.com/show/ep1/other.ts?part=3"
        );
        assert_eq!(s[3].byte_range, None);

        let map = s[0].map.as_ref().unwrap();
        assert_eq!(
            map.url.as_str(),
            "https://cdn.example.com/show/ep1/init.mp4"
        );
        assert_eq!(map.byte_range, Some(0..720));
        assert_eq!(s[4].map, s[0].map);

        let k = s[0].key.as_ref().unwrap();
        assert_eq!(k.method, KeyMethod::Aes128);
        assert_eq!(
            k.url.as_ref().unwrap().as_str(),
            "https://keys.example.com/k?id=7&x=y"
        );
        let iv: [u8; 16] = std::array::from_fn(|i| i as u8);
        assert_eq!(k.iv, Some(iv));
        assert_eq!(s[0].iv(), Some(iv));
        assert_eq!(s[1].key, s[0].key);
        // A key without an IV: the media sequence number. The FairPlay key
        // beside it doesn't replace it.
        let k2 = s[2].key.as_ref().unwrap();
        assert_eq!(k2.method, KeyMethod::Aes128);
        assert_eq!(
            k2.url.as_ref().unwrap().as_str(),
            "https://cdn.example.com/show/keys/k2.bin"
        );
        assert_eq!(k2.iv, None);
        let mut seq_iv = [0u8; 16];
        seq_iv[15] = 102;
        assert_eq!(s[2].iv(), Some(seq_iv));

        assert!(!s[2].discontinuity);
        assert!(s[3].discontinuity);
        assert_eq!(s[3].key, None);
        assert_eq!(s[3].iv(), None);
        assert_eq!(s[3].url.as_str(), "https://ads.example.com/ad.ts");
        // Only a DRM key: kept, so the caller sees it can't decrypt it.
        assert_eq!(s[4].key.as_ref().unwrap().method, KeyMethod::SampleAes);
        assert!(!s[4].discontinuity);
    }

    #[test]
    fn live_playlists_are_not_ended() {
        let m = media("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n");
        assert!(!m.ended);
        assert_eq!(m.segments[0].sequence, 0);
        let vod = media("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2,\na.ts\n");
        assert!(vod.ended);
        // Nothing but a header: an empty media playlist.
        assert!(
            media("#EXTM3U\n#EXT-X-TARGETDURATION:2\n")
                .segments
                .is_empty()
        );
    }

    #[test]
    fn reads_attribute_lists() {
        let a = Attrs::parse(r#"A=1,B="x, y=z",C=0x1F, D = YES ,E="",F"#).unwrap();
        let get = |n| a.get(n).map(|v| v.text);
        assert_eq!(get("A"), Some("1"));
        assert_eq!(get("B"), Some("x, y=z"));
        assert_eq!(get("C"), Some("0x1F"));
        assert_eq!(get("D"), Some("YES"));
        assert_eq!(get("E"), Some(""));
        assert_eq!(get("F"), None);
        assert!(Attrs::parse(r#"A="never closed"#).is_err());
        assert!(Attrs::parse("").unwrap().0.is_empty());

        let iv = |s| Value { text: s }.hex_iv();
        let mut one = [0u8; 16];
        one[15] = 1;
        assert_eq!(iv("0x1").unwrap(), one);
        assert_eq!(iv("0X00000000000000000000000000000001").unwrap(), one);
        assert!(iv("0x").is_err());
        assert!(iv("1234").is_err());
        assert!(iv("0x0g").is_err());
        assert!(iv("0x+1").is_err());
        assert!(iv("0x000000000000000000000000000000001").is_err());
    }

    #[test]
    fn rejects_broken_playlists() {
        let b = base();
        assert!(parse("", &b).is_err());
        assert!(parse("<html>404</html>", &b).is_err());
        assert!(parse("#EXTM3U\n#EXTINF:abc,\na.ts\n", &b).is_err());
        assert!(parse("#EXTM3U\n#EXTINF:-1,\na.ts\n", &b).is_err());
        assert!(parse("#EXTM3U\n#EXTINF:2,\n#EXT-X-BYTERANGE:x@1\na.ts\n", &b).is_err());
        assert!(parse("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,IV=0xZZ\n", &b).is_err());
        assert!(parse("#EXTM3U\n#EXT-X-MAP:BYTERANGE=\"1@0\"\n", &b).is_err());
        // A URI line before any EXTINF.
        assert!(parse("#EXTM3U\n#EXT-X-TARGETDURATION:2\na.ts\n", &b).is_err());
        // Variants and segments together.
        assert!(
            parse(
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\na.m3u8\n#EXTINF:1,\nb.ts\n",
                &b
            )
            .is_err()
        );
        // A variant whose URI never comes.
        assert!(parse("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\n", &b).is_err());
        assert!(parse("#EXTM3U\n#EXT-X-STREAM-INF:RESOLUTION=big\na.m3u8\n", &b).is_err());
        // A range past the end of any file.
        let huge = format!(
            "#EXTM3U\n#EXTINF:1,\n#EXT-X-BYTERANGE:10@{}\na.ts\n",
            u64::MAX
        );
        assert!(parse(&huge, &b).is_err());
    }

    #[test]
    fn never_panics_on_junk() {
        // Every prefix of the test playlists, and some line-level mangling.
        for text in [MASTER, MEDIA] {
            for end in (0..text.len()).filter(|&i| text.is_char_boundary(i)) {
                let _ = parse(&text[..end], &base());
            }
            for (i, line) in text.lines().enumerate() {
                let half = (0..=line.len() / 2)
                    .rev()
                    .find(|&k| line.is_char_boundary(k))
                    .unwrap_or(0);
                let mangled: String = text
                    .lines()
                    .enumerate()
                    .map(|(j, l)| if i == j { &line[..half] } else { l })
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = parse(&mangled, &base());
            }
        }
    }
}
