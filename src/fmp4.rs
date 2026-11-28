//! Joining fragmented MP4 (ISO BMFF) tracks into one file, the way DASH
//! serves them: per track, an init segment (`ftyp` + `moov`) and media
//! segments (`styp`, `sidx`, then `moof` + `mdat` pairs).
//!
//! The result is itself a fragmented MP4: one `ftyp`; one `moov` holding
//! every track (renumbered 1, 2, …), their durations, and an `mvex` with
//! each track's `trex` and the whole duration (`mehd`); then the tracks'
//! fragments in the order they're handed in (the caller interleaves them by
//! time); and an `mfra` at the end listing every fragment, so players can
//! seek without reading them all first.
//!
//! Samples, codec setup (`stsd`) and edit lists are copied byte for byte.
//! Only what has to change is rewritten, in place and without changing any
//! box's size (so the `trun` data offsets, relative to their `moof`, stay
//! right): track IDs (`tkhd`, `trex`, `tfhd`), fragment sequence numbers
//! (`mfhd`), durations, and an explicit `tfhd` base data offset if a
//! segment uses one. Segment-level boxes that would point at the wrong
//! bytes once segments are joined (`sidx`, `ssix`) are dropped, and so are
//! `styp`, `emsg`, `prft` and padding.

use std::ops::Range;

use anyhow::{Context, Result, bail, ensure};

/// The movie timescale of the joined file (milliseconds).
const MOVIE_TIMESCALE: u32 = 1000;

/// A box in a buffer: where it starts and ends, its header's length, its type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bx {
    start: usize,
    header: usize,
    end: usize,
    kind: [u8; 4],
}

impl Bx {
    fn body(&self) -> Range<usize> {
        self.start + self.header..self.end
    }

    fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    fn is(&self, kind: &[u8; 4]) -> bool {
        &self.kind == kind
    }
}

fn fourcc(kind: [u8; 4]) -> String {
    kind.iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

fn be_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(buf[at..at + 4].try_into().unwrap())
}

fn be_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(buf[at..at + 8].try_into().unwrap())
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

/// The boxes laid end to end in `buf[range]`.
fn children(buf: &[u8], range: Range<usize>) -> Result<Vec<Bx>> {
    let mut out = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let left = range.end - at;
        ensure!(left >= 8, "truncated box header at byte {at}");
        let kind: [u8; 4] = buf[at + 4..at + 8].try_into().unwrap();
        let (header, size) = match be_u32(buf, at) {
            1 => {
                ensure!(left >= 16, "truncated box header at byte {at}");
                (16, be_u64(buf, at + 8))
            }
            // To the end of what contains it.
            0 => (8, left as u64),
            n => (8, u64::from(n)),
        };
        ensure!(
            size >= header as u64 && size <= left as u64,
            "box '{}' at byte {at} has a bad size ({size}, {left} bytes left)",
            fourcc(kind)
        );
        let end = at + size as usize;
        out.push(Bx {
            start: at,
            header,
            end,
            kind,
        });
        at = end;
    }
    Ok(out)
}

fn find(boxes: &[Bx], kind: &[u8; 4]) -> Option<Bx> {
    boxes.iter().copied().find(|b| b.is(kind))
}

fn child(buf: &[u8], parent: Bx, kind: &[u8; 4]) -> Result<Bx> {
    find(&children(buf, parent.body())?, kind)
        .with_context(|| format!("no '{}' in '{}'", fourcc(*kind), fourcc(parent.kind)))
}

/// A full box's body, checked to hold at least `len` bytes; returns where
/// it starts and its version.
fn full_box(buf: &[u8], b: Bx, len_v0: usize, len_v1: usize) -> Result<(usize, u8)> {
    let body = b.body();
    ensure!(!body.is_empty(), "empty '{}' box", fourcc(b.kind));
    let version = buf[body.start];
    let need = if version == 1 { len_v1 } else { len_v0 };
    ensure!(
        body.len() >= need,
        "'{}' box too short ({} bytes, version {version})",
        fourcc(b.kind),
        body.len()
    );
    Ok((body.start, version))
}

/// The timescale in an `mvhd` or `mdhd` (same layout up to the duration).
fn timescale_of(buf: &[u8], b: Bx) -> Result<u32> {
    let (at, v) = full_box(buf, b, 20, 32)?;
    let ts = be_u32(buf, at + if v == 1 { 20 } else { 12 });
    ensure!(ts > 0, "'{}' has a zero timescale", fourcc(b.kind));
    Ok(ts)
}

