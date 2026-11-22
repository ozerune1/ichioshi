//! The built-in video player: libmpv, set up for anime. It draws into a
//! texture the app shows (see `video`), and the app draws the controls.

pub mod ffi;
pub mod inhibit;
pub mod video;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// The player's settings, saved with the app's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Smooth out color banding, common in anime's flat colors and gradients.
    pub deband: bool,
    pub skip_openings: bool,
    pub skip_endings: bool,
    /// Go on to the next episode when one ends.
    pub autoplay: bool,
    pub volume: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            deband: true,
            skip_openings: false,
            skip_endings: false,
            autoplay: true,
            volume: 100.0,
        }
    }
}

/// Where the picture goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Into the app's window (OpenGL, see `video::Video`).
    Window,
    /// Nowhere, and no sound either: tests.
    Headless,
    /// Into a texture, without sound: tests.
    #[cfg(test)]
    SilentWindow,
}

/// mpv's options: high-quality scaling, debanding, and subtitles shown the
/// way fansubbers typeset them.
fn options(output: Output, settings: &Settings) -> Vec<(&'static str, String)> {
    let yes_no = |b: bool| if b { "yes" } else { "no" }.to_owned();
    let mut o: Vec<(&'static str, String)> = vec![
        // Nothing of the user's mpv setup (config, scripts) gets in.
        ("config", "no".into()),
        ("load-scripts", "no".into()),
        ("ytdl", "no".into()),
        ("osc", "no".into()),
        ("input-default-bindings", "no".into()),
        ("input-vo-keyboard", "no".into()),
        ("terminal", "no".into()),
        // The app draws its own controls and messages.
        ("osd-level", "0".into()),
        ("idle", "yes".into()),
        // Stay on the last frame at the end, so the app can go on to the next.
        ("keep-open", "yes".into()),
        ("hwdec", "auto-safe".into()),
        // Sharp, ringing-free upscaling (lines are what anime is made of).
        ("profile", "high-quality".into()),
        ("scale", "ewa_lanczossharp".into()),
        ("cscale", "ewa_lanczossharp".into()),
        ("dscale", "mitchell".into()),
        ("dither-depth", "auto".into()),
        ("deband", yes_no(settings.deband)),
        ("deband-iterations", "4".into()),
        ("deband-threshold", "35".into()),
        ("deband-range", "16".into()),
        ("deband-grain", "4".into()),
        // Subtitles: styled ASS as authored, fonts from the file, shown
        // right after a seek, and the default track kept even when it's in
        // the audio's language (signs & songs for dubs).
        ("embeddedfonts", "yes".into()),
        ("sub-ass-override", "scale".into()),
        ("sub-fix-timing", "no".into()),
        ("demuxer-mkv-subtitle-preroll", "yes".into()),
        ("subs-with-matching-audio", "yes".into()),
        ("sub-auto", "fuzzy".into()),
        (
            "volume",
            format!("{:.0}", settings.volume.clamp(0.0, 150.0)),
        ),
        ("volume-max", "150".into()),
        ("audio-client-name", "SeaDex Downloader".into()),
    ];
    match output {
        Output::Window => o.push(("vo", "libmpv".into())),
        Output::Headless => {
            o.push(("vo", "null".into()));
            o.push(("ao", "null".into()));
        }
        #[cfg(test)]
        Output::SilentWindow => {
            o.push(("vo", "libmpv".into()));
            o.push(("ao", "null".into()));
        }
    }
    o
}

/// One libmpv instance. libmpv's client API is thread-safe.
pub struct Mpv {
    api: &'static ffi::Api,
    handle: *mut ffi::Handle,
    wakeup: Option<Box<Wake>>,
}

type Wake = Box<dyn Fn() + Send + Sync>;

// SAFETY: every libmpv client function may be called from any thread.
unsafe impl Send for Mpv {}
// SAFETY: as above.
unsafe impl Sync for Mpv {}

fn c(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("no NULs left")
}

