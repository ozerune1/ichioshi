//! End to end: HLS that ffmpeg makes, read, decrypted and remuxed here,
//! joined by the app's muxer (`fmp4`), and checked with ffprobe and a
//! full decode. Tests skip (saying so) when ffmpeg or ffprobe is missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail, ensure};
use url::Url;

use super::aes128;
use super::m3u8::{self, KeyMethod, Media, Playlist, Segment};
use super::mp4frag::{Remuxer, TrackKind, Unsupported};
use crate::fmp4::{Init, Muxer, Track};

/// Seconds of test video, at 25 fps.
const SECONDS: f64 = 10.0;
const FPS: f64 = 25.0;

/// A directory for a test's files, in the build's own directory.
fn tempdir() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/tmp");
    std::fs::create_dir_all(&root).unwrap();
    let mut dir = tempfile::tempdir_in(root).unwrap();
    dir.disable_cleanup(std::env::var_os("HLS_KEEP").is_some());
    dir
}

fn have(tool: &str) -> bool {
    let arg = if tool == "openssl" {
        "version"
    } else {
        "-version"
    };
    Command::new(tool)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success())
}

macro_rules! need {
    ($($tool:expr),+) => {
        $(
            if !have($tool) {
                println!("skipped: {} isn't installed", $tool);
                return;
            }
        )+
    };
}

fn run(tool: &str, args: &[&str]) -> Result<std::process::Output> {
    let out = Command::new(tool)
        .args(args)
        .output()
        .with_context(|| format!("running {tool}"))?;
    ensure!(
        out.status.success(),
        "{tool} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(out)
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

struct Encode<'a> {
    video: bool,
    audio: bool,
    video_codec: &'a [&'a str],
    /// More output options.
    extra: &'a [&'a str],
    segment_file: &'a str,
}

const X264: &[&str] = &[
    "-c:v",
    "libx264",
    "-preset",
    "veryfast",
    "-bf",
    "3",
    "-g",
    "50",
    "-keyint_min",
    "50",
    "-sc_threshold",
    "0",
    "-pix_fmt",
    "yuv420p",
];

impl Default for Encode<'_> {
    fn default() -> Self {
        Encode {
            video: true,
            audio: true,
            video_codec: X264,
            extra: &[],
            segment_file: "seg%03d.ts",
        }
    }
}

/// testsrc2 and a sine (or either) as HLS with 2 s TS segments, into
/// `dir`; returns the media playlist.
fn encode(dir: &Path, e: Encode) -> Result<PathBuf> {
    let video = format!("testsrc2=size=320x240:rate={FPS}:duration={SECONDS}");
    let audio = format!("sine=frequency=440:sample_rate=48000:duration={SECONDS}");
    let segments = dir.join(e.segment_file);
    let playlist = dir.join("index.m3u8");
    let mut args = vec!["-v", "error", "-y"];
    if e.video {
        args.extend(["-f", "lavfi", "-i", &video]);
    }
    if e.audio {
        args.extend(["-f", "lavfi", "-i", &audio]);
    }
    if e.video {
        args.extend(e.video_codec);
    }
    if e.audio {
        args.extend(["-c:a", "aac", "-b:a", "96k"]);
    }
    args.extend(e.extra);
    args.extend(["-f", "hls", "-hls_time", "2", "-hls_segment_type", "mpegts"]);
    args.extend([
        "-hls_playlist_type",
        "vod",
        "-hls_segment_filename",
        s(&segments),
    ]);
    args.push(s(&playlist));
    run("ffmpeg", &args)?;
    Ok(playlist)
}

fn read_playlist(path: &Path) -> Result<(Playlist, Url)> {
    let url = Url::from_file_path(path).map_err(|_| anyhow!("not an absolute path"))?;
    let text = std::fs::read_to_string(path)?;
    Ok((m3u8::parse(&text, &url)?, url))
}

fn media_playlist(path: &Path) -> Result<Media> {
    match read_playlist(path)? {
        (Playlist::Media(m), _) => Ok(m),
        (Playlist::Master(m), _) => {
            let v = m.best_variant(Some(1080)).context("no variants")?;
            let path = v
                .url
                .to_file_path()
                .map_err(|_| anyhow!("not a file URL"))?;
            match read_playlist(&path)? {
                (Playlist::Media(m), _) => Ok(m),
                _ => bail!("a master playlist in a master playlist"),
            }
        }
    }
}

