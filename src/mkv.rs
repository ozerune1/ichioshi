//! Just enough Matroska (RFC 9559) to read a file's tracks and to write a
//! copy that leaves tracks out, so remuxing needs no external tools.
//!
//! What's kept is copied byte for byte: blocks, codec data, chapters,
//! attachments, tags. Only what points at file positions is rebuilt: the
//! SeekHead, the Cues (one per video keyframe, for seeking) and the sizes of
//! elements that changed. Track numbers stay as they were, so blocks need no
//! rewriting.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

const EBML: u32 = 0x1A45_DFA3;
const DOC_TYPE: u32 = 0x4282;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TITLE: u32 = 0x7BA9;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const FLAG_DEFAULT: u32 = 0x88;
const FLAG_FORCED: u32 = 0x55AA;
const FLAG_HEARING_IMPAIRED: u32 = 0x55AB;
const FLAG_COMMENTARY: u32 = 0x55AF;
const NAME: u32 = 0x536E;
const LANGUAGE: u32 = 0x22_B59C;
const LANGUAGE_BCP47: u32 = 0x22_B59D;
const CODEC_ID: u32 = 0x86;
const AUDIO: u32 = 0xE1;
const CHANNELS: u32 = 0x9F;
const CHAPTERS: u32 = 0x1043_A770;
const CLUSTER: u32 = 0x1F43_B675;
const TIMESTAMP: u32 = 0xE7;
const POSITION: u32 = 0xA7;
const PREV_SIZE: u32 = 0xAB;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const REFERENCE_BLOCK: u32 = 0xFB;
const BLOCK_DURATION: u32 = 0x9B;
const CONTENT_ENCODINGS: u32 = 0x6D80;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;
const CUE_RELATIVE_POSITION: u32 = 0xF0;
const CUE_DURATION: u32 = 0xB2;
const ATTACHMENTS: u32 = 0x1941_A469;
const TAGS: u32 = 0x1254_C367;
const TAG: u32 = 0x7373;
const TARGETS: u32 = 0x63C0;
const TAG_TRACK_UID: u32 = 0x63C5;
const TAG_ATTACHMENT_UID: u32 = 0x63C6;
const VOID: u32 = 0xEC;
const CRC32: u32 = 0xBF;

/// Elements that live directly in a Segment; one of these ends an
/// unknown-size Cluster.
const SEGMENT_CHILDREN: [u32; 8] = [
    SEEK_HEAD,
    INFO,
    TRACKS,
    CHAPTERS,
    CLUSTER,
    CUES,
    ATTACHMENTS,
    TAGS,
];
/// Largest element read into memory at once (a cluster, the track list...).
const MAX_IN_MEMORY: u64 = 1 << 30;
/// Space reserved at the start of the Segment for the SeekHead.
const SEEK_HEAD_SPACE: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitles,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackInfo {
    pub number: u64,
    pub uid: u64,
    pub kind: TrackKind,
    pub codec_id: String,
    /// As written: the BCP 47 tag if there is one, else the ISO 639-2 code
    /// ("eng" when absent, per the spec).
    pub language: String,
    pub name: String,
    pub default: bool,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub commentary: bool,
    pub channels: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct MkvInfo {
    pub title: Option<String>,
    pub tracks: Vec<TrackInfo>,
}

/// What a remux keeps.
pub struct Keep {
    /// Track numbers to keep; every other track is left out.
    pub tracks: Vec<u64>,
    /// Kept tracks to mark as default.
    pub default: Vec<u64>,
    /// Kept tracks to mark as not default.
    pub not_default: Vec<u64>,
    /// The new Segment title (none if empty).
    pub title: String,
    /// Keep attachments (fonts for styled subtitles).
    pub attachments: bool,
}

/// A short codec name for display, from the Matroska codec ID.
pub fn codec_name(codec_id: &str) -> String {
    let name = match codec_id {
        "S_TEXT/ASS" | "S_ASS" => "ASS",
        "S_TEXT/SSA" | "S_SSA" => "SSA",
        "S_TEXT/UTF8" | "S_TEXT/ASCII" => "SRT",
        "S_TEXT/WEBVTT" => "WebVTT",
        "S_HDMV/PGS" => "PGS",
        "S_HDMV/TEXTST" => "TextST",
        "S_VOBSUB" => "VobSub",
        "A_FLAC" => "FLAC",
        "A_OPUS" => "Opus",
        "A_VORBIS" => "Vorbis",
        "A_AC3" => "AC-3",
        "A_EAC3" => "E-AC-3",
        "A_TRUEHD" => "TrueHD",
        "A_MPEG/L3" => "MP3",
        "A_MPEG/L2" => "MP2",
        "V_MPEG4/ISO/AVC" => "H.264",
        "V_MPEGH/ISO/HEVC" => "HEVC",
        "V_AV1" => "AV1",
        "V_VP9" => "VP9",
        "V_VP8" => "VP8",
        id if id.starts_with("A_AAC") => "AAC",
        id if id.starts_with("A_DTS") => "DTS",
        id if id.starts_with("A_PCM") => "PCM",
        other => other,
    };
    name.to_owned()
}

/// Read a file's title and tracks.
pub fn probe(path: &Path) -> Result<MkvInfo> {
    let src = Source::open(path)?;
    // The track list comes before the first Cluster in practice; scan the
    // whole file only if it doesn't.
    let mut layout = layout(&src, false)?;
    if !layout.top.iter().any(|t| t.id == TRACKS) {
        layout = layout_full(&src)?;
    }
    let mut info = MkvInfo::default();
    for top in &layout.top {
        match top.id {
            INFO => {
                let data = src.read(top.data_start, top.end)?;
                info.title = children(&data)
                    .iter()
                    .find(|e| e.id == TITLE)
                    .map(|e| string(e.data));
            }
            TRACKS => {
                let data = src.read(top.data_start, top.end)?;
                info.tracks = children(&data)
                    .iter()
                    .filter(|e| e.id == TRACK_ENTRY)
                    .map(|e| parse_track(e.data))
                    .collect();
            }
            _ => {}
        }
    }
    if info.tracks.is_empty() {
        bail!("{} has no tracks", path.display());
    }
    Ok(info)
}

/// How a file's default tracks were changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultsEdit {
    /// They already were as asked.
    Unchanged,
    /// Only the track list was rewritten, in its own space and the padding
    /// after it: a few bytes at the start of the file.
    InPlace,
    /// The track list didn't fit, so the whole file was rewritten.
    Rewritten,
}

