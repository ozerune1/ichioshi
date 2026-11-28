//! A demuxer for MPEG transport streams (ISO/IEC 13818-1) as HLS serves
//! them: one program, H.264 video and AAC (ADTS) audio.
//!
//! Bytes are pushed in as they come, a segment at a time; a packet cut at
//! the end of one push is finished by the next. Packets are 188 bytes;
//! after junk the next sync byte (0x47) that has another one 188 bytes on
//! is taken as the start of a packet. The PAT points at the PMT, which
//! lists the elementary streams. Codecs other than H.264 and AAC are listed
//! too, so the caller can say what isn't supported, but their data is
//! dropped.
//!
//! PES packets are put back together (a payload unit start begins one; a
//! video PES's length is often 0, for "until the next one"). Their
//! timestamps (33 bits at 90 kHz, so they wrap every 26.5 hours) are
//! unwrapped onto one line across the whole stream. Continuity-counter
//! gaps are let through (the damaged frame is kept); a repeated packet is
//! dropped.
//!
//! H.264 comes out an access unit at a time (one per PES with a PTS), as
//! an MP4 sample: each NAL unit after a 4-byte length, access unit
//! delimiters and filler dropped. The first SPS and PPS are kept for the
//! `avcC`. AAC comes out a raw frame at a time (the ADTS header taken off),
//! each frame 1024 samples after the one before it in its PES.

use std::borrow::Cow;
use std::collections::HashMap;

use anyhow::{Context, Result, bail, ensure};

/// Timestamps' ticks per second.
pub const TIMESCALE: u32 = 90_000;

const PACKET: usize = 188;
const SYNC: u8 = 0x47;
/// Timestamps wrap at 2^33.
const WRAP: i64 = 1 << 33;
/// The largest PES (an access unit) taken; a 4K keyframe is a few MB.
const MAX_PES: usize = 64 << 20;
/// The largest PSI section (PAT, PMT) is 1024 bytes.
const MAX_SECTION: usize = 4096;
/// Samples in an AAC frame.
pub const AAC_FRAME: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Aac,
    /// One this doesn't demux, by name: "HEVC", "AC-3", "ID3 metadata", …
    Other(&'static str),
}

impl Codec {
    pub fn name(&self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Aac => "AAC",
            Codec::Other(name) => name,
        }
    }
}

/// An elementary stream, as the PMT lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub pid: u16,
    pub stream_type: u8,
    pub kind: Kind,
    pub codec: Codec,
}

/// An H.264 access unit or an AAC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The elementary stream it's from.
    pub pid: u16,
    /// Presentation and decode time, at 90 kHz, unwrapped (so they may be
    /// negative, or past 2^33).
    pub pts: i64,
    pub dts: i64,
    /// An MP4 sample: length-prefixed NAL units, or a raw AAC frame.
    pub data: Vec<u8>,
    /// An IDR access unit (always for audio).
    pub keyframe: bool,
}

/// What an `avcC` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcConfig {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub info: Sps,
}

/// What an `esds` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    /// The AudioSpecificConfig: object type, sampling index, channels.
    pub asc: [u8; 2],
    /// 2 is AAC LC.
    pub object_type: u8,
    pub sample_rate: u32,
    pub channels: u16,
}

/// What an SPS says that's needed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sps {
    pub profile_idc: u8,
    /// The constraint_set flags byte.
    pub constraints: u8,
    pub level_idc: u8,
    /// 0 monochrome, 1 4:2:0, 2 4:2:2, 3 4:4:4.
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    /// The picture's size after cropping, in pixels.
    pub width: u32,
    pub height: u32,
}

/// Pushes go in; streams, codec setups and frames come out.
#[derive(Debug, Default)]
pub struct Demuxer {
    /// A packet cut at the end of the last push.
    carry: Vec<u8>,
    pmt_pid: Option<u16>,
    /// PSI sections being put together, by PID.
    sections: HashMap<u16, Vec<u8>>,
    streams: Vec<Stream>,
    /// The streams demuxed here (H.264 and AAC), by PID.
    es: HashMap<u16, Es>,
    /// Each PID's last packet with a payload: its continuity counter and
    /// payload, to tell a repeated packet.
    last: HashMap<u16, (u8, Vec<u8>)>,
    clock: Clock,
    frames: Vec<Frame>,
}

#[derive(Debug)]
struct Es {
    /// The PES being put together.
    pes: Vec<u8>,
    /// Whether `pes` holds a PES's start (if not, data is dropped until
    /// the next one starts).
    started: bool,
    state: EsState,
}

#[derive(Debug)]
enum EsState {
    H264(Avc),
    Aac(Adts),
}

#[derive(Debug, Default)]
struct Avc {
    sps: Option<(Vec<u8>, Sps)>,
    pps: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct Adts {
    /// Bytes after the last whole frame.
    pending: Vec<u8>,
    /// The PTS of the frame starting `pending`, if it started in an
    /// earlier PES than the last one.
    pending_pts: Option<i64>,
    /// The last PES's PTS, and how many frames have started in it.
    base: Option<i64>,
    count: u64,
    config: Option<AacConfig>,
}

/// Unwraps 33-bit timestamps: each is taken as the one nearest the last.
#[derive(Debug, Default)]
struct Clock {
    last: Option<i64>,
}

impl Clock {
    fn unwrap(&mut self, raw: u64) -> i64 {
        let raw = (raw & (WRAP as u64 - 1)) as i64;
        let t = match self.last {
            None => raw,
            Some(last) => {
                let d = raw.wrapping_sub(last).rem_euclid(WRAP);
                let d = if d >= WRAP / 2 { d - WRAP } else { d };
                last.saturating_add(d)
            }
        };
        self.last = Some(t);
        t
    }
}

impl Demuxer {
    #[allow(dead_code, reason = "used by tests")]
    pub fn new() -> Self {
        Self::default()
    }