/// What happened in mpv.
#[derive(Debug, PartialEq)]
pub enum MpvEvent {
    StartFile,
    FileLoaded,
    EndFile {
        reason: EndReason,
        error: Option<String>,
    },
    PlaybackRestart,
    Property {
        name: String,
        value: Value,
    },
    Shutdown,
    Other,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Error,
    Other,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Value {
    None,
    Flag(bool),
    Int(i64),
    Double(f64),
    Str(String),
}

impl Mpv {
    pub fn new(options: &[(&str, String)]) -> Result<Self> {
        let api = ffi::api().map_err(|e| anyhow!("{e}"))?;
        if api.major_version() != 2 {
            bail!(
                "this libmpv (API {}) is too old or too new",
                api.major_version()
            );
        }
        // SAFETY: no preconditions.
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            bail!("libmpv couldn't start");
        }
        let mpv = Mpv {
            api,
            handle,
            wakeup: None,
        };
        for (name, value) in options {
            // SAFETY: valid handle and NUL-terminated strings.
            let r = unsafe { (api.set_option_string)(handle, c(name).as_ptr(), c(value).as_ptr()) };
            if r < 0 {
                tracing::warn!("mpv option {name}={value}: {}", api.error(r));
            }
        }
        // SAFETY: valid, not yet initialized handle.
        let r = unsafe { (api.initialize)(handle) };
        if r < 0 {
            bail!("libmpv couldn't start: {}", api.error(r));
        }
        Ok(mpv)
    }

    pub(crate) fn raw(&self) -> *mut ffi::Handle {
        self.handle
    }

    pub(crate) fn api(&self) -> &'static ffi::Api {
        self.api
    }

    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| c(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned
            .iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        // SAFETY: a NULL-terminated array of valid strings, alive for the call.
        let r = unsafe { (self.api.command)(self.handle, ptrs.as_mut_ptr()) };
        if r < 0 {
            bail!("{}: {}", args.join(" "), self.api.error(r));
        }
        Ok(())
    }

    pub fn set(&self, name: &str, value: &str) -> Result<()> {
        // SAFETY: valid handle and strings.
        let r = unsafe {
            (self.api.set_property_string)(self.handle, c(name).as_ptr(), c(value).as_ptr())
        };
        if r < 0 {
            bail!("{name}={value}: {}", self.api.error(r));
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<String> {
        // SAFETY: valid handle and string; the result is freed with mpv_free.
        unsafe {
            let p = (self.api.get_property_string)(self.handle, c(name).as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.api.free)(p.cast());
            Some(s)
        }
    }

    fn get_raw<T: Default>(&self, name: &str, format: c_int) -> Option<T> {
        let mut v = T::default();
        // SAFETY: `format` matches T (double, int64 or flag as int).
        let r = unsafe {
            (self.api.get_property)(
                self.handle,
                c(name).as_ptr(),
                format,
                (&raw mut v).cast::<c_void>(),
            )
        };
        (r >= 0).then_some(v)
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.get_raw(name, ffi::FORMAT_DOUBLE)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        self.get_raw(name, ffi::FORMAT_INT64)
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        self.get_raw::<c_int>(name, ffi::FORMAT_FLAG)
            .map(|v| v != 0)
    }

    pub fn observe(&self, name: &str, format: c_int) {
        // SAFETY: valid handle and string.
        let r = unsafe { (self.api.observe_property)(self.handle, 0, c(name).as_ptr(), format) };
        if r < 0 {
            tracing::warn!("observing {name}: {}", self.api.error(r));
        }
    }

    /// Call `f` (from any thread) when there are events to read. Set once.
    pub fn on_wakeup(&mut self, f: impl Fn() + Send + Sync + 'static) {
        let boxed: Box<Wake> = Box::new(Box::new(f));
        let data = (&raw const *boxed).cast_mut().cast::<c_void>();
        // SAFETY: `data` points at the boxed closure, kept until mpv is destroyed.
        unsafe { (self.api.set_wakeup_callback)(self.handle, Some(call_wake), data) };
        self.wakeup = Some(boxed);
    }

    /// The next event, without waiting.
    pub fn next_event(&self) -> Option<MpvEvent> {
        // SAFETY: mpv_wait_event returns a valid event, alive until the next
        // call on this handle; its data matches its id.
        unsafe {
            let ev = &*(self.api.wait_event)(self.handle, 0.0);
            Some(match ev.event_id {
                ffi::EVENT_NONE => return None,
                ffi::EVENT_SHUTDOWN => MpvEvent::Shutdown,
                ffi::EVENT_START_FILE => MpvEvent::StartFile,
                ffi::EVENT_FILE_LOADED => MpvEvent::FileLoaded,
                ffi::EVENT_PLAYBACK_RESTART => MpvEvent::PlaybackRestart,
                ffi::EVENT_END_FILE => {
                    let d = &*ev.data.cast::<ffi::EventEndFile>();
                    let reason = match d.reason {
                        ffi::END_FILE_EOF => EndReason::Eof,
                        ffi::END_FILE_STOP => EndReason::Stop,
                        ffi::END_FILE_QUIT => EndReason::Quit,
                        ffi::END_FILE_ERROR => EndReason::Error,
                        _ => EndReason::Other,
                    };
                    MpvEvent::EndFile {
                        reason,
                        error: (d.error < 0).then(|| self.api.error(d.error)),
                    }
                }
                ffi::EVENT_PROPERTY_CHANGE => {
                    let p = &*ev.data.cast::<ffi::EventProperty>();
                    let name = CStr::from_ptr(p.name).to_string_lossy().into_owned();
                    let value = if p.data.is_null() {
                        Value::None
                    } else {
                        match p.format {
                            ffi::FORMAT_FLAG => Value::Flag(*p.data.cast::<c_int>() != 0),
                            ffi::FORMAT_INT64 => Value::Int(*p.data.cast::<i64>()),
                            ffi::FORMAT_DOUBLE => Value::Double(*p.data.cast::<f64>()),
                            ffi::FORMAT_STRING => Value::Str(
                                CStr::from_ptr(*p.data.cast::<*const c_char>())
                                    .to_string_lossy()
                                    .into_owned(),
                            ),
                            _ => Value::None,
                        }
                    };
                    MpvEvent::Property { name, value }
                }
                _ => MpvEvent::Other,
            })
        }
    }
}

unsafe extern "C" fn call_wake(data: *mut c_void) {
    // SAFETY: `data` is the boxed closure set in `on_wakeup`.
    let f = unsafe { &*data.cast::<Wake>() };
    f();
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // SAFETY: valid handle, not used again. The wakeup closure is
        // dropped after, once mpv can no longer call it.
        unsafe { (self.api.terminate_destroy)(self.handle) };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Sub,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub kind: TrackKind,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub codec: Option<String>,
    pub channels: Option<i64>,
    pub default: bool,
    pub forced: bool,
    pub external: bool,
    pub selected: bool,
}

impl Track {
    /// "Japanese · FLAC 2.0", "English · Signs & Songs".
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        match self.lang.as_deref() {
            Some(code) if !code.is_empty() && code != "und" => {
                parts.push(crate::remux::language_name(code))
            }
            _ => {}
        }
        if let Some(t) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            // Titles often repeat the language ("English (Full)").
            if parts.first().is_none_or(|l| t.trim() != l) {
                parts.push(t.trim().to_owned());
            }
        }
        if self.kind == TrackKind::Audio || parts.is_empty() {
            let codec = self.codec.as_deref().map(codec_name).unwrap_or_default();
            let ch = match self.channels {
                Some(1) => " 1.0",
                Some(2) => " 2.0",
                Some(6) => " 5.1",
                Some(8) => " 7.1",
                _ => "",
            };
            if !codec.is_empty() {
                parts.push(format!("{codec}{ch}"));
            }
        }
        if self.forced {
            parts.push("forced".into());
        }
        if self.external {
            parts.push("external".into());
        }
        if parts.is_empty() {
            parts.push(format!("Track {}", self.id));
        }
        parts.join(" · ")
    }
}