/// A segment's bytes, decrypted: what the app downloads.
fn load(seg: &Segment, keys: &mut HashMap<Url, [u8; 16]>) -> Result<Vec<u8>> {
    let path = seg
        .url
        .to_file_path()
        .map_err(|_| anyhow!("not a file URL"))?;
    let mut bytes = std::fs::read(&path)?;
    if let Some(r) = &seg.byte_range {
        bytes = bytes
            .get(r.start as usize..r.end as usize)
            .context("byte range past the end of the file")?
            .to_vec();
    }
    let Some(key) = &seg.key else {
        return Ok(bytes);
    };
    ensure!(
        key.method == KeyMethod::Aes128,
        "{:?} isn't supported",
        key.method
    );
    let url = key.url.as_ref().context("a key without a URI")?;
    let k = match keys.get(url) {
        Some(k) => *k,
        None => {
            let path = url.to_file_path().map_err(|_| anyhow!("not a file URL"))?;
            let k: [u8; 16] = std::fs::read(path)?
                .try_into()
                .map_err(|_| anyhow!("a key that isn't 16 bytes"))?;
            keys.insert(url.clone(), k);
            k
        }
    };
    aes128::decrypt(&k, &seg.iv().context("no IV")?, &bytes)
}

/// The whole pipeline, as the app will run it: the playlist from disk,
/// each segment decrypted and pushed, the tracks known after the first
/// segment(s), the muxer's header, a fragment per track per segment, the
/// end. Returns the tracks' kinds.
fn remux(playlist: &Path, out: &Path) -> Result<Vec<TrackKind>> {
    let media = media_playlist(playlist)?;
    ensure!(media.ended, "the playlist isn't complete");
    let mut keys = HashMap::new();
    let mut remuxer = Remuxer::new();
    let mut muxer: Option<Muxer> = None;
    let mut kinds = Vec::new();
    let mut file = Vec::new();
    for seg in &media.segments {
        let bytes = load(seg, &mut keys)?;
        if seg.discontinuity {
            remuxer.discontinuity()?;
        }
        remuxer.push(&bytes)?;
        if muxer.is_none() {
            let tracks = match remuxer.tracks() {
                None => continue,
                Some(t) => t?,
            };
            let mut joined = Vec::new();
            for t in tracks {
                kinds.push(t.kind);
                let duration = (media.duration() * f64::from(t.timescale)).round() as u64;
                joined.push(Track {
                    init: Init::parse(t.init)?,
                    duration,
                });
            }
            let (m, header) = Muxer::new(&joined)?;
            file.extend(header);
            muxer = Some(m);
        }
        let m = muxer.as_mut().context("no muxer")?;
        for (track, fragment) in remuxer.take_fragments()? {
            file.extend(m.fragments(track, &fragment)?);
        }
    }
    let mut m = muxer.context("the tracks never became known")?;
    for (track, fragment) in remuxer.finish()? {
        file.extend(m.fragments(track, &fragment)?);
    }
    file.extend(m.finish());
    std::fs::write(out, &file)?;
    Ok(kinds)
}

/// ffprobe's `key=value`s for `entries` of `file` (one stream if
/// `select`).
fn probe(
    file: &Path,
    select: Option<&str>,
    entries: &str,
    count: bool,
) -> Result<HashMap<String, String>> {
    let mut args = vec!["-v", "error"];
    if let Some(sel) = select {
        args.extend(["-select_streams", sel]);
    }
    if count {
        args.push("-count_frames");
    }
    args.extend([
        "-show_entries",
        entries,
        "-of",
        "default=noprint_wrappers=1",
    ]);
    args.push(s(file));
    let out = run("ffprobe", &args)?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

fn number(map: &HashMap<String, String>, key: &str) -> f64 {
    map.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no number for {key} in {map:?}"))
}

/// The first and last presentation times of a stream's packets (the last
/// one's end), in seconds.
fn span(file: &Path, select: &str) -> Result<(f64, f64)> {
    let out = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            select,
            "-show_entries",
            "packet=pts_time,duration_time",
            "-of",
            "csv=p=0",
            s(file),
        ],
    )?;
    let mut first = f64::MAX;
    let mut last = f64::MIN;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.split(',');
        let pts: f64 = parts.next().unwrap_or("").parse()?;
        let dur: f64 = parts.next().unwrap_or("0").parse().unwrap_or(0.0);
        first = first.min(pts);
        last = last.max(pts + dur);
    }
    Ok((first, last))
}

