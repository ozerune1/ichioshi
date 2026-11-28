//! Fragmented MP4 (ISO BMFF) for one track, written from samples: an init
//! segment (`ftyp` + `moov`) and media segments (`moof` + `mdat`), shaped
//! like a DASH track's, so the app's `fmp4` muxer joins them the same way.
//! [`Remuxer`] makes them from MPEG-TS segments (see [`super::mpegts`]),
//! so an HLS download ends up one seekable MP4 like a DASH one.
//!
//! Each track is numbered 1 in its own init segment (the muxer renumbers
//! them). There are no edit lists: video and audio share one zero point,
//! the earliest decode time of either, so each starts where it did in the
//! TS and they stay in sync. Fragments say their decode time (`tfdt`) and
//! each sample's duration, size, flags and composition offset (`trun`
//! version 1, so offsets may be negative), with data offsets from the
//! `moof`.

use std::collections::{HashMap, HashSet};
use std::fmt;

use anyhow::{Context, Result, bail, ensure};

use super::mpegts::{self, AAC_FRAME, Codec, Demuxer, Frame, Kind, Stream};

const TRACK_ID: u32 = 1;
/// Sample flags: a sync sample (depends on nothing), and any other.
const SYNC_SAMPLE: u32 = 0x0200_0000;
const OTHER_SAMPLE: u32 = 0x0101_0000;
/// More samples than any segment has (an hour of 240 fps).
const MAX_SAMPLES: usize = 1 << 20;
/// Pushes after which a stream still without its setup (an audio PID that
/// never carries anything, say) is left out.
const DECIDE_AFTER: usize = 3;
/// Video timestamps' timescale (MPEG-TS's).
const VIDEO_TIMESCALE: u32 = mpegts::TIMESCALE;
/// A video sample's duration when there's nothing to tell it by (30 fps).
const DEFAULT_FRAME: u32 = 3000;

/// A sample of a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub data: Vec<u8>,
    /// In the track's timescale.
    pub duration: u32,
    /// Presentation time minus decode time.
    pub composition_offset: i32,
    pub keyframe: bool,
}

/// The init segment of an H.264 track (`avc1` with an `avcC`).
pub fn avc_init(
    sps: &[u8],
    pps: &[u8],
    width: u32,
    height: u32,
    timescale: u32,
) -> Result<Vec<u8>> {
    ensure!(sps.len() >= 4, "the SPS is too short");
    let sps_len = u16::try_from(sps.len()).context("the SPS is too long")?;
    let pps_len = u16::try_from(pps.len()).context("the PPS is too long")?;
    let w = u16::try_from(width).context("the video is too wide")?;
    let h = u16::try_from(height).context("the video is too tall")?;
    ensure!(timescale > 0, "a zero timescale");
    let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    avcc.extend_from_slice(&sps_len.to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1);
    avcc.extend_from_slice(&pps_len.to_be_bytes());
    avcc.extend_from_slice(pps);
    // High profiles say their chroma format and bit depths too.
    if let Ok(info) = mpegts::parse_sps(sps)
        && matches!(info.profile_idc, 100 | 110 | 122 | 244)
    {
        avcc.extend_from_slice(&[
            0xFC | info.chroma_format_idc,
            0xF8 | (info.bit_depth_luma - 8),
            0xF8 | (info.bit_depth_chroma - 8),
            0,
        ]);
    }
    Ok(init(
        timescale,
        b"vide",
        (width, height),
        |o| full(o, b"vmhd", 0, 1, |o| o.extend_from_slice(&[0; 8])),
        |o| {
            bx(o, b"avc1", |o| {
                o.extend_from_slice(&[0; 6]);
                o.extend_from_slice(&1u16.to_be_bytes()); // data reference
                o.extend_from_slice(&[0; 16]);
                o.extend_from_slice(&w.to_be_bytes());
                o.extend_from_slice(&h.to_be_bytes());
                o.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
                o.extend_from_slice(&0x0048_0000u32.to_be_bytes());
                o.extend_from_slice(&[0; 4]);
                o.extend_from_slice(&1u16.to_be_bytes()); // frames per sample
                o.extend_from_slice(&[0; 32]); // compressor name
                o.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
                o.extend_from_slice(&0xFFFFu16.to_be_bytes());
                bx(o, b"avcC", |o| o.extend_from_slice(&avcc));
            })
        },
    ))
}