/// Make the audio and subtitle tracks in `on` the default ones and every
/// other audio and subtitle track not default. Nothing is removed, so the
/// other tracks are still there to switch to. Video and other tracks keep
/// their flags.
///
/// mkvmerge leaves padding (a Void) after the track list, and leaves out
/// default flags that are on, so turning one off adds three bytes; those
/// come out of the padding and the rest of the file stays put. Without
/// room, the file is rewritten (through a temporary copy beside it).
pub fn set_default_tracks(path: &Path, on: &HashSet<u64>) -> Result<DefaultsEdit> {
    let src = Source::open(path)?;
    let layout = layout(&src, false)?;
    let at = layout
        .top
        .iter()
        .position(|t| t.id == TRACKS)
        .context("no track list before the video data")?;
    let tracks = &layout.top[at];
    let data = src.read(tracks.data_start, tracks.end)?;

    let mut changed = false;
    let mut body = Vec::new();
    let mut numbers = Vec::new();
    let mut switchable = Vec::new();
    for e in children(&data) {
        match e.id {
            // A checksum would no longer match; it's optional.
            CRC32 | VOID => changed = true,
            TRACK_ENTRY => {
                let t = parse_track(e.data);
                numbers.push(t.number);
                let media = matches!(t.kind, TrackKind::Audio | TrackKind::Subtitles);
                if media {
                    switchable.push(t.number);
                }
                let want = on.contains(&t.number);
                if !media || t.default == want {
                    body.extend_from_slice(e.raw);
                    continue;
                }
                changed = true;
                let mut fields = Vec::new();
                let mut had_flag = false;
                for f in children(e.data) {
                    match f.id {
                        CRC32 | VOID => {}
                        FLAG_DEFAULT => {
                            fields.extend(uint_element(FLAG_DEFAULT, u64::from(want)));
                            had_flag = true;
                        }
                        _ => fields.extend_from_slice(f.raw),
                    }
                }
                if !had_flag {
                    fields.extend(uint_element(FLAG_DEFAULT, u64::from(want)));
                }
                body.extend(element(TRACK_ENTRY, &fields));
            }
            _ => body.extend_from_slice(e.raw),
        }
    }
    if !changed {
        return Ok(DefaultsEdit::Unchanged);
    }
    let new_tracks = element(TRACKS, &body);
    let room_end = match layout.top.get(at + 1) {
        Some(next) if next.id == VOID && next.start == tracks.end => next.end,
        _ => tracks.end,
    };
    let room = (room_end - tracks.start) as usize;
    let fits = new_tracks.len() == room || new_tracks.len() + 2 <= room;
    if fits {
        let mut bytes = new_tracks;
        if bytes.len() < room {
            let pad = room - bytes.len();
            bytes.extend(void(pad));
        }
        drop(src);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        write_all_at(&file, &bytes, tracks.start)?;
        file.sync_data()?;
        return Ok(DefaultsEdit::InPlace);
    }

    // No room: rewrite the file with every track, flags changed.
    let title = probe(path)?.title.unwrap_or_default();
    let keep = Keep {
        tracks: numbers,
        default: switchable
            .iter()
            .copied()
            .filter(|n| on.contains(n))
            .collect(),
        not_default: switchable
            .iter()
            .copied()
            .filter(|n| !on.contains(n))
            .collect(),
        title,
        attachments: true,
    };
    let mut tmp_name = path.file_name().context("no file name")?.to_os_string();
    tmp_name.push(".defaults.tmp");
    let tmp = path.with_file_name(tmp_name);
    let result = remux(path, &tmp, &keep, &AtomicBool::new(false), |_| {});
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(DefaultsEdit::Rewritten)
}

