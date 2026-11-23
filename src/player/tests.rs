use std::path::{Path, PathBuf};

use super::*;

/// A 6-second episode made with ffmpeg: red over blue, Japanese (default)
/// and English audio, full (default) and signs & songs subtitles, and
/// chapters Opening 0–2 s, Part A 2–4 s, Ending 4–6 s. None without ffmpeg.
pub(crate) fn sample_episode(dir: &Path, name: &str) -> Option<PathBuf> {
    let ass = |text: &str| {
        format!(
            "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n[V4+ Styles]\n\
             Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
             Style: Default,Arial,16,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n\n\
             [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:00.50,0:00:05.50,Default,,0,0,0,,{text}\n"
        )
    };
    std::fs::create_dir_all(dir).unwrap();
    let work = tempfile::tempdir_in(dir).unwrap();
    let full = work.path().join("full.ass");
    let signs = work.path().join("signs.ass");
    let chapters = work.path().join("chapters.txt");
    std::fs::write(&full, ass("Hello")).unwrap();
    std::fs::write(&signs, ass("{\\pos(160,20)}SIGN")).unwrap();
    let mut meta = String::from(";FFMETADATA1\n");
    for (start, title) in [(0, "Opening"), (2000, "Part A"), (4000, "Ending")] {
        meta.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={start}\nEND={}\ntitle={title}\n",
            start + 2000
        ));
    }
    std::fs::write(&chapters, meta).unwrap();
    let out = dir.join(name);
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args(["-f", "lavfi", "-i", "color=c=red:s=320x120:d=6:r=24"])
        .args(["-f", "lavfi", "-i", "color=c=blue:s=320x120:d=6:r=24"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=6",
            "-f",
            "lavfi",
            "-i",
            "sine=f=660:d=6",
        ])
        .arg("-i")
        .arg(&full)
        .arg("-i")
        .arg(&signs)
        .arg("-i")
        .arg(&chapters)
        .args([
            "-filter_complex",
            "[0:v][1:v]vstack[v]",
            "-map",
            "[v]",
            "-map",
            "2:a",
            "-map",
            "3:a",
            "-map",
            "4",
            "-map",
            "5",
        ])
        .args(["-map_metadata", "6", "-map_chapters", "6"])
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
            "-metadata:s:a:1",
            "language=eng",
            "-metadata:s:a:1",
            "title=English Dub",
        ])
        .args([
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:0",
            "title=Full Subtitles",
        ])
        .args([
            "-metadata:s:s:1",
            "language=eng",
            "-metadata:s:s:1",
            "title=Signs & Songs",
        ])
        .args([
            "-disposition:a:0",
            "default",
            "-disposition:a:1",
            "0",
            "-disposition:s:0",
            "default",
            "-disposition:s:1",
            "0",
        ])
        .arg(&out)
        .status();
    match status {
        Ok(s) if s.success() => Some(out),
        _ => {
            eprintln!("skipping: ffmpeg couldn't make a sample episode");
            None
        }
    }
}

#[test]
fn names_episode_parts_by_chapter() {
    for t in [
        "Opening",
        "OP",
        "OP1",
        "Intro",
        "opening song",
        "Opening Credits",
    ] {
        assert_eq!(part_of(t), Part::Opening, "{t}");
    }
    for t in [
        "Ending",
        "ED",
        "ED 2",
        "Credits",
        "Outro",
        "Ending Theme",
        "End Credits",
    ] {
        assert_eq!(part_of(t), Part::Ending, "{t}");
    }
    assert_eq!(part_of("Next Episode Preview"), Part::Preview);
    for t in [
        "Part A",
        "Chapter 01",
        "Prologue",
        "Episode",
        "Opening Scene",
        "Edward",
    ] {
        assert_eq!(part_of(t), Part::Other, "{t}");
    }
}

#[test]
fn labels_tracks() {
    let t = |kind, lang: Option<&str>, title: Option<&str>, codec: &str, channels| Track {
        id: 1,
        kind,
        title: title.map(Into::into),
        lang: lang.map(Into::into),
        codec: Some(codec.into()),
        channels,
        default: false,
        forced: false,
        external: false,
        selected: false,
    };
    assert_eq!(
        t(TrackKind::Audio, Some("jpn"), None, "flac", Some(2)).label(),
        "Japanese · FLAC 2.0"
    );
    assert_eq!(
        t(
            TrackKind::Audio,
            Some("eng"),
            Some("English"),
            "eac3",
            Some(6)
        )
        .label(),
        "English · E-AC-3 5.1"
    );
    assert_eq!(
        t(
            TrackKind::Sub,
            Some("eng"),
            Some("Signs & Songs"),
            "ass",
            None
        )
        .label(),
        "English · Signs & Songs"
    );
    assert_eq!(
        t(TrackKind::Sub, None, None, "hdmv_pgs_subtitle", None).label(),
        "PGS"
    );
    // Two-letter codes, as mpv often gives them.
    assert_eq!(
        t(
            TrackKind::Audio,
            Some("es"),
            Some("Latin American"),
            "eac3",
            Some(6)
        )
        .label(),
        "Spanish · Latin American · E-AC-3 5.1"
    );
    assert_eq!(
        t(
            TrackKind::Sub,
            Some("pt"),
            Some("Brazilian"),
            "subrip",
            None
        )
        .label(),
        "Portuguese · Brazilian"
    );
}