/// The init segment of an AAC track (`mp4a` with an `esds`), its
/// timescale the sample rate.
pub fn aac_init(asc: &[u8], sample_rate: u32, channels: u16) -> Result<Vec<u8>> {
    ensure!(sample_rate > 0, "a zero sample rate");
    ensure!(asc.len() < 128, "the AudioSpecificConfig is too long");
    let mut decoder = vec![
        0x40, // MPEG-4 audio
        0x15, // an audio stream
        0, 0, 0, // buffer size
    ];
    decoder.extend_from_slice(&[0; 8]); // max and average bit rate
    descriptor(&mut decoder, 0x05, asc);
    let mut es = vec![0, 0, 0]; // ES_ID (0 in MP4 files), flags
    descriptor(&mut es, 0x04, &decoder);
    descriptor(&mut es, 0x06, &[0x02]);
    let mut esds = Vec::new();
    descriptor(&mut esds, 0x03, &es);
    // A 16.16 number: rates over 65535 Hz don't fit (and players go by
    // the AudioSpecificConfig anyway).
    let rate = if sample_rate <= 0xFFFF {
        sample_rate << 16
    } else {
        0
    };
    Ok(init(
        sample_rate,
        b"soun",
        (0, 0),
        |o| full(o, b"smhd", 0, 0, |o| o.extend_from_slice(&[0; 4])),
        |o| {
            bx(o, b"mp4a", |o| {
                o.extend_from_slice(&[0; 6]);
                o.extend_from_slice(&1u16.to_be_bytes()); // data reference
                o.extend_from_slice(&[0; 8]);
                o.extend_from_slice(&channels.to_be_bytes());
                o.extend_from_slice(&16u16.to_be_bytes()); // sample size
                o.extend_from_slice(&[0; 4]);
                o.extend_from_slice(&rate.to_be_bytes());
                full(o, b"esds", 0, 0, |o| o.extend_from_slice(&esds));
            })
        },
    ))
}

/// A media segment: a `moof` for `samples` starting at `base_decode_time`
/// (in the track's timescale), and an `mdat` with their data.
pub fn fragment(sequence: u32, base_decode_time: u64, samples: &[Sample]) -> Result<Vec<u8>> {
    ensure!(!samples.is_empty(), "a fragment without samples");
    ensure!(
        samples.len() <= MAX_SAMPLES,
        "too many samples for a fragment"
    );
    let mut data_len = 0u64;
    for s in samples {
        ensure!(u32::try_from(s.data.len()).is_ok(), "a sample over 4 GB");
        data_len += s.data.len() as u64;
    }
    // A 64-bit size if it has to be.
    let large = data_len + 8 > u64::from(u32::MAX);
    let mdat_header = if large { 16 } else { 8 };
    let offsets = samples.iter().any(|s| s.composition_offset != 0);
    let flags = 0x0001 | 0x0100 | 0x0200 | 0x0400 | if offsets { 0x0800 } else { 0 };

    let mut out = Vec::with_capacity(data_len as usize + 128 + samples.len() * 16);
    let mut offset_at = 0;
    bx(&mut out, b"moof", |o| {
        full(o, b"mfhd", 0, 0, |o| {
            o.extend_from_slice(&sequence.to_be_bytes())
        });
        bx(o, b"traf", |o| {
            // default-base-is-moof: data offsets count from the moof.
            full(o, b"tfhd", 0, 0x02_0000, |o| {
                o.extend_from_slice(&TRACK_ID.to_be_bytes())
            });
            full(o, b"tfdt", 1, 0, |o| {
                o.extend_from_slice(&base_decode_time.to_be_bytes())
            });
            full(o, b"trun", 1, flags, |o| {
                o.extend_from_slice(&(samples.len() as u32).to_be_bytes());
                offset_at = o.len();
                o.extend_from_slice(&[0; 4]);
                for s in samples {
                    o.extend_from_slice(&s.duration.to_be_bytes());
                    o.extend_from_slice(&(s.data.len() as u32).to_be_bytes());
                    let flags = if s.keyframe {
                        SYNC_SAMPLE
                    } else {
                        OTHER_SAMPLE
                    };
                    o.extend_from_slice(&flags.to_be_bytes());
                    if offsets {
                        o.extend_from_slice(&s.composition_offset.to_be_bytes());
                    }
                }
            });
        });
    });
    let data_offset = i32::try_from(out.len() + mdat_header).context("the moof is too big")?;
    out[offset_at..offset_at + 4].copy_from_slice(&data_offset.to_be_bytes());
    if large {
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&(data_len + 16).to_be_bytes());
    } else {
        out.extend_from_slice(&(data_len as u32 + 8).to_be_bytes());
        out.extend_from_slice(b"mdat");
    }
    for s in samples {
        out.extend_from_slice(&s.data);
    }
    Ok(out)
}