/// Sets the duration in an `mvhd` or `mdhd`.
fn set_header_duration(buf: &mut [u8], b: Bx, duration: u64) -> Result<()> {
    let (at, v) = full_box(buf, b, 20, 32)?;
    if v == 1 {
        put_u64(buf, at + 24, duration);
    } else {
        put_u32(buf, at + 16, u32::try_from(duration).unwrap_or(u32::MAX));
    }
    Ok(())
}

/// One track's init segment.
#[derive(Debug, Clone)]
pub struct Init {
    bytes: Vec<u8>,
    ftyp: Option<Range<usize>>,
    trak: Range<usize>,
    trex: Option<Range<usize>>,
    track_id: u32,
    movie_timescale: u32,
    timescale: u32,
    handler: [u8; 4],
}

impl Init {
    /// Reads an init segment: `ftyp` (optional) and a `moov` with one track.
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        let top = children(&bytes, 0..bytes.len()).context("reading the init segment")?;
        let ftyp = find(&top, b"ftyp").map(|b| b.range());
        let moov = find(&top, b"moov").context("no moov box in the init segment")?;
        let in_moov = children(&bytes, moov.body())?;
        let mvhd = find(&in_moov, b"mvhd").context("no mvhd in the init segment")?;
        let movie_timescale = timescale_of(&bytes, mvhd)?;
        let traks: Vec<Bx> = in_moov.iter().copied().filter(|b| b.is(b"trak")).collect();
        ensure!(
            traks.len() == 1,
            "expected one track in the init segment, found {}",
            traks.len()
        );
        let trak = traks[0];
        let tkhd = child(&bytes, trak, b"tkhd")?;
        let (at, v) = full_box(&bytes, tkhd, 24, 36)?;
        let track_id = be_u32(&bytes, at + if v == 1 { 20 } else { 12 });
        let mdia = child(&bytes, trak, b"mdia")?;
        let in_mdia = children(&bytes, mdia.body())?;
        let mdhd = find(&in_mdia, b"mdhd").context("no mdhd in the track")?;
        let timescale = timescale_of(&bytes, mdhd)?;
        let hdlr = find(&in_mdia, b"hdlr").context("no hdlr in the track")?;
        let (at, _) = full_box(&bytes, hdlr, 12, 12)?;
        let handler: [u8; 4] = bytes[at + 8..at + 12].try_into().unwrap();
        ensure!(find(&in_mdia, b"minf").is_some(), "no minf in the track");
        let mut trex = None;
        if let Some(mvex) = find(&in_moov, b"mvex") {
            for b in children(&bytes, mvex.body())? {
                if b.is(b"trex") {
                    let (at, _) = full_box(&bytes, b, 24, 24)?;
                    if be_u32(&bytes, at + 4) == track_id {
                        trex = Some(b.range());
                    }
                }
            }
        }
        Ok(Init {
            ftyp,
            trak: trak.range(),
            trex,
            track_id,
            movie_timescale,
            timescale,
            handler,
            bytes,
        })
    }

    /// The track's handler: `vide`, `soun`, `subt`, …
    pub fn handler(&self) -> [u8; 4] {
        self.handler
    }

    /// The track's media timescale (`mdhd`): ticks per second.
    pub fn timescale(&self) -> u32 {
        self.timescale
    }

    /// The track's ID in its own init segment.
    #[allow(dead_code, reason = "used by tests")]
    pub fn track_id(&self) -> u32 {
        self.track_id
    }

    /// This track's `trak`, renumbered `id`, with its duration set
    /// (`duration` in the track's timescale) and its edit list's durations
    /// moved to the joined file's movie timescale.
    fn trak_for(&self, id: u32, duration: u64) -> Result<Vec<u8>> {
        let mut trak = self.bytes[self.trak.clone()].to_vec();
        let outer = children(&trak, 0..trak.len())?[0];
        let kids = children(&trak, outer.body())?;
        let tkhd = find(&kids, b"tkhd").context("no tkhd")?;
        let (at, v) = full_box(&trak, tkhd, 24, 36)?;
        let movie_duration = rescale(duration, self.timescale, MOVIE_TIMESCALE);
        if v == 1 {
            put_u32(&mut trak, at + 20, id);
            put_u64(&mut trak, at + 28, movie_duration);
        } else {
            put_u32(&mut trak, at + 12, id);
            put_u32(
                &mut trak,
                at + 20,
                u32::try_from(movie_duration).unwrap_or(u32::MAX),
            );
        }
        let mdia = find(&kids, b"mdia").context("no mdia")?;
        let mdhd = find(&children(&trak, mdia.body())?, b"mdhd").context("no mdhd")?;
        set_header_duration(&mut trak, mdhd, duration)?;
        if let Some(edts) = find(&kids, b"edts") {
            for elst in children(&trak, edts.body())?
                .into_iter()
                .filter(|b| b.is(b"elst"))
            {
                self.rescale_elst(&mut trak, elst)?;
            }
        }
        Ok(trak)
    }

    /// Edit durations are in the movie timescale, which changes; media
    /// times (in the track's own timescale) don't.
    fn rescale_elst(&self, buf: &mut [u8], elst: Bx) -> Result<()> {
        let (at, v) = full_box(buf, elst, 8, 8)?;
        let count = be_u32(buf, at + 4) as usize;
        let entry = if v == 1 { 20 } else { 12 };
        ensure!(
            elst.body().len() >= 8 + count * entry,
            "elst too short for {count} entries"
        );
        for i in 0..count {
            let e = at + 8 + i * entry;
            if v == 1 {
                let d = be_u64(buf, e);
                put_u64(buf, e, rescale(d, self.movie_timescale, MOVIE_TIMESCALE));
            } else {
                let d = u64::from(be_u32(buf, e));
                let d = rescale(d, self.movie_timescale, MOVIE_TIMESCALE);
                put_u32(buf, e, u32::try_from(d).unwrap_or(u32::MAX));
            }
        }
        Ok(())
    }

    /// This track's `trex`, renumbered `id` (a plain one if it had none).
    fn trex_for(&self, id: u32) -> Vec<u8> {
        let mut out = Vec::new();
        match &self.trex {
            Some(r) => {
                out.extend_from_slice(&self.bytes[r.clone()]);
                let b = children(&out, 0..out.len()).unwrap()[0];
                put_u32(&mut out, b.body().start + 4, id);
            }
            None => write_box(&mut out, b"trex", |o| {
                o.extend_from_slice(&[0; 4]);
                o.extend_from_slice(&id.to_be_bytes());
                o.extend_from_slice(&1u32.to_be_bytes());
                o.extend_from_slice(&[0; 12]);
            }),
        }
        out
    }
}