/// Write a copy of `input` to `output` with only `keep.tracks`. `progress`
/// gets 0.0–1.0; setting `cancel` stops it with an error.
pub fn remux(
    input: &Path,
    output: &Path,
    keep: &Keep,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<()> {
    let src = Source::open(input)?;
    let layout = layout_full(&src)?;
    let all = |id: u32| layout.top.iter().filter(move |t| t.id == id);

    let tracks_top = all(TRACKS).next().context("no track list")?;
    let tracks_data = src.read(tracks_top.data_start, tracks_top.end)?;
    let entries: Vec<(TrackInfo, Elem<'_>)> = children(&tracks_data)
        .into_iter()
        .filter(|e| e.id == TRACK_ENTRY)
        .map(|e| (parse_track(e.data), e))
        .collect();
    for n in &keep.tracks {
        if !entries.iter().any(|(t, _)| t.number == *n) {
            bail!("{} has no track {n}", input.display());
        }
    }
    let kept: Vec<&TrackInfo> = entries
        .iter()
        .map(|(t, _)| t)
        .filter(|t| keep.tracks.contains(&t.number))
        .collect();
    let kept_numbers: HashSet<u64> = kept.iter().map(|t| t.number).collect();
    let kept_uids: HashSet<u64> = kept.iter().map(|t| t.uid).collect();
    // Cues point at keyframes of the video (or, without video, the first kept
    // track), and, like mkvmerge, at every subtitle line, so players can show
    // a line already on screen at the point they seek to.
    let cue_track = kept
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .or(kept.first())
        .map(|t| t.number);
    let subtitle_tracks: HashSet<u64> = kept
        .iter()
        .filter(|t| t.kind == TrackKind::Subtitles)
        .map(|t| t.number)
        .collect();

    let mut out = Out {
        w: BufWriter::with_capacity(1 << 20, File::create(output)?),
        pos: 0,
    };
    out.write(&src.read(0, layout.ebml_end)?)?;
    out.write(&id_bytes(SEGMENT))?;
    let size_pos = out.pos;
    out.write(&[0x01, 0, 0, 0, 0, 0, 0, 0])?; // patched at the end
    let segment_start = out.pos;
    let seek_head_pos = out.pos;
    out.write(&void(SEEK_HEAD_SPACE))?; // the SeekHead goes here at the end
    let mut index: Vec<(u32, u64)> = Vec::new();

    if let Some(info) = all(INFO).next() {
        let data = src.read(info.data_start, info.end)?;
        let mut body: Vec<u8> = children(&data)
            .iter()
            .filter(|e| !matches!(e.id, TITLE | CRC32 | VOID))
            .flat_map(|e| e.raw.iter().copied())
            .collect();
        if !keep.title.is_empty() {
            body.extend(element(TITLE, keep.title.as_bytes()));
        }
        index.push((INFO, out.pos - segment_start));
        out.write(&element(INFO, &body))?;
    }

    let mut body = Vec::new();
    for (track, entry) in &entries {
        if !kept_numbers.contains(&track.number) {
            continue;
        }
        let set_default = if keep.default.contains(&track.number) {
            Some(true)
        } else if keep.not_default.contains(&track.number) {
            Some(false)
        } else {
            None
        };
        let mut fields: Vec<u8> = children(entry.data)
            .iter()
            .filter(|e| {
                !matches!(e.id, CRC32 | VOID) && !(set_default.is_some() && e.id == FLAG_DEFAULT)
            })
            .flat_map(|e| e.raw.iter().copied())
            .collect();
        if let Some(on) = set_default {
            fields.extend(uint_element(FLAG_DEFAULT, u64::from(on)));
        }
        body.extend(element(TRACK_ENTRY, &fields));
    }
    index.push((TRACKS, out.pos - segment_start));
    out.write(&element(TRACKS, &body))?;

    if let Some(chapters) = all(CHAPTERS).next() {
        index.push((CHAPTERS, out.pos - segment_start));
        out.write(&element(
            CHAPTERS,
            &src.read(chapters.data_start, chapters.end)?,
        ))?;
    }

    if keep.attachments {
        for (i, attachments) in all(ATTACHMENTS).enumerate() {
            if i == 0 {
                index.push((ATTACHMENTS, out.pos - segment_start));
            }
            let mut header = id_bytes(ATTACHMENTS);
            let len = attachments.end - attachments.data_start;
            put_size(&mut header, len, size_len(len));
            out.write(&header)?;
            out.copy(&src, attachments.data_start, attachments.end)?;
        }
    }

    // Tags, except those about tracks (or attachments) that are left out.
    let mut tags = Vec::new();
    for top in all(TAGS) {
        let data = src.read(top.data_start, top.end)?;
        for tag in children(&data).into_iter().filter(|e| e.id == TAG) {
            let targets = children(tag.data).into_iter().find(|e| e.id == TARGETS);
            let target_ids = |id| {
                targets.as_ref().map_or_else(Vec::new, |t| {
                    children(t.data)
                        .iter()
                        .filter(|e| e.id == id)
                        .map(|e| uint(e.data))
                        .collect()
                })
            };
            let track_uids = target_ids(TAG_TRACK_UID);
            let about_removed_track =
                !track_uids.is_empty() && !track_uids.iter().any(|u| kept_uids.contains(u));
            let about_removed_attachment =
                !keep.attachments && !target_ids(TAG_ATTACHMENT_UID).is_empty();
            if !about_removed_track && !about_removed_attachment {
                tags.extend_from_slice(tag.raw);
            }
        }
    }
    if !tags.is_empty() {
        index.push((TAGS, out.pos - segment_start));
        out.write(&element(TAGS, &tags))?;
    }

    let mut cues: Vec<Cue> = Vec::new();
    let total = src.len.max(1) as f32;
    for cluster in all(CLUSTER) {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let data = src.read(cluster.data_start, cluster.end)?;
        let mut body = Vec::with_capacity(data.len());
        let mut timestamp = 0u64;
        let mut blocks = 0usize;
        let mut keyframes = Vec::new();
        for e in children(&data) {
            match e.id {
                TIMESTAMP => {
                    timestamp = uint(e.data);
                    body.extend_from_slice(e.raw);
                }
                SIMPLE_BLOCK | BLOCK_GROUP => {
                    let (block, keyframe, duration) = if e.id == SIMPLE_BLOCK {
                        (e.data, None, None)
                    } else {
                        let group = children(e.data);
                        let Some(block) = group.iter().find(|c| c.id == BLOCK) else {
                            continue;
                        };
                        let duration = group
                            .iter()
                            .find(|c| c.id == BLOCK_DURATION)
                            .map(|c| uint(c.data));
                        (
                            block.data,
                            Some(!group.iter().any(|c| c.id == REFERENCE_BLOCK)),
                            duration,
                        )
                    };
                    let Some((track, n)) = vint(block, false) else {
                        continue;
                    };
                    if !kept_numbers.contains(&track) {
                        continue;
                    }
                    if block.len() >= n + 3 {
                        // SimpleBlocks flag keyframes; a BlockGroup is one if it references nothing.
                        let is_keyframe = keyframe.unwrap_or(block[n + 2] & 0x80 != 0);
                        if (Some(track) == cue_track && is_keyframe)
                            || subtitle_tracks.contains(&track)
                        {
                            let relative = i64::from(i16::from_be_bytes([block[n], block[n + 1]]));
                            let time = (timestamp as i64 + relative).max(0) as u64;
                            keyframes.push(Cue {
                                time,
                                track,
                                cluster: 0,
                                relative: body.len() as u64,
                                duration,
                            });
                        }
                    }
                    body.extend_from_slice(e.raw);
                    blocks += 1;
                }
                // Positions and checksums would be wrong after removing blocks.
                CRC32 | VOID | POSITION | PREV_SIZE => {}
                _ => body.extend_from_slice(e.raw),
            }
        }
        if blocks > 0 {
            let position = out.pos - segment_start;
            cues.extend(keyframes.into_iter().map(|cue| Cue {
                cluster: position,
                ..cue
            }));
            out.write(&element(CLUSTER, &body))?;
        }
        progress((cluster.end as f32 / total).min(0.99));
    }

    if !cues.is_empty() {
        // One CuePoint per time, in time order, with a position per track.
        cues.sort_by_key(|c| (c.time, c.track));
        let mut body = Vec::new();
        for group in cues.chunk_by(|a, b| a.time == b.time) {
            let mut point = uint_element(CUE_TIME, group[0].time);
            for cue in group {
                let mut positions = [
                    uint_element(CUE_TRACK, cue.track),
                    uint_element(CUE_CLUSTER_POSITION, cue.cluster),
                    uint_element(CUE_RELATIVE_POSITION, cue.relative),
                ]
                .concat();
                if let Some(duration) = cue.duration {
                    positions.extend(uint_element(CUE_DURATION, duration));
                }
                point.extend(element(CUE_TRACK_POSITIONS, &positions));
            }
            body.extend(element(CUE_POINT, &point));
        }
        index.push((CUES, out.pos - segment_start));
        out.write(&element(CUES, &body))?;
    }

    // Fill in the SeekHead and the Segment's size.
    let segment_size = out.pos - segment_start;
    let mut file = out.w.into_inner().map_err(|e| e.into_error())?;
    let entries: Vec<u8> = index
        .iter()
        .flat_map(|&(id, position)| {
            element(
                SEEK,
                &[
                    element(SEEK_ID, &id_bytes(id)),
                    uint_element(SEEK_POSITION, position),
                ]
                .concat(),
            )
        })
        .collect();
    let mut seek_head = element(SEEK_HEAD, &entries);
    if SEEK_HEAD_SPACE.checked_sub(seek_head.len()) == Some(1) {
        // A Void can't be one byte long: take the byte with a longer size field.
        seek_head = id_bytes(SEEK_HEAD);
        put_size(
            &mut seek_head,
            entries.len() as u64,
            size_len(entries.len() as u64) + 1,
        );
        seek_head.extend(&entries);
    }
    let rest = SEEK_HEAD_SPACE
        .checked_sub(seek_head.len())
        .context("too many index entries")?;
    file.seek(SeekFrom::Start(seek_head_pos))?;
    file.write_all(&seek_head)?;
    if rest > 0 {
        file.write_all(&void(rest))?;
    }
    let mut size = Vec::new();
    put_size(&mut size, segment_size, 8);
    file.seek(SeekFrom::Start(size_pos))?;
    file.write_all(&size)?;
    file.flush()?;
    progress(1.0);
    Ok(())
}

/// A cue: where a keyframe or subtitle line is, for seeking.
struct Cue {
    time: u64,
    track: u64,
    /// The Cluster's position in the Segment.
    cluster: u64,
    /// The block's position in the Cluster.
    relative: u64,
    duration: Option<u64>,
}

/// What a subtitle track contains: how many lines (events) and the text of
/// the first few, to tell tracks apart when their names don't.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubtitleSample {
    pub events: usize,
    pub lines: Vec<String>,
}

/// Count the lines of the given subtitle tracks and read the first
/// `keep_lines` of each. Walks the block headers without reading video or
/// audio data. Image subtitles (PGS, VobSub) and compressed tracks are
/// counted but not read.
pub fn subtitle_samples(
    path: &Path,
    tracks: &[u64],
    keep_lines: usize,
) -> Result<HashMap<u64, SubtitleSample>> {
    let src = Source::open(path)?;
    let layout = layout_full(&src)?;
    let tracks_top = layout
        .top
        .iter()
        .find(|t| t.id == TRACKS)
        .context("no track list")?;
    let tracks_data = src.read(tracks_top.data_start, tracks_top.end)?;
    let mut readable: HashMap<u64, String> = HashMap::new();
    for entry in children(&tracks_data)
        .into_iter()
        .filter(|e| e.id == TRACK_ENTRY)
    {
        let info = parse_track(entry.data);
        let compressed = children(entry.data)
            .iter()
            .any(|e| e.id == CONTENT_ENCODINGS);
        if tracks.contains(&info.number) && !compressed {
            readable.insert(info.number, info.codec_id);
        }
    }
    let mut samples: HashMap<u64, SubtitleSample> = tracks
        .iter()
        .map(|&t| (t, SubtitleSample::default()))
        .collect();
    let note = |track: u64, block: &[u8], n: usize, samples: &mut HashMap<u64, SubtitleSample>| {
        let Some(sample) = samples.get_mut(&track) else {
            return;
        };
        sample.events += 1;
        if sample.lines.len() < keep_lines
            && let Some(codec) = readable.get(&track)
            && let Some(text) = block
                .get(n + 3..)
                .and_then(|payload| subtitle_text(codec, payload))
        {
            sample.lines.push(text);
        }
    };
    for cluster in layout.top.iter().filter(|t| t.id == CLUSTER) {
        let mut pos = cluster.data_start;
        while pos < cluster.end {
            let Some((h, data_start)) = src.header_at(pos)? else {
                break;
            };
            let Some(size) = h.size else { break };
            let end = data_start + size;
            match h.id {
                SIMPLE_BLOCK => {
                    let head = src.read(data_start, (data_start + 12).min(end))?;
                    if let Some((track, n)) = vint(&head, false)
                        && samples.contains_key(&track)
                    {
                        note(track, &src.read(data_start, end)?, n, &mut samples);
                    }
                }
                // Subtitles usually come in BlockGroups (with a duration).
                BLOCK_GROUP if size <= 4 << 20 => {
                    let group = src.read(data_start, end)?;
                    for block in children(&group).into_iter().filter(|c| c.id == BLOCK) {
                        if let Some((track, n)) = vint(block.data, false) {
                            note(track, block.data, n, &mut samples);
                        }
                    }
                }
                _ => {}
            }
            pos = end;
        }
    }
    Ok(samples)
}

/// The readable text of one subtitle event, without styling.
fn subtitle_text(codec_id: &str, payload: &[u8]) -> Option<String> {
    static MARKUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^}]*\}|<[^>]*>").unwrap());
    let raw = String::from_utf8_lossy(payload);
    let text = match codec_id {
        // ReadOrder, Layer, Style, Name, MarginL, MarginR, MarginV, Effect, Text
        "S_TEXT/ASS" | "S_ASS" | "S_TEXT/SSA" | "S_SSA" => raw
            .splitn(9, ',')
            .nth(8)?
            .replace("\\N", " ")
            .replace("\\n", " "),
        "S_TEXT/UTF8" | "S_TEXT/ASCII" | "S_TEXT/WEBVTT" => raw.into_owned(),
        _ => return None,
    };
    let text = MARKUP.replace_all(&text, "");
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}'))
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