fn codec_name(codec: &str) -> String {
    match codec {
        "flac" => "FLAC",
        "aac" => "AAC",
        "opus" => "Opus",
        "ac3" => "AC-3",
        "eac3" => "E-AC-3",
        "truehd" => "TrueHD",
        "dts" => "DTS",
        "mp3" => "MP3",
        "vorbis" => "Vorbis",
        "pcm_s16le" | "pcm_s24le" => "PCM",
        "ass" | "ssa" => "ASS",
        "subrip" => "SRT",
        "hdmv_pgs_subtitle" => "PGS",
        "dvd_subtitle" => "VobSub",
        other => return other.to_ascii_uppercase(),
    }
    .to_owned()
}

/// A part of an episode, by its chapter name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Opening,
    Ending,
    Preview,
    Other,
}

/// "Opening", "OP", "Intro", "OP1"; "Ending", "ED", "Credits"; "Preview".
pub fn part_of(title: &str) -> Part {
    static OP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(op\s*\d*|opening(\s+(song|theme|credits))?|intro)\s*\d*\s*$").unwrap()
    });
    static ED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(ed\s*\d*|ending(\s+(song|theme|credits))?|outro|credits|end\s+credits)\s*\d*\s*$").unwrap()
    });
    static PREVIEW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bpreview\b").unwrap());
    if OP.is_match(title) {
        Part::Opening
    } else if ED.is_match(title) {
        Part::Ending
    } else if PREVIEW.is_match(title) {
        Part::Preview
    } else {
        Part::Other
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub title: String,
    pub start: f64,
    pub part: Part,
}