/// `ftyp` + `moov` for one track; `media_header` writes its `vmhd` or
/// `smhd`, `sample_entry` its `stsd` entry.
fn init(
    timescale: u32,
    handler: &[u8; 4],
    (width, height): (u32, u32),
    media_header: impl FnOnce(&mut Vec<u8>),
    sample_entry: impl FnOnce(&mut Vec<u8>),
) -> Vec<u8> {
    let sound = handler == b"soun";
    let mut out = Vec::new();
    bx(&mut out, b"ftyp", |o| {
        o.extend_from_slice(b"iso5");
        o.extend_from_slice(&512u32.to_be_bytes());
        o.extend_from_slice(b"iso5iso6mp41");
    });
    bx(&mut out, b"moov", |o| {
        full(o, b"mvhd", 0, 0, |o| {
            o.extend_from_slice(&[0; 8]); // creation, modification
            o.extend_from_slice(&timescale.to_be_bytes());
            o.extend_from_slice(&0u32.to_be_bytes()); // duration
            o.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
            o.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
            o.extend_from_slice(&[0; 10]);
            matrix(o);
            o.extend_from_slice(&[0; 24]);
            o.extend_from_slice(&(TRACK_ID + 1).to_be_bytes()); // next track
        });
        bx(o, b"trak", |o| {
            // Enabled, in the movie.
            full(o, b"tkhd", 0, 3, |o| {
                o.extend_from_slice(&[0; 8]);
                o.extend_from_slice(&TRACK_ID.to_be_bytes());
                o.extend_from_slice(&[0; 4]);
                o.extend_from_slice(&0u32.to_be_bytes()); // duration
                o.extend_from_slice(&[0; 8]);
                o.extend_from_slice(&[0; 4]); // layer, alternate group
                let volume: u16 = if sound { 0x0100 } else { 0 };
                o.extend_from_slice(&volume.to_be_bytes());
                o.extend_from_slice(&[0; 2]);
                matrix(o);
                o.extend_from_slice(&(width << 16).to_be_bytes());
                o.extend_from_slice(&(height << 16).to_be_bytes());
            });
            bx(o, b"mdia", |o| {
                full(o, b"mdhd", 0, 0, |o| {
                    o.extend_from_slice(&[0; 8]);
                    o.extend_from_slice(&timescale.to_be_bytes());
                    o.extend_from_slice(&0u32.to_be_bytes()); // duration
                    o.extend_from_slice(&0x55C4u16.to_be_bytes()); // "und"
                    o.extend_from_slice(&[0; 2]);
                });
                full(o, b"hdlr", 0, 0, |o| {
                    o.extend_from_slice(&[0; 4]);
                    o.extend_from_slice(handler);
                    o.extend_from_slice(&[0; 12]);
                    let name: &[u8] = if sound {
                        b"SoundHandler\0"
                    } else {
                        b"VideoHandler\0"
                    };
                    o.extend_from_slice(name);
                });
                bx(o, b"minf", |o| {
                    media_header(o);
                    bx(o, b"dinf", |o| {
                        full(o, b"dref", 0, 0, |o| {
                            o.extend_from_slice(&1u32.to_be_bytes());
                            // In this file.
                            full(o, b"url ", 0, 1, |_| {});
                        })
                    });
                    bx(o, b"stbl", |o| {
                        full(o, b"stsd", 0, 0, |o| {
                            o.extend_from_slice(&1u32.to_be_bytes());
                            sample_entry(o);
                        });
                        // No samples here: they're all in fragments.
                        full(o, b"stts", 0, 0, |o| o.extend_from_slice(&[0; 4]));
                        full(o, b"stsc", 0, 0, |o| o.extend_from_slice(&[0; 4]));
                        full(o, b"stsz", 0, 0, |o| o.extend_from_slice(&[0; 8]));
                        full(o, b"stco", 0, 0, |o| o.extend_from_slice(&[0; 4]));
                    });
                });
            });
        });
        bx(o, b"mvex", |o| {
            full(o, b"trex", 0, 0, |o| {
                o.extend_from_slice(&TRACK_ID.to_be_bytes());
                o.extend_from_slice(&1u32.to_be_bytes()); // sample description
                o.extend_from_slice(&[0; 12]); // duration, size, flags
            })
        });
    });
    out
}

fn bx(out: &mut Vec<u8>, kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(kind);
    body(out);
    // (Callers keep boxes far under 4 GB.)
    let size = u32::try_from(out.len() - start).unwrap_or(u32::MAX);
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

fn full(
    out: &mut Vec<u8>,
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    body: impl FnOnce(&mut Vec<u8>),
) {
    bx(out, kind, |o| {
        o.push(version);
        o.extend_from_slice(&flags.to_be_bytes()[1..]);
        body(o);
    });
}

fn matrix(out: &mut Vec<u8>) {
    for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        out.extend_from_slice(&m.to_be_bytes());
    }
}