    /// The streams in the PMT, in its order (none before the PMT is read).
    pub fn streams(&self) -> &[Stream] {
        &self.streams
    }

    /// A video stream's SPS and PPS, once both have come.
    pub fn avc_config(&self, pid: u16) -> Option<AvcConfig> {
        match &self.es.get(&pid)?.state {
            EsState::H264(Avc {
                sps: Some((sps, info)),
                pps: Some(pps),
            }) => Some(AvcConfig {
                sps: sps.clone(),
                pps: pps.clone(),
                info: *info,
            }),
            _ => None,
        }
    }

    /// An audio stream's setup, once a frame has come.
    pub fn aac_config(&self, pid: u16) -> Option<AacConfig> {
        match &self.es.get(&pid)?.state {
            EsState::Aac(a) => a.config,
            _ => None,
        }
    }

    /// The frames finished so far, in the order they were in the stream.
    pub fn take_frames(&mut self) -> Vec<Frame> {
        std::mem::take(&mut self.frames)
    }

    /// Demuxes `bytes`, the next part of the stream. Fails if there's no TS
    /// packet in them, or on what can't be read past.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        // A push starts on a packet, unless it finishes a cut one: that
        // has to be followed by another packet to count (else the push
        // before was cut short, and this is something new).
        let mut synced = self.carry.is_empty();
        let buf: Cow<[u8]> = if synced {
            Cow::Borrowed(bytes)
        } else {
            let mut joined = std::mem::take(&mut self.carry);
            joined.extend_from_slice(bytes);
            Cow::Owned(joined)
        };
        let mut at = 0;
        let mut packets = 0usize;
        while at < buf.len() {
            if !(synced && buf[at] == SYNC) {
                let Some(found) = buf[at..].iter().position(|&b| b == SYNC) else {
                    at = buf.len();
                    break;
                };
                at += found;
                // A 0x47 in junk isn't a packet unless another follows.
                if buf.get(at + PACKET).is_some_and(|&b| b != SYNC) {
                    at += 1;
                    synced = false;
                    continue;
                }
            }
            let Some(packet) = buf.get(at..at + PACKET) else {
                break;
            };
            self.packet(packet)?;
            packets += 1;
            at += PACKET;
            synced = true;
        }
        self.carry = buf[at..].to_vec();
        ensure!(
            packets > 0 || bytes.len() < 2 * PACKET,
            "no MPEG-TS packets in {} bytes (not a TS segment?)",
            bytes.len()
        );
        Ok(())
    }

    /// Ends the stream (or a stretch of it, before a discontinuity):
    /// finishes each stream's last PES, and drops a cut packet and any
    /// part of an AAC frame.
    pub fn flush(&mut self) -> Result<()> {
        self.carry.clear();
        self.last.clear();
        let pids: Vec<u16> = self.streams.iter().map(|s| s.pid).collect();
        for pid in pids {
            self.finish_pes(pid)?;
            if let Some(Es {
                state: EsState::Aac(a),
                ..
            }) = self.es.get_mut(&pid)
            {
                a.pending.clear();
                a.pending_pts = None;
                a.base = None;
                a.count = 0;
            }
        }
        Ok(())
    }

    fn packet(&mut self, p: &[u8]) -> Result<()> {
        let (Some(&b1), Some(&b2), Some(&b3)) = (p.get(1), p.get(2), p.get(3)) else {
            return Ok(());
        };
        // Transport error: the packet is known to be bad.
        if b1 & 0x80 != 0 {
            return Ok(());
        }
        let start = b1 & 0x40 != 0;
        let pid = (u16::from(b1 & 0x1F) << 8) | u16::from(b2);
        if pid == 0x1FFF {
            return Ok(());
        }
        let control = (b3 >> 4) & 3;
        let counter = b3 & 0x0F;
        let mut at = 4;
        if control & 2 != 0 {
            let Some(&len) = p.get(4) else {
                return Ok(());
            };
            at = 5 + usize::from(len);
        }
        if control & 1 == 0 {
            return Ok(());
        }
        let Some(payload) = p.get(at..).filter(|p| !p.is_empty()) else {
            return Ok(());
        };
        if let Some((last, data)) = self.last.get_mut(&pid) {
            if *last == counter && data.as_slice() == payload {
                return Ok(());
            }
            *last = counter;
            data.clear();
            data.extend_from_slice(payload);
        } else {
            self.last.insert(pid, (counter, payload.to_vec()));
        }
        if pid == 0 || Some(pid) == self.pmt_pid {
            self.section_data(pid, start, payload);
        } else if self.es.contains_key(&pid) {
            self.pes_data(pid, start, payload)?;
        }
        Ok(())
    }

    fn section_data(&mut self, pid: u16, start: bool, payload: &[u8]) {
        let mut buf = self.sections.remove(&pid).unwrap_or_default();
        let mut complete = Vec::new();
        if start {
            let pointer = usize::from(payload[0]);
            let rest = &payload[1..];
            let (tail, head) = rest.split_at(pointer.min(rest.len()));
            if !buf.is_empty() {
                buf.extend_from_slice(tail);
                take_sections(&mut buf, &mut complete);
            }
            // Whatever is left of an earlier section is lost.
            buf.clear();
            buf.extend_from_slice(head);
        } else if !buf.is_empty() {
            buf.extend_from_slice(payload);
        }
        take_sections(&mut buf, &mut complete);
        if buf.len() > MAX_SECTION {
            buf.clear();
        }
        self.sections.insert(pid, buf);
        for section in complete {
            match section.first() {
                Some(0x00) if pid == 0 => self.pat(&section),
                Some(0x02) if Some(pid) == self.pmt_pid => self.pmt(&section),
                _ => {}
            }
        }
    }

