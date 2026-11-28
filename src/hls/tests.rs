//! The downloader against a mock server serving HLS that ffmpeg makes.
// The mock lock is held for the whole test on purpose.
#![allow(clippy::await_holding_lock)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;
use crate::mock_http::{Resp, mock};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// ffmpeg's HLS of 6 s of test video and audio in `dir` (`video` codec
/// args), 2 s segments, AES-128 encrypted with `key` if given.
fn make_hls(dir: &Path, video: &[&str], key: Option<&[u8; 16]>) -> bool {
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=25:duration=6",
    ])
    .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
    .args(video)
    .args(["-c:a", "aac", "-b:a", "64k", "-f", "hls", "-hls_time", "2"])
    .args(["-hls_playlist_type", "vod", "-hls_segment_type", "mpegts"])
    .arg("-hls_segment_filename")
    .arg(dir.join("seg%d.ts"));
    if let Some(key) = key {
        std::fs::write(dir.join("k.key"), key).unwrap();
        std::fs::write(
            dir.join("k.info"),
            format!("/keys/k.key\n{}\n", dir.join("k.key").display()),
        )
        .unwrap();
        cmd.arg("-hls_key_info_file").arg(dir.join("k.info"));
    }
    cmd.arg(dir.join("media.m3u8"))
        .status()
        .is_ok_and(|s| s.success())
}

fn probe(file: &Path, entries: &str) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            entries,
            "-of",
            "compact=p=0:nk=1",
        ])
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Serves `dir`'s files; counts requests carrying `referer`.
async fn serve(dir: &Path, referer: &'static str) -> (String, Arc<AtomicUsize>) {
    let dir = dir.to_owned();
    let refs = Arc::new(AtomicUsize::new(0));
    let counted = refs.clone();
    let base = mock(move |req| {
        if req.headers.get("referer").map(String::as_str) == Some(referer) {
            counted.fetch_add(1, Ordering::SeqCst);
        }
        let path = req.target.split('?').next().unwrap();
        let name = path.trim_start_matches("/keys/").trim_start_matches('/');
        match std::fs::read(dir.join(name)) {
            Ok(bytes) => Resp::binary(200, bytes),
            Err(_) => Resp::html(404, "missing"),
        }
    })
    .await;
    (base, refs)
}

#[tokio::test]
async fn downloads_encrypted_ts_with_subtitles_into_an_mp4() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: needs ffmpeg and ffprobe");
        return;
    }
    let src = tempfile::tempdir().unwrap();
    let key = *b"0123456789abcdef";
    assert!(make_hls(
        src.path(),
        &["-c:v", "libx264", "-g", "50", "-bf", "2"],
        Some(&key)
    ));
    // Where the stream starts on its clock: the first segment's, decrypted.
    let playlist = std::fs::read_to_string(src.path().join("media.m3u8")).unwrap();
    let base: Url = "http://x/media.m3u8".parse().unwrap();
    let Ok(m3u8::Playlist::Media(media)) = m3u8::parse(&playlist, &base) else {
        panic!("ffmpeg's playlist")
    };
    let seg0 = std::fs::read(src.path().join("seg0.ts")).unwrap();
    let plain = aes128::decrypt(&key, &media.segments[0].iv().unwrap(), &seg0).unwrap();
    std::fs::write(src.path().join("plain0.ts"), plain).unwrap();
    let start: f64 = probe(&src.path().join("plain0.ts"), "format=start_time")
        .trim()
        .parse()
        .unwrap();
    std::fs::write(
        src.path().join("subs.vtt"),
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\n\n00:00:03.000 --> 00:00:04.500\n<i>Hello</i> there\n",
    )
    .unwrap();
    std::fs::write(
        src.path().join("subs.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:6.0,\nsubs.vtt\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    std::fs::write(
        src.path().join("master.m3u8"),
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"subs.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=150000,RESOLUTION=160x120,SUBTITLES=\"subs\"\nmissing-low.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=500000,RESOLUTION=320x240,CODECS=\"avc1.64000d,mp4a.40.2\",SUBTITLES=\"subs\"\nmedia.m3u8\n",
    )
    .unwrap();
    let (base, referers) = serve(src.path(), "https://site.example/").await;
    let _net = crate::mock_http::mocked();
    let client = reqwest::Client::new();
    let s = Session::new(&client, "test");
    let url: Url = format!("{base}/master.m3u8").parse().unwrap();
    assert_eq!(
        resolution(&s, &url, None).await.unwrap(),
        Some(240),
        "the best variant's"
    );
    check(&s, &url, Some("https://site.example/"))
        .await
        .unwrap();

    let out = tempfile::tempdir().unwrap();
    let stem = out.path().join("Show 2.5 - 01");
    let mut last = (0, None);
    let file = download(
        &s,
        &url,
        Some("https://site.example/"),
        &stem,
        &AtomicBool::new(false),
        |done, total| last = (done, total),
    )
    .await
    .unwrap();
    assert_eq!(file, out.path().join("Show 2.5 - 01.mp4"));
    assert!(!out.path().join("Show 2.5 - 01.mp4.part").exists());
    assert!(last.0 > 0 && last.1 == Some(last.0), "{last:?}");
    assert!(
        referers.load(Ordering::SeqCst) >= 5,
        "the site's Referer sent"
    );
    let streams = probe(&file, "stream=codec_name");
    assert!(
        streams.contains("h264") && streams.contains("aac"),
        "{streams}"
    );
    let duration: f64 = probe(&file, "format=duration").trim().parse().unwrap();
    assert!((duration - 6.0).abs() < 0.3, "{duration}");
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&file)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        decoded.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );

    // The subtitles: an ASS beside it, timed to the video (whose 0 is where
    // the stream started on its clock).
    let ass = std::fs::read_to_string(out.path().join("Show 2.5 - 01.en.ass")).unwrap();
    let line = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
    assert!(line.ends_with("{\\i1}Hello{\\i0} there"), "{line}");
    let at = line.split(',').nth(1).unwrap();
    let (m, sec) = at.rsplit_once(':').unwrap();
    let at =
        m.split(':').nth(1).unwrap().parse::<f64>().unwrap() * 60.0 + sec.parse::<f64>().unwrap();
    assert!((at - (3.0 - start)).abs() < 0.2, "{at} vs {start}");
    assert_eq!(
        crate::sidecar::find(&file).len(),
        1,
        "the player finds the subtitles"
    );
}

