//! The player page (iced, GStreamer): the picture, with controls made for
//! anime: skip the opening or ending, next episode, audio and subtitle
//! tracks by name, chapters on the seek bar.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::sync::Arc;

use anyhow::anyhow;
use iced::widget::image;
use iced_video_player::Video;

use crate::gst_video::{self, Frames};
use crate::player::{Chapter, Part, Track, TrackKind};

pub use super::player_types::*;

pub struct Player {
    pub video: Video,
    pub series: u64,
    pub episode: u32,
    pub path: PathBuf,
    pub series_title: String,
    /// The episode's own title.
    pub name: Option<String>,
    pub tracks: Vec<Track>,
    pub chapters: Vec<Chapter>,
    pub audio: Option<usize>,
    pub subs: Option<usize>,
    /// 0–150.
    pub volume: f64,
    /// Where the seek bar is being dragged to.
    dragging: Option<f64>,
    /// Paused by you. (GStreamer's own state passes through "paused" while
    /// it settles after a seek, so it can't say.)
    want_paused: bool,
    pub ended: bool,
    pub last_saved: Instant,
    /// When the mouse last moved: the controls show for a while after.
    pub active: Instant,
    /// Chapters skipped automatically in this episode.
    pub skipped: HashSet<usize>,
    pub panel: Option<Panel>,
    /// Each subtitle track's number in the file and codec, by index.
    sub_tracks: Vec<(u64, String)>,
    /// The file's tracks as read (to choose again by other languages).
    info: crate::mkv::MkvInfo,
    /// Text subtitles, drawn by libass over the picture.
    text_subs: Option<Arc<crate::mkv::TextSubtitles>>,
    ass: Option<crate::subs::Renderer>,
    /// What libass drew last, and for which moment and size.
    pub overlay: Option<image::Handle>,
    drawn_for: Option<(i64, (u32, u32), i32)>,
    /// The window's size in pixels (subtitles are drawn at the picture's
    /// size in it).
    pub view_px: (u32, u32),
    /// Pixels per point.
    pub scale: f32,
    pub sub_delay_ms: i64,
}