/// Options to read `file` with: a playlist's key file has to be let
/// through.
fn input(file: &Path) -> Vec<&str> {
    let mut args = vec!["-v", "error"];
    if file.extension().is_some_and(|e| e == "m3u8") {
        args.extend(["-allowed_extensions", "ALL"]);
    }
    args.extend(["-i", s(file)]);
    args
}

/// Each decoded frame's MD5, in order: `stream` is `v` or `a`.
fn decoded(file: &Path, stream: &str) -> Vec<String> {
    let map = format!("0:{stream}:0");
    let mut args = input(file);
    args.extend(["-map", &map, "-f", "framemd5", "-"]);
    let out = run("ffmpeg", &args).unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.rsplit(',').next())
        .map(|h| h.trim().to_string())
        .collect()
}

/// What every output must be: H.264 and AAC (or either); all the
/// sources' video frames, decoding to the same pictures (and the same
/// sound, from a single source; across a discontinuity, audio that would
/// overlap is dropped); the right duration; audio and video starting
/// together; and decoding without a word from ffmpeg.
fn check(out: &Path, sources: &[&Path], seconds: f64, video: bool, audio: bool) {
    let types = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
            s(out),
        ],
    )
    .unwrap();
    let types = String::from_utf8_lossy(&types.stdout).to_string();
    let types: Vec<&str> = types.lines().collect();
    let want: Vec<&str> = [(video, "video"), (audio, "audio")]
        .into_iter()
        .filter_map(|(has, kind)| has.then_some(kind))
        .collect();
    assert_eq!(types, want);
    let f = probe(out, None, "format=duration", false).unwrap();
    let duration = number(&f, "duration");
    assert!(
        (duration - seconds).abs() < 0.1,
        "duration {duration}, not {seconds}"
    );
    let mut starts = Vec::new();
    if video {
        let v = probe(
            out,
            Some("v:0"),
            "stream=codec_name,start_time,nb_read_frames",
            true,
        )
        .unwrap();
        assert_eq!(v["codec_name"], "h264");
        let frames: Vec<String> = sources.iter().flat_map(|s| decoded(s, "v")).collect();
        assert_eq!(
            number(&v, "nb_read_frames"),
            frames.len() as f64,
            "video frames"
        );
        assert!(decoded(out, "v") == frames, "the pictures differ");
        let (start, end) = span(out, "v:0").unwrap();
        assert!(
            (end - start - seconds).abs() < 0.1,
            "video from {start} to {end}, not {seconds} s"
        );
        starts.push(number(&v, "start_time"));
    }
    if audio {
        let a = probe(
            out,
            Some("a:0"),
            "stream=codec_name,start_time,sample_rate",
            false,
        )
        .unwrap();
        assert_eq!(a["codec_name"], "aac");
        assert_eq!(a["sample_rate"], "48000");
        if let [source] = sources {
            assert!(
                decoded(out, "a") == decoded(source, "a"),
                "the sound differs"
            );
        }
        let (start, end) = span(out, "a:0").unwrap();
        assert!(
            (end - start - seconds).abs() < 0.1,
            "audio from {start} to {end}, not {seconds} s"
        );
        starts.push(number(&a, "start_time"));
    }
    if let [v_start, a_start] = starts[..] {
        assert!(
            (v_start - a_start).abs() <= 1.0 / FPS + 1e-6,
            "video starts at {v_start}, audio at {a_start}"
        );
    }
    let mut args = input(out);
    args.extend(["-f", "null", "-"]);
    let decode = run("ffmpeg", &args).unwrap();
    let said = String::from_utf8_lossy(&decode.stderr);
    assert!(said.trim().is_empty(), "ffmpeg said: {said}");
}

