//! WebVTT subtitles (as HLS serves them, a file per segment) made into one
//! ASS file, which the desktop player reads beside a video (see
//! [`crate::sidecar`]) and mpv does too.

/// One cue: start and end in seconds of the video, its text in ASS.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The cues in one WebVTT file. Its times are moved by `offset` seconds,
/// and by its `X-TIMESTAMP-MAP` (an HLS segment's: `MPEGTS` is where
/// `LOCAL` falls on the stream's 90 kHz clock, and `zero` is where the
/// joined video starts on that clock, in seconds).
pub fn cues(vtt: &str, zero: f64) -> Vec<Cue> {
    let vtt = vtt.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut shift = 0.0;
    let mut out = Vec::new();
    for block in vtt.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        if let Some(map) = lines
            .iter()
            .find_map(|l| l.strip_prefix("X-TIMESTAMP-MAP="))
        {
            let mut mpegts = None;
            let mut local = None;
            for part in map.split(',') {
                if let Some(v) = part.trim().strip_prefix("MPEGTS:") {
                    mpegts = v.trim().parse::<u64>().ok();
                }
                if let Some(v) = part.trim().strip_prefix("LOCAL:") {
                    local = timestamp(v.trim());
                }
            }
            if let (Some(m), Some(l)) = (mpegts, local) {
                shift = m as f64 / 90_000.0 - l - zero;
            }
        }
        let Some(at) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let mut times = lines[at].split("-->");
        let (Some(start), Some(end)) = (
            times.next().and_then(|t| timestamp(t.trim())),
            times
                .next()
                .and_then(|t| t.split_whitespace().next())
                .and_then(timestamp),
        ) else {
            continue;
        };
        let text = lines[at + 1..]
            .iter()
            .map(|l| ass_text(l))
            .collect::<Vec<_>>()
            .join("\\N");
        if text.trim().is_empty() || end <= start {
            continue;
        }
        out.push(Cue {
            start: (start + shift).max(0.0),
            end: (end + shift).max(0.0),
            text,
        });
    }
    out
}

/// "01:02:03.456" or "02:03.456" in seconds.
fn timestamp(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (h.parse::<f64>().ok()?, m.parse::<f64>().ok()?, s),
        [m, s] => (0.0, m.parse::<f64>().ok()?, s),
        _ => return None,
    };
    let sec: f64 = sec.replace(',', ".").parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// A cue line in ASS: italics, bold and underline kept as overrides, other
/// tags dropped, entities decoded, braces (ASS's override marks) escaped.
fn ass_text(line: &str) -> String {
    let plain = |s: &str| {
        crate::sites::web::unescape(s)
            .replace('{', "(")
            .replace('}', ")")
    };
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        out.push_str(&plain(&rest[..open]));
        let Some(close) = rest[open..].find('>') else {
            out.push_str(&plain(&rest[open..]));
            return out;
        };
        let tag = &rest[open + 1..open + close];
        let (closing, name) = match tag.strip_prefix('/') {
            Some(n) => (true, n),
            None => (false, tag),
        };
        let name = name.split(['.', ' ']).next().unwrap_or_default();
        if matches!(name, "i" | "b" | "u") {
            out.push_str(&format!("{{\\{name}{}}}", if closing { 0 } else { 1 }));
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(&plain(rest));
    out
}

/// The cues as an ASS file, in order, with a plain style.
pub fn ass(cues: &[Cue]) -> String {
    let mut cues = cues.to_vec();
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    cues.dedup_by(|b, a| a.start == b.start && a.end == b.end && a.text == b.text);
    let mut out = String::from(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,Arial,72,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3.5,1.5,2,96,96,60,1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );
    for c in cues {
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            clock(c.start),
            clock(c.end),
            c.text
        ));
    }
    out
}

/// Seconds as ASS writes times: "0:01:02.35".
fn clock(t: f64) -> String {
    let cs = (t * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        cs / 6000 % 60,
        cs / 100 % 60,
        cs % 100
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_segments_and_writes_ass() {
        let seg = "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n1\n00:00:11.000 --> 00:00:13.500 align:middle\n<i>Brother</i>, really?\nTogether &amp; {now}\n\n00:12.000 --> 00:12.000\nempty\n";
        // LOCAL 0 is 10 s on the stream's clock, where the joined video
        // starts 1.5 s in: a cue at LOCAL 11 s is 19.5 s into the video.
        let c = cues(seg, 1.5);
        assert_eq!(c.len(), 1);
        assert!((c[0].start - 19.5).abs() < 1e-9, "{c:?}");
        assert!((c[0].end - 22.0).abs() < 1e-9);
        assert_eq!(c[0].text, "{\\i1}Brother{\\i0}, really?\\NTogether & (now)");
        let file = ass(&[c[0].clone(), c[0].clone()]);
        assert!(file.contains("[Script Info]"));
        assert_eq!(
            file.matches("Dialogue:").count(),
            1,
            "duplicates across segments go"
        );
        assert!(file.contains("Dialogue: 0,0:00:19.50,0:00:22.00,Default,,0,0,0,,{\\i1}Brother"));
        // Without a map: as they are.
        let c = cues("WEBVTT\n\n01:00:00.250 --> 01:00:01.000\nHi\n", 10.0);
        assert!((c[0].start - 3600.25).abs() < 1e-9);
        assert_eq!(clock(3725.456), "1:02:05.46");
    }
}
