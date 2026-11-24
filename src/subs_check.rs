//! By hand: the desktop player's subtitles against mpv's, line by line, for
//! a Jellyfin library.
//!
//! For each episode it gets the subtitles and fonts the way the player does
//! (the track your languages choose, fetched from Jellyfin), draws each
//! line's first, middle and last frame, and the frame after it, with
//! [`crate::subs::Renderer`], and has mpv draw the same frames of a black
//! video with the same subtitle file and the fonts mpv would load from the
//! episode. Frames that differ are counted and the worst are saved.
//!
//! ```text
//! SUBS_CHECK_MANIFEST=<items.json> SUBS_CHECK_OUT=<dir> [SUBS_CHECK_JOBS=6] \
//!   cargo test --release --lib subs_check -- --ignored --nocapture
//! ```
//!
//! The manifest lists episodes: `id`, `series`, `season`, `episode`, the
//! video's `w`, `h`, `fps_num`, `fps_den` and `colorspace`, the language
//! `prefs`, and `attachments` as `[index, file name, MIME type]`. Results go
//! to `results.jsonl` (a run picks up where it left off) and `png/`.

use std::collections::{BTreeSet, HashSet};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::servers::jellyfin::{self, Jellyfin, Playable};
use crate::subs::{Drawn, Renderer};

#[derive(Deserialize, Clone)]
struct Item {
    id: String,
    series: String,
    season: Option<i64>,
    episode: Option<i64>,
    w: u32,
    h: u32,
    fps_num: u64,
    fps_den: u64,
    colorspace: String,
    prefs: crate::prefs::LangPrefs,
    attachments: Vec<(u64, String, String)>,
}

#[derive(Serialize, Default)]
struct Outcome {
    id: String,
    series: String,
    season: Option<i64>,
    episode: Option<i64>,
    /// "ass", "plain", "image", "none" or "error".
    kind: String,
    codec: String,
    track: String,
    lines: usize,
    frames: usize,
    /// Frames drawn visibly differently from mpv.
    differ: usize,
    /// Frames off only by rounding.
    rounding: usize,
    /// Colours shifted as VSFilter shows them.
    shifted: bool,
    /// Jellyfin's copy of the styles differs from the file's.
    styles_rewritten: bool,
    /// Of those, frames where only mpv (or only we) showed something.
    only_mpv: usize,
    only_ours: usize,
    /// The largest number of pixels that differed in one frame.
    most_pixels: usize,
    /// Font attachments: the player's, and mpv's.
    fonts_ours: usize,
    fonts_mpv: usize,
    fonts_skipped: Vec<String>,
    header: Vec<(String, String)>,
    worst: Vec<Mismatch>,
    error: Option<String>,
    secs: f64,
}

#[derive(Serialize, Clone)]
struct Mismatch {
    t_ms: i64,
    pixels: usize,
    big: usize,
    ours_lit: usize,
    mpv_lit: usize,
    lines: Vec<String>,
    picture: Option<String>,
}

/// Both pictures where they differ (ours, mpv's: RGB), and its size.
type Crop = (Vec<u8>, Vec<u8>, u32, u32);

/// A pixel differs if a channel is off by more than this.
const NEAR: u8 = 12;
/// …and differs a lot past this.
const FAR: u8 = 40;
/// A frame differs visibly if this many pixels differ a lot, or a hundred
/// times as many a little (a colour off across a sign); fewer, a little, is
/// rounding (stacked soft glows), counted apart.
const FRAME_PIXELS: usize = 40;
/// The most frames one episode is checked at.
const MAX_FRAMES: usize = 60_000;

/// mpv's font attachments: by MIME type, else by extension (sd_ass.c).
fn mpv_font(name: &str, mime: &str) -> bool {
    const MIMES: [&str; 9] = [
        "application/x-truetype-font",
        "application/vnd.ms-opentype",
        "application/x-font-ttf",
        "application/x-font",
        "application/font-sfnt",
        "font/collection",
        "font/otf",
        "font/sfnt",
        "font/ttf",
    ];
    let lower = name.to_ascii_lowercase();
    MIMES.contains(&mime)
        || [".ttf", ".ttc", ".otf", ".otc"]
            .iter()
            .any(|e| lower.ends_with(e))
}