fn parse_track(data: &[u8]) -> TrackInfo {
    let mut t = TrackInfo {
        number: 0,
        uid: 0,
        kind: TrackKind::Other,
        codec_id: String::new(),
        language: "eng".to_owned(),
        name: String::new(),
        default: true,
        forced: false,
        hearing_impaired: false,
        commentary: false,
        channels: None,
    };
    let mut bcp47 = None;
    for e in children(data) {
        match e.id {
            TRACK_NUMBER => t.number = uint(e.data),
            TRACK_UID => t.uid = uint(e.data),
            TRACK_TYPE => {
                t.kind = match uint(e.data) {
                    1 => TrackKind::Video,
                    2 => TrackKind::Audio,
                    17 => TrackKind::Subtitles,
                    _ => TrackKind::Other,
                }
            }
            FLAG_DEFAULT => t.default = uint(e.data) != 0,
            FLAG_FORCED => t.forced = uint(e.data) != 0,
            FLAG_HEARING_IMPAIRED => t.hearing_impaired = uint(e.data) != 0,
            FLAG_COMMENTARY => t.commentary = uint(e.data) != 0,
            NAME => t.name = string(e.data),
            LANGUAGE => t.language = string(e.data),
            LANGUAGE_BCP47 => bcp47 = Some(string(e.data)),
            CODEC_ID => t.codec_id = string(e.data),
            AUDIO => {
                t.channels = children(e.data)
                    .iter()
                    .find(|c| c.id == CHANNELS)
                    .map(|c| uint(c.data))
            }
            _ => {}
        }
    }
    if let Some(tag) = bcp47.filter(|t| !t.is_empty()) {
        t.language = tag;
    }
    t
}