/// Where playback is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub path: Option<PathBuf>,
    pub loaded: bool,
    pub pos: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub chapter: Option<usize>,
    pub eof: bool,
    /// The picture's size, once known.
    pub size: Option<(i64, i64)>,
}

/// Something the app should act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Happened {
    Loaded,
    /// Played to the end.
    Ended,
    Failed(String),
}

pub struct Player {
    // Dropped before `mpv`: the render context must go first.
    pub video: Option<video::Video>,
    mpv: Mpv,
    pub now: Status,
    pub tracks: Vec<Track>,
    pub chapters: Vec<Chapter>,
    pub output: Output,
}

impl Player {
    /// Start mpv; `wake` is called (from any thread) when there's news.
    pub fn new(
        output: Output,
        settings: &Settings,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        let mut mpv = Mpv::new(&options(output, settings))?;
        for (name, format) in [
            ("time-pos", ffi::FORMAT_DOUBLE),
            ("duration", ffi::FORMAT_DOUBLE),
            ("pause", ffi::FORMAT_FLAG),
            ("volume", ffi::FORMAT_DOUBLE),
            ("mute", ffi::FORMAT_FLAG),
            ("chapter", ffi::FORMAT_INT64),
            ("eof-reached", ffi::FORMAT_FLAG),
            ("track-list", ffi::FORMAT_NONE),
            ("chapter-list", ffi::FORMAT_NONE),
            ("dwidth", ffi::FORMAT_INT64),
            ("dheight", ffi::FORMAT_INT64),
        ] {
            mpv.observe(name, format);
        }
        mpv.on_wakeup(wake);
        let now = Status {
            volume: settings.volume,
            ..Status::default()
        };
        Ok(Self {
            video: None,
            mpv,
            now,
            tracks: Vec::new(),
            chapters: Vec::new(),
            output,
        })
    }

    pub fn mpv(&self) -> &Mpv {
        &self.mpv
    }

    /// Play `path`, from `start` seconds in.
    pub fn load(&mut self, path: &Path, start: Option<f64>) -> Result<()> {
        let start = start
            .filter(|s| *s > 0.0)
            .map_or("none".to_owned(), |s| format!("{s:.3}"));
        self.mpv.set("start", &start)?;
        self.mpv.set("pause", "no")?;
        self.mpv
            .command(&["loadfile", &path.to_string_lossy(), "replace"])?;
        self.now = Status {
            path: Some(path.to_owned()),
            volume: self.now.volume,
            muted: self.now.muted,
            ..Status::default()
        };
        self.tracks.clear();
        self.chapters.clear();
        Ok(())
    }

    pub fn stop(&mut self) {
        let _ = self.mpv.command(&["stop"]);
        self.now = Status {
            volume: self.now.volume,
            muted: self.now.muted,
            ..Status::default()
        };
        self.tracks.clear();
        self.chapters.clear();
    }

