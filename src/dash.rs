//! Just enough MPEG-DASH (ISO/IEC 23009-1) to download a static (on demand)
//! presentation: its representations, and each one's init segment and media
//! segments with their URLs and times.
//!
//! Segments must be described by a `SegmentTemplate` (with a
//! `SegmentTimeline`, or a fixed `@duration`), on the Period, AdaptationSet
//! or Representation (lower levels override higher ones). `BaseURL`s are
//! followed. `SegmentBase` and `SegmentList`, live (`dynamic`)
//! presentations and more than one Period aren't supported: the first
//! Period is used, and the others are said to be ignored.

use std::fmt::Write as _;

use anyhow::{Context, Result, bail, ensure};
use roxmltree::Node;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Text,
    Other,
}

#[derive(Debug, Clone)]
pub struct Representation {
    #[allow(dead_code, reason = "read in tests and debug output")]
    pub id: String,
    pub kind: Kind,
    pub mime_type: String,
    pub codecs: String,
    /// Bits per second, as the MPD says.
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Ticks per second of segment times and durations.
    pub timescale: u64,
    pub init: Option<Url>,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub url: Url,
    /// When it starts, in ticks from the start of the Period.
    pub time: u64,
    pub duration: u64,
}

impl Representation {
    /// The segments' total duration, in ticks.
    pub fn duration(&self) -> u64 {
        self.segments.iter().map(|s| s.duration).sum()
    }

    /// `ticks` in seconds.
    pub fn seconds(&self, ticks: u64) -> f64 {
        ticks as f64 / self.timescale as f64
    }
}

#[derive(Debug, Clone)]
pub struct Mpd {
    /// The presentation's duration (seconds), if the MPD says.
    #[allow(dead_code, reason = "read in tests")]
    pub duration: Option<f64>,
    pub representations: Vec<Representation>,
}

/// Reads the MPD at `url` (relative URLs in it are resolved against it).
pub fn parse(xml: &str, url: &Url) -> Result<Mpd> {
    let doc = roxmltree::Document::parse(xml).context("the MPD isn't XML")?;
    let mpd = doc.root_element();
    ensure!(
        mpd.tag_name().name() == "MPD",
        "not an MPD (root is <{}>)",
        mpd.tag_name().name()
    );
    if mpd.attribute("type") == Some("dynamic") {
        bail!("live (dynamic) MPDs aren't supported");
    }
    let duration = mpd
        .attribute("mediaPresentationDuration")
        .and_then(iso_duration);
    let base = base_url(url, mpd)?;
    let mut periods = elements(mpd, "Period");
    let period = periods.next().context("the MPD has no Period")?;
    if periods.next().is_some() {
        tracing::warn!("the MPD has several Periods; only the first is downloaded");
    }
    let period_start = period
        .attribute("start")
        .and_then(iso_duration)
        .unwrap_or(0.0);
    let period_duration = period
        .attribute("duration")
        .and_then(iso_duration)
        .or_else(|| duration.map(|d| d - period_start));
    let base = base_url(&base, period)?;

    let mut representations = Vec::new();
    let mut skipped = Vec::new();
    for set in elements(period, "AdaptationSet") {
        let set_base = base_url(&base, set)?;
        for rep in elements(set, "Representation") {
            let levels = [rep, set, period];
            match representation(&levels, &base_url(&set_base, rep)?, period_duration) {
                Ok(r) => representations.push(r),
                Err(e) => {
                    let id = rep.attribute("id").unwrap_or("?");
                    tracing::warn!("skipping representation {id}: {e:#}");
                    skipped.push(format!("{id}: {e:#}"));
                }
            }
        }
    }
    if representations.is_empty() {
        if skipped.is_empty() {
            bail!("the MPD has no representations");
        }
        bail!(
            "no usable representations in the MPD ({})",
            skipped.join("; ")
        );
    }
    Ok(Mpd {
        duration,
        representations,
    })
}

fn elements<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn base_url(base: &Url, node: Node) -> Result<Url> {
    match elements(node, "BaseURL").next().and_then(|b| b.text()) {
        Some(text) if !text.trim().is_empty() => base
            .join(text.trim())
            .with_context(|| format!("bad BaseURL {text:?}")),
        _ => Ok(base.clone()),
    }
}