    fn pat(&mut self, s: &[u8]) {
        let Some(body) = section_body(s) else {
            return;
        };
        for &[p0, p1, hi, lo] in body.as_chunks::<4>().0 {
            let program = u16::from_be_bytes([p0, p1]);
            // Program 0 is the network information table.
            if program != 0 {
                let pid = (u16::from(hi & 0x1F) << 8) | u16::from(lo);
                if self.pmt_pid != Some(pid) {
                    if let Some(old) = self.pmt_pid {
                        self.sections.remove(&old);
                    }
                    self.pmt_pid = Some(pid);
                }
                return;
            }
        }
    }

    fn pmt(&mut self, s: &[u8]) {
        let Some(body) = section_body(s) else {
            return;
        };
        let Some(info_len) = body.get(2..4) else {
            return;
        };
        let info_len = usize::from(u16::from_be_bytes([info_len[0] & 0x0F, info_len[1]]));
        let mut rest = body.get(4 + info_len..).unwrap_or(&[]);
        while rest.len() >= 5 {
            let stream_type = rest[0];
            let pid = (u16::from(rest[1] & 0x1F) << 8) | u16::from(rest[2]);
            let es_len = usize::from(u16::from_be_bytes([rest[3] & 0x0F, rest[4]]));
            let descriptors = rest.get(5..5 + es_len).unwrap_or(&rest[5..]);
            rest = rest.get(5 + es_len..).unwrap_or(&[]);
            let (kind, codec) = classify(stream_type, descriptors);
            let stream = Stream {
                pid,
                stream_type,
                kind,
                codec,
            };
            match self.streams.iter_mut().find(|s| s.pid == pid) {
                Some(old) if *old == stream => continue,
                Some(old) => *old = stream,
                None => self.streams.push(stream),
            }
            let state = match codec {
                Codec::H264 => Some(EsState::H264(Avc::default())),
                Codec::Aac => Some(EsState::Aac(Adts::default())),
                Codec::Other(_) => None,
            };
            match state {
                Some(state) => {
                    self.es.insert(
                        pid,
                        Es {
                            pes: Vec::new(),
                            started: false,
                            state,
                        },
                    );
                }
                None => {
                    self.es.remove(&pid);
                }
            }
        }
    }

    fn pes_data(&mut self, pid: u16, start: bool, payload: &[u8]) -> Result<()> {
        if start {
            self.finish_pes(pid)?;
        }
        let Some(es) = self.es.get_mut(&pid) else {
            return Ok(());
        };
        if start {
            es.started = true;
        } else if !es.started {
            return Ok(());
        }
        es.pes.extend_from_slice(payload);
        ensure!(
            es.pes.len() <= MAX_PES,
            "a PES packet over {} MB",
            MAX_PES >> 20
        );
        // A PES that says its length is done when it has it all.
        if let Some(len) = es.pes.get(4..6) {
            let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
            if len > 0 && es.pes.len() >= 6 + len {
                es.pes.truncate(6 + len);
                self.finish_pes(pid)?;
            }
        }
        Ok(())
    }

    /// Hands a stream's PES, if there is one, to its codec.
    fn finish_pes(&mut self, pid: u16) -> Result<()> {
        let Some(es) = self.es.get_mut(&pid) else {
            return Ok(());
        };
        if !es.started {
            return Ok(());
        }
        es.started = false;
        let pes = std::mem::take(&mut es.pes);
        let Some(header) = pes_header(&pes) else {
            return Ok(());
        };
        let payload = pes.get(header.payload..).unwrap_or(&[]);
        let times = header.pts.map(|pts| {
            let pts = self.clock.unwrap(pts);
            let dts = header.dts.map_or(pts, |dts| self.clock.unwrap(dts));
            (pts, dts)
        });
        match &mut es.state {
            EsState::H264(avc) => avc.access_unit(pid, times, payload, &mut self.frames),
            EsState::Aac(adts) => adts.data(pid, times.map(|t| t.0), payload, &mut self.frames),
        }
    }
}

/// The PSI sections that are complete at the start of `buf`, moved to
/// `out`.
fn take_sections(buf: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
    loop {
        match buf.first() {
            None => return,
            // Stuffing: no more sections in this packet.
            Some(0xFF) => {
                buf.clear();
                return;
            }
            _ => {}
        }
        let Some(len) = buf.get(1..3) else {
            return;
        };
        let len = 3 + usize::from(u16::from_be_bytes([len[0] & 0x0F, len[1]]));
        if buf.len() < len {
            return;
        }
        out.push(buf.drain(..len).collect());
    }
}

/// A long-form section's body: after its 8-byte header, before its CRC.
/// None if it's too short, or not yet in effect.
fn section_body(s: &[u8]) -> Option<&[u8]> {
    let header = s.get(..8)?;
    // section_syntax_indicator, current_next_indicator.
    if header[1] & 0x80 == 0 || header[5] & 0x01 == 0 {
        return None;
    }
    s.get(8..s.len().checked_sub(4)?)
}