/// `v` ticks at `from` per second, in ticks at `to` per second (rounded up,
/// so a track is never cut short).
fn rescale(v: u64, from: u32, to: u32) -> u64 {
    if from == to {
        return v;
    }
    let r = (u128::from(v) * u128::from(to)).div_ceil(u128::from(from));
    u64::try_from(r).unwrap_or(u64::MAX)
}

fn write_box(out: &mut Vec<u8>, kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(kind);
    body(out);
    let size = u32::try_from(out.len() - start).expect("header box over 4 GB");
    put_u32(out, start, size);
}

/// A track of the joined file: its init segment and its whole duration, in
/// its own timescale.
#[derive(Debug, Clone)]
pub struct Track {
    pub init: Init,
    pub duration: u64,
}

#[derive(Debug)]
struct Joined {
    /// The track's ID in its segments.
    source_id: u32,
    /// Its ID in the joined file.
    id: u32,
    /// (decode time, file offset of the `moof`) of each fragment.
    fragments: Vec<(u64, u64)>,
}

/// Writes tracks' fragments into one file: [`Muxer::new`]'s header first,
/// then each [`Muxer::fragments`]' bytes in the order they're made, then
/// [`Muxer::finish`]'s index.
#[derive(Debug)]
pub struct Muxer {
    tracks: Vec<Joined>,
    sequence: u32,
    /// Bytes handed out so far: where the next fragment will start.
    position: u64,
}