/// An attribute from the nearest of `levels` that has it.
fn inherited<'a>(levels: &[Node<'a, '_>], name: &str) -> Option<&'a str> {
    levels.iter().find_map(|n| n.attribute(name))
}

fn number<T: std::str::FromStr>(v: Option<&str>, what: &str) -> Result<Option<T>> {
    v.map(|s| {
        s.trim()
            .parse::<T>()
            .map_err(|_| anyhow::anyhow!("bad {what} {s:?}"))
    })
    .transpose()
}

fn representation(
    levels: &[Node; 3],
    base: &Url,
    period_duration: Option<f64>,
) -> Result<Representation> {
    let rep = levels[0];
    let id = rep.attribute("id").unwrap_or_default().to_owned();
    let mime_type = inherited(&levels[..2], "mimeType")
        .unwrap_or_default()
        .to_owned();
    let codecs = inherited(&levels[..2], "codecs")
        .unwrap_or_default()
        .to_owned();
    let bandwidth = number(rep.attribute("bandwidth"), "bandwidth")?.unwrap_or(0);
    let width = number(inherited(&levels[..2], "width"), "width")?;
    let height = number(inherited(&levels[..2], "height"), "height")?;
    let kind = kind(levels[1].attribute("contentType"), &mime_type, &codecs);

    let templates: Vec<Node> = levels
        .iter()
        .filter_map(|n| elements(*n, "SegmentTemplate").next())
        .collect();
    if templates.is_empty() {
        if levels.iter().any(|n| {
            elements(*n, "SegmentBase").next().is_some()
                || elements(*n, "SegmentList").next().is_some()
        }) {
            bail!("SegmentBase / SegmentList aren't supported");
        }
        bail!("no SegmentTemplate");
    }
    let attr = |name: &str| templates.iter().find_map(|t| t.attribute(name));
    let timescale: u64 = number(attr("timescale"), "timescale")?.unwrap_or(1);
    ensure!(timescale > 0, "zero timescale");
    let start_number: u64 = number(attr("startNumber"), "startNumber")?.unwrap_or(1);
    let offset: u64 =
        number(attr("presentationTimeOffset"), "presentationTimeOffset")?.unwrap_or(0);
    let media = attr("media").context("SegmentTemplate without @media")?;
    let fill = |template: &str, number: u64, time: u64| -> Result<Url> {
        let path = fill_template(template, &id, number, time, bandwidth)?;
        base.join(&path)
            .with_context(|| format!("bad segment URL {path:?}"))
    };
    let init = attr("initialization")
        .map(|t| fill(t, start_number, 0))
        .transpose()?;

    // (time from the template's point of view, duration) of each segment.
    let mut timing: Vec<(u64, u64)> = Vec::new();
    let period_end = period_duration.map(|d| offset + (d * timescale as f64).round() as u64);
    if let Some(timeline) = templates
        .iter()
        .find_map(|t| elements(*t, "SegmentTimeline").next())
    {
        let entries: Vec<Node> = elements(timeline, "S").collect();
        let mut next = 0u64;
        for (i, s) in entries.iter().enumerate() {
            let t = number(s.attribute("t"), "S@t")?.unwrap_or(next);
            let d: u64 = number(s.attribute("d"), "S@d")?.context("S without @d")?;
            ensure!(d > 0, "S with a zero duration");
            let r: i64 = number(s.attribute("r"), "S@r")?.unwrap_or(0);
            let count = if r >= 0 {
                r as u64 + 1
            } else {
                // Until the next S's start, or the end of the Period.
                let end = match entries.get(i + 1).and_then(|n| n.attribute("t")) {
                    Some(t) => t.parse().ok(),
                    None => period_end,
                }
                .context("S@r=-1 with no end to repeat to")?;
                end.saturating_sub(t).div_ceil(d)
            };
            ensure!(count < 1_000_000, "implausibly many segments");
            for k in 0..count {
                timing.push((t + k * d, d));
            }
            next = t + count * d;
        }
    } else {
        let d: u64 = number(attr("duration"), "SegmentTemplate@duration")?
            .context("SegmentTemplate with neither a SegmentTimeline nor @duration")?;
        ensure!(d > 0, "zero segment duration");
        let total =
            period_duration.context("segments of fixed duration, but no Period duration")?;
        let count = ((total * timescale as f64) / d as f64).ceil() as u64;
        ensure!(count < 1_000_000, "implausibly many segments");
        for k in 0..count {
            timing.push((offset + k * d, d));
        }
    }
    ensure!(!timing.is_empty(), "no segments");
    let segments = timing
        .into_iter()
        .enumerate()
        .map(|(i, (t, d))| {
            Ok(Segment {
                url: fill(media, start_number + i as u64, t)?,
                time: t.saturating_sub(offset),
                duration: d,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Representation {
        id,
        kind,
        mime_type,
        codecs,
        bandwidth,
        width,
        height,
        timescale,
        init,
        segments,
    })
}

fn kind(content_type: Option<&str>, mime_type: &str, codecs: &str) -> Kind {
    let by_name = |s: &str| match s.split('/').next().unwrap_or_default() {
        "video" => Some(Kind::Video),
        "audio" => Some(Kind::Audio),
        "text" => Some(Kind::Text),
        _ => None,
    };
    if let Some(k) = content_type
        .and_then(by_name)
        .or_else(|| by_name(mime_type))
    {
        return k;
    }
    let codec = codecs.split('.').next().unwrap_or_default();
    match codec {
        "avc1" | "avc3" | "hev1" | "hvc1" | "av01" | "vp09" | "vp08" | "dvh1" | "dvhe" => {
            Kind::Video
        }
        "mp4a" | "opus" | "ac-3" | "ec-3" | "flac" | "Opus" | "fLaC" => Kind::Audio,
        "stpp" | "wvtt" => Kind::Text,
        _ => Kind::Other,
    }
}

/// A SegmentTemplate's `$RepresentationID$`, `$Number$`, `$Time$` and
/// `$Bandwidth$` (with an optional `%0<width>d`) filled in; `$$` is `$`.
fn fill_template(
    template: &str,
    id: &str,
    number: u64,
    time: u64,
    bandwidth: u64,
) -> Result<String> {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let j = after
            .find('$')
            .with_context(|| format!("unclosed $ in {template:?}"))?;
        let ident = &after[..j];
        rest = &after[j + 1..];
        let (name, format) = match ident.find('%') {
            Some(k) => (&ident[..k], Some(&ident[k..])),
            None => (ident, None),
        };
        let value = match name {
            "" => {
                out.push('$');
                continue;
            }
            "RepresentationID" => {
                out.push_str(id);
                continue;
            }
            "Number" => number,
            "Time" => time,
            "Bandwidth" => bandwidth,
            other => bail!("unknown ${other}$ in {template:?}"),
        };
        match format {
            None => write!(out, "{value}").unwrap(),
            Some(f) => {
                let width = f
                    .strip_prefix('%')
                    .and_then(|f| f.strip_suffix('d'))
                    .and_then(|w| {
                        if w.is_empty() {
                            Some(0)
                        } else {
                            w.parse::<usize>().ok()
                        }
                    })
                    .with_context(|| format!("bad format {f:?} in {template:?}"))?;
                write!(out, "{value:0width$}").unwrap();
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// An ISO 8601 duration (`PT16M14.9S`, `P1DT2H`) in seconds.
pub fn iso_duration(s: &str) -> Option<f64> {
    let s = s.trim().strip_prefix('P')?;
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };
    let mut total = 0.0;
    for (part, units) in [
        (
            date,
            &[
                ('Y', 365.0 * 86400.0),
                ('M', 30.0 * 86400.0),
                ('W', 7.0 * 86400.0),
                ('D', 86400.0),
            ][..],
        ),
        (time, &[('H', 3600.0), ('M', 60.0), ('S', 1.0)][..]),
    ] {
        let mut rest = part;
        while !rest.is_empty() {
            let end = rest.find(|c: char| c.is_ascii_alphabetic())?;
            let value: f64 = rest[..end].parse().ok()?;
            let unit = rest[end..].chars().next()?;
            let &(_, scale) = units.iter().find(|(u, _)| *u == unit)?;
            total += value * scale;
            rest = &rest[end + 1..];
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// hstream.moe's, as of 2026-11 (Sister Breeder 6, 2160).
    const HSTREAM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
	xmlns="urn:mpeg:dash:schema:mpd:2011"
	xmlns:xlink="http://www.w3.org/1999/xlink"
	xsi:schemaLocation="urn:mpeg:DASH:schema:MPD:2011 http://standards.iso.org/ittf/PubliclyAvailableStandards/MPEG-DASH_schema_files/DASH-MPD.xsd"
	profiles="urn:mpeg:dash:profile:isoff-live:2011"
	type="static"
	mediaPresentationDuration="PT16M14.9S"
	maxSegmentDuration="PT10.0S"
	minBufferTime="PT20.0S">
	<ProgramInformation>
	</ProgramInformation>
	<ServiceDescription id="0">
	</ServiceDescription>
	<Period id="0" start="PT0.0S">
		<AdaptationSet id="0" contentType="video" startWithSAP="1" segmentAlignment="true" bitstreamSwitching="true" frameRate="24000/1001" maxWidth="3840" maxHeight="2160" par="16:9">
			<Representation id="0" mimeType="video/mp4" codecs="av01.0.12M.08" bandwidth="1974344" width="3840" height="2160" sar="1:1">
				<SegmentTemplate timescale="24000" initialization="chunks/init-stream$RepresentationID$.webp" media="chunks/chunk-stream$RepresentationID$-$Number%05d$.webp" startNumber="1">
					<SegmentTimeline>
						<S t="0" d="240240" r="96" />
						<S d="96096" />
					</SegmentTimeline>
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
		<AdaptationSet id="1" contentType="audio" startWithSAP="1" segmentAlignment="true" bitstreamSwitching="true">
			<Representation id="1" mimeType="audio/mp4" codecs="mp4a.40.2" bandwidth="160000" audioSamplingRate="48000">
				<AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="2" />
				<SegmentTemplate timescale="48000" initialization="chunks/init-stream$RepresentationID$.webp" media="chunks/chunk-stream$RepresentationID$-$Number%05d$.webp" startNumber="1">
					<SegmentTimeline>
						<S t="0" d="479232" />
						<S d="480256" r="95" />
						<S d="208896" />
					</SegmentTimeline>
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
	</Period>
</MPD>
"#;

    fn url() -> Url {
        Url::parse("https://cdn.example/2025/Show/E06/2160/manifest.mpd").unwrap()
    }

    #[test]
    fn reads_hstreams_mpd() {
        let mpd = parse(HSTREAM, &url()).unwrap();
        assert!((mpd.duration.unwrap() - 974.9).abs() < 1e-9);
        let [video, audio] = &mpd.representations[..] else {
            panic!("{:?}", mpd.representations)
        };
        assert_eq!(video.kind, Kind::Video);
        assert_eq!((video.width, video.height), (Some(3840), Some(2160)));
        assert_eq!(video.codecs, "av01.0.12M.08");
        assert_eq!(video.bandwidth, 1_974_344);
        assert_eq!(video.timescale, 24000);
        assert_eq!(
            video.init.as_ref().unwrap().as_str(),
            "https://cdn.example/2025/Show/E06/2160/chunks/init-stream0.webp"
        );
        assert_eq!(video.segments.len(), 98);
        assert_eq!(
            video.segments[0].url.as_str(),
            "https://cdn.example/2025/Show/E06/2160/chunks/chunk-stream0-00001.webp"
        );
        assert_eq!(
            video.segments[97].url.path(),
            "/2025/Show/E06/2160/chunks/chunk-stream0-00098.webp"
        );
        assert_eq!(video.segments[1].time, 240_240);
        assert_eq!(video.segments[97].time, 97 * 240_240);
        assert_eq!(video.segments[97].duration, 96_096);
        assert_eq!(video.duration(), 97 * 240_240 + 96_096);
        assert!((video.seconds(video.duration()) - 974.974).abs() < 0.001);

        assert_eq!(audio.kind, Kind::Audio);
        assert_eq!(audio.bandwidth, 160_000);
        assert_eq!(audio.height, None);
        assert_eq!(audio.segments.len(), 98);
        assert_eq!(audio.segments[1].time, 479_232);
        assert_eq!(audio.segments[2].time, 479_232 + 480_256);
        assert_eq!(
            audio.segments[97].url.path(),
            "/2025/Show/E06/2160/chunks/chunk-stream1-00098.webp"
        );
    }

    #[test]
    fn follows_base_urls_times_and_open_repeats() {
        let xml = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT20S">
          <BaseURL>https://other.example/media/</BaseURL>
          <Period>
            <SegmentTemplate timescale="1000" media="$RepresentationID$/t$Time$-$Bandwidth$.m4s" initialization="$RepresentationID$/init.mp4"/>
            <AdaptationSet mimeType="audio/mp4" codecs="opus">
              <BaseURL>a/</BaseURL>
              <Representation id="en" bandwidth="96000">
                <SegmentTemplate presentationTimeOffset="500">
                  <SegmentTimeline><S t="500" d="6000" r="-1"/></SegmentTimeline>
                </SegmentTemplate>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#;
        let mpd = parse(xml, &url()).unwrap();
        let rep = &mpd.representations[0];
        assert_eq!(rep.kind, Kind::Audio);
        assert_eq!(rep.timescale, 1000);
        // 20 s from t=500 (the offset) in 6 s steps: 4 segments.
        let urls: Vec<&str> = rep.segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://other.example/media/a/en/t500-96000.m4s",
                "https://other.example/media/a/en/t6500-96000.m4s",
                "https://other.example/media/a/en/t12500-96000.m4s",
                "https://other.example/media/a/en/t18500-96000.m4s",
            ]
        );
        assert_eq!(rep.segments[1].time, 6000);
        assert_eq!(
            rep.init.as_ref().unwrap().as_str(),
            "https://other.example/media/a/en/init.mp4"
        );
    }

    #[test]
    fn counts_fixed_duration_segments() {
        let xml = r#"<MPD type="static" mediaPresentationDuration="PT1M0.5S"><Period>
          <AdaptationSet contentType="video">
            <Representation id="v" codecs="avc1.64001f" width="1280" height="720" bandwidth="1">
              <SegmentTemplate timescale="90000" duration="900000" startNumber="0" media="v_$Number%03d$.mp4"/>
            </Representation>
          </AdaptationSet></Period></MPD>"#;
        let rep = &parse(xml, &url()).unwrap().representations[0];
        assert_eq!(rep.kind, Kind::Video);
        assert_eq!(rep.segments.len(), 7);
        assert_eq!(rep.segments[0].url.path(), "/2025/Show/E06/2160/v_000.mp4");
        assert_eq!(rep.segments[6].url.path(), "/2025/Show/E06/2160/v_006.mp4");
        assert_eq!(rep.segments[6].time, 6 * 900_000);
        assert!(rep.init.is_none());
    }

    #[test]
    fn says_what_it_cant_read() {
        let live = r#"<MPD type="dynamic"><Period/></MPD>"#;
        assert!(format!("{:#}", parse(live, &url()).unwrap_err()).contains("live"));
        let base = r#"<MPD><Period><AdaptationSet><Representation id="v">
            <BaseURL>v.mp4</BaseURL><SegmentBase indexRange="0-99"/></Representation></AdaptationSet></Period></MPD>"#;
        assert!(format!("{:#}", parse(base, &url()).unwrap_err()).contains("SegmentBase"));
        assert!(parse("<html>404</html>", &url()).is_err());
        assert!(parse("not xml", &url()).is_err());
    }

    #[test]
    fn fills_templates() {
        assert_eq!(
            fill_template("a$Number%05d$b", "0", 7, 0, 0).unwrap(),
            "a00007b"
        );
        assert_eq!(
            fill_template("$RepresentationID$-$Number$", "x", 12, 0, 0).unwrap(),
            "x-12"
        );
        assert_eq!(fill_template("$$$Time$$$", "x", 0, 99, 0).unwrap(), "$99$");
        assert!(fill_template("$Nope$", "x", 0, 0, 0).is_err());
        assert!(fill_template("$Number", "x", 0, 0, 0).is_err());
        assert!(fill_template("$Number%x$", "x", 0, 0, 0).is_err());
    }

    #[test]
    fn reads_durations() {
        assert!((iso_duration("PT16M14.9S").unwrap() - 974.9).abs() < 1e-9);
        assert_eq!(iso_duration("PT0.0S"), Some(0.0));
        assert_eq!(iso_duration("P1DT1H"), Some(90000.0));
        assert_eq!(iso_duration("PT1H2M3S"), Some(3723.0));
        assert_eq!(iso_duration("16M"), None);
        assert_eq!(iso_duration("PT5X"), None);
    }
}