#[test]
fn remuxes_plain_hls() {
    need!("ffmpeg", "ffprobe");
    let dir = tempdir();
    let playlist = encode(
        dir.path(),
        Encode {
            extra: &["-master_pl_name", "master.m3u8"],
            ..Encode::default()
        },
    )
    .unwrap();
    // Through the master playlist, to its one variant.
    let master = dir.path().join("master.m3u8");
    match read_playlist(&master).unwrap().0 {
        Playlist::Master(m) => {
            let v = m.best_variant(None).unwrap();
            assert_eq!(v.url, Url::from_file_path(&playlist).unwrap());
            assert!(v.codecs.as_deref().unwrap().contains("avc1"));
        }
        other => panic!("not a master playlist: {other:?}"),
    }
    let out = dir.path().join("out.mp4");
    let kinds = remux(&master, &out).unwrap();
    assert_eq!(kinds, [TrackKind::Video, TrackKind::Audio]);
    check(&out, &[&playlist], SECONDS, true, true);
}

#[test]
fn remuxes_aes_128_hls() {
    need!("ffmpeg", "ffprobe");
    let dir = tempdir();
    let key: [u8; 16] = std::array::from_fn(|i| (i as u8).wrapping_mul(37) ^ 0x5A);
    let key_file = dir.path().join("key.bin");
    std::fs::write(&key_file, key).unwrap();
    let info = dir.path().join("key.info");
    std::fs::write(&info, format!("key.bin\n{}\n", s(&key_file))).unwrap();
    let playlist = encode(
        dir.path(),
        Encode {
            extra: &["-hls_key_info_file", s(&info), "-start_number", "7"],
            ..Encode::default()
        },
    )
    .unwrap();
    let media = media_playlist(&playlist).unwrap();
    assert_eq!(media.segments[0].sequence, 7);
    // Given no IV, ffmpeg takes the first segment's number, and says so
    // once for them all.
    let iv = media.segments[0].iv().unwrap();
    for seg in &media.segments {
        let k = seg.key.as_ref().unwrap();
        assert_eq!(k.method, KeyMethod::Aes128);
        assert_eq!(k.url, Some(Url::from_file_path(&key_file).unwrap()));
        assert_eq!(seg.iv(), Some(iv));
    }
    let out = dir.path().join("out.mp4");
    remux(&playlist, &out).unwrap();
    assert_eq!(decoded(&playlist, "v").len() as f64, SECONDS * FPS);
    check(&out, &[&playlist], SECONDS, true, true);

    // The segments decrypted by OpenSSL (to the same bytes), encrypted
    // again with each one's sequence number as its IV, in a playlist
    // without IVs: the same file comes out.
    if !have("openssl") {
        println!("openssl isn't installed: sequence-number IVs not tested");
        return;
    }
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let again = dir.path().join("again");
    std::fs::create_dir(&again).unwrap();
    let mut text = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{}\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"../key.bin\"\n",
        media.segments[0].sequence
    );
    for seg in &media.segments {
        let n = seg.sequence;
        let plain = again.join(format!("plain{n}.ts"));
        let encrypted = seg.url.to_file_path().unwrap();
        let openssl = |dir: &str, iv: [u8; 16], from: &Path, to: &Path| {
            let (key, iv) = (hex(&key), hex(&iv));
            run(
                "openssl",
                &[
                    "aes-128-cbc",
                    dir,
                    "-K",
                    &key,
                    "-iv",
                    &iv,
                    "-in",
                    s(from),
                    "-out",
                    s(to),
                ],
            )
            .unwrap();
        };
        openssl("-d", seg.iv().unwrap(), &encrypted, &plain);
        assert_eq!(
            load(seg, &mut HashMap::new()).unwrap(),
            std::fs::read(&plain).unwrap()
        );
        openssl(
            "-e",
            u128::from(n).to_be_bytes(),
            &plain,
            &again.join(format!("s{n}.ts")),
        );
        text.push_str(&format!("#EXTINF:{},\ns{n}.ts\n", seg.duration));
    }
    text.push_str("#EXT-X-ENDLIST\n");
    let playlist = again.join("index.m3u8");
    std::fs::write(&playlist, text).unwrap();
    let media = media_playlist(&playlist).unwrap();
    for seg in &media.segments {
        assert_eq!(seg.key.as_ref().unwrap().iv, None);
        assert_eq!(seg.iv(), Some(u128::from(seg.sequence).to_be_bytes()));
    }
    let out2 = again.join("out.mp4");
    remux(&playlist, &out2).unwrap();
    assert!(std::fs::read(&out).unwrap() == std::fs::read(&out2).unwrap());
}