// ── Reading ─────────────────────────────────────────────────────────────

/// An EBML variable-length integer at the start of `buf`: (value, length).
/// IDs keep their length marker bits; sizes don't.
fn vint(buf: &[u8], keep_marker: bool) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    if first == 0 {
        return None;
    }
    let len = first.leading_zeros() as usize + 1;
    let rest = buf.get(1..len)?;
    let first = if keep_marker {
        u64::from(first)
    } else {
        u64::from(first) & ((1u64 << (8 - len)) - 1)
    };
    Some((
        rest.iter().fold(first, |v, &b| (v << 8) | u64::from(b)),
        len,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Header {
    id: u32,
    /// `None` for "unknown size".
    size: Option<u64>,
    len: usize,
}

fn header(buf: &[u8]) -> Option<Header> {
    let (id, id_len) = vint(buf, true)?;
    if id_len > 4 {
        return None;
    }
    let (size, size_len) = vint(&buf[id_len..], false)?;
    let unknown = size == (1u64 << (7 * size_len)) - 1;
    Some(Header {
        id: id as u32,
        size: (!unknown).then_some(size),
        len: id_len + size_len,
    })
}

struct Elem<'a> {
    id: u32,
    /// Header and data.
    raw: &'a [u8],
    data: &'a [u8],
}

/// The elements in a master element's data. Stops at anything malformed or
/// cut off, so damaged files lose only their damaged end.
fn children(mut buf: &[u8]) -> Vec<Elem<'_>> {
    let mut out = Vec::new();
    while let Some(h) = header(buf) {
        let Some(end) = h
            .size
            .and_then(|s| usize::try_from(s).ok())
            .and_then(|s| s.checked_add(h.len))
        else {
            break;
        };
        if end > buf.len() {
            break;
        }
        out.push(Elem {
            id: h.id,
            raw: &buf[..end],
            data: &buf[h.len..end],
        });
        buf = &buf[end..];
    }
    out
}