/// Frame `k`'s time in whole milliseconds (as both players round it).
fn pts_ms(k: i64, num: u64, den: u64) -> i64 {
    let (num, den) = (num as i128, den as i128);
    ((k as i128 * den * 2000 + num) / (2 * num)) as i64
}

/// The first frame shown at or after `ms`.
fn frame_at(ms: i64, num: u64, den: u64) -> i64 {
    let mut k = (ms as i128 * num as i128 / (den as i128 * 1000)) as i64 - 1;
    while pts_ms(k, num, den) < ms {
        k += 1;
    }
    k.max(0)
}

/// The frames to check: each line's first, middle and last frame, and the
/// one after it.
fn frames_to_check(track: &crate::mkv::SubTrack, num: u64, den: u64) -> Vec<i64> {
    let mut ks = BTreeSet::new();
    for e in &track.events {
        let first = frame_at(e.start_ms, num, den);
        let after = frame_at(e.start_ms + e.duration_ms, num, den);
        if after <= first {
            continue; // never on a frame
        }
        let last = after - 1;
        ks.extend([first, (first + last) / 2, last, after]);
    }
    ks.into_iter()
        .map(|k| pts_ms(k, num, den))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A line, shortly: "12 Default: Hello there".
fn describe(track: &crate::mkv::SubTrack, i: usize, ass: bool) -> String {
    let data = String::from_utf8_lossy(&track.events[i].data);
    let (style, text) = if ass {
        let f: Vec<&str> = data.splitn(9, ',').collect();
        (
            f.get(2).copied().unwrap_or(""),
            f.get(8).copied().unwrap_or(""),
        )
    } else {
        ("", &*data)
    };
    let text: String = text.chars().take(90).collect();
    format!("{i} {style}: {text}")
}

/// Black frames for mpv, made once per size and colour space.
fn black(work: &Path, w: u32, h: u32, cs: &str) -> Result<PathBuf> {
    static MADE: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
    let cs = match cs {
        "bt709" | "bt470bg" | "smpte170m" | "bt2020nc" => cs,
        _ => "",
    };
    let path = work.join(format!(
        "black-{w}x{h}-{}.264",
        if cs.is_empty() { "untagged" } else { cs }
    ));
    let mut made = MADE.lock().unwrap();
    let made = made.get_or_insert_with(HashSet::new);
    if made.contains(&path) || path.exists() {
        made.insert(path.clone());
        return Ok(path);
    }
    let tmp = path.with_extension("tmp.264");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=black:s={w}x{h}:r=24"))
        .args([
            "-frames:v",
            &MAX_FRAMES.to_string(),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .args(["-pix_fmt", "yuv420p"]);
    if !cs.is_empty() {
        let (prim, trc, mat) = match cs {
            "bt709" => (1, 1, 1),
            "bt470bg" => (5, 6, 5),
            "smpte170m" => (6, 6, 6),
            _ => (9, 14, 9), // bt2020nc
        };
        cmd.args([
            "-bsf:v",
            &format!(
                "h264_metadata=colour_primaries={prim}:transfer_characteristics={trc}:matrix_coefficients={mat}"
            ),
        ]);
    }
    let status = cmd.args(["-f", "h264"]).arg(&tmp).status()?;
    if !status.success() {
        bail!("ffmpeg couldn't make black frames");
    }
    std::fs::rename(&tmp, &path)?;
    made.insert(path.clone());
    Ok(path)
}

/// Up to three tries (Jellyfin sometimes drops a connection).
fn retry<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut tries = 0;
    loop {
        match f() {
            Err(_) if tries < 2 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            r => return r,
        }
    }
}

struct Shared {
    out: PathBuf,
    work: PathBuf,
    results: Mutex<std::fs::File>,
}

fn check(j: &Jellyfin, rt: &tokio::runtime::Runtime, item: &Item, shared: &Shared) -> Outcome {
    let started = Instant::now();
    let mut o = Outcome {
        id: item.id.clone(),
        series: item.series.clone(),
        season: item.season,
        episode: item.episode,
        ..Default::default()
    };
    if let Err(e) = check_into(j, rt, item, shared, &mut o) {
        o.kind = "error".into();
        o.error = Some(format!("{e:#}"));
    }
    o.secs = started.elapsed().as_secs_f64();
    o
}

fn check_into(
    j: &Jellyfin,
    rt: &tokio::runtime::Runtime,
    item: &Item,
    shared: &Shared,
    o: &mut Outcome,
) -> Result<()> {
    let p: Playable = retry(|| rt.block_on(j.playable(&item.id)))?;
    // The track the player shows (as in open_stream).
    let described = crate::gst_video::describe_info(&jellyfin::track_info(&p), Some(&item.prefs));
    let Some((number, codec_id)) = described
        .subs
        .and_then(|i| described.sub_tracks.get(i).cloned())
    else {
        o.kind = "none".into();
        return Ok(());
    };
    let stream = p
        .streams
        .iter()
        .find(|s| s.kind == "Subtitle" && s.index == number)
        .context("the chosen track isn't in the file")?;
    o.codec = stream.codec.clone();
    o.track = format!(
        "{} {}",
        stream.language.as_deref().unwrap_or("und"),
        stream.title.as_deref().unwrap_or("")
    );
    if !crate::gst_video::is_text_subtitle(&codec_id) {
        o.kind = "image".into();
        return Ok(());
    }
    let ass = matches!(stream.codec.as_str(), "ass" | "ssa");
    o.kind = if ass { "ass" } else { "plain" }.into();

    // The subtitles, as the player gets them (text_subtitles: Jellyfin's
    // lines, the file's own styles).
    let bytes = retry(|| rt.block_on(j.subtitle(&p, stream)))?;
    let track = retry(|| {
        rt.block_on(j.text_subtitles_only(&p))
            .tracks
            .remove(&number)
            .context("the player didn't get the track")
    })?;
    // mpv gets what the file has: its styles, and the same lines (Jellyfin
    // keeps those as they are).
    let mut bytes = bytes;
    if ass && let Some(original) = rt.block_on(j.subtitle_headers(&p)).get(&number) {
        let served = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
        let original = String::from_utf8_lossy(original).replace("\r\n", "\n");
        let (head, lines) = served.split_once("[Events]").unwrap_or((&served, ""));
        let original_head = original.split("[Events]").next().unwrap_or_default();
        o.styles_rewritten = head.trim() != original_head.trim();
        bytes = format!("{}\n\n[Events]{lines}", original_head.trim_end()).into_bytes();
    }
    o.lines = track.events.len();
    if ass {
        for line in String::from_utf8_lossy(&track.codec_private).lines() {
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                if [
                    "YCbCr Matrix",
                    "ScaledBorderAndShadow",
                    "PlayResX",
                    "PlayResY",
                    "LayoutResX",
                    "LayoutResY",
                    "WrapStyle",
                    "ScriptType",
                ]
                .contains(&k)
                {
                    o.header.push((k.into(), v.trim().into()));
                }
            }
        }
    }
    let dir = shared.work.join(&item.id);
    let fonts_dir = dir.join("fonts");
    std::fs::create_dir_all(&fonts_dir)?;
    let mut subs = crate::mkv::TextSubtitles::default();
    let mut names = HashSet::new();
    // mpv's fonts, attached to its video as the episode has them.
    let mut attach: Vec<(PathBuf, String, String)> = Vec::new();
    for (index, name, mime) in &item.attachments {
        let ours = jellyfin::is_font(name);
        let theirs = mpv_font(name, mime);
        if !ours && !theirs {
            continue;
        }
        let data = retry(|| rt.block_on(j.attachment(&p, *index)))?;
        if ours {
            subs.fonts.push((name.clone(), data.clone()));
            o.fonts_ours += 1;
        } else {
            o.fonts_skipped.push(format!("{name} ({mime})"));
        }
        if theirs {
            let mut file = name.replace(['/', '\\'], "_");
            if file.is_empty() || !names.insert(file.clone()) {
                file = format!("{index}-{file}");
            }
            std::fs::write(fonts_dir.join(&file), &data)?;
            attach.push((fonts_dir.join(file), name.clone(), mime.clone()));
            o.fonts_mpv += 1;
        }
    }
    subs.tracks.insert(number, track.clone());
    let sub_file = dir.join(if ass { "sub.ass" } else { "sub.srt" });
    std::fs::write(&sub_file, &bytes)?;

    // The frames, and a black video with exactly those frames for mpv.
    let mut times = frames_to_check(&track, item.fps_num, item.fps_den);
    if times.len() > MAX_FRAMES {
        let step = times.len().div_ceil(MAX_FRAMES);
        times = times.into_iter().step_by(step).collect();
    }
    o.frames = times.len();
    if times.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(());
    }
    let (w, h) = (item.w, item.h);
    let black = black(&shared.work, w, h, &item.colorspace)?;
    let cut = dir.join("cut.264");
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&black)
        .args(["-frames:v", &times.len().to_string(), "-c", "copy"])
        .arg(&cut)
        .status()?
        .success();
    if !ok {
        bail!("ffmpeg couldn't cut the black frames");
    }
    let ts = dir.join("ts.txt");
    let mut list = String::from("# timestamp format v2\n");
    for t in &times {
        list += &format!("{t}\n");
    }
    std::fs::write(&ts, list)?;
    let video = dir.join("black.mkv");
    // The subtitles and fonts go in the file, as in the episode: mpv reads
    // them with the video (an external file loads late in encode mode).
    let mut mkvmerge = Command::new("mkvmerge");
    mkvmerge
        .arg("-q")
        .arg("-o")
        .arg(&video)
        .arg("--timestamps")
        .arg(format!("0:{}", ts.display()))
        .arg(&cut)
        .args(["--sub-charset", "0:UTF-8"])
        .arg(&sub_file);
    for (path, name, mime) in &attach {
        let mime = if mime.is_empty() {
            "application/octet-stream"
        } else {
            mime
        };
        mkvmerge
            .args([
                "--attachment-mime-type",
                mime,
                "--attachment-name",
                name,
                "--attach-file",
            ])
            .arg(path);
    }
    let ok = mkvmerge.status()?.code().is_some_and(|c| c < 2);
    if !ok {
        bail!("mkvmerge couldn't make the video");
    }
    let log = std::fs::File::create(dir.join("mpv.log"))?;
    let mut mpv = Command::new("mpv")
        .args([
            "--no-config",
            "--really-quiet",
            "--msg-level=all=warn",
            "--no-audio",
        ])
        .args(["--osd-level=0", "--sub-auto=no", "--sid=1"])
        .arg(format!(
            "--sub-fonts-dir={}",
            shared.work.join("no-fonts").display()
        ))
        .arg(&video)
        .args([
            "--vf=format=rgb24",
            "--o=-",
            "--of=rawvideo",
            "--ovc=rawvideo",
        ])
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()?;
    let mut theirs = BufReader::with_capacity(1 << 22, mpv.stdout.take().unwrap());

    // Ours, frame by frame in order, as the player draws them.
    let mut r = Renderer::new(&subs)?;
    // As mpv sees the black video: tagged like the episode, else guessed.
    let matrix = match item.colorspace.as_str() {
        "bt709" => crate::subs::VideoMatrix::Bt709,
        "bt470bg" | "smpte170m" => crate::subs::VideoMatrix::Bt601,
        "" => crate::subs::VideoMatrix::guess((w, h)),
        _ => crate::subs::VideoMatrix::Other,
    };
    r.set_track(subs.tracks.get(&number), (w, h), matrix);
    o.shifted = ass
        && matrix == crate::subs::VideoMatrix::Bt709
        && o.header
            .iter()
            .all(|(k, v)| k != "YCbCr Matrix" || v.eq_ignore_ascii_case("TV.601") || v.is_empty());
    let size = (w * h) as usize;
    let mut overlay: Option<Vec<u8>> = None;
    let mut frame = vec![0u8; size * 3];
    let mut worst: Vec<(usize, Mismatch, Option<Crop>)> = Vec::new();
    for &t in &times {
        match r.draw(t, (w, h), 0.0) {
            Drawn::Unchanged => {}
            Drawn::Empty => overlay = None,
            Drawn::Image(px) => overlay = Some(px),
        }
        theirs
            .read_exact(&mut frame)
            .with_context(|| format!("mpv stopped before {t} ms"))?;
        let (mut near, mut far, mut ours_lit, mut mpv_lit) = (0usize, 0usize, 0usize, 0usize);
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for i in 0..size {
            let m = &frame[i * 3..i * 3 + 3];
            let ours = match &overlay {
                Some(px) => {
                    let p = &px[i * 4..i * 4 + 4];
                    let a = p[3] as u32;
                    [
                        ((p[0] as u32 * a + 127) / 255) as u8,
                        ((p[1] as u32 * a + 127) / 255) as u8,
                        ((p[2] as u32 * a + 127) / 255) as u8,
                    ]
                }
                None => [0, 0, 0],
            };
            if ours.iter().any(|&c| c > 8) {
                ours_lit += 1;
            }
            if m.iter().any(|&c| c > 8) {
                mpv_lit += 1;
            }
            let d = (0..3).map(|c| ours[c].abs_diff(m[c])).max().unwrap();
            if d > NEAR {
                near += 1;
                if d > FAR {
                    far += 1;
                }
                let (x, y) = ((i as u32) % w, (i as u32) / w);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        let differs = if ass {
            if far <= FRAME_PIXELS && near > FRAME_PIXELS * 10 && near <= FRAME_PIXELS * 100 {
                o.rounding += 1;
            }
            far > FRAME_PIXELS || near > FRAME_PIXELS * 100
        } else {
            // Plain text is styled the player's own way: only whether it
            // shows is compared.
            (ours_lit > 50) != (mpv_lit > 50)
        };
        if !differs {
            continue;
        }
        o.differ += 1;
        if ours_lit <= 50 && mpv_lit > 50 {
            o.only_mpv += 1;
        }
        if mpv_lit <= 50 && ours_lit > 50 {
            o.only_ours += 1;
        }
        o.most_pixels = o.most_pixels.max(near);
        let score = far * 10 + near;
        if worst.len() < 3 || worst.iter().any(|(s, ..)| *s < score) {
            let lines = (0..track.events.len())
                .filter(|&i| {
                    let e = &track.events[i];
                    e.start_ms <= t && t < e.start_ms + e.duration_ms
                })
                .take(12)
                .map(|i| describe(&track, i, ass))
                .collect();
            // Both pictures, cut to where they differ (with a margin).
            let m = 24;
            let (cx0, cy0) = (x0.saturating_sub(m), y0.saturating_sub(m));
            let (cx1, cy1) = ((x1 + m).min(w - 1), (y1 + m).min(h - 1));
            let (cw, ch) = (cx1 - cx0 + 1, cy1 - cy0 + 1);
            let mut a = Vec::with_capacity((cw * ch * 3) as usize);
            let mut b = Vec::with_capacity((cw * ch * 3) as usize);
            for y in cy0..=cy1 {
                for x in cx0..=cx1 {
                    let i = (y * w + x) as usize;
                    match &overlay {
                        Some(px) => {
                            let p = &px[i * 4..i * 4 + 4];
                            let al = p[3] as u32;
                            a.extend((0..3).map(|c| ((p[c] as u32 * al + 127) / 255) as u8));
                        }
                        None => a.extend([0, 0, 0]),
                    }
                    b.extend_from_slice(&frame[i * 3..i * 3 + 3]);
                }
            }
            worst.push((
                score,
                Mismatch {
                    t_ms: t,
                    pixels: near,
                    big: far,
                    ours_lit,
                    mpv_lit,
                    lines,
                    picture: None,
                },
                Some((a, b, cw, ch)),
            ));
            worst.sort_by_key(|w| std::cmp::Reverse(w.0));
            worst.truncate(3);
        }
    }
    let _ = mpv.wait();
    for (_, mut m, crop) in worst {
        if let Some((a, b, cw, ch)) = crop {
            let mut img = image::RgbImage::new(cw * 2 + 8, ch);
            for y in 0..ch {
                for x in 0..cw {
                    let i = ((y * cw + x) * 3) as usize;
                    img.put_pixel(x, y, image::Rgb([a[i], a[i + 1], a[i + 2]]));
                    img.put_pixel(cw + 8 + x, y, image::Rgb([b[i], b[i + 1], b[i + 2]]));
                }
                for x in cw..cw + 8 {
                    img.put_pixel(x, y, image::Rgb([255, 0, 255]));
                }
            }
            let name: String = format!(
                "{}-s{}e{}-{}ms.png",
                item.series,
                item.season.unwrap_or(0),
                item.episode.unwrap_or(0),
                m.t_ms
            )
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
            let path = shared.out.join("png").join(&name);
            if img.save(&path).is_ok() {
                m.picture = Some(name);
            }
        }
        o.worst.push(m);
    }
    if o.differ == 0 {
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        // Keep the subtitles and mpv's log for a closer look; not the video.
        let _ = std::fs::remove_file(&video);
        let _ = std::fs::remove_file(&cut);
    }
    Ok(())
}

#[test]
#[ignore = "needs SUBS_CHECK_MANIFEST, a Jellyfin sign-in and mpv"]
fn matches_mpv() {
    let (Some(manifest), Some(out)) = (
        std::env::var_os("SUBS_CHECK_MANIFEST"),
        std::env::var_os("SUBS_CHECK_OUT"),
    ) else {
        return;
    };
    let items: Vec<Item> = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    let out = PathBuf::from(out);
    let work = out.join("work");
    std::fs::create_dir_all(out.join("png")).unwrap();
    std::fs::create_dir_all(work.join("no-fonts")).unwrap();
    let state = std::env::var_os("SUBS_CHECK_STATE").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/share/ichioshi"),
        PathBuf::from,
    );
    let account = crate::servers::Config::load(&state).jellyfin;
    assert!(account.signed_in(), "not signed in to Jellyfin");
    let client = reqwest::Client::builder()
        .user_agent("ichioshi-subs-check")
        .build()
        .unwrap();
    let j = Jellyfin::new(&account, &client);
    let results_path = out.join("results.jsonl");
    let done: HashSet<String> = std::fs::read_to_string(&results_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["id"].as_str().map(str::to_owned))
        .collect();
    let todo: Vec<&Item> = items.iter().filter(|i| !done.contains(&i.id)).collect();
    let limit: usize = std::env::var("SUBS_CHECK_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    let todo = &todo[..todo.len().min(limit)];
    let jobs: usize = std::env::var("SUBS_CHECK_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    println!(
        "{} to check ({} done before), {jobs} at a time",
        todo.len(),
        done.len()
    );
    let shared = Shared {
        out: out.clone(),
        work,
        results: Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&results_path)
                .unwrap(),
        ),
    };
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                loop {
                    let n = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = todo.get(n) else { break };
                    let o = check(&j, &rt, item, &shared);
                    println!(
                        "[{}/{}] {} s{:?}e{:?}: {} {} lines, {} frames, {} differ{} ({:.0}s)",
                        n + 1,
                        todo.len(),
                        o.series,
                        o.season,
                        o.episode,
                        o.kind,
                        o.lines,
                        o.frames,
                        o.differ,
                        o.error
                            .as_deref()
                            .map(|e| format!(", error: {e}"))
                            .unwrap_or_default(),
                        o.secs
                    );
                    let mut f = shared.results.lock().unwrap();
                    let _ = writeln!(f, "{}", serde_json::to_string(&o).unwrap());
                }
            });
        }
    });
}