#[test]
fn remuxes_video_only_hls() {
    need!("ffmpeg", "ffprobe");
    let dir = tempdir();
    let playlist = encode(
        dir.path(),
        Encode {
            audio: false,
            ..Encode::default()
        },
    )
    .unwrap();
    let out = dir.path().join("out.mp4");
    let kinds = remux(&playlist, &out).unwrap();
    assert_eq!(kinds, [TrackKind::Video]);
    check(&out, &[&playlist], SECONDS, true, false);
}

#[test]
fn remuxes_audio_only_hls() {
    need!("ffmpeg", "ffprobe");
    let dir = tempdir();
    let playlist = encode(
        dir.path(),
        Encode {
            video: false,
            ..Encode::default()
        },
    )
    .unwrap();
    let out = dir.path().join("out.mp4");
    let kinds = remux(&playlist, &out).unwrap();
    assert_eq!(kinds, [TrackKind::Audio]);
    check(&out, &[&playlist], SECONDS, false, true);
}

/// One TS file, the segments byte ranges of it.
#[test]
fn remuxes_byte_range_hls() {
    need!("ffmpeg", "ffprobe");
    let dir = tempdir();
    let playlist = encode(
        dir.path(),
        Encode {
            extra: &["-hls_flags", "single_file"],
            segment_file: "all.ts",
            ..Encode::default()
        },
    )
    .unwrap();
    let media = media_playlist(&playlist).unwrap();
    assert!(media.segments.len() >= 4);
    assert!(media.segments.iter().all(|s| s.byte_range.is_some()));
    assert!(media.segments.windows(2).all(|w| {
        w[0].byte_range.as_ref().unwrap().end == w[1].byte_range.as_ref().unwrap().start
    }));
    let out = dir.path().join("out.mp4");
    remux(&playlist, &out).unwrap();
    check(&out, &[&playlist], SECONDS, true, true);
}

/// Timestamps that pass 2^33 (26.5 hours) partway through.
#[test]
fn remuxes_across_the_timestamp_wrap() {
    need!("ffmpeg", "ffprobe");
    let dir = tempdir();
    // The wrap is at 95443.7 s.
    let playlist = encode(
        dir.path(),
        Encode {
            extra: &["-output_ts_offset", "95440"],
            ..Encode::default()
        },
    )
    .unwrap();
    // Check the fixture does wrap: a late segment's timestamps are small.
    let media = media_playlist(&playlist).unwrap();
    let last = media.segments.last().unwrap();
    let pts = probe(
        &last.url.to_file_path().unwrap(),
        Some("v:0"),
        "stream=start_time",
        false,
    )
    .unwrap();
    assert!(number(&pts, "start_time") < 100.0, "{pts:?}");
    let out = dir.path().join("out.mp4");
    remux(&playlist, &out).unwrap();
    check(&out, &[&playlist], SECONDS, true, true);
}