/// An MPEG-4 descriptor (in an `esds`): tag, length, body.
fn descriptor(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(tag);
    let len = body.len() as u32;
    out.extend_from_slice(&[
        0x80 | (len >> 21 & 0x7F) as u8,
        0x80 | (len >> 14 & 0x7F) as u8,
        0x80 | (len >> 7 & 0x7F) as u8,
        (len & 0x7F) as u8,
    ]);
    out.extend_from_slice(body);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

/// A track the [`Remuxer`] makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// Its init segment, for `fmp4::Init::parse`.
    pub init: Vec<u8>,
    pub kind: TrackKind,
    /// Ticks per second: 90 kHz for video, the sample rate for audio.
    pub timescale: u32,
    /// RFC 6381, like `avc1.64001f` or `mp4a.40.2`.
    pub codec: String,
}

/// A stream this can't remux, by codec: "HEVC", "AC-3", …
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub codec: String,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} isn't supported", self.codec)
    }
}

impl std::error::Error for Unsupported {}

/// Turns MPEG-TS segments, pushed in order, into fragmented MP4: an init
/// segment per track (once [`Remuxer::tracks`] knows them), then a
/// fragment per track per segment.
///
/// The video and the audio are the first H.264 and AAC streams in the PMT.
/// If the first video or audio stream is something else (and no H.264 or
/// AAC one is there), that's [`Unsupported`]: the caller can keep the TS
/// as it is instead.
///
/// Video samples' durations come from the next sample's decode time, so
/// each segment's last one waits for the next segment (the very last takes
/// the one before's). Video before the first keyframe is dropped. Audio
/// frames are 1024 samples each, placed one after another: where the
/// timestamps have a gap it's kept, and a frame that would overlap the
/// ones before by half a frame or more is dropped.
#[derive(Debug, Default)]
pub struct Remuxer {
    demux: Demuxer,
    pushes: usize,
    tracks: Tracks,
    /// Where both tracks' times count from (90 kHz).
    zero: i64,
    /// Frames not yet in a fragment, by PID.
    queues: HashMap<u16, Vec<Frame>>,
    /// Video streams that have had a keyframe since the start (or the last
    /// discontinuity).
    keyed: HashSet<u16>,
    /// Added to every timestamp, to carry on after a discontinuity.
    shift: i64,
    /// Where the stream so far ends, for the next timestamps to continue
    /// from (after a discontinuity); and whether that's the video's end
    /// (the next video carries on from it, the audio keeps its place
    /// beside the video).
    resume_at: Option<(i64, bool)>,
    /// Each stream's last decode time and the step to it, to tell where
    /// the stream ends.
    ends: HashMap<u16, (i64, i64)>,
    sequence: u32,
}

#[derive(Debug, Default)]
enum Tracks {
    #[default]
    Undecided,
    Ready(Vec<Track>),
    Unsupported(Unsupported),
}

#[derive(Debug)]
struct Track {
    pid: u16,
    info: TrackInfo,
    /// The next sample's decode time, in the track's timescale.
    next: u64,
    /// The last video sample's duration, for the very last one.
    last_duration: u32,
}