fn uint(data: &[u8]) -> u64 {
    data.iter().take(8).fold(0, |v, &b| (v << 8) | u64::from(b))
}

fn string(data: &[u8]) -> String {
    String::from_utf8_lossy(data)
        .trim_end_matches('\0')
        .trim()
        .to_owned()
}

/// A file read with positional reads (no shared cursor, no read-ahead).
struct Source {
    file: File,
    len: u64,
}

impl Source {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }

    fn read(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        let end = end.min(self.len);
        if end < start || end - start > MAX_IN_MEMORY {
            bail!("element at {start} is too large or malformed");
        }
        let mut buf = vec![0; (end - start) as usize];
        read_exact_at(&self.file, &mut buf, start)?;
        Ok(buf)
    }

    /// The element header at `pos` and where its data starts; `None` at the
    /// end of the file or at bytes that aren't an element.
    fn header_at(&self, pos: u64) -> Result<Option<(Header, u64)>> {
        if pos >= self.len {
            return Ok(None);
        }
        let buf = self.read(pos, pos + 12)?;
        Ok(header(&buf).map(|h| (h, pos + h.len as u64)))
    }
}

/// An element directly inside the Segment.
struct Top {
    id: u32,
    /// Where its header starts.
    start: u64,
    data_start: u64,
    end: u64,
}

struct Layout {
    /// The EBML header is `0..ebml_end`.
    ebml_end: u64,
    top: Vec<Top>,
}

fn layout_full(src: &Source) -> Result<Layout> {
    layout(src, true)
}

/// Find the Segment's elements, skipping over their data. With `all` false,
/// stop at the first Cluster.
fn layout(src: &Source, all: bool) -> Result<Layout> {
    let not_matroska = || anyhow::anyhow!("not a Matroska (MKV) file");
    let (h, data) = src.header_at(0)?.ok_or_else(not_matroska)?;
    if h.id != EBML {
        return Err(not_matroska());
    }
    let ebml_end = data + h.size.ok_or_else(not_matroska)?;
    let doc_type = children(&src.read(data, ebml_end)?)
        .iter()
        .find(|e| e.id == DOC_TYPE)
        .map(|e| string(e.data))
        .unwrap_or_default();
    if doc_type != "matroska" && doc_type != "webm" {
        return Err(not_matroska());
    }

    let mut pos = ebml_end;
    let (segment, segment_data) = loop {
        let (h, data) = src.header_at(pos)?.context("no Segment in the file")?;
        if h.id == SEGMENT {
            break (h, data);
        }
        pos = data + h.size.context("malformed element before the Segment")?;
    };
    let segment_end = segment
        .size
        .map_or(src.len, |s| (segment_data + s).min(src.len));

    let mut top = Vec::new();
    let mut pos = segment_data;
    while pos < segment_end {
        let Some((h, data_start)) = src.header_at(pos)? else {
            break;
        };
        let end = match h.size {
            Some(s) => data_start.saturating_add(s).min(segment_end),
            None if h.id == CLUSTER => unknown_cluster_end(src, data_start, segment_end)?,
            None => segment_end,
        };
        top.push(Top {
            id: h.id,
            start: pos,
            data_start,
            end,
        });
        if !all && h.id == CLUSTER {
            break;
        }
        pos = end;
    }
    Ok(Layout { ebml_end, top })
}

/// Where an unknown-size Cluster ends: at the next Segment-level element.
fn unknown_cluster_end(src: &Source, mut pos: u64, segment_end: u64) -> Result<u64> {
    while pos < segment_end {
        let Some((h, data_start)) = src.header_at(pos)? else {
            return Ok(pos);
        };
        if SEGMENT_CHILDREN.contains(&h.id) {
            return Ok(pos);
        }
        let Some(size) = h.size else {
            return Ok(segment_end);
        };
        pos = data_start + size;
    }
    Ok(segment_end)
}

// ── Writing ─────────────────────────────────────────────────────────────

struct Out {
    w: BufWriter<File>,
    pos: u64,
}

impl Out {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.w.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    fn copy(&mut self, src: &Source, start: u64, end: u64) -> Result<()> {
        let mut pos = start;
        while pos < end {
            let chunk = src.read(pos, end.min(pos + (8 << 20)))?;
            if chunk.is_empty() {
                break;
            }
            self.write(&chunk)?;
            pos += chunk.len() as u64;
        }
        Ok(())
    }
}

fn id_bytes(id: u32) -> Vec<u8> {
    let skip = (id.leading_zeros() / 8).min(3) as usize;
    id.to_be_bytes()[skip..].to_vec()
}

/// The shortest size field for `size` (all-ones means "unknown", so it's avoided).
fn size_len(size: u64) -> usize {
    (1..=8).find(|&n| size < (1u64 << (7 * n)) - 1).unwrap_or(8)
}