/// Two encodes one after the other, the second's timestamps starting over
/// after an EXT-X-DISCONTINUITY.
#[test]
fn remuxes_across_a_discontinuity() {
    need!("ffmpeg", "ffprobe");
    let dir = tempdir();
    let mut parts = Vec::new();
    for name in ["a", "b"] {
        let sub = dir.path().join(name);
        std::fs::create_dir(&sub).unwrap();
        parts.push(encode(&sub, Encode::default()).unwrap());
    }
    let mut text = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:3\n");
    for (i, part) in parts.iter().enumerate() {
        let body = std::fs::read_to_string(part).unwrap();
        if i > 0 {
            text.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let name = ["a", "b"][i];
        for line in body.lines() {
            if line.starts_with("#EXTINF") {
                text.push_str(line);
                text.push('\n');
            } else if !line.starts_with('#') && !line.is_empty() {
                text.push_str(&format!("{name}/{line}\n"));
            }
        }
    }
    text.push_str("#EXT-X-ENDLIST\n");
    let playlist = dir.path().join("joined.m3u8");
    std::fs::write(&playlist, text).unwrap();
    let media = media_playlist(&playlist).unwrap();
    assert_eq!(media.segments.iter().filter(|s| s.discontinuity).count(), 1);
    let out = dir.path().join("out.mp4");
    remux(&playlist, &out).unwrap();
    check(&out, &[&parts[0], &parts[1]], 2.0 * SECONDS, true, true);
}

#[test]
fn says_hevc_is_unsupported() {
    need!("ffmpeg");
    let encoders = run("ffmpeg", &["-hide_banner", "-encoders"]).unwrap();
    if !String::from_utf8_lossy(&encoders.stdout).contains("libx265") {
        println!("skipped: this ffmpeg has no libx265");
        return;
    }
    let dir = tempdir();
    let playlist = encode(
        dir.path(),
        Encode {
            video_codec: &["-c:v", "libx265", "-x265-params", "log-level=none"],
            ..Encode::default()
        },
    )
    .unwrap();
    let media = media_playlist(&playlist).unwrap();
    let mut remuxer = Remuxer::new();
    remuxer
        .push(&load(&media.segments[0], &mut HashMap::new()).unwrap())
        .unwrap();
    let expected = Unsupported {
        codec: "HEVC".to_string(),
    };
    assert_eq!(remuxer.tracks(), Some(Err(expected.clone())));
    let err = remuxer.take_fragments().unwrap_err();
    assert_eq!(err.downcast_ref::<Unsupported>(), Some(&expected));
    assert_eq!(
        remux(&playlist, &dir.path().join("out.mp4"))
            .unwrap_err()
            .to_string(),
        "HEVC isn't supported"
    );
}

/// Damaged and cut segments: errors or a worse file, never a panic, and
/// whatever comes out still joins.
#[test]
fn survives_damaged_segments() {
    need!("ffmpeg");
    let dir = tempdir();
    let playlist = encode(dir.path(), Encode::default()).unwrap();
    let media = media_playlist(&playlist).unwrap();
    let segments: Vec<Vec<u8>> = media.segments[..2]
        .iter()
        .map(|s| load(s, &mut HashMap::new()).unwrap())
        .collect();
    let mut seed = 0x9E37_79B9u32;
    let mut random = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for round in 0..150 {
        let mut first = segments[0].clone();
        match round % 3 {
            // Bytes changed here and there.
            0 => {
                for _ in 0..1 + random() % 40 {
                    let at = random() as usize % first.len();
                    first[at] = random() as u8;
                }
            }
            // Cut short.
            1 => first.truncate(random() as usize % first.len()),
            // A run of junk.
            _ => {
                let at = random() as usize % first.len();
                let len = (random() as usize % 2000).min(first.len() - at);
                for b in &mut first[at..at + len] {
                    *b = random() as u8;
                }
            }
        }
        let mut remuxer = Remuxer::new();
        let mut fragments = Vec::new();
        let ok = (|| -> Result<()> {
            remuxer.push(&first)?;
            fragments.extend(remuxer.take_fragments()?);
            remuxer.push(&segments[1])?;
            fragments.extend(remuxer.take_fragments()?);
            Ok(())
        })();
        let tracks = remuxer.tracks();
        if ok.is_ok()
            && let Ok(last) = remuxer.finish()
        {
            fragments.extend(last);
        }
        if let Some(Ok(tracks)) = tracks {
            let joined: Vec<Track> = tracks
                .into_iter()
                .map(|t| Track {
                    init: Init::parse(t.init).unwrap(),
                    duration: 1,
                })
                .collect();
            let (mut mux, _) = Muxer::new(&joined).unwrap();
            for (track, fragment) in fragments {
                mux.fragments(track, &fragment).unwrap();
            }
        }
    }
}