impl Remuxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remuxes the next segment (decrypted). Fails on what isn't MPEG-TS,
    /// or what can't be read past.
    pub fn push(&mut self, segment: &[u8]) -> Result<()> {
        self.demux.push(segment)?;
        self.pushes += 1;
        self.ingest();
        self.decide(false)
    }

    /// Says the next segment follows a discontinuity (`EXT-X-DISCONTINUITY`):
    /// its timestamps may start over, so they're moved on to carry on from
    /// where the stream so far ends (the video's end, if there's video).
    /// Video waits for a keyframe again.
    pub fn discontinuity(&mut self) -> Result<()> {
        self.demux.flush()?;
        self.ingest();
        self.decide(false)?;
        let video = self.video_pids();
        let end = |video_only: bool| {
            self.ends
                .iter()
                .filter(|(pid, _)| !video_only || video.contains(pid))
                .map(|(_, &(dts, step))| dts.saturating_add(step))
                .max()
        };
        self.resume_at = match end(true) {
            Some(at) => Some((at, true)),
            None => end(false).map(|at| (at, false)),
        };
        self.keyed.clear();
        Ok(())
    }

    /// The tracks, video first: none until a segment has had the codecs'
    /// setup (push another), or the codec that isn't supported.
    pub fn tracks(&self) -> Option<Result<Vec<TrackInfo>, Unsupported>> {
        match &self.tracks {
            Tracks::Undecided => None,
            Tracks::Unsupported(u) => Some(Err(u.clone())),
            Tracks::Ready(tracks) => Some(Ok(tracks.iter().map(|t| t.info.clone()).collect())),
        }
    }

    /// Where both tracks' times count from, in seconds on the stream's own
    /// (90 kHz) clock: what's 0 in the MP4 (once the tracks are known).
    pub fn zero_seconds(&self) -> f64 {
        self.zero as f64 / 90_000.0
    }

    /// The fragments ready so far, as (track index, media segment): at
    /// most one per track, video first. None before the tracks are known.
    pub fn take_fragments(&mut self) -> Result<Vec<(usize, Vec<u8>)>> {
        self.fragments(false)
    }

    /// Ends the stream: the last fragments.
    pub fn finish(mut self) -> Result<Vec<(usize, Vec<u8>)>> {
        self.demux.flush()?;
        self.ingest();
        self.decide(true)?;
        if let Tracks::Undecided = self.tracks {
            if self.demux.streams().is_empty() {
                bail!("no program (PAT and PMT) in the stream");
            }
            bail!("no video or audio frames in the stream");
        }
        self.fragments(true)
    }

    /// The H.264 streams (just the one in use, once the tracks are known).
    fn video_pids(&self) -> Vec<u16> {
        let chosen: Option<Vec<u16>> = match &self.tracks {
            Tracks::Ready(tracks) => Some(tracks.iter().map(|t| t.pid).collect()),
            _ => None,
        };
        self.demux
            .streams()
            .iter()
            .filter(|s| s.codec == Codec::H264)
            .map(|s| s.pid)
            .filter(|pid| chosen.as_ref().is_none_or(|c| c.contains(pid)))
            .collect()
    }

    /// Moves the demuxer's frames into the queues.
    fn ingest(&mut self) {
        let frames = self.demux.take_frames();
        let video = self.video_pids();
        if let Some((at, from_video)) = self.resume_at {
            // The first video frame's decode time goes where the video
            // ended; else the first frame's time where the stream did.
            let first_video = frames
                .iter()
                .find(|f| from_video && video.contains(&f.pid) && f.keyframe)
                .map(|f| f.dts);
            let first = first_video.or_else(|| frames.iter().map(|f| f.dts.min(f.pts)).min());
            if let Some(first) = first {
                self.shift = at.saturating_sub(first);
                self.resume_at = None;
            }
        }
        let chosen: Option<Vec<u16>> = match &self.tracks {
            Tracks::Ready(tracks) => Some(tracks.iter().map(|t| t.pid).collect()),
            _ => None,
        };
        for mut f in frames {
            if f.data.is_empty() || chosen.as_ref().is_some_and(|c| !c.contains(&f.pid)) {
                continue;
            }
            f.pts = f.pts.saturating_add(self.shift);
            f.dts = f.dts.saturating_add(self.shift);
            if video.contains(&f.pid) && !self.keyed.contains(&f.pid) {
                if !f.keyframe {
                    continue;
                }
                self.keyed.insert(f.pid);
            }
            let end = self.ends.entry(f.pid).or_insert((f.dts, 0));
            if f.dts > end.0 {
                *end = (f.dts, f.dts.saturating_sub(end.0));
            }
            self.queues.entry(f.pid).or_default().push(f);
        }
    }

    /// Settles the tracks once each stream has its setup and a frame (or
    /// enough segments have gone by, or the stream ends, for those that
    /// do).
    fn decide(&mut self, finishing: bool) -> Result<()> {
        if !matches!(self.tracks, Tracks::Undecided) || self.demux.streams().is_empty() {
            return Ok(());
        }
        let streams = self.demux.streams().to_vec();
        let pick = |kind: Kind, codec: Codec| -> Result<Option<Stream>, Unsupported> {
            let mut of_kind = streams.iter().filter(|s| s.kind == kind);
            match of_kind.clone().find(|s| s.codec == codec) {
                Some(s) => Ok(Some(s.clone())),
                None => match of_kind.next() {
                    Some(s) => Err(Unsupported {
                        codec: s.codec.name().to_string(),
                    }),
                    None => Ok(None),
                },
            }
        };
        let picked =
            pick(Kind::Video, Codec::H264).and_then(|v| Ok((v, pick(Kind::Audio, Codec::Aac)?)));
        let chosen: Vec<Stream> = match picked {
            Err(u) => {
                self.tracks = Tracks::Unsupported(u);
                return Ok(());
            }
            Ok((None, None)) => {
                self.tracks = Tracks::Unsupported(Unsupported {
                    codec: "a stream without video or audio".to_string(),
                });
                return Ok(());
            }
            Ok((v, a)) => v.into_iter().chain(a).collect(),
        };
        let ready = |s: &Stream| {
            let setup = match s.codec {
                Codec::H264 => self.demux.avc_config(s.pid).is_some(),
                _ => self.demux.aac_config(s.pid).is_some(),
            };
            setup && self.queues.get(&s.pid).is_some_and(|q| !q.is_empty())
        };
        let all = chosen.iter().all(ready);
        let late = finishing || self.pushes >= DECIDE_AFTER;
        if !all && !(late && chosen.iter().any(ready)) {
            return Ok(());
        }
        let chosen: Vec<Stream> = chosen.into_iter().filter(|s| ready(s)).collect();
        self.zero = chosen
            .iter()
            .filter_map(|s| self.queues.get(&s.pid)?.first().map(|f| f.dts))
            .min()
            .unwrap_or(0);
        let mut tracks = Vec::new();
        for s in &chosen {
            let first = self.queues.get(&s.pid).and_then(|q| q.first());
            let since_zero = first.map_or(0, |f| f.dts.saturating_sub(self.zero).max(0) as u64);
            let track = if s.codec == Codec::H264 {
                let c = self.demux.avc_config(s.pid).context("no SPS and PPS")?;
                Track {
                    pid: s.pid,
                    info: TrackInfo {
                        init: avc_init(
                            &c.sps,
                            &c.pps,
                            c.info.width,
                            c.info.height,
                            VIDEO_TIMESCALE,
                        )?,
                        kind: TrackKind::Video,
                        timescale: VIDEO_TIMESCALE,
                        codec: format!(
                            "avc1.{:02x}{:02x}{:02x}",
                            c.info.profile_idc, c.info.constraints, c.info.level_idc
                        ),
                    },
                    next: since_zero,
                    last_duration: DEFAULT_FRAME,
                }
            } else {
                let c = self.demux.aac_config(s.pid).context("no AAC setup")?;
                Track {
                    pid: s.pid,
                    info: TrackInfo {
                        init: aac_init(&c.asc, c.sample_rate, c.channels)?,
                        kind: TrackKind::Audio,
                        timescale: c.sample_rate,
                        codec: format!("mp4a.40.{}", c.object_type),
                    },
                    next: rescale(since_zero, c.sample_rate),
                    last_duration: AAC_FRAME,
                }
            };
            tracks.push(track);
        }
        self.queues
            .retain(|pid, _| chosen.iter().any(|s| s.pid == *pid));
        self.tracks = Tracks::Ready(tracks);
        Ok(())
    }

    fn fragments(&mut self, last: bool) -> Result<Vec<(usize, Vec<u8>)>> {
        let tracks = match &mut self.tracks {
            Tracks::Undecided => return Ok(Vec::new()),
            Tracks::Unsupported(u) => return Err(u.clone().into()),
            Tracks::Ready(tracks) => tracks,
        };
        let mut out = Vec::new();
        for (i, track) in tracks.iter_mut().enumerate() {
            let Some(queue) = self.queues.get_mut(&track.pid) else {
                continue;
            };
            let (base, samples) = match track.info.kind {
                TrackKind::Video => video_samples(queue, track, last),
                TrackKind::Audio => audio_samples(queue, track, self.zero),
            };
            if samples.is_empty() {
                continue;
            }
            out.push((i, fragment(self.sequence, base, &samples)?));
            self.sequence = self.sequence.wrapping_add(1);
        }
        Ok(out)
    }
}