/// What a PMT stream type (and, for private data, its descriptors) is.
fn classify(stream_type: u8, descriptors: &[u8]) -> (Kind, Codec) {
    use Codec::Other;
    use Kind::{Audio, Video};
    match stream_type {
        0x1B => (Video, Codec::H264),
        0x0F => (Audio, Codec::Aac),
        0x01 => (Video, Other("MPEG-1 video")),
        0x02 => (Video, Other("MPEG-2 video")),
        0x10 => (Video, Other("MPEG-4 Part 2 video")),
        0x24 => (Video, Other("HEVC")),
        0x33 => (Video, Other("VVC")),
        0x42 => (Video, Other("AVS")),
        0xEA => (Video, Other("VC-1")),
        0xDB => (Video, Other("H.264 (SAMPLE-AES encrypted)")),
        0x03 | 0x04 => (Audio, Other("MP3 (MPEG audio)")),
        0x11 => (Audio, Other("AAC (LATM)")),
        0x81 => (Audio, Other("AC-3")),
        0x87 => (Audio, Other("E-AC-3")),
        0xCF => (Audio, Other("AAC (SAMPLE-AES encrypted)")),
        0xC1 => (Audio, Other("AC-3 (SAMPLE-AES encrypted)")),
        0xC2 => (Audio, Other("E-AC-3 (SAMPLE-AES encrypted)")),
        0x15 => (Kind::Other, Other("ID3 metadata")),
        0x86 => (Kind::Other, Other("SCTE-35 cues")),
        0x06 => private_data(descriptors),
        _ => (Kind::Other, Other("an unknown stream type")),
    }
}

/// Private data (stream type 6) is told apart by its descriptors.
fn private_data(mut d: &[u8]) -> (Kind, Codec) {
    use Codec::Other;
    use Kind::{Audio, Video};
    while let [tag, len, rest @ ..] = d {
        let len = usize::from(*len);
        let body = rest.get(..len).unwrap_or(rest);
        let found = match (*tag, body) {
            (0x6A, _) | (0x05, b"AC-3") => Some((Audio, Other("AC-3"))),
            (0x7A, _) | (0x05, b"EAC3") => Some((Audio, Other("E-AC-3"))),
            (0x7B, _) | (0x05, b"DTS1" | b"DTS2" | b"DTS3") => Some((Audio, Other("DTS"))),
            (0x05, b"Opus") => Some((Audio, Other("Opus"))),
            (0x05, b"BSSD") => Some((Audio, Other("SMPTE 302M audio"))),
            (0x05, b"HEVC") => Some((Video, Other("HEVC"))),
            (0x59, _) => Some((Kind::Other, Other("DVB subtitles"))),
            (0x56, _) => Some((Kind::Other, Other("teletext"))),
            _ => None,
        };
        if let Some(found) = found {
            return found;
        }
        d = rest.get(len..).unwrap_or(&[]);
    }
    (Kind::Other, Other("private data"))
}

struct PesHeader {
    pts: Option<u64>,
    dts: Option<u64>,
    /// Where the payload starts.
    payload: usize,
}

/// None if it isn't a PES with a payload to use.
fn pes_header(b: &[u8]) -> Option<PesHeader> {
    if b.get(..3)? != [0, 0, 1] {
        return None;
    }
    let stream_id = *b.get(3)?;
    // Streams without the optional header (padding, private stream 2, …).
    if matches!(
        stream_id,
        0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF
    ) {
        return None;
    }
    let flags = *b.get(7)?;
    let header_len = usize::from(*b.get(8)?);
    let payload = 9 + header_len;
    if *b.get(6)? & 0xC0 != 0x80 || b.len() < payload {
        return None;
    }
    let (pts, dts) = match flags >> 6 {
        2 if header_len >= 5 => (timestamp(b.get(9..14)?), None),
        3 if header_len >= 10 => (timestamp(b.get(9..14)?), timestamp(b.get(14..19)?)),
        _ => (None, None),
    };
    Some(PesHeader { pts, dts, payload })
}

/// A 33-bit PTS or DTS from its 5 bytes (marker bits not checked).
fn timestamp(b: &[u8]) -> Option<u64> {
    let [a, b1, c, d, e] = *b else {
        return None;
    };
    Some(
        (u64::from(a >> 1 & 0x07) << 30)
            | (u64::from(b1) << 22)
            | (u64::from(c >> 1) << 15)
            | (u64::from(d) << 7)
            | u64::from(e >> 1),
    )
}

impl Avc {
    /// A video PES: an access unit if it has a PTS, else more of the last.
    fn access_unit(
        &mut self,
        pid: u16,
        times: Option<(i64, i64)>,
        payload: &[u8],
        frames: &mut Vec<Frame>,
    ) -> Result<()> {
        let mut data = Vec::with_capacity(payload.len() + 16);
        let mut keyframe = false;
        for nal in nal_units(payload) {
            match nal[0] & 0x1F {
                // Access unit delimiter, filler.
                9 | 12 => continue,
                5 => keyframe = true,
                7 if self.sps.is_none() => {
                    if let Ok(info) = parse_sps(nal) {
                        self.sps = Some((nal.to_vec(), info));
                    }
                }
                8 if self.pps.is_none() => self.pps = Some(nal.to_vec()),
                _ => {}
            }
            let len = u32::try_from(nal.len()).context("a NAL unit over 4 GB")?;
            data.extend_from_slice(&len.to_be_bytes());
            data.extend_from_slice(nal);
        }
        match times {
            Some((pts, dts)) => frames.push(Frame {
                pid,
                pts,
                dts,
                data,
                keyframe,
            }),
            None => {
                // The rest of an access unit not yet handed out, or lost.
                if let Some(f) = frames.iter_mut().rev().find(|f| f.pid == pid) {
                    f.data.extend_from_slice(&data);
                    f.keyframe |= keyframe;
                }
            }
        }
        Ok(())
    }
}

/// The NAL units in Annex B bytes (each after a 00 00 01 start code).
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i + 2] > 1 {
            i += 3;
        } else if data[i + 2] == 1 && data[i] == 0 && data[i + 1] == 0 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).copied().unwrap_or(data.len());
        // Zeros before a start code (a 4-byte one's first, trailing ones).
        while end > s + 3 && data[end - 1] == 0 {
            end -= 1;
        }
        if end > s + 3 {
            out.push(&data[s + 3..end]);
        }
    }
    out
}

/// The sample rates an ADTS sampling frequency index means.
const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

struct AdtsHeader {
    object_type: u8,
    rate_index: u8,
    channel_config: u8,
    header_len: usize,
    frame_len: usize,
    blocks: u8,
}