impl Player {
    /// Start playing `path` (from `start` seconds in).
    /// Start playing `uri` (a file's, or a stream's; `key` names it), whose
    /// tracks are `described`, from `start` seconds in.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        uri: &str,
        key: &Path,
        described: gst_video::Described,
        start: Option<f64>,
        (series, episode): (u64, u32),
        series_title: String,
        name: Option<String>,
        volume: f64,
    ) -> anyhow::Result<Self> {
        let path = key;
        let (pipeline, sink) = gst_video::pipeline(uri, Frames::Nv12)?;
        let gst_video::Described {
            tracks,
            chapters,
            audio,
            subs,
            sub_tracks,
            info,
        } = described;
        let mut video =
            Video::from_gst_pipeline(pipeline, sink, None).map_err(|e| anyhow!("{e}"))?;
        if let Some(a) = audio {
            gst_video::set_audio(&video.pipeline(), a);
        }
        // Text subtitles are drawn here (libass); GStreamer only draws
        // image ones (PGS, VobSub).
        let bitmap = subs.filter(|i| {
            sub_tracks
                .get(*i)
                .is_some_and(|(_, c)| !gst_video::is_text_subtitle(c))
        });
        gst_video::set_subtitles(&video.pipeline(), bitmap);
        video.set_volume((volume / 100.0).clamp(0.0, 1.5));
        if let Some(at) = start {
            let _ = video.seek(Duration::from_secs_f64(at), true);
        }
        Ok(Self {
            video,
            series,
            episode,
            path: path.to_owned(),
            series_title,
            name,
            tracks,
            chapters,
            audio,
            subs,
            volume,
            dragging: None,
            want_paused: false,
            ended: false,
            last_saved: Instant::now(),
            active: Instant::now(),
            skipped: HashSet::new(),
            panel: None,
            sub_tracks,
            text_subs: None,
            ass: None,
            overlay: None,
            drawn_for: None,
            view_px: (1440, 900),
            scale: 1.0,
            sub_delay_ms: 0,
            info,
        })
    }

    pub fn position(&self) -> f64 {
        self.dragging
            .unwrap_or_else(|| self.video.position().as_secs_f64())
    }

    pub fn duration(&self) -> f64 {
        self.video.duration().as_secs_f64()
    }

    pub fn paused(&self) -> bool {
        self.want_paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.want_paused = paused;
        self.video.set_paused(paused);
    }

    /// The chapter playing, its index, and when it ends.
    pub fn current_part(&self) -> Option<(usize, &Chapter, f64)> {
        let pos = self.position();
        let i = self.chapters.iter().rposition(|c| c.start <= pos + 0.05)?;
        let end = self
            .chapters
            .get(i + 1)
            .map_or(self.duration(), |n| n.start);
        Some((i, &self.chapters[i], end))
    }

    /// Watched: to the end, into the ending (past halfway), or 90% through.
    pub fn watched_enough(&self) -> bool {
        let (pos, dur) = (self.position(), self.duration());
        dur > 0.0
            && (self.ended
                || pos >= dur * 0.9
                || self
                    .chapters
                    .iter()
                    .any(|c| c.part == Part::Ending && c.start > dur * 0.5 && pos >= c.start))
    }

    /// Whether the video has reached its end (the bus says so; the widget
    /// only notices while it's drawn).
    pub fn reached_end(&self) -> bool {
        use gstreamer::prelude::*;
        !self.ended
            && (self.video.eos()
                || self
                    .video
                    .pipeline()
                    .bus()
                    .is_some_and(|bus| bus.pop_filtered(&[gstreamer::MessageType::Eos]).is_some()))
    }

    pub fn seek_to(&mut self, secs: f64, accurate: bool) {
        let secs = secs.clamp(0.0, self.duration().max(0.0));
        self.ended = false;
        let _ = self.video.seek(Duration::from_secs_f64(secs), accurate);
        self.draw_subs(Some(secs));
    }

    /// The file's subtitles are read: show the chosen ones.
    /// Switch to the audio and subtitles `prefs` choose.
    pub fn use_languages(&mut self, prefs: &crate::prefs::LangPrefs) {
        let (audio, subs) = gst_video::choose(&self.info, prefs);
        if let Some(i) = audio
            && self.audio != Some(i)
        {
            gst_video::set_audio(&self.video.pipeline(), i);
            self.audio = Some(i);
        }
        self.subs = subs;
        self.apply_subs();
    }

    pub fn subs_loaded(&mut self, subs: Arc<crate::mkv::TextSubtitles>) {
        if crate::subs::available() {
            self.ass = crate::subs::Renderer::new(&subs).ok();
        }
        self.text_subs = Some(subs);
        self.apply_subs();
    }

    /// Route the chosen subtitles: text to libass, images to GStreamer.
    fn apply_subs(&mut self) {
        let chosen = self.subs.and_then(|i| self.sub_tracks.get(i).cloned());
        let text = chosen
            .as_ref()
            .filter(|(_, codec)| gst_video::is_text_subtitle(codec));
        gst_video::set_subtitles(
            &self.video.pipeline(),
            if chosen.is_some() && text.is_none() {
                self.subs
            } else {
                None
            },
        );
        let (w, h) = self.video.size();
        let size = (w.max(0) as u32, h.max(0) as u32);
        let matrix = gst_video::video_matrix(&self.video.pipeline(), size);
        if let Some(ass) = &mut self.ass {
            let track = text.and_then(|(n, _)| self.text_subs.as_ref()?.tracks.get(n));
            ass.set_track(track, size, matrix);
        }
        self.overlay = None;
        self.drawn_for = None;
        self.draw_subs(None);
    }

    /// The picture's size in the window, in pixels.
    fn picture_px(&self) -> (u32, u32) {
        let (vw, vh) = self.video.size();
        let (ww, wh) = self.view_px;
        if vw <= 0 || vh <= 0 || ww == 0 || wh == 0 {
            return (0, 0);
        }
        let s = (ww as f64 / vw as f64).min(wh as f64 / vh as f64);
        (
            (vw as f64 * s).round() as u32,
            (vh as f64 * s).round() as u32,
        )
    }

    /// Draw the subtitles for the moment shown (or `at`, after a seek).
    pub fn draw_subs(&mut self, at: Option<f64>) {
        if self.ass.is_none() {
            return;
        }
        let size = self.picture_px();
        let at = at.unwrap_or_else(|| self.video.position().as_secs_f64());
        let now = (at * 1000.0).round() as i64 - self.sub_delay_ms;
        // Dialogue moves up out of the controls' way while they show (as
        // Netflix does, and mpv's sub-pos): by as much of the picture as
        // they cover.
        let raise = if self.controls_shown() && size.1 > 0 {
            let covered =
                CONTROLS_HEIGHT * self.scale as f64 - (self.view_px.1 as f64 - size.1 as f64) / 2.0;
            (covered.max(0.0) / size.1 as f64 * 100.0).ceil() + 1.0
        } else {
            0.0
        };
        if self.drawn_for == Some((now, size, raise as i32)) {
            return;
        }
        self.drawn_for = Some((now, size, raise as i32));
        let Some(ass) = &mut self.ass else { return };
        match ass.draw(now, size, raise) {
            crate::subs::Drawn::Unchanged => {}
            crate::subs::Drawn::Empty => self.overlay = None,
            crate::subs::Drawn::Image(px) => {
                self.overlay = Some(image::Handle::from_rgba(size.0, size.1, px))
            }
        }
    }

    pub fn skip_part(&mut self) {
        if let Some((_, _, end)) = self.current_part() {
            self.seek_to(end, true);
        }
    }

    /// The controls' own changes (the app handles going places).
    pub fn update(&mut self, msg: Msg) {
        // What you do brings the controls up; what the video does doesn't.
        if !matches!(msg, Msg::Frame | Msg::SubsLoaded(..)) {
            self.active = Instant::now();
        }
        match msg {
            Msg::TogglePause => {
                if self.ended {
                    self.seek_to(0.0, true);
                    self.set_paused(false);
                } else {
                    self.set_paused(!self.want_paused);
                }
            }
            Msg::Seek(to) => {
                self.dragging = Some(to);
                let _ = self.video.seek(Duration::from_secs_f64(to), false);
                self.draw_subs(Some(to));
            }
            Msg::SeekDone => {
                if let Some(to) = self.dragging.take() {
                    self.seek_to(to, true);
                }
            }
            Msg::SeekBy(d) => {
                let to = self.position() + d;
                self.seek_to(to, true);
            }
            Msg::Volume(v) => {
                self.volume = v.clamp(0.0, 150.0);
                self.video.set_volume(self.volume / 100.0);
                self.video.set_muted(false);
            }
            Msg::Mute => {
                let muted = self.video.muted();
                self.video.set_muted(!muted);
            }
            Msg::Audio(c) => {
                if let Some(i) = c.index {
                    gst_video::set_audio(&self.video.pipeline(), i);
                    self.audio = Some(i);
                }
            }
            Msg::Subs(c) => {
                self.subs = c.index;
                self.apply_subs();
            }
            Msg::Languages(prefs) => self.use_languages(&prefs),
            Msg::Frame => self.draw_subs(None),
            Msg::SubsLoaded(path, Some(subs)) if path == self.path => self.subs_loaded(subs),
            Msg::SubsLoaded(..) => {}
            Msg::SubDelay(d) => {
                self.sub_delay_ms += d;
                self.drawn_for = None;
                self.draw_subs(None);
            }
            Msg::SkipPart => self.skip_part(),
            Msg::Surface => match self.panel.take() {
                Some(_) => {}
                None => self.update(Msg::TogglePause),
            },
            Msg::Panel(panel) => {
                self.panel = if self.panel == Some(panel) {
                    None
                } else {
                    Some(panel)
                }
            }
            Msg::Ended => self.ended = true,
            Msg::Moved | Msg::Next | Msg::Prev | Msg::Fullscreen | Msg::Back | Msg::Setting(..) => {
            }
        }
    }

    pub(super) fn choices(&self, kind: TrackKind) -> Vec<TrackChoice> {
        let mut out: Vec<TrackChoice> = self
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| TrackChoice {
                index: Some(t.id as usize),
                label: t.label(),
            })
            .collect();
        if kind == TrackKind::Sub {
            out.insert(
                0,
                TrackChoice {
                    index: None,
                    label: "Off".into(),
                },
            );
        }
        out
    }

    pub(super) fn chosen(&self, kind: TrackKind) -> Option<usize> {
        if kind == TrackKind::Audio {
            self.audio
        } else {
            self.subs
        }
    }

    pub(super) fn dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Whether the controls are up: paused, at the end, in a panel, or the
    /// mouse moved lately.
    pub(super) fn controls_shown(&self) -> bool {
        self.paused()
            || self.ended
            || self.panel.is_some()
            || self.dragging()
            || self.active.elapsed() < Duration::from_millis(2500)
    }
}