impl Muxer {
    /// The muxer and the file's header (`ftyp` + `moov`). Tracks are
    /// numbered in this order, from 1; the first's `ftyp` is the file's.
    pub fn new(tracks: &[Track]) -> Result<(Self, Vec<u8>)> {
        ensure!(!tracks.is_empty(), "no tracks to join");
        let mut out = Vec::new();
        match tracks[0].init.ftyp.clone() {
            Some(r) => out.extend_from_slice(&tracks[0].init.bytes[r]),
            None => write_box(&mut out, b"ftyp", |o| {
                o.extend_from_slice(b"iso5");
                o.extend_from_slice(&0u32.to_be_bytes());
                o.extend_from_slice(b"iso5iso6mp41");
            }),
        }
        let movie_duration = tracks
            .iter()
            .map(|t| rescale(t.duration, t.init.timescale, MOVIE_TIMESCALE))
            .max()
            .unwrap_or(0);
        let next_id = u32::try_from(tracks.len() + 1)?;
        let mut traks = Vec::new();
        for (i, t) in tracks.iter().enumerate() {
            traks.push(t.init.trak_for(i as u32 + 1, t.duration)?);
        }
        write_box(&mut out, b"moov", |o| {
            write_mvhd(o, movie_duration, next_id);
            for trak in &traks {
                o.extend_from_slice(trak);
            }
            write_box(o, b"mvex", |o| {
                write_box(o, b"mehd", |o| {
                    o.extend_from_slice(&[1, 0, 0, 0]);
                    o.extend_from_slice(&movie_duration.to_be_bytes());
                });
                for (i, t) in tracks.iter().enumerate() {
                    o.extend_from_slice(&t.init.trex_for(i as u32 + 1));
                }
            });
        });
        let muxer = Muxer {
            tracks: tracks
                .iter()
                .enumerate()
                .map(|(i, t)| Joined {
                    source_id: t.init.track_id,
                    id: i as u32 + 1,
                    fragments: Vec::new(),
                })
                .collect(),
            sequence: 1,
            position: out.len() as u64,
        };
        Ok((muxer, out))
    }

    /// A media segment of track `track` (an index into the tracks given to
    /// [`Muxer::new`]), as bytes to append to the file: its `moof` + `mdat`
    /// pairs, renumbered. Fails, changing nothing, if it isn't a media
    /// segment of that track.
    pub fn fragments(&mut self, track: usize, segment: &[u8]) -> Result<Vec<u8>> {
        let joined = self.tracks.get(track).context("no such track")?;
        let (source_id, id) = (joined.source_id, joined.id);
        let top = children(segment, 0..segment.len()).context("reading the media segment")?;
        let mut out = Vec::with_capacity(segment.len());
        let mut sequence = self.sequence;
        let mut index = Vec::new();
        let mut moof_seen = false;
        let mut mdat_seen = false;
        for b in &top {
            match &b.kind {
                b"moof" => {
                    let at = self.position + out.len() as u64;
                    let mut moof = segment[b.range()].to_vec();
                    let time = rewrite_moof(
                        &mut moof,
                        b.header,
                        source_id,
                        id,
                        sequence,
                        b.start as u64,
                        at,
                    )?;
                    sequence = sequence.wrapping_add(1);
                    if let Some(time) = time {
                        index.push((time, at));
                    }
                    out.extend_from_slice(&moof);
                    moof_seen = true;
                }
                b"mdat" => {
                    ensure!(moof_seen, "mdat before any moof in the media segment");
                    let start = out.len();
                    out.extend_from_slice(&segment[b.range()]);
                    if be_u32(segment, b.start) == 0 {
                        // "To the end of the file": not once more follows.
                        let size = u32::try_from(b.end - b.start).context("mdat over 4 GB")?;
                        put_u32(&mut out, start, size);
                    }
                    mdat_seen = true;
                }
                // Index and signalling boxes about the segment on its own.
                b"styp" | b"sidx" | b"ssix" | b"emsg" | b"prft" | b"free" | b"skip" => {}
                other => {
                    tracing::debug!("dropping a '{}' box from a media segment", fourcc(*other))
                }
            }
        }
        ensure!(
            moof_seen && mdat_seen,
            "no moof + mdat in the media segment"
        );
        self.sequence = sequence;
        self.position += out.len() as u64;
        self.tracks[track].fragments.extend(index);
        Ok(out)
    }

    /// The file's end: an `mfra` listing each track's fragments.
    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        write_box(&mut out, b"mfra", |o| {
            for t in &self.tracks {
                write_box(o, b"tfra", |o| {
                    o.extend_from_slice(&[1, 0, 0, 0]);
                    o.extend_from_slice(&t.id.to_be_bytes());
                    // traf, trun and sample numbers: one byte each.
                    o.extend_from_slice(&0u32.to_be_bytes());
                    o.extend_from_slice(&(t.fragments.len() as u32).to_be_bytes());
                    for &(time, offset) in &t.fragments {
                        o.extend_from_slice(&time.to_be_bytes());
                        o.extend_from_slice(&offset.to_be_bytes());
                        o.extend_from_slice(&[1, 1, 1]);
                    }
                });
            }
            let size = o.len() as u32 + 16;
            write_box(o, b"mfro", |o| {
                o.extend_from_slice(&[0; 4]);
                o.extend_from_slice(&size.to_be_bytes());
            });
        });
        out
    }

    /// Where the next fragment will start: the bytes handed out so far.
    #[allow(dead_code, reason = "used by tests")]
    pub fn position(&self) -> u64 {
        self.position
    }
}

