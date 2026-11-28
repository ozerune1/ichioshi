//! Subtitle files beside a video ("<name>.en.ass"), as episodes from
//! streaming sites come: read into tracks the way an MKV's are, so the
//! desktop player (libass) shows them and your languages pick them. (Phones
//! play with mpv, which finds such files itself.)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::mkv::{MkvInfo, SubEvent, SubTrack, TrackInfo, TrackKind};

/// Track numbers given to subtitle files: past any a file has.
pub const FIRST_NUMBER: u64 = 10_000;

/// The ASS files beside `video` ("<stem>.ass", "<stem>.<lang>.ass"), with
/// their languages (ISO 639-2; "und" if the name doesn't say): those that
/// say first, then by name.
pub fn find(video: &Path) -> Vec<(String, PathBuf)> {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem()) else {
        return Vec::new();
    };
    let stem = stem.to_string_lossy();
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|f| {
            let path = f.path();
            let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
            if ext != "ass" && ext != "ssa" {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let lang = if name == *stem {
                "und".to_owned()
            } else {
                let code = name.strip_prefix(&*stem)?.strip_prefix('.')?;
                crate::remux::normalize_language(code)
            };
            Some((lang, path))
        })
        .collect();
    // Those that say their language first.
    found.sort_by(|a, b| (a.0 == "und", &a.1).cmp(&(b.0 == "und", &b.1)));
    found
}

/// Add a subtitle track for each file to what's known of the video's,
/// numbered from [`FIRST_NUMBER`]. The first is the default if the video
/// has no default subtitles of its own.
pub fn add_tracks(info: &mut MkvInfo, files: &[(String, PathBuf)]) {
    let has_default = info
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Subtitles && t.default);
    for (i, (lang, path)) in files.iter().enumerate() {
        info.tracks.push(TrackInfo {
            number: FIRST_NUMBER + i as u64,
            uid: 0,
            kind: TrackKind::Subtitles,
            codec_id: "S_TEXT/ASS".to_owned(),
            language: lang.clone(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            default: i == 0 && !has_default,
            forced: false,
            hearing_impaired: false,
            commentary: false,
            channels: None,
        });
    }
}

/// Read an ASS file as Matroska carries one: its header, through its
/// events' Format line, as the codec's private data; each Dialogue line as
/// a block ("ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,
/// Text") with its start and duration.
pub fn read_ass(path: &Path) -> Result<SubTrack> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    parse_ass(&text)
}

fn parse_ass(text: &str) -> Result<SubTrack> {
    let text = text.trim_start_matches('\u{feff}');
    let mut header = String::new();
    let mut format: Vec<String> = Vec::new();
    let mut in_events = false;
    let mut events = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_events = trimmed.eq_ignore_ascii_case("[events]");
            header.push_str(trimmed);
            header.push('\n');
            continue;
        }
        if !in_events {
            header.push_str(line.trim_end());
            header.push('\n');
            continue;
        }
        if let Some(f) = trimmed.strip_prefix("Format:") {
            format = f
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect();
            header.push_str(trimmed);
            header.push('\n');
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("Dialogue:") else {
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
        if fields.len() != format.len() {
            continue;
        }
        let get = |name: &str| {
            format
                .iter()
                .position(|f| f == name)
                .map(|i| fields[i])
                .unwrap_or("")
        };
        let (Some(start), Some(end)) = (time_ms(get("start")), time_ms(get("end"))) else {
            continue;
        };
        let layer = match get("layer") {
            "" => "0",
            l => l,
        };
        let data = format!(
            "{},{layer},{},{},{},{},{},{},{}",
            events.len(),
            get("style"),
            get("name"),
            get("marginl"),
            get("marginr"),
            get("marginv"),
            get("effect"),
            get("text"),
        );
        events.push(SubEvent {
            start_ms: start,
            duration_ms: (end - start).max(0),
            data: data.into_bytes(),
        });
    }
    if events.is_empty() && !header.to_ascii_lowercase().contains("[events]") {
        bail!("not an ASS file");
    }
    Ok(SubTrack {
        codec_id: "S_TEXT/ASS".to_owned(),
        codec_private: header.into_bytes(),
        events,
    })
}

/// "0:01:02.50" -> 62500.
fn time_ms(t: &str) -> Option<i64> {
    let mut parts = t.trim().split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    let (secs, frac) = s.split_once('.').unwrap_or((s, "0"));
    let cs: i64 = format!("{frac:0<2}").get(..2)?.parse().ok()?;
    Some(
        h.parse::<i64>().ok()? * 3_600_000
            + m.parse::<i64>().ok()? * 60_000
            + secs.parse::<i64>().ok()? * 1000
            + cs * 10,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASS: &str = "\u{feff}[Script Info]\nTitle: Episode 1\nScriptType: v4.00+\nPlayResX: 1920\n\n\
[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,64\n\n\
[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Comment: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,not shown\n\
Dialogue: 0,0:00:01.50,0:00:04.00,Default,,0,0,0,,Hello, there\n\
Dialogue: 1,0:01:02.05,0:01:03.5,Sign,Tag,10,20,30,,{\\pos(10,10)}Sign\n";

    #[test]
    fn reads_ass_as_matroska_does() {
        let t = parse_ass(ASS).unwrap();
        assert_eq!(t.codec_id, "S_TEXT/ASS");
        let header = String::from_utf8(t.codec_private.clone()).unwrap();
        assert!(header.starts_with("[Script Info]"));
        assert!(header.contains("Style: Default,Arial,64"));
        assert!(header.trim_end().ends_with("Effect, Text"), "{header}");
        assert!(!header.contains("Dialogue"));
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.events[0].start_ms, 1500);
        assert_eq!(t.events[0].duration_ms, 2500);
        assert_eq!(
            String::from_utf8(t.events[0].data.clone()).unwrap(),
            "0,0,Default,,0,0,0,,Hello, there"
        );
        assert_eq!(t.events[1].start_ms, 62_050);
        assert_eq!(t.events[1].duration_ms, 1450);
        assert_eq!(
            String::from_utf8(t.events[1].data.clone()).unwrap(),
            "1,1,Sign,Tag,10,20,30,,{\\pos(10,10)}Sign"
        );
    }

    #[test]
    fn finds_files_beside_the_video_and_adds_them() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("Show - 01.mp4");
        for name in [
            "Show - 01.mp4",
            "Show - 01.en.ass",
            "Show - 02.en.ass",
            "Show - 01.ass",
        ] {
            std::fs::write(dir.path().join(name), ASS).unwrap();
        }
        let found = find(&video);
        let names: Vec<_> = found
            .iter()
            .map(|(l, p)| {
                (
                    l.as_str(),
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("eng", "Show - 01.en.ass".to_owned()),
                ("und", "Show - 01.ass".to_owned())
            ]
        );
        let mut info = MkvInfo::default();
        add_tracks(&mut info, &found);
        assert_eq!(info.tracks.len(), 2);
        assert!(info.tracks[0].default && !info.tracks[1].default);
        assert_eq!(info.tracks[0].number, FIRST_NUMBER);
        assert!(read_ass(&found[0].1).is_ok());
    }
}
