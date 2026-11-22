//! Video through GStreamer, for the iced GUI: one `playbin` whose frames
//! (NV12, for iced_video_player's GPU widget) already have styled
//! subtitles drawn in by libass (`assrender`, which playbin inserts).
//!
//! Tracks come from the file itself (Matroska: names, languages, default
//! flags), matched to playbin's streams in file order.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::player::{Chapter, Track, TrackKind, part_of};

/// playbin's flags, with and without subtitles.
const FLAGS: &str = "soft-colorbalance+deinterlace+soft-volume+audio+video";
const FLAGS_TEXT: &str = "soft-colorbalance+deinterlace+soft-volume+text+audio+video";

/// What the frames are made into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frames {
    /// For iced_video_player (it converts to RGB on the GPU).
    Nv12,
    /// For tests and thumbnails.
    #[cfg(test)]
    Rgba,
}

/// A playbin for `path`, and the appsink its frames arrive at.
pub fn pipeline(path: &Path, frames: Frames) -> Result<(gst::Pipeline, gst_app::AppSink)> {
    gst::init()?;
    let uri = url::Url::from_file_path(path)
        .map_err(|()| anyhow!("{} isn't a full path", path.display()))?;
    let format = match frames {
        Frames::Nv12 => "NV12",
        #[cfg(test)]
        Frames::Rgba => "RGBA",
    };
    // Dithered: 10-bit anime down to 8 bits without banding.
    let sink = gst::parse::bin_from_description(
        &format!(
            "videoscale ! videoconvert dither=floyd-steinberg ! \
             appsink name=frames drop=true max-buffers=2 caps=video/x-raw,format={format},pixel-aspect-ratio=1/1"
        ),
        true,
    )?;
    let appsink = sink
        .by_name("frames")
        .context("no appsink")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow!("not an appsink"))?;
    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", uri.as_str())
        .property("video-sink", &sink)
        .build()
        .context("GStreamer's playbin isn't available")?;
    playbin.set_property_from_str("flags", FLAGS_TEXT);
    // Tests make no sound.
    #[cfg(test)]
    playbin.set_property(
        "audio-sink",
        gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()?,
    );
    let pipeline = playbin
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("playbin isn't a pipeline"))?;
    Ok((pipeline, appsink))
}

/// Tracks and chapters, from the file (Matroska), as the player lists them:
/// a track's `id` is its index among playbin's streams of that kind.
pub fn describe(path: &Path) -> (Vec<Track>, Vec<Chapter>, Option<usize>, Option<usize>) {
    let Ok(info) = crate::mkv::probe(path) else {
        return (Vec::new(), Vec::new(), None, None);
    };
    let mut tracks = Vec::new();
    let (mut audio_i, mut sub_i) = (0i64, 0i64);
    let (mut audio, mut subs) = (None, None);
    for t in &info.tracks {
        let (kind, index) = match t.kind {
            crate::mkv::TrackKind::Audio => {
                audio_i += 1;
                (TrackKind::Audio, audio_i - 1)
            }
            crate::mkv::TrackKind::Subtitles => {
                sub_i += 1;
                (TrackKind::Sub, sub_i - 1)
            }
            _ => continue,
        };
        // The file's defaults (the library sets them from your languages).
        if t.default {
            match kind {
                TrackKind::Audio if audio.is_none() => audio = Some(index as usize),
                TrackKind::Sub if subs.is_none() => subs = Some(index as usize),
                _ => {}
            }
        }
        tracks.push(Track {
            id: index,
            kind,
            title: Some(t.name.clone()).filter(|n| !n.is_empty()),
            lang: Some(crate::remux::normalize_language(&t.language)),
            codec: Some(
                crate::mkv::codec_name(&t.codec_id)
                    .to_lowercase()
                    .replace('-', ""),
            ),
            channels: t.channels.map(|c| c as i64),
            default: t.default,
            forced: t.forced,
            external: false,
            selected: false,
        });
    }
    if audio.is_none() && audio_i > 0 {
        audio = Some(0);
    }
    let chapters = info
        .chapters
        .iter()
        .map(|(start, title)| Chapter {
            part: part_of(title),
            title: title.clone(),
            start: *start,
        })
        .collect();
    (tracks, chapters, audio, subs)
}

pub fn set_audio(pipeline: &gst::Pipeline, index: usize) {
    pipeline.set_property("current-audio", index as i32);
}

/// Show subtitle track `index`, or none.
pub fn set_subtitles(pipeline: &gst::Pipeline, index: Option<usize>) {
    match index {
        Some(i) => {
            pipeline.set_property_from_str("flags", FLAGS_TEXT);
            pipeline.set_property("current-text", i as i32);
        }
        None => pipeline.set_property_from_str("flags", FLAGS),
    }
}