fn write_mvhd(out: &mut Vec<u8>, duration: u64, next_id: u32) {
    write_box(out, b"mvhd", |o| {
        o.extend_from_slice(&[1, 0, 0, 0]);
        o.extend_from_slice(&[0; 16]); // creation, modification
        o.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
        o.extend_from_slice(&duration.to_be_bytes());
        o.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        o.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        o.extend_from_slice(&[0; 10]);
        for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            o.extend_from_slice(&m.to_be_bytes());
        }
        o.extend_from_slice(&[0; 24]); // pre_defined
        o.extend_from_slice(&next_id.to_be_bytes());
    });
}

/// Renumbers a `moof` (whose header is `header` bytes) in place: sequence
/// number, track IDs, and an explicit base data offset moved from
/// `from` (where the `moof` was in its segment) to `to` (where it goes in
/// the file). Returns its first track fragment's decode time.
fn rewrite_moof(
    moof: &mut [u8],
    header: usize,
    source_id: u32,
    id: u32,
    sequence: u32,
    from: u64,
    to: u64,
) -> Result<Option<u64>> {
    let mut time = None;
    let mut trafs = 0;
    for b in children(moof, header..moof.len())? {
        match &b.kind {
            b"mfhd" => {
                let (at, _) = full_box(moof, b, 8, 8)?;
                put_u32(moof, at + 4, sequence);
            }
            b"traf" => {
                trafs += 1;
                let kids = children(moof, b.body())?;
                let tfhd = find(&kids, b"tfhd").context("traf without tfhd")?;
                let (at, _) = full_box(moof, tfhd, 8, 8)?;
                let flags = be_u32(moof, at) & 0x00FF_FFFF;
                let found = be_u32(moof, at + 4);
                if found != source_id {
                    bail!("media segment is for track {found}, not {source_id}");
                }
                put_u32(moof, at + 4, id);
                if flags & 0x01 != 0 {
                    ensure!(tfhd.body().len() >= 16, "tfhd too short");
                    let base = be_u64(moof, at + 8);
                    let moved = i128::from(base) - i128::from(from) + i128::from(to);
                    let moved =
                        u64::try_from(moved).context("base data offset before its segment")?;
                    put_u64(moof, at + 8, moved);
                }
                if let Some(tfdt) = find(&kids, b"tfdt") {
                    let (at, v) = full_box(moof, tfdt, 8, 12)?;
                    let t = if v == 1 {
                        be_u64(moof, at + 4)
                    } else {
                        u64::from(be_u32(moof, at + 4))
                    };
                    time.get_or_insert(t);
                }
            }
            _ => {}
        }
    }
    ensure!(trafs > 0, "moof without traf");
    Ok(time)
}

/// Whether `bytes` look like a whole media segment: boxes laid end to end,
/// a `moof` and an `mdat` among them (a cut-short download doesn't).
pub fn check_media_segment(bytes: &[u8]) -> Result<()> {
    let top = children(bytes, 0..bytes.len())?;
    ensure!(
        find(&top, b"moof").is_some() && find(&top, b"mdat").is_some(),
        "no moof + mdat in the media segment"
    );
    Ok(())
}