#[tokio::test]
async fn keeps_codecs_an_mp4_isnt_made_of_as_ts() {
    if !have("ffmpeg") {
        eprintln!("skipping: needs ffmpeg");
        return;
    }
    let src = tempfile::tempdir().unwrap();
    assert!(make_hls(src.path(), &["-c:v", "mpeg2video"], None));
    let (base, _) = serve(src.path(), "x").await;
    let _net = crate::mock_http::mocked();
    let client = reqwest::Client::new();
    let s = Session::new(&client, "test");
    let out = tempfile::tempdir().unwrap();
    let file = download(
        &s,
        &format!("{base}/media.m3u8").parse().unwrap(),
        None,
        &out.path().join("Show - 01"),
        &AtomicBool::new(false),
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(file, out.path().join("Show - 01.ts"));
    let mut whole = Vec::new();
    for i in 0..3 {
        whole.extend(std::fs::read(src.path().join(format!("seg{i}.ts"))).unwrap());
    }
    assert_eq!(
        std::fs::read(&file).unwrap(),
        whole,
        "the segments, as they came"
    );
}

#[tokio::test]
async fn cancels_and_cleans_up() {
    if !have("ffmpeg") {
        eprintln!("skipping: needs ffmpeg");
        return;
    }
    let src = tempfile::tempdir().unwrap();
    assert!(make_hls(src.path(), &["-c:v", "libx264"], None));
    let (base, _) = serve(src.path(), "x").await;
    let _net = crate::mock_http::mocked();
    let client = reqwest::Client::new();
    let s = Session::new(&client, "test");
    let out = tempfile::tempdir().unwrap();
    // Cancelled once the first segment is in, part way through.
    let cancel = Arc::new(AtomicBool::new(false));
    let set = cancel.clone();
    let mut reports = 0;
    let err = download(
        &s,
        &format!("{base}/media.m3u8").parse().unwrap(),
        None,
        &out.path().join("Show - 01"),
        &cancel,
        |_, _| {
            reports += 1;
            set.store(true, Ordering::SeqCst);
        },
    )
    .await
    .unwrap_err();
    assert!(err.is::<Cancelled>(), "{err:#}");
    assert_eq!(reports, 1, "stopped after the first segment");
    assert_eq!(
        std::fs::read_dir(out.path()).unwrap().count(),
        0,
        "nothing left behind"
    );
}
