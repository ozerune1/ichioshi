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
}

/// A playbin for `path`, and the appsink its frames arrive at.
pub fn pipeline(path: &Path, frames: Frames) -> Result<(gst::Pipeline, gst_app::AppSink)> {
    gst::init()?;
    let uri = url::Url::from_file_path(path)
        .map_err(|()| anyhow!("{} isn't a full path", path.display()))?;
    let format = match frames {
        Frames::Nv12 => "NV12",
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

/// What the player needs to know about a file's tracks and chapters.
#[derive(Debug, Default)]
pub struct Described {
    /// As the player lists them: a track's `id` is its index among
    /// playbin's streams of its kind.
    pub tracks: Vec<Track>,
    pub chapters: Vec<Chapter>,
    /// The file's default audio and subtitles (indices).
    pub audio: Option<usize>,
    pub subs: Option<usize>,
    /// Each subtitle track's number in the file and codec, by index.
    pub sub_tracks: Vec<(u64, String)>,
}

/// Tracks and chapters, from the file (Matroska).
pub fn describe(path: &Path) -> Described {
    let Ok(info) = crate::mkv::probe(path) else {
        return Described::default();
    };
    let mut d = Described::default();
    let (mut audio_i, mut sub_i) = (0i64, 0i64);
    for t in &info.tracks {
        let (kind, index) = match t.kind {
            crate::mkv::TrackKind::Audio => {
                audio_i += 1;
                (TrackKind::Audio, audio_i - 1)
            }
            crate::mkv::TrackKind::Subtitles => {
                sub_i += 1;
                d.sub_tracks.push((t.number, t.codec_id.clone()));
                (TrackKind::Sub, sub_i - 1)
            }
            _ => continue,
        };
        // The file's defaults (the library sets them from your languages).
        if t.default {
            match kind {
                TrackKind::Audio if d.audio.is_none() => d.audio = Some(index as usize),
                TrackKind::Sub if d.subs.is_none() => d.subs = Some(index as usize),
                _ => {}
            }
        }
        d.tracks.push(Track {
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
    if d.audio.is_none() && audio_i > 0 {
        d.audio = Some(0);
    }
    d.chapters = info
        .chapters
        .iter()
        .map(|(start, title)| Chapter {
            part: part_of(title),
            title: title.clone(),
            start: *start,
        })
        .collect();
    d
}

/// Whether libass can draw a subtitle codec (text: SRT, ASS, WebVTT).
pub fn is_text_subtitle(codec_id: &str) -> bool {
    codec_id.starts_with("S_TEXT/") || matches!(codec_id, "S_ASS" | "S_SSA")
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

    /// A plain clip `secs` long, without chapters (small: 320x240).
    pub(crate) fn plain_sample(dir: &Path, secs: u32) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let out = dir.join(format!("plain-{secs}.mkv"));
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=s=320x240:r=24:d={secs}"),
            ])
            .args(["-f", "lavfi", "-i", &format!("sine=f=440:d={secs}")])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "flac",
            ])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(out)
    }

    /// A clip with one subtitle line, 2–6 s (SRT or ASS).
    pub(crate) fn one_line(dir: &Path, kind: &str) -> Option<std::path::PathBuf> {
        let sub = dir.join(format!("line.{kind}"));
        let text = if kind == "srt" {
            "1\n00:00:02,000 --> 00:00:06,000\nHELLO THERE\n".to_owned()
        } else {
            "[Script Info]\nScriptType: v4.00+\nPlayResX: 640\nPlayResY: 360\n\n[V4+ Styles]\n\
             Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
             Style: Default,Inter,40,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1\n\n\
             [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:02.00,0:00:06.00,Default,,0,0,0,,HELLO THERE\n"
                .to_owned()
        };
        std::fs::write(&sub, text).ok()?;
        let out = dir.join(format!("line-{kind}.mkv"));
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=640x360:r=24:d=10",
                "-f",
                "lavfi",
                "-i",
                "sine=d=10",
            ])
            .arg("-i")
            .arg(&sub)
            .args([
                "-map",
                "0",
                "-map",
                "1",
                "-map",
                "2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "24",
                "-pix_fmt",
                "yuv420p",
            ])
            .args([
                "-c:a",
                "flac",
                "-c:s",
                if kind == "srt" { "srt" } else { "ass" },
            ])
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
        let Described {
            tracks,
            chapters,
            audio,
            subs,
            sub_tracks,
        } = describe(&file);
        assert_eq!(
            sub_tracks
                .iter()
                .map(|(_, c)| c.as_str())
                .collect::<Vec<_>>(),
            ["S_TEXT/ASS", "S_TEXT/ASS"]
        );
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
}