/// The queue's video frames as samples (all but the last, unless `last`),
/// and the first's decode time.
fn video_samples(queue: &mut Vec<Frame>, track: &mut Track, last: bool) -> (u64, Vec<Sample>) {
    let n = if last {
        queue.len()
    } else {
        queue.len().saturating_sub(1)
    };
    let frames: Vec<Frame> = queue.drain(..n).collect();
    // Each one's successor's decode time (the held-back one's, for the
    // last, if there is one).
    let mut next_dts: Vec<Option<i64>> = frames.iter().skip(1).map(|f| Some(f.dts)).collect();
    next_dts.push(queue.first().map(|f| f.dts));
    let base = track.next;
    let mut samples = Vec::with_capacity(frames.len());
    for (f, next) in frames.into_iter().zip(next_dts) {
        let duration = match next {
            // (Out-of-order or repeated decode times: still forward.)
            Some(next) => next.saturating_sub(f.dts).clamp(1, i64::from(u32::MAX)) as u32,
            None => track.last_duration,
        };
        track.last_duration = duration;
        track.next = track.next.saturating_add(u64::from(duration));
        samples.push(Sample {
            composition_offset: f
                .pts
                .saturating_sub(f.dts)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                as i32,
            keyframe: f.keyframe,
            data: f.data,
            duration,
        });
    }
    (base, samples)
}