#[test]
fn frame_times_round_like_the_players() {
    // 23.976 fps: frame 1 is 41.708 ms, frame 3 is 125.125 ms.
    assert_eq!(pts_ms(1, 24000, 1001), 42);
    assert_eq!(pts_ms(3, 24000, 1001), 125);
    assert_eq!(frame_at(42, 24000, 1001), 1);
    assert_eq!(frame_at(43, 24000, 1001), 2);
    assert_eq!(frame_at(0, 24000, 1001), 0);
    let track = crate::mkv::SubTrack {
        codec_id: "S_TEXT/ASS".into(),
        codec_private: Vec::new(),
        events: vec![crate::mkv::SubEvent {
            start_ms: 1000,
            duration_ms: 500,
            data: Vec::new(),
        }],
    };
    // Frames 24 (1001 ms) to 35 (1460 ms) show it; 36 (1502 ms) doesn't.
    assert_eq!(
        frames_to_check(&track, 24000, 1001),
        vec![1001, 1210, 1460, 1502]
    );
}

/// By hand: one frame of a subtitle file, drawn as the player draws it, to
/// `SUBS_FRAME_OUT` (PNG over black). `SUBS_FRAME_SUB=<file.ass|.srt>`,
/// `SUBS_FRAME_FONTS=<dir>`, `SUBS_FRAME_T=<ms>[,<ms>…]` (drawn in order,
/// the last saved), `SUBS_FRAME_SIZE=1920x1080`.
#[test]
#[ignore = "needs SUBS_FRAME_SUB"]
fn draws_one_frame() {
    let Some(sub) = std::env::var_os("SUBS_FRAME_SUB") else {
        return;
    };
    let sub = PathBuf::from(sub);
    let bytes = std::fs::read(&sub).unwrap();
    let codec = if sub.extension().is_some_and(|e| e == "ass") {
        "ass"
    } else {
        "subrip"
    };
    let track = jellyfin::subtitle_track(codec, &bytes);
    let mut subs = crate::mkv::TextSubtitles::default();
    if let Some(dir) = std::env::var_os("SUBS_FRAME_FONTS") {
        for f in std::fs::read_dir(dir).unwrap().flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if jellyfin::is_font(&name) {
                subs.fonts.push((name, std::fs::read(f.path()).unwrap()));
            }
        }
    }
    subs.tracks.insert(1, track);
    let (w, h) = std::env::var("SUBS_FRAME_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1920u32, 1080u32));
    let mut r = Renderer::new(&subs).unwrap();
    r.set_track(
        subs.tracks.get(&1),
        (w, h),
        crate::subs::VideoMatrix::guess((w, h)),
    );
    let mut overlay = None;
    for t in std::env::var("SUBS_FRAME_T").unwrap().split(',') {
        match r.draw(t.trim().parse().unwrap(), (w, h), 0.0) {
            Drawn::Unchanged => println!("{t}: unchanged"),
            Drawn::Empty => {
                println!("{t}: empty");
                overlay = None
            }
            Drawn::Image(px) => {
                println!("{t}: drawn");
                overlay = Some(px)
            }
        }
    }
    let mut img = image::RgbImage::new(w, h);
    if let Some(px) = overlay {
        for (i, p) in px.chunks(4).enumerate() {
            let a = p[3] as u32;
            let c = |v: u8| ((v as u32 * a + 127) / 255) as u8;
            img.put_pixel(
                i as u32 % w,
                i as u32 / w,
                image::Rgb([c(p[0]), c(p[1]), c(p[2])]),
            );
        }
    }
    img.save(std::env::var("SUBS_FRAME_OUT").unwrap()).unwrap();
}