/// The ADTS header at the start of `b`, if there is a plausible one.
fn adts_header(b: &[u8]) -> Option<AdtsHeader> {
    let h = b.get(..7)?;
    // Sync word, and layer 0.
    if h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
        return None;
    }
    let header_len = if h[1] & 0x01 != 0 { 7 } else { 9 };
    let rate_index = (h[2] >> 2) & 0x0F;
    let frame_len =
        (usize::from(h[3] & 0x03) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
    if usize::from(rate_index) >= AAC_RATES.len() || frame_len <= header_len {
        return None;
    }
    Some(AdtsHeader {
        object_type: (h[2] >> 6) + 1,
        rate_index,
        channel_config: ((h[2] & 0x01) << 2) | (h[3] >> 6),
        header_len,
        frame_len,
        blocks: (h[6] & 0x03) + 1,
    })
}

impl Adts {
    /// An audio PES's payload, with its PTS if it has one.
    fn data(
        &mut self,
        pid: u16,
        pts: Option<i64>,
        payload: &[u8],
        frames: &mut Vec<Frame>,
    ) -> Result<()> {
        if let Some(pts) = pts {
            // A frame started in the PES before keeps that one's time.
            if !self.pending.is_empty() && self.pending_pts.is_none() {
                self.pending_pts = self.next_pts();
            }
            self.base = Some(pts);
            self.count = 0;
        }
        self.pending.extend_from_slice(payload);
        let mut at = 0;
        loop {
            let Some(found) = self.pending[at..].iter().position(|&b| b == 0xFF) else {
                at = self.pending.len();
                break;
            };
            at += found;
            if self.pending.len() - at < 7 {
                break;
            }
            let Some(h) = adts_header(&self.pending[at..]) else {
                at += 1;
                continue;
            };
            let Some(frame) = self.pending.get(at..at + h.frame_len) else {
                break;
            };
            ensure!(
                h.blocks == 1,
                "ADTS frames holding several AAC frames aren't supported"
            );
            let raw = frame[h.header_len..].to_vec();
            at += h.frame_len;
            if self.config.is_none() {
                self.config = Some(aac_config(&h)?);
            }
            let pts = match self.pending_pts.take() {
                Some(pts) => Some(pts),
                None => self.next_pts(),
            };
            // (Before any timestamp, a frame can't be placed.)
            if let Some(pts) = pts {
                frames.push(Frame {
                    pid,
                    pts,
                    dts: pts,
                    data: raw,
                    keyframe: true,
                });
            }
        }
        self.pending.drain(..at);
        Ok(())
    }

    /// The time of the next frame to start in the last PES.
    fn next_pts(&mut self) -> Option<i64> {
        let base = self.base?;
        let rate = i64::from(self.config.map_or(48000, |c| c.sample_rate));
        let offset =
            (self.count as i64).saturating_mul(i64::from(AAC_FRAME) * i64::from(TIMESCALE)) / rate;
        self.count += 1;
        Some(base.saturating_add(offset))
    }
}

fn aac_config(h: &AdtsHeader) -> Result<AacConfig> {
    let channels = match h.channel_config {
        0 => bail!("AAC with its channel layout in the stream (a PCE) isn't supported"),
        7 => 8,
        n => u16::from(n),
    };
    Ok(AacConfig {
        asc: [
            (h.object_type << 3) | (h.rate_index >> 1),
            ((h.rate_index & 1) << 7) | (h.channel_config << 3),
        ],
        object_type: h.object_type,
        sample_rate: AAC_RATES[usize::from(h.rate_index)],
        channels,
    })
}

/// Reads bits of an RBSP (emulation prevention bytes already taken out).
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32> {
        let byte = *self
            .data
            .get(self.pos / 8)
            .context("the SPS is cut short")?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(u32::from(bit))
    }

    fn bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    fn flag(&mut self) -> Result<bool> {
        Ok(self.bit()? == 1)
    }

    /// Unsigned exp-Golomb.
    fn ue(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            ensure!(zeros < 32, "a bad exp-Golomb code in the SPS");
        }
        let v = (1u64 << zeros) - 1 + u64::from(self.bits(zeros)?);
        u32::try_from(v).context("a bad exp-Golomb code in the SPS")
    }

    /// Signed exp-Golomb.
    fn se(&mut self) -> Result<i64> {
        let k = i64::from(self.ue()?);
        Ok(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
    }
}