/// The queue's audio frames as samples, and the first's decode time.
fn audio_samples(queue: &mut Vec<Frame>, track: &mut Track, zero: i64) -> (u64, Vec<Sample>) {
    let rate = track.info.timescale;
    let frame = u64::from(AAC_FRAME);
    let mut base = track.next;
    let mut samples: Vec<Sample> = Vec::with_capacity(queue.len());
    for f in std::mem::take(queue) {
        let at = rescale(f.pts.saturating_sub(zero).max(0) as u64, rate);
        if at.saturating_add(frame / 2) < track.next {
            // Over what's there already.
            continue;
        }
        if at > track.next.saturating_add(frame / 2) {
            // A gap: the frame before lasts until this one.
            let gap = at - track.next;
            match samples.last_mut() {
                Some(before) => {
                    let longer = u64::from(before.duration).saturating_add(gap);
                    before.duration = u32::try_from(longer).unwrap_or(u32::MAX);
                }
                None => base = at,
            }
            track.next = at;
        }
        track.next = track.next.saturating_add(frame);
        samples.push(Sample {
            data: f.data,
            duration: AAC_FRAME,
            composition_offset: 0,
            keyframe: true,
        });
    }
    (base, samples)
}

/// 90 kHz ticks in ticks at `rate` per second, rounded.
fn rescale(ticks: u64, rate: u32) -> u64 {
    let r = (u128::from(ticks) * u128::from(rate) + u128::from(VIDEO_TIMESCALE / 2))
        / u128::from(VIDEO_TIMESCALE);
    u64::try_from(r).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::{Init, Muxer, Track as MuxTrack};

    const SPS: [u8; 27] = [
        0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00, 0x00,
        0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x60, 0xc6, 0x58,
    ];
    const PPS: [u8; 5] = [0x68, 0xeb, 0xe3, 0xcb, 0x22];

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// The boxes laid end to end in `b`: (type, start, end).
    fn boxes(b: &[u8]) -> Vec<([u8; 4], usize, usize)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < b.len() {
            let size = be32(b, at) as usize;
            out.push((b[at + 4..at + 8].try_into().unwrap(), at, at + size));
            at += size;
        }
        out
    }

    fn find(b: &[u8], path: &[&[u8; 4]]) -> (usize, usize) {
        let (mut start, mut end) = (0, b.len());
        for kind in path {
            let (_, s, e) = boxes(&b[start..end])
                .into_iter()
                .find(|(k, _, _)| k == *kind)
                .unwrap_or_else(|| panic!("no {}", String::from_utf8_lossy(*kind)));
            let skip = match *kind {
                b"stsd" => 16,
                b"avc1" => 8 + 78,
                b"mp4a" => 8 + 28,
                b"dref" => 16,
                _ => 8,
            };
            (start, end) = (start + s + skip, start + e);
        }
        (start, end)
    }

    #[test]
    fn writes_init_segments_the_muxer_reads() {
        let video = avc_init(&SPS, &PPS, 1920, 1080, 90_000).unwrap();
        let audio = aac_init(&[0x11, 0x90], 48_000, 2).unwrap();
        let v = Init::parse(video.clone()).unwrap();
        let a = Init::parse(audio.clone()).unwrap();
        assert_eq!(
            (v.handler(), v.timescale(), v.track_id()),
            (*b"vide", 90_000, 1)
        );
        assert_eq!(
            (a.handler(), a.timescale(), a.track_id()),
            (*b"soun", 48_000, 1)
        );

        // The avcC: header, the SPS and PPS, and High's extension.
        let path: [&[u8; 4]; 8] = [
            b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"avc1", b"avcC",
        ];
        let (s, e) = find(&video, &path);
        let avcc = &video[s..e];
        assert_eq!(&avcc[..6], &[1, 0x64, 0x00, 0x28, 0xFF, 0xE1]);
        assert_eq!(&avcc[8..8 + SPS.len()], &SPS);
        assert_eq!(&avcc[avcc.len() - 4..], &[0xFD, 0xF8, 0xF8, 0]);
        let (s, _) = find(&video, &path[..7]);
        // avc1's width and height (after its 24 bytes of zeros and index).
        let entry = s - 78;
        assert_eq!(&video[entry + 24..entry + 28], &[0x07, 0x80, 0x04, 0x38]);

        // The esds holds the AudioSpecificConfig.
        let (s, e) = find(
            &audio,
            &[
                b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"mp4a", b"esds",
            ],
        );
        let esds = &audio[s..e];
        assert!(
            esds.windows(7)
                .any(|w| w == [0x05, 0x80, 0x80, 0x80, 2, 0x11, 0x90])
        );

        assert!(avc_init(&SPS[..3], &PPS, 1, 1, 1).is_err());
        assert!(avc_init(&SPS, &PPS, 70_000, 1, 1).is_err());
        assert!(aac_init(&[0x11, 0x90], 0, 2).is_err());
    }

    fn samples() -> Vec<Sample> {
        (0..5)
            .map(|i| Sample {
                data: vec![i as u8; 10 + i],
                duration: 3600,
                composition_offset: [7200, -3600, 0, 3600, 0][i],
                keyframe: i == 0,
            })
            .collect()
    }

    #[test]
    fn writes_fragments_the_muxer_joins() {
        let s = samples();
        let frag = fragment(9, 123_456, &s).unwrap();
        let top = boxes(&frag);
        assert_eq!(
            top.iter().map(|b| &b.0).collect::<Vec<_>>(),
            [b"moof", b"mdat"]
        );
        let (moof_end, mdat_start) = (top[0].2, top[1].1);
        assert_eq!(moof_end, mdat_start);
        let (s0, _) = find(&frag, &[b"moof", b"traf", b"tfdt"]);
        assert_eq!(frag[s0], 1);
        assert_eq!(
            u64::from_be_bytes(frag[s0 + 4..s0 + 12].try_into().unwrap()),
            123_456
        );
        let (t, _) = find(&frag, &[b"moof", b"traf", b"trun"]);
        assert_eq!(&frag[t..t + 4], &[1, 0, 0x0F, 0x01]);
        assert_eq!(be32(&frag, t + 4), 5);
        assert_eq!(be32(&frag, t + 8) as usize, mdat_start + 8);
        // Second sample: duration, size, flags, a negative offset.
        let e = t + 12 + 16;
        assert_eq!(be32(&frag, e), 3600);
        assert_eq!(be32(&frag, e + 4), 11);
        assert_eq!(be32(&frag, e + 8), OTHER_SAMPLE);
        assert_eq!(be32(&frag, e + 12) as i32, -3600);
        assert_eq!(be32(&frag, t + 12 + 8), SYNC_SAMPLE);
        assert_eq!(&frag[mdat_start + 8..mdat_start + 18], &[0; 10]);

        // No offsets: no column for them.
        let plain: Vec<Sample> = s
            .iter()
            .map(|s| Sample {
                composition_offset: 0,
                ..s.clone()
            })
            .collect();
        let frag2 = fragment(1, 0, &plain).unwrap();
        let (t, _) = find(&frag2, &[b"moof", b"traf", b"trun"]);
        assert_eq!(&frag2[t..t + 4], &[1, 0, 0x07, 0x01]);
        assert!(fragment(1, 0, &[]).is_err());

        let video = Init::parse(avc_init(&SPS, &PPS, 1920, 1080, 90_000).unwrap()).unwrap();
        let (mut mux, header) = Muxer::new(&[MuxTrack {
            init: video,
            duration: 36_000,
        }])
        .unwrap();
        let mut file = header;
        file.extend(mux.fragments(0, &frag).unwrap());
        file.extend(mux.fragments(0, &frag2).unwrap());
        file.extend(mux.finish());
        let kinds: Vec<[u8; 4]> = boxes(&file).into_iter().map(|b| b.0).collect();
        assert_eq!(
            kinds,
            [
                *b"ftyp", *b"moov", *b"moof", *b"mdat", *b"moof", *b"mdat", *b"mfra"
            ]
        );
    }

    #[test]
    fn rescales_to_the_sample_rate() {
        assert_eq!(rescale(90_000, 48_000), 48_000);
        assert_eq!(rescale(1920, 48_000), 1024);
        assert_eq!(rescale(2090, 44_100), 1024);
        assert_eq!(rescale(0, 44_100), 0);
    }

    #[test]
    fn says_what_is_unsupported() {
        let u = Unsupported {
            codec: "HEVC".to_string(),
        };
        assert_eq!(u.to_string(), "HEVC isn't supported");
        let mut r = Remuxer::new();
        assert!(r.tracks().is_none());
        assert!(r.take_fragments().unwrap().is_empty());
        assert!(r.push(b"<html>nope</html>".repeat(40).as_slice()).is_err());
        assert!(Remuxer::new().finish().is_err());
    }
}