fn put_size(out: &mut Vec<u8>, size: u64, len: usize) {
    let marked = size | (1u64 << (7 * len));
    out.extend_from_slice(&marked.to_be_bytes()[8 - len..]);
}

fn element(id: u32, data: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    put_size(&mut out, data.len() as u64, size_len(data.len() as u64));
    out.extend_from_slice(data);
    out
}

fn uint_element(id: u32, value: u64) -> Vec<u8> {
    let skip = (value.leading_zeros() / 8).min(7) as usize;
    element(id, &value.to_be_bytes()[skip..])
}

/// A Void element exactly `total` bytes long (at least 2).
fn void(total: usize) -> Vec<u8> {
    for len in 1..=8 {
        let Some(payload) = total.checked_sub(1 + len) else {
            break;
        };
        if (payload as u64) < (1u64 << (7 * len)) - 1 {
            let mut out = vec![VOID as u8];
            put_size(&mut out, payload as u64, len);
            out.resize(total, 0);
            return out;
        }
    }
    unreachable!("Void of {total} bytes")
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small Matroska file with named tracks `(number, type, language,
    /// name, explicit FlagDefault)`, padding after the track list, and a
    /// cluster of video blocks.
    pub(crate) fn with_tracks(entries: &[(u64, u64, &str, &str, Option<u64>)]) -> Vec<u8> {
        let mut seg = element(INFO, &uint_element(0x2A_D7B1, 1_000_000));
        let mut tracks = Vec::new();
        for &(n, kind, lang, name, default) in entries {
            let codec: &[u8] = match kind {
                1 => b"V_MPEG4/ISO/AVC",
                2 => b"A_AAC",
                _ => b"S_TEXT/ASS",
            };
            let mut f = uint_element(TRACK_NUMBER, n);
            f.extend(uint_element(TRACK_UID, n + 100));
            f.extend(uint_element(TRACK_TYPE, kind));
            f.extend(element(CODEC_ID, codec));
            f.extend(element(LANGUAGE, lang.as_bytes()));
            f.extend(element(NAME, name.as_bytes()));
            if let Some(d) = default {
                f.extend(uint_element(FLAG_DEFAULT, d));
            }
            tracks.extend(element(TRACK_ENTRY, &f));
        }
        seg.extend(element(TRACKS, &tracks));
        seg.extend(void(256));
        let mut cluster = uint_element(TIMESTAMP, 0);
        cluster.extend(element(SIMPLE_BLOCK, &[0x81, 0, 0, 0x80, 1, 2, 3]));
        seg.extend(element(CLUSTER, &cluster));
        let mut file = element(EBML, &element(DOC_TYPE, b"matroska"));
        file.extend(element(SEGMENT, &seg));
        file
    }

    /// A small Matroska file: tracks `(number, type, explicit FlagDefault)`,
    /// `void` bytes of padding after the track list (0 for none), then a
    /// cluster of video blocks.
    fn synthetic(entries: &[(u64, u64, Option<u64>)], void_len: usize) -> Vec<u8> {
        let mut seg = element(INFO, &uint_element(0x2A_D7B1, 1_000_000));
        let mut tracks = Vec::new();
        for &(n, kind, default) in entries {
            let codec: &[u8] = match kind {
                1 => b"V_MPEG4/ISO/AVC",
                2 => b"A_AAC",
                _ => b"S_TEXT/UTF8",
            };
            let mut f = uint_element(TRACK_NUMBER, n);
            f.extend(uint_element(TRACK_UID, n + 100));
            f.extend(uint_element(TRACK_TYPE, kind));
            f.extend(element(CODEC_ID, codec));
            if let Some(d) = default {
                f.extend(uint_element(FLAG_DEFAULT, d));
            }
            tracks.extend(element(TRACK_ENTRY, &f));
        }
        seg.extend(element(TRACKS, &tracks));
        if void_len > 0 {
            seg.extend(void(void_len));
        }
        let mut cluster = uint_element(TIMESTAMP, 0);
        for i in 0..4u8 {
            cluster.extend(element(
                SIMPLE_BLOCK,
                &[0x81, 0, i, 0x80, b'f', b'r', b'a', b'm', b'e', b'0' + i],
            ));
        }
        seg.extend(element(CLUSTER, &cluster));
        let mut file = element(EBML, &element(DOC_TYPE, b"matroska"));
        file.extend(element(SEGMENT, &seg));
        file
    }

    fn defaults(path: &Path) -> Vec<(u64, bool)> {
        probe(path)
            .unwrap()
            .tracks
            .iter()
            .map(|t| (t.number, t.default))
            .collect()
    }

    const TRACKS_1_TO_5: [(u64, u64, Option<u64>); 5] = [
        (1, 1, None),
        (2, 2, None),
        (3, 2, None),
        (4, 17, None),
        (5, 17, Some(0)),
    ];

    #[test]
    fn sets_default_tracks_in_place_using_the_padding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.mkv");
        let original = synthetic(&TRACKS_1_TO_5, 64);
        std::fs::write(&path, &original).unwrap();
        let on: HashSet<u64> = [3, 5].into();
        assert_eq!(
            set_default_tracks(&path, &on).unwrap(),
            DefaultsEdit::InPlace
        );
        assert_eq!(
            defaults(&path),
            [(1, true), (2, false), (3, true), (4, false), (5, true)]
        );
        let edited = std::fs::read(&path).unwrap();
        assert_eq!(edited.len(), original.len(), "same size");
        let cluster = original
            .windows(4)
            .position(|w| w == id_bytes(CLUSTER).as_slice())
            .unwrap();
        assert_eq!(
            edited[cluster..],
            original[cluster..],
            "everything after the padding untouched"
        );
        assert_eq!(
            set_default_tracks(&path, &on).unwrap(),
            DefaultsEdit::Unchanged
        );
        // And back: the flags are now explicit, so this needs no padding at all.
        assert_eq!(
            set_default_tracks(&path, &[2, 4].into()).unwrap(),
            DefaultsEdit::InPlace
        );
        assert_eq!(
            defaults(&path),
            [(1, true), (2, true), (3, false), (4, true), (5, false)]
        );
    }

    #[test]
    fn flips_explicit_flags_without_padding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.mkv");
        let explicit = [
            (1, 1, Some(1)),
            (2, 2, Some(1)),
            (3, 2, Some(0)),
            (4, 17, Some(0)),
        ];
        std::fs::write(&path, synthetic(&explicit, 0)).unwrap();
        assert_eq!(
            set_default_tracks(&path, &[3, 4].into()).unwrap(),
            DefaultsEdit::InPlace
        );
        assert_eq!(
            defaults(&path),
            [(1, true), (2, false), (3, true), (4, true)]
        );
    }

    #[test]
    fn rewrites_the_file_when_there_is_no_room() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.mkv");
        std::fs::write(&path, synthetic(&TRACKS_1_TO_5, 0)).unwrap();
        assert_eq!(
            set_default_tracks(&path, &[3].into()).unwrap(),
            DefaultsEdit::Rewritten
        );
        assert_eq!(
            defaults(&path),
            [(1, true), (2, false), (3, true), (4, false), (5, false)]
        );
        let rewritten = std::fs::read(&path).unwrap();
        for i in 0..4u8 {
            let frame = [b'f', b'r', b'a', b'm', b'e', b'0' + i];
            assert!(rewritten.windows(6).any(|w| w == frame), "frame {i} kept");
        }
        assert!(!dir.path().join("c.mkv.defaults.tmp").exists());
    }

    #[test]
    fn variable_length_integers() {
        assert_eq!(vint(&[0x81], false), Some((1, 1)));
        assert_eq!(vint(&[0x40, 0x02], false), Some((2, 2)));
        assert_eq!(
            vint(&[0x1A, 0x45, 0xDF, 0xA3], true),
            Some((0x1A45_DFA3, 4))
        );
        assert_eq!(
            vint(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], false),
            Some(((1 << 56) - 1, 8))
        );
        assert_eq!(vint(&[0x00], false), None);
        assert_eq!(vint(&[0x40], false), None, "cut off");
        // Unknown size.
        assert_eq!(header(&[0x1F, 0x43, 0xB6, 0x75, 0xFF]).unwrap().size, None);
    }

    #[test]
    fn writes_elements_that_read_back() {
        for size in [0u64, 1, 126, 127, 128, 16_382, 16_383, 1 << 30] {
            let mut buf = Vec::new();
            put_size(&mut buf, size, size_len(size));
            assert_eq!(
                vint(&buf, false),
                Some((size, size_len(size))),
                "size {size}"
            );
        }
        let e = uint_element(TRACK_NUMBER, 300);
        let h = header(&e).unwrap();
        assert_eq!(
            (h.id, h.size, uint(&e[h.len..])),
            (TRACK_NUMBER, Some(2), 300)
        );
        assert_eq!(uint_element(FLAG_DEFAULT, 0), vec![0x88, 0x81, 0x00]);
        assert_eq!(id_bytes(CLUSTER), vec![0x1F, 0x43, 0xB6, 0x75]);
        for total in [2, 3, 128, 129, 130, 256] {
            let v = void(total);
            assert_eq!(v.len(), total);
            let h = header(&v).unwrap();
            assert_eq!((h.id, h.len as u64 + h.size.unwrap()), (VOID, total as u64));
        }
    }

    #[test]
    fn parses_track_entries_with_spec_defaults() {
        let entry = [
            uint_element(TRACK_NUMBER, 3),
            uint_element(TRACK_TYPE, 17),
            element(CODEC_ID, b"S_TEXT/ASS"),
            element(NAME, b"Signs"),
            element(LANGUAGE, b"eng"),
            element(LANGUAGE_BCP47, b"en-US"),
            uint_element(FLAG_FORCED, 1),
        ]
        .concat();
        let t = parse_track(&entry);
        assert_eq!(
            (t.number, t.kind, t.name.as_str()),
            (3, TrackKind::Subtitles, "Signs")
        );
        assert_eq!(
            subtitle_text(
                "S_TEXT/ASS",
                b"12,0,Default,,0,0,0,,{\\i1}Hello,\\Nworld{\\i0}"
            ),
            Some("Hello, world".into())
        );
        assert_eq!(
            subtitle_text(
                "S_TEXT/UTF8",
                "{\\an8}<i>\u{200e}She's a passive waiter.</i>\n".as_bytes()
            ),
            Some("She's a passive waiter.".into())
        );
        assert_eq!(subtitle_text("S_HDMV/PGS", b"\x00\x01"), None);
        assert_eq!(t.language, "en-US", "BCP 47 wins");
        assert!(t.forced && t.default, "FlagDefault defaults to 1");
        let bare = parse_track(&uint_element(TRACK_NUMBER, 1));
        assert_eq!(bare.language, "eng", "Language defaults to eng");
        assert_eq!(codec_name("A_AAC/MPEG4/LC"), "AAC");
    }
}