/// A file's top-level box types in order, with sizes; for checks.
#[cfg(test)]
fn top_level(buf: &[u8]) -> Result<Vec<(String, usize, usize)>> {
    Ok(children(buf, 0..buf.len())?
        .into_iter()
        .map(|b| (fourcc(b.kind), b.start, b.end - b.start))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_box(&mut out, kind, |o| o.extend_from_slice(body));
        out
    }

    fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![version];
        b.extend_from_slice(&flags.to_be_bytes()[1..]);
        b.extend_from_slice(body);
        bx(kind, &b)
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn u32s(vals: &[u32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// An init segment like ffmpeg's DASH muxer writes: one track, numbered
    /// 1, movie timescale = the track's.
    fn init(handler: &[u8; 4], timescale: u32, media_time: u32) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"iso5\0\0\x02\0iso5iso6mp41");
        let mvhd = full(
            b"mvhd",
            0,
            0,
            &cat(&[u32s(&[0, 0, timescale, 0]), vec![0; 80], u32s(&[2])]),
        );
        let tkhd = full(b"tkhd", 0, 3, &cat(&[u32s(&[0, 0, 1, 0, 0]), vec![0; 60]]));
        let elst = full(b"elst", 0, 0, &u32s(&[1, 0, media_time, 0x0001_0000]));
        let edts = bx(b"edts", &elst);
        let mdhd = full(
            b"mdhd",
            0,
            0,
            &cat(&[u32s(&[0, 0, timescale, 0]), vec![0x55, 0xc4, 0, 0]]),
        );
        let hdlr = full(
            b"hdlr",
            0,
            0,
            &cat(&[u32s(&[0]), handler.to_vec(), vec![0; 12], b"x\0".to_vec()]),
        );
        let stsd = full(b"stsd", 0, 0, &u32s(&[0]));
        let stbl = bx(b"stbl", &cat(&[stsd, full(b"stts", 0, 0, &u32s(&[0]))]));
        let minf = bx(b"minf", &stbl);
        let mdia = bx(b"mdia", &cat(&[mdhd, hdlr, minf]));
        let trak = bx(b"trak", &cat(&[tkhd, edts, mdia]));
        let trex = full(b"trex", 0, 0, &u32s(&[1, 1, 0, 0, 0]));
        let mvex = bx(b"mvex", &trex);
        let udta = bx(b"udta", b"junk");
        cat(&[ftyp, bx(b"moov", &cat(&[mvhd, trak, mvex, udta]))])
    }

    /// A media segment like ffmpeg's: styp, sidx, moof (mfhd, traf: tfhd
    /// with default-base-is-moof, tfdt, trun with a data offset), mdat.
    /// `payload` is the samples; `base_offset` adds an explicit tfhd base
    /// data offset instead (the moof's position in the segment).
    fn segment(time: u64, payload: &[u8], base_offset: bool) -> Vec<u8> {
        let styp = bx(b"styp", b"msdh\0\0\0\0msdhmsix");
        let sidx = full(b"sidx", 1, 0, &[0; 32]);
        let prefix = styp.len() + sidx.len();
        let build = |data_offset: u32| {
            let mfhd = full(b"mfhd", 0, 0, &u32s(&[9]));
            let tfhd = if base_offset {
                full(
                    b"tfhd",
                    0,
                    0x01,
                    &cat(&[u32s(&[1]), (prefix as u64).to_be_bytes().to_vec()]),
                )
            } else {
                full(b"tfhd", 0, 0x02_0038, &u32s(&[1, 1000, 4, 0]))
            };
            let tfdt = full(b"tfdt", 1, 0, &time.to_be_bytes());
            let trun = full(
                b"trun",
                0,
                0x01,
                &u32s(&[payload.len() as u32 / 4, data_offset]),
            );
            bx(
                b"moof",
                &cat(&[mfhd, bx(b"traf", &cat(&[tfhd, tfdt, trun]))]),
            )
        };
        let moof_len = build(0).len();
        let moof = build(moof_len as u32 + 8);
        cat(&[styp, sidx, moof, bx(b"mdat", payload)])
    }

    struct Parsed {
        buf: Vec<u8>,
    }

    impl Parsed {
        fn top(&self) -> Vec<Bx> {
            children(&self.buf, 0..self.buf.len()).unwrap()
        }

        fn kids(&self, b: Bx) -> Vec<Bx> {
            children(&self.buf, b.body()).unwrap()
        }

        fn u32_at(&self, at: usize) -> u32 {
            be_u32(&self.buf, at)
        }
    }

    fn joined(base_offset: bool) -> (Vec<u8>, Vec<Vec<u8>>) {
        let video = Init::parse(init(b"vide", 24000, 0)).unwrap();
        let audio = Init::parse(init(b"soun", 48000, 1024)).unwrap();
        assert_eq!(video.handler(), *b"vide");
        assert_eq!(audio.timescale(), 48000);
        let (mut mux, header) = Muxer::new(&[
            Track {
                init: video,
                duration: 240_240 * 2,
            },
            Track {
                init: audio,
                duration: 480_256 * 2,
            },
        ])
        .unwrap();
        let payloads: Vec<Vec<u8>> = (0..4u8).map(|i| vec![i + 1; 64 + 4 * i as usize]).collect();
        let mut file = header;
        file.extend(
            mux.fragments(0, &segment(0, &payloads[0], base_offset))
                .unwrap(),
        );
        file.extend(
            mux.fragments(1, &segment(0, &payloads[1], base_offset))
                .unwrap(),
        );
        file.extend(
            mux.fragments(0, &segment(240_240, &payloads[2], base_offset))
                .unwrap(),
        );
        file.extend(
            mux.fragments(1, &segment(480_256, &payloads[3], base_offset))
                .unwrap(),
        );
        assert_eq!(mux.position(), file.len() as u64);
        file.extend(mux.finish());
        (file, payloads)
    }

    #[test]
    fn joins_two_tracks_into_one_fragmented_file() {
        let (file, payloads) = joined(false);
        let p = Parsed { buf: file };
        let top = p.top();
        let kinds: Vec<String> = top.iter().map(|b| fourcc(b.kind)).collect();
        assert_eq!(
            kinds,
            [
                "ftyp", "moov", "moof", "mdat", "moof", "mdat", "moof", "mdat", "moof", "mdat",
                "mfra"
            ]
        );

        // moov: mvhd (ms, the longer track's duration, next ID 3), two
        // renumbered tracks, mvex with mehd and a trex for each.
        let moov = p.kids(top[1]);
        let kinds: Vec<String> = moov.iter().map(|b| fourcc(b.kind)).collect();
        assert_eq!(kinds, ["mvhd", "trak", "trak", "mvex"]);
        let mvhd = moov[0].body().start;
        assert_eq!(p.buf[mvhd], 1);
        assert_eq!(p.u32_at(mvhd + 20), 1000);
        assert_eq!(be_u64(&p.buf, mvhd + 24), 20_020); // 480480 / 24000 s
        assert_eq!(p.u32_at(moov[0].end - 4), 3);
        for (i, trak) in [moov[1], moov[2]].into_iter().enumerate() {
            let kids = p.kids(trak);
            let tkhd = find(&kids, b"tkhd").unwrap().body().start;
            assert_eq!(p.u32_at(tkhd + 12), i as u32 + 1, "tkhd track ID");
            // 960512 / 48000 s is 20010.7 ms: rounded up.
            assert_eq!(
                p.u32_at(tkhd + 20),
                [20_020, 20_011][i],
                "tkhd duration (ms)"
            );
            let mdia = find(&kids, b"mdia").unwrap();
            let mdhd = find(&p.kids(mdia), b"mdhd").unwrap().body().start;
            assert_eq!(p.u32_at(mdhd + 16), [480_480, 960_512][i], "mdhd duration");
            // The audio's edit list still skips its 1024 priming samples.
            let edts = find(&kids, b"edts").unwrap();
            let elst = find(&p.kids(edts), b"elst").unwrap().body().start;
            assert_eq!(p.u32_at(elst + 12), [0, 1024][i]);
        }
        let mvex = p.kids(moov[3]);
        assert_eq!(fourcc(mvex[0].kind), "mehd");
        assert_eq!(be_u64(&p.buf, mvex[0].body().start + 4), 20_020);
        for (i, trex) in mvex[1..].iter().enumerate() {
            assert_eq!(fourcc(trex.kind), "trex");
            assert_eq!(p.u32_at(trex.body().start + 4), i as u32 + 1);
        }

        // Fragments: sequence numbers 1-4, track IDs 1 2 1 2, data offsets
        // still pointing at their mdat's payload, which is unchanged.
        for (n, pair) in top[2..10].chunks(2).enumerate() {
            let (moof, mdat) = (pair[0], pair[1]);
            let kids = p.kids(moof);
            let mfhd = find(&kids, b"mfhd").unwrap().body().start;
            assert_eq!(p.u32_at(mfhd + 4), n as u32 + 1);
            let traf = p.kids(find(&kids, b"traf").unwrap());
            let tfhd = find(&traf, b"tfhd").unwrap().body().start;
            assert_eq!(p.u32_at(tfhd + 4), n as u32 % 2 + 1);
            let trun = find(&traf, b"trun").unwrap().body().start;
            let offset = p.u32_at(trun + 8) as usize;
            assert_eq!(moof.start + offset, mdat.body().start);
            assert_eq!(&p.buf[mdat.body()], &payloads[n][..]);
        }

        // mfra: each track's fragments, by decode time and moof offset.
        let mfra = p.kids(top[10]);
        let kinds: Vec<String> = mfra.iter().map(|b| fourcc(b.kind)).collect();
        assert_eq!(kinds, ["tfra", "tfra", "mfro"]);
        for (i, tfra) in mfra[..2].iter().enumerate() {
            let at = tfra.body().start;
            assert_eq!(p.u32_at(at + 4), i as u32 + 1);
            assert_eq!(p.u32_at(at + 12), 2);
            let times = [[0, 240_240], [0, 480_256]][i];
            for e in 0..2 {
                let entry = at + 16 + e * 19;
                assert_eq!(be_u64(&p.buf, entry), times[e]);
                let moof = be_u64(&p.buf, entry + 8) as usize;
                assert_eq!(moof, top[2 + i * 2 + e * 4].start);
                assert_eq!(&p.buf[moof + 4..moof + 8], b"moof");
            }
        }
        let mfro = mfra[2].body().start;
        assert_eq!(p.u32_at(mfro + 4) as usize, top[10].end - top[10].start);
        assert_eq!(top[10].end, p.buf.len());
    }

    /// An explicit base data offset (absolute, in the segment) is moved to
    /// where the fragment lands in the file.
    #[test]
    fn moves_explicit_base_data_offsets() {
        let (file, payloads) = joined(true);
        let p = Parsed { buf: file };
        let top = p.top();
        for (n, pair) in top[2..10].chunks(2).enumerate() {
            let (moof, mdat) = (pair[0], pair[1]);
            let traf = p.kids(find(&p.kids(moof), b"traf").unwrap());
            let tfhd = find(&traf, b"tfhd").unwrap().body().start;
            let base = be_u64(&p.buf, tfhd + 8) as usize;
            assert_eq!(base, moof.start);
            let trun = find(&traf, b"trun").unwrap().body().start;
            let data = base + p.u32_at(trun + 8) as usize;
            assert_eq!(&p.buf[data..mdat.end], &payloads[n][..]);
        }
    }

    #[test]
    fn rejects_what_is_not_a_media_segment_of_the_track() {
        let video = Init::parse(init(b"vide", 24000, 0)).unwrap();
        let (mut mux, header) = Muxer::new(&[Track {
            init: video,
            duration: 1,
        }])
        .unwrap();
        let before = mux.position();
        assert_eq!(before, header.len() as u64);
        // An init segment, a truncated segment, an HTML error page.
        assert!(mux.fragments(0, &init(b"vide", 24000, 0)).is_err());
        let seg = segment(0, &[0; 16], false);
        assert!(mux.fragments(0, &seg[..seg.len() - 3]).is_err());
        assert!(mux.fragments(0, b"<html><body>502</body></html>").is_err());
        // A segment of another track.
        let mut other = seg.clone();
        let moof = top_level(&other).unwrap()[2].1;
        let tfhd_id = moof + 8 + 16 + 8 + 12; // moof hdr, mfhd, traf hdr, tfhd hdr + flags
        put_u32(&mut other, tfhd_id, 7);
        assert!(mux.fragments(0, &other).is_err());
        assert!(mux.fragments(1, &seg).is_err());
        // Nothing was handed out, so nothing moved.
        assert_eq!(mux.position(), before);
        assert!(mux.fragments(0, &seg).is_ok());
    }

    #[test]
    fn reads_large_and_open_ended_boxes() {
        // A 64-bit size, then a size-0 box running to the end.
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(b"free");
        buf.extend_from_slice(&20u64.to_be_bytes());
        buf.extend_from_slice(&[7; 4]);
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(b"mdat");
        buf.extend_from_slice(&[9; 5]);
        let boxes = children(&buf, 0..buf.len()).unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!((boxes[0].header, boxes[0].end), (16, 20));
        assert_eq!(&buf[boxes[0].body()], &[7; 4]);
        assert_eq!(boxes[1].end, buf.len());
        // Sizes that run past the end, or are smaller than a header.
        assert!(children(&[0, 0, 0, 9, b'f', b'r', b'e', b'e'], 0..8).is_err());
        assert!(children(&[0, 0, 0, 4, b'f', b'r', b'e', b'e'], 0..8).is_err());
        assert!(children(&[0, 0, 0], 0..3).is_err());
    }

    /// An mdat "to the end of the file" gets its real size once something
    /// follows it.
    #[test]
    fn closes_open_ended_mdats() {
        let video = Init::parse(init(b"vide", 24000, 0)).unwrap();
        let (mut mux, header) = Muxer::new(&[Track {
            init: video,
            duration: 1,
        }])
        .unwrap();
        let mut seg = segment(0, &[5; 16], false);
        let mdat = top_level(&seg).unwrap()[3].1;
        put_u32(&mut seg, mdat, 0);
        let mut file = header;
        file.extend(mux.fragments(0, &seg).unwrap());
        file.extend(mux.fragments(0, &seg).unwrap());
        file.extend(mux.finish());
        let kinds: Vec<String> = top_level(&file).unwrap().into_iter().map(|b| b.0).collect();
        assert_eq!(
            kinds,
            ["ftyp", "moov", "moof", "mdat", "moof", "mdat", "mfra"]
        );
    }

    #[test]
    fn rescales_up() {
        assert_eq!(rescale(240_240, 24000, 1000), 10_010);
        assert_eq!(rescale(1, 48000, 1000), 1);
        assert_eq!(rescale(0, 48000, 1000), 0);
        assert_eq!(rescale(5, 1000, 1000), 5);
    }
}