/// One frame at `at` seconds, as RGBA (tests, comparisons).
#[cfg(test)]
pub fn frame_at(path: &Path, at: f64, subtitles: Option<usize>) -> Result<image::RgbaImage> {
    let (pipeline, sink) = pipeline(path, Frames::Rgba)?;
    pipeline.set_property("mute", true);
    set_subtitles(&pipeline, subtitles);
    let result = (|| {
        pipeline.set_state(gst::State::Paused)?;
        let (r, _, _) = pipeline.state(gst::ClockTime::from_seconds(10));
        r?;
        pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_nseconds((at * 1e9) as u64),
        )?;
        let (r, _, _) = pipeline.state(gst::ClockTime::from_seconds(10));
        r?;
        let sample = sink.pull_preroll().map_err(|e| anyhow!("no frame: {e}"))?;
        let caps = sample.caps().context("no caps")?;
        let info = gstreamer_video::VideoInfo::from_caps(caps)?;
        let buffer = sample.buffer().context("no buffer")?;
        let map = buffer.map_readable()?;
        let (w, h, stride) = (info.width(), info.height(), info.stride()[0] as usize);
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h as usize {
            pixels.extend_from_slice(&map[y * stride..y * stride + w as usize * 4]);
        }
        image::RgbaImage::from_raw(w, h, pixels).context("frame size")
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Whether GStreamer (with playbin and libass) is there; tests skip without.
#[cfg(test)]
pub fn available() -> bool {
    gst::init().is_ok()
        && ["playbin", "assrender", "matroskademux"]
            .iter()
            .all(|e| gst::ElementFactory::find(e).is_some())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 1080p clip with typeset subtitles (a rotated, blurred, outlined
    /// sign and a dialogue line) and chapters, for comparing renderers.
    pub(crate) fn typeset_sample(dir: &Path) -> Option<std::path::PathBuf> {
        let ass = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 0\n\n[V4+ Styles]\n\
            Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
            Style: Default,Inter,72,&H00FFFFFF,&H000000FF,&H00101010,&H80000000,-1,0,0,0,100,100,0,0,1,4.5,2,2,120,120,70,1\n\
            Style: Sign,Inter Display Black,96,&H00F0F8FF,&H000000FF,&H002020C0,&H00000000,-1,0,0,0,100,100,0,0,1,6,0,7,0,0,0,1\n\n\
            [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
            Dialogue: 0,0:00:00.00,0:00:05.00,Default,,0,0,0,,This is dialogue, with an outline and a shadow.\n\
            Dialogue: 1,0:00:00.00,0:00:05.00,Sign,,0,0,0,,{\\pos(180,170)\\frz9\\blur2\\fscx110}APOTHECARY\n\
            Dialogue: 1,0:00:00.00,0:00:05.00,Sign,,0,0,0,,{\\an9\\pos(1780,140)\\frz-6\\bord3\\fs64\\c&H40C0FF&\\3c&H101010&}The Jade Pavilion\n";
        std::fs::create_dir_all(dir).ok()?;
        let subs = dir.join("typeset.ass");
        let chapters = dir.join("chapters.txt");
        std::fs::write(&subs, ass).ok()?;
        std::fs::write(
            &chapters,
            ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=2000\ntitle=Opening\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=2000\nEND=5000\ntitle=Part A\n",
        )
        .ok()?;
        let out = dir.join("typeset.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=1920x1080:r=24:d=5",
            ])
            .args(["-f", "lavfi", "-i", "sine=f=440:d=5"])
            .arg("-i")
            .arg(&subs)
            .arg("-i")
            .arg(&chapters)
            .args([
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2",
                "-map_chapters",
                "3",
            ])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "flac",
                "-c:s",
                "ass",
            ])
            .args([
                "-metadata:s:a:0",
                "language=jpn",
                "-metadata:s:s:0",
                "language=eng",
                "-metadata:s:s:0",
                "title=Full Subtitles",
            ])
            .args(["-disposition:s:0", "default"])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(out)
    }

    #[test]
    fn describes_tracks_and_chapters() {
        let dir = tempfile::tempdir().unwrap();
        let Some(file) = crate::player::tests::sample_episode(dir.path(), "Show - 01.mkv") else {
            return;
        };
        let (tracks, chapters, audio, subs) = describe(&file);
        let labels: Vec<String> = tracks.iter().map(Track::label).collect();
        assert_eq!(
            labels,
            [
                "Japanese · FLAC 1.0",
                "English · English Dub · FLAC 1.0",
                "English · Full Subtitles",
                "English · Signs & Songs"
            ]
        );
        assert_eq!((audio, subs), (Some(0), Some(0)), "the file's defaults");
        let parts: Vec<_> = chapters.iter().map(|c| (c.start, c.part)).collect();
        assert_eq!(
            parts,
            [
                (0.0, crate::player::Part::Opening),
                (2.0, crate::player::Part::Other),
                (4.0, crate::player::Part::Ending)
            ]
        );
    }

    /// GStreamer draws styled subtitles into the frames (libass): the
    /// picture where the sign is changes with subtitles on.
    #[test]
    fn draws_styled_subtitles_into_frames() {
        if !available() {
            eprintln!("skipping: no GStreamer with playbin and assrender");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let Some(file) = typeset_sample(dir.path()) else {
            return;
        };
        let with = frame_at(&file, 1.0, Some(0)).unwrap();
        let without = frame_at(&file, 1.0, None).unwrap();
        assert_eq!(with.dimensions(), (1920, 1080));
        // The dialogue line's area, bottom middle.
        let differs = |x0: u32, y0: u32, x1: u32, y1: u32| {
            (y0..y1)
                .step_by(4)
                .flat_map(|y| (x0..x1).step_by(4).map(move |x| (x, y)))
                .filter(|&(x, y)| with.get_pixel(x, y) != without.get_pixel(x, y))
                .count()
        };
        assert!(differs(400, 960, 1520, 1040) > 200, "dialogue drawn in");
        assert!(differs(150, 120, 900, 300) > 200, "the sign drawn in");
        if let Some(out) = std::env::var_os("COMPARE_DIR") {
            with.save(Path::new(&out).join("gstreamer.png")).unwrap();
        }
    }
}