    /// Read mpv's news.
    pub fn poll(&mut self) -> Vec<Happened> {
        let mut out = Vec::new();
        while let Some(ev) = self.mpv.next_event() {
            match ev {
                MpvEvent::StartFile => {
                    self.now.loaded = false;
                    self.now.eof = false;
                }
                MpvEvent::FileLoaded => {
                    self.now.loaded = true;
                    // The start position was for this file only.
                    let _ = self.mpv.set("start", "none");
                    self.read_tracks();
                    self.read_chapters();
                    out.push(Happened::Loaded);
                }
                MpvEvent::EndFile {
                    reason: EndReason::Error,
                    error,
                } => {
                    self.now.loaded = false;
                    out.push(Happened::Failed(
                        error.unwrap_or_else(|| "it couldn't be played".into()),
                    ));
                }
                MpvEvent::Property { name, value } => match (name.as_str(), value) {
                    ("time-pos", Value::Double(v)) => self.now.pos = v,
                    ("duration", Value::Double(v)) => self.now.duration = v,
                    ("pause", Value::Flag(v)) => self.now.paused = v,
                    ("volume", Value::Double(v)) => self.now.volume = v,
                    ("mute", Value::Flag(v)) => self.now.muted = v,
                    ("chapter", Value::Int(v)) => self.now.chapter = usize::try_from(v).ok(),
                    ("chapter", _) => self.now.chapter = None,
                    ("eof-reached", Value::Flag(v)) => {
                        if v && !self.now.eof && self.now.loaded {
                            out.push(Happened::Ended);
                        }
                        self.now.eof = v;
                    }
                    ("track-list", _) => self.read_tracks(),
                    ("chapter-list", _) => self.read_chapters(),
                    ("dwidth" | "dheight", _) => {
                        self.now.size = self
                            .mpv
                            .get_i64("dwidth")
                            .zip(self.mpv.get_i64("dheight"))
                            .filter(|(w, h)| *w > 0 && *h > 0);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        out
    }

    fn read_tracks(&mut self) {
        let n = self.mpv.get_i64("track-list/count").unwrap_or(0);
        let m = &self.mpv;
        self.tracks = (0..n)
            .filter_map(|i| {
                let key = |k: &str| format!("track-list/{i}/{k}");
                let kind = match m.get(&key("type"))?.as_str() {
                    "video" => TrackKind::Video,
                    "audio" => TrackKind::Audio,
                    "sub" => TrackKind::Sub,
                    _ => return None,
                };
                Some(Track {
                    id: m.get_i64(&key("id"))?,
                    kind,
                    title: m.get(&key("title")),
                    lang: m.get(&key("lang")),
                    codec: m.get(&key("codec")),
                    channels: m.get_i64(&key("demux-channel-count")),
                    default: m.get_flag(&key("default")).unwrap_or(false),
                    forced: m.get_flag(&key("forced")).unwrap_or(false),
                    external: m.get_flag(&key("external")).unwrap_or(false),
                    selected: m.get_flag(&key("selected")).unwrap_or(false),
                })
            })
            .collect();
    }

    fn read_chapters(&mut self) {
        let n = self.mpv.get_i64("chapter-list/count").unwrap_or(0);
        self.chapters = (0..n)
            .map(|i| {
                let title = self
                    .mpv
                    .get(&format!("chapter-list/{i}/title"))
                    .unwrap_or_default();
                let start = self
                    .mpv
                    .get_f64(&format!("chapter-list/{i}/time"))
                    .unwrap_or(0.0);
                Chapter {
                    part: part_of(&title),
                    title,
                    start,
                }
            })
            .collect();
    }

    pub fn tracks(&self, kind: TrackKind) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(move |t| t.kind == kind)
    }

    pub fn selected(&self, kind: TrackKind) -> Option<&Track> {
        self.tracks(kind).find(|t| t.selected)
    }

    pub fn set_audio(&self, id: i64) {
        let _ = self.mpv.set("aid", &id.to_string());
    }

    pub fn set_subtitles(&self, id: Option<i64>) {
        let _ = self
            .mpv
            .set("sid", &id.map_or("no".to_owned(), |i| i.to_string()));
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.mpv.set("pause", if paused { "yes" } else { "no" });
    }

    pub fn toggle_pause(&self) {
        if self.now.eof {
            // At the end: from the top.
            self.seek_to(0.0);
            self.set_paused(false);
        } else {
            self.set_paused(!self.now.paused);
        }
    }

    pub fn seek_to(&self, secs: f64) {
        let _ = self
            .mpv
            .command(&["seek", &format!("{:.3}", secs.max(0.0)), "absolute+exact"]);
    }

    pub fn seek_by(&self, secs: f64) {
        let _ = self
            .mpv
            .command(&["seek", &format!("{secs:.3}"), "relative+exact"]);
    }

    pub fn set_volume(&self, volume: f64) {
        let _ = self
            .mpv
            .set("volume", &format!("{:.0}", volume.clamp(0.0, 150.0)));
    }

    pub fn set_muted(&self, muted: bool) {
        let _ = self.mpv.set("mute", if muted { "yes" } else { "no" });
    }

    pub fn set_deband(&self, on: bool) {
        let _ = self.mpv.set("deband", if on { "yes" } else { "no" });
    }

    /// The chapter playing, and when it ends.
    pub fn current_part(&self) -> Option<(&Chapter, f64)> {
        let i = self.now.chapter?;
        let ch = self.chapters.get(i)?;
        let end = self
            .chapters
            .get(i + 1)
            .map_or(self.now.duration, |n| n.start);
        Some((ch, end))
    }

    /// Past the opening (or ending): to the start of the next chapter.
    pub fn skip_part(&self) {
        if let Some((_, end)) = self.current_part() {
            self.seek_to(end);
        }
    }

    /// Has it been watched: played to the end, into the ending, or 90% through.
    pub fn watched_enough(&self) -> bool {
        let s = &self.now;
        if s.duration <= 0.0 {
            return false;
        }
        s.eof
            || s.pos >= s.duration * 0.9
            || self
                .chapters
                .iter()
                .any(|c| c.part == Part::Ending && c.start > s.duration * 0.5 && s.pos >= c.start)
    }
}

#[cfg(test)]
pub(crate) mod tests;