/// An SPS NAL unit's profile, chroma format, bit depths and picture size.
pub fn parse_sps(nal: &[u8]) -> Result<Sps> {
    ensure!(
        nal.first().is_some_and(|b| b & 0x1F == 7),
        "not an SPS NAL unit"
    );
    // Emulation prevention: 00 00 03 is 00 00.
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in &nal[1..] {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    let r = &mut Bits {
        data: &rbsp,
        pos: 0,
    };
    let profile_idc = r.bits(8)? as u8;
    let constraints = r.bits(8)? as u8;
    let level_idc = r.bits(8)? as u8;
    r.ue()?; // seq_parameter_set_id
    let mut chroma_format_idc = 1;
    let mut separate_planes = false;
    let mut bit_depth_luma = 8;
    let mut bit_depth_chroma = 8;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = r.ue()?;
        ensure!(chroma_format_idc <= 3, "a bad chroma format in the SPS");
        if chroma_format_idc == 3 {
            separate_planes = r.flag()?;
        }
        bit_depth_luma = r.ue()?.saturating_add(8);
        bit_depth_chroma = r.ue()?.saturating_add(8);
        ensure!(
            bit_depth_luma <= 14 && bit_depth_chroma <= 14,
            "a bad bit depth in the SPS"
        );
        r.flag()?; // qpprime_y_zero_transform_bypass_flag
        if r.flag()? {
            // seq_scaling_matrix_present_flag: skip the lists.
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.flag()? {
                    skip_scaling_list(r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            r.flag()?; // delta_pic_order_always_zero_flag
            r.se()?; // offset_for_non_ref_pic
            r.se()?; // offset_for_top_to_bottom_field
            let cycle = r.ue()?;
            ensure!(cycle <= 255, "a bad picture order count cycle in the SPS");
            for _ in 0..cycle {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.flag()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = u64::from(r.ue()?) + 1;
    let height_map_units = u64::from(r.ue()?) + 1;
    let frame_mbs_only = r.flag()?;
    if !frame_mbs_only {
        r.flag()?; // mb_adaptive_frame_field_flag
    }
    r.flag()?; // direct_8x8_inference_flag
    let fields: u64 = if frame_mbs_only { 1 } else { 2 };
    let mut width = width_mbs * 16;
    let mut height = height_map_units * 16 * fields;
    if r.flag()? {
        // frame_cropping_flag: offsets are in chroma samples, or rows of
        // fields.
        let (unit_x, unit_y) = if chroma_format_idc == 0 || separate_planes {
            (1, fields)
        } else {
            let sub_width = if chroma_format_idc == 3 { 1 } else { 2 };
            let sub_height = if chroma_format_idc == 1 { 2 } else { 1 };
            (sub_width, sub_height * fields)
        };
        let left = u64::from(r.ue()?);
        let right = u64::from(r.ue()?);
        let top = u64::from(r.ue()?);
        let bottom = u64::from(r.ue()?);
        let crop_x = (left + right) * unit_x;
        let crop_y = (top + bottom) * unit_y;
        ensure!(
            crop_x < width && crop_y < height,
            "the SPS crops away the whole picture"
        );
        width -= crop_x;
        height -= crop_y;
    }
    Ok(Sps {
        profile_idc,
        constraints,
        level_idc,
        chroma_format_idc: chroma_format_idc as u8,
        bit_depth_luma: bit_depth_luma as u8,
        bit_depth_chroma: bit_depth_chroma as u8,
        width: u32::try_from(width).context("the SPS's picture is too wide")?,
        height: u32::try_from(height).context("the SPS's picture is too tall")?,
    })
}

fn skip_scaling_list(r: &mut Bits, size: usize) -> Result<()> {
    let mut last = 8i64;
    let mut next = 8i64;
    for _ in 0..size {
        if next != 0 {
            next = (last + r.se()? + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// SPSs x264 wrote (through ffmpeg), with what ffprobe says of them.
    #[test]
    fn reads_real_spss() {
        let cases = [
            // 1080p High: 1088 rows coded, 8 cropped.
            (
                "67640028acd940780227e5c044000003000400000300c83c60c658",
                100,
                1,
                8,
                1920,
                1080,
            ),
            (
                "6742c01fd9005005bb011000000300100000030320f1832480",
                66,
                1,
                8,
                1280,
                720,
            ),
            // Not a whole number of macroblocks either way.
            (
                "674d401eeca05217f1126022000003000200000300641e2c5b2c",
                77,
                1,
                8,
                642,
                362,
            ),
            // High 4:4:4 Predictive.
            (
                "67f4000d919b28283f6022000003000200000300641e28532c",
                244,
                3,
                8,
                320,
                240,
            ),
            // Interlaced (frame_mbs_only_flag 0): map units are field pairs.
            (
                "67640028acd94078044fde0220000003002000000643e2c5b2c0",
                100,
                1,
                8,
                1920,
                1080,
            ),
            // High 10 with the JVT scaling lists.
            (
                "676e001ea6cd940d83de6f011000000300100000030320f162d960",
                110,
                1,
                10,
                854,
                480,
            ),
            // High 4:2:2, 10-bit.
            (
                "677a001eb6cd940b43db011000000300100000030320f162d960",
                122,
                2,
                10,
                720,
                480,
            ),
        ];
        for (sps, profile, chroma, depth, width, height) in cases {
            let s = parse_sps(&hex(sps)).unwrap();
            assert_eq!(
                (
                    s.profile_idc,
                    s.chroma_format_idc,
                    s.bit_depth_luma,
                    s.width,
                    s.height
                ),
                (profile, chroma, depth, width, height),
                "{sps}"
            );
            assert_eq!(s.bit_depth_chroma, depth);
        }
        let s = parse_sps(&hex(cases[0].0)).unwrap();
        assert_eq!((s.constraints, s.level_idc), (0, 40));
    }

    #[test]
    fn rejects_broken_spss() {
        assert!(parse_sps(&[]).is_err());
        assert!(parse_sps(&[0x68, 0x64, 0, 0x28]).is_err());
        let good = hex("67640028acd940780227e5c044000003000400000300c83c60c658");
        for end in 0..10 {
            assert!(parse_sps(&good[..end]).is_err(), "{end} bytes");
        }
        // All ones after the header, all zeros: no panic either way.
        let _ = parse_sps(&[0x67, 100, 0, 40, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(parse_sps(&[0x67, 100, 0, 40, 0, 0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn splits_annex_b() {
        let data = [
            0, 0, 0, 1, 9, 0xF0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 0, 1, 0x65, 4, 5,
            0, 0,
        ];
        let nals = nal_units(&data);
        assert_eq!(
            nals,
            [&[9, 0xF0][..], &[0x67, 1, 2], &[0x68, 3], &[0x65, 4, 5]]
        );
        assert!(nal_units(&[]).is_empty());
        assert!(nal_units(&[0, 0, 1]).is_empty());
        assert_eq!(nal_units(&[7, 7, 0, 0, 1, 0x41]), [&[0x41][..]]);
    }

    #[test]
    fn unwraps_timestamps() {
        let mut c = Clock::default();
        assert_eq!(c.unwrap((1 << 33) - 3000), WRAP - 3000);
        assert_eq!(c.unwrap(500), WRAP + 500);
        assert_eq!(c.unwrap((1 << 33) - 100), WRAP - 100);
        assert_eq!(c.unwrap(3600), WRAP + 3600);
        let mut c = Clock::default();
        assert_eq!(c.unwrap(10), 10);
        assert_eq!(c.unwrap((1 << 33) - 10), -10);
    }

    #[test]
    fn reads_adts_headers() {
        // AAC LC, 48 kHz (index 3), stereo, 100 bytes, no CRC.
        let len = 100usize;
        let h = [
            0xFF,
            0xF1,
            (1 << 6) | (3 << 2),
            (2 << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1F,
            0xFC,
        ];
        let parsed = adts_header(&h).unwrap();
        assert_eq!(parsed.frame_len, 100);
        assert_eq!(parsed.header_len, 7);
        let c = aac_config(&parsed).unwrap();
        assert_eq!(c.asc, [0x11, 0x90]);
        assert_eq!((c.object_type, c.sample_rate, c.channels), (2, 48000, 2));
        assert!(adts_header(&h[..6]).is_none());
        let mut bad = h;
        bad[2] = (1 << 6) | (14 << 2);
        assert!(adts_header(&bad).is_none());
        // With a CRC (protection_absent 0): a 9-byte header. HE-AAC's
        // profile bits say LC; 44.1 kHz mono.
        let mut crc = h;
        crc[1] = 0xF0;
        crc[2] = (1 << 6) | (4 << 2);
        crc[3] = (1 << 6) | (crc[3] & 3);
        let parsed = adts_header(&crc).unwrap();
        assert_eq!(parsed.header_len, 9);
        let c = aac_config(&parsed).unwrap();
        assert_eq!((c.sample_rate, c.channels), (44100, 1));
        assert_eq!(c.asc, [0x12, 0x08]);
    }

    /// Builds TS packets: PAT, PMT, then the given PES payloads.
    fn ts_packet(pid: u16, start: bool, counter: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![
            SYNC,
            (if start { 0x40 } else { 0 }) | (pid >> 8) as u8,
            pid as u8,
        ];
        let room = PACKET - 4;
        if payload.len() >= room {
            p.push(0x10 | counter);
            p.extend_from_slice(&payload[..room]);
        } else {
            // Stuffing in an adaptation field.
            p.push(0x30 | counter);
            let af = room - payload.len() - 1;
            p.push(af as u8);
            if af > 0 {
                p.push(0);
                p.extend(std::iter::repeat_n(0xFF, af - 1));
            }
            p.extend_from_slice(payload);
        }
        assert_eq!(p.len(), PACKET);
        p
    }

    fn section(table: u8, id: u16, body: &[u8]) -> Vec<u8> {
        let len = 5 + body.len() + 4;
        let mut s = vec![0, table, 0xB0 | (len >> 8) as u8, len as u8];
        s.extend_from_slice(&id.to_be_bytes());
        s.extend_from_slice(&[0xC1, 0, 0]);
        s.extend_from_slice(body);
        s.extend_from_slice(&[0; 4]); // CRC, not checked
        s
    }

    fn pat_pmt(streams: &[(u8, u16)]) -> Vec<u8> {
        let pat = section(0, 1, &[0, 1, 0xF0, 0x00]);
        let mut pmt_body = vec![0xE1, 0x00, 0xF0, 0x00];
        for &(t, pid) in streams {
            pmt_body.extend_from_slice(&[t, 0xE0 | (pid >> 8) as u8, pid as u8, 0xF0, 0]);
        }
        let pmt = section(2, 1, &pmt_body);
        [
            ts_packet(0, true, 0, &pat),
            ts_packet(0x1000, true, 0, &pmt),
        ]
        .concat()
    }

    fn pes(stream_id: u8, pts: Option<u64>, payload: &[u8], sized: bool) -> Vec<u8> {
        let mut header = vec![0x80, 0, 0];
        if let Some(pts) = pts {
            header[1] = 0x80;
            header[2] = 5;
            header.extend_from_slice(&[
                0x21 | ((pts >> 29) as u8 & 0x0E),
                (pts >> 22) as u8,
                ((pts >> 14) as u8) | 1,
                (pts >> 7) as u8,
                ((pts << 1) as u8) | 1,
            ]);
        }
        let len = if sized {
            header.len() + payload.len()
        } else {
            0
        };
        let mut p = vec![0, 0, 1, stream_id, (len >> 8) as u8, len as u8];
        p.extend_from_slice(&header);
        p.extend_from_slice(payload);
        p
    }

    fn packets(pid: u16, data: &[u8], counter: &mut u8) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, chunk) in data.chunks(PACKET - 4).enumerate() {
            out.extend(ts_packet(pid, i == 0, *counter, chunk));
            *counter = (*counter + 1) & 0x0F;
        }
        out
    }

    fn adts(payload_len: usize, fill: u8) -> Vec<u8> {
        let len = payload_len + 7;
        let mut f = vec![
            0xFF,
            0xF1,
            (1 << 6) | (3 << 2),
            (2 << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1F,
            0xFC,
        ];
        f.extend(std::iter::repeat_n(fill, payload_len));
        f
    }

    #[test]
    fn demuxes_hand_made_packets() {
        let mut ts = pat_pmt(&[(0x1B, 0x100), (0x0F, 0x101), (0x15, 0x102)]);
        let sps = hex("6742c01fd9005005bb011000000300100000030320f1832480");
        let mut au = vec![0, 0, 0, 1, 9, 0xF0, 0, 0, 0, 1];
        au.extend_from_slice(&sps);
        au.extend_from_slice(&[0, 0, 1, 0x68, 0xCE, 0x38, 0x80, 0, 0, 1, 0x65]);
        au.extend(std::iter::repeat_n(0xAB, 500));
        let (mut vc, mut ac) = (0, 0);
        // A video PES of unknown length, spread over packets.
        ts.extend(packets(
            0x100,
            &pes(0xE0, Some(900_000), &au, false),
            &mut vc,
        ));
        // Three ADTS frames over two PESs: the second frame is cut by the
        // PES boundary.
        let frames: Vec<u8> = [adts(300, 1), adts(200, 2), adts(100, 3)].concat();
        let (first, second) = frames.split_at(400);
        ts.extend(packets(
            0x101,
            &pes(0xC0, Some(900_000), first, true),
            &mut ac,
        ));
        ts.extend(packets(
            0x101,
            &pes(0xC0, Some(905_760), second, true),
            &mut ac,
        ));
        // The next video PES ends the first.
        ts.extend(packets(
            0x100,
            &pes(0xE0, Some(903_600), &[0, 0, 1, 0x41, 0x9A], false),
            &mut vc,
        ));

        let mut d = Demuxer::new();
        // In two pushes, cut mid-packet.
        let (a, b) = ts.split_at(PACKET * 3 + 77);
        d.push(a).unwrap();
        d.push(b).unwrap();
        let kinds: Vec<(u16, Codec)> = d.streams().iter().map(|s| (s.pid, s.codec)).collect();
        assert_eq!(
            kinds,
            [
                (0x100, Codec::H264),
                (0x101, Codec::Aac),
                (0x102, Codec::Other("ID3 metadata"))
            ]
        );
        let avc = d.avc_config(0x100).unwrap();
        assert_eq!(avc.sps, sps);
        assert_eq!(avc.pps, [0x68, 0xCE, 0x38, 0x80]);
        assert_eq!((avc.info.width, avc.info.height), (1280, 720));
        let aac = d.aac_config(0x101).unwrap();
        assert_eq!((aac.sample_rate, aac.channels), (48000, 2));

        let frames = d.take_frames();
        let video: Vec<&Frame> = frames.iter().filter(|f| f.pid == 0x100).collect();
        let audio: Vec<&Frame> = frames.iter().filter(|f| f.pid == 0x101).collect();
        assert_eq!(video.len(), 1);
        assert!(video[0].keyframe);
        assert_eq!(video[0].pts, 900_000);
        // SPS, PPS, IDR (the AUD dropped), each after its length.
        let mut want = (sps.len() as u32).to_be_bytes().to_vec();
        want.extend_from_slice(&sps);
        want.extend_from_slice(&[0, 0, 0, 4, 0x68, 0xCE, 0x38, 0x80, 0, 0, 1, 245, 0x65]);
        want.extend(std::iter::repeat_n(0xAB, 500));
        assert_eq!(video[0].data, want);
        // The cut frame keeps its first PES's timing: 900000 + 1920.
        let times: Vec<i64> = audio.iter().map(|f| f.pts).collect();
        assert_eq!(times, [900_000, 901_920, 905_760]);
        assert_eq!(audio[1].data, vec![2; 200]);

        // The last video PES comes out at the end.
        d.flush().unwrap();
        let last = d.take_frames();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].pts, 903_600);
        assert!(!last[0].keyframe);
        assert_eq!(last[0].data, [0, 0, 0, 2, 0x41, 0x9A]);
    }

    #[test]
    fn names_unsupported_streams() {
        let mut d = Demuxer::new();
        d.push(&pat_pmt(&[(0x24, 0x100), (0x81, 0x101), (0x06, 0x102)]))
            .unwrap();
        let names: Vec<&str> = d.streams().iter().map(|s| s.codec.name()).collect();
        assert_eq!(names, ["HEVC", "AC-3", "private data"]);
        assert_eq!(d.streams()[0].kind, Kind::Video);
        assert_eq!(
            private_data(&[0x05, 4, b'O', b'p', b'u', b's']).1,
            Codec::Other("Opus")
        );
        assert_eq!(
            private_data(&[0x0A, 4, 1, 2, 3, 0, 0x6A, 1, 0]).1,
            Codec::Other("AC-3")
        );
        assert_eq!(private_data(&[0x6A, 200]).1, Codec::Other("AC-3"));
    }

    #[test]
    fn rejects_what_is_not_ts() {
        let mut d = Demuxer::new();
        assert!(
            d.push(
                b"<html><body>403 Forbidden</body></html>"
                    .repeat(20)
                    .as_slice()
            )
            .is_err()
        );
        // Small pushes may just be the start of a packet.
        assert!(d.push(&[SYNC, 0, 0]).is_ok());
        assert!(d.push(&[0u8; 2 * PACKET]).is_err());
    }

    #[test]
    fn never_panics_on_junk() {
        let mut ts = pat_pmt(&[(0x1B, 0x100), (0x0F, 0x101)]);
        let mut c = 0;
        let au = [0, 0, 1, 0x67, 0x64, 0, 0x28, 0xAC, 0, 0, 1, 0x65, 1, 2, 3];
        ts.extend(packets(0x100, &pes(0xE0, Some(1), &au, false), &mut c));
        ts.extend(packets(
            0x101,
            &pes(0xC0, Some(1), &adts(50, 9), true),
            &mut c,
        ));
        // Every byte set to a few values in turn, and every cut.
        let mut seed = 0x1234_5678u32;
        for i in 0..ts.len() {
            for v in [0x00, 0xFF, 0x47, (seed >> 24) as u8] {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut m = ts.clone();
                m[i] = v;
                let mut d = Demuxer::new();
                let _ = d.push(&m);
                let _ = d.flush();
            }
            let mut d = Demuxer::new();
            let _ = d.push(&ts[..i]);
            let _ = d.flush();
        }
    }
}
