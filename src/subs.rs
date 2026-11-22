//! Subtitles drawn by libass (loaded at run time), the way mpv does it:
//! from the file's own lines and fonts, at the size the picture is shown,
//! for whatever moment is on screen. Seeking can't lose a line, and a
//! delay is just a different moment.

use std::ffi::{CString, c_char, c_int, c_longlong};
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use libloading::Library;

use crate::mkv::{SubTrack, TextSubtitles};

enum AssLibrary {}
enum AssRenderer {}
enum AssTrack {}

#[repr(C)]
struct AssImage {
    w: c_int,
    h: c_int,
    stride: c_int,
    bitmap: *const u8,
    color: u32,
    dst_x: c_int,
    dst_y: c_int,
    next: *const AssImage,
    kind: c_int,
}

struct Api {
    _lib: Library,
    library_init: unsafe extern "C" fn() -> *mut AssLibrary,
    library_done: unsafe extern "C" fn(*mut AssLibrary),
    add_font: unsafe extern "C" fn(*mut AssLibrary, *const c_char, *const c_char, c_int),
    renderer_init: unsafe extern "C" fn(*mut AssLibrary) -> *mut AssRenderer,
    renderer_done: unsafe extern "C" fn(*mut AssRenderer),
    set_frame_size: unsafe extern "C" fn(*mut AssRenderer, c_int, c_int),
    set_storage_size: unsafe extern "C" fn(*mut AssRenderer, c_int, c_int),
    set_line_position: unsafe extern "C" fn(*mut AssRenderer, f64),
    set_fonts: unsafe extern "C" fn(
        *mut AssRenderer,
        *const c_char,
        *const c_char,
        c_int,
        *const c_char,
        c_int,
    ),
    new_track: unsafe extern "C" fn(*mut AssLibrary) -> *mut AssTrack,
    free_track: unsafe extern "C" fn(*mut AssTrack),
    process_codec_private: unsafe extern "C" fn(*mut AssTrack, *const c_char, c_int),
    process_chunk:
        unsafe extern "C" fn(*mut AssTrack, *const c_char, c_int, c_longlong, c_longlong),
    render_frame: unsafe extern "C" fn(
        *mut AssRenderer,
        *mut AssTrack,
        c_longlong,
        *mut c_int,
    ) -> *const AssImage,
}

fn api() -> Result<&'static Api> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut last = String::new();
        for name in ["libass.so.9", "libass.so", "libass-9.dll", "libass.9.dylib"] {
            // SAFETY: loading libass runs its initializers only.
            match unsafe { Library::new(name) } {
                Ok(lib) => return bind(lib).map_err(|e| format!("{name}: {e}")),
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!("libass isn't installed ({last})"))
    })
    .as_ref()
    .map_err(|e| anyhow!("{e}"))
}

fn bind(lib: Library) -> Result<Api, libloading::Error> {
    macro_rules! sym {
        ($name:literal) => {{
            // SAFETY: the types match libass's ass.h (ABI 9).
            let symbol: libloading::Symbol<_> =
                unsafe { lib.get(concat!($name, "\0").as_bytes())? };
            *symbol
        }};
    }
    Ok(Api {
        library_init: sym!("ass_library_init"),
        library_done: sym!("ass_library_done"),
        add_font: sym!("ass_add_font"),
        renderer_init: sym!("ass_renderer_init"),
        renderer_done: sym!("ass_renderer_done"),
        set_frame_size: sym!("ass_set_frame_size"),
        set_storage_size: sym!("ass_set_storage_size"),
        set_line_position: sym!("ass_set_line_position"),
        set_fonts: sym!("ass_set_fonts"),
        new_track: sym!("ass_new_track"),
        free_track: sym!("ass_free_track"),
        process_codec_private: sym!("ass_process_codec_private"),
        process_chunk: sym!("ass_process_chunk"),
        render_frame: sym!("ass_render_frame"),
        _lib: lib,
    })
}

pub fn available() -> bool {
    api().is_ok()
}

/// The style plain-text subtitles (SRT, WebVTT) get: white, bold, a soft
/// outline and shadow, near the bottom.
const PLAIN_HEADER: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nScaledBorderAndShadow: yes\nWrapStyle: 0\n\n\
[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Inter,62,&H00FFFFFF,&H000000FF,&H00141414,&H8C000000,-1,0,0,0,100,100,0,0,1,3.4,1.4,2,160,160,66,1\n\n\
[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

/// SRT/WebVTT text as an ASS line: line breaks, italic, bold, underline.
pub fn plain_to_ass(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text.trim_matches(['\r', '\n']);
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let tag = rest[i + 1..i + j].trim().to_ascii_lowercase();
        out.push_str(match tag.as_str() {
            "i" => "{\\i1}",
            "/i" => "{\\i0}",
            "b" => "{\\b1}",
            "/b" => "{\\b0}",
            "u" => "{\\u1}",
            "/u" => "{\\u0}",
            _ => "",
        });
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out.replace("\r\n", "\\N")
        .replace('\n', "\\N")
        .replace('{', "(")
        .replace("(\\", "{\\")
}

pub enum Drawn {
    /// As last time.
    Unchanged,
    /// Nothing on screen.
    Empty,
    /// RGBA (straight alpha), the size asked for.
    Image(Vec<u8>),
}

pub struct Renderer {
    api: &'static Api,
    lib: *mut AssLibrary,
    renderer: *mut AssRenderer,
    track: *mut AssTrack,
    size: (i32, i32),
    raise: f64,
    showing: bool,
}

// SAFETY: used from one thread at a time (the UI's); libass objects aren't
// shared.
unsafe impl Send for Renderer {}

impl Renderer {
    /// A renderer knowing the file's fonts (and the system's).
    pub fn new(subs: &TextSubtitles) -> Result<Self> {
        let api = api()?;
        // SAFETY: plain libass calls on objects made here.
        unsafe {
            let lib = (api.library_init)();
            if lib.is_null() {
                bail!("libass couldn't start");
            }
            for (name, data) in &subs.fonts {
                let name = CString::new(name.replace('\0', "")).unwrap_or_default();
                (api.add_font)(
                    lib,
                    name.as_ptr(),
                    data.as_ptr().cast(),
                    data.len() as c_int,
                );
            }
            let renderer = (api.renderer_init)(lib);
            if renderer.is_null() {
                (api.library_done)(lib);
                bail!("libass couldn't start");
            }
            (api.set_fonts)(
                renderer,
                std::ptr::null(),
                c"sans-serif".as_ptr(),
                1,
                std::ptr::null(),
                1,
            );
            Ok(Self {
                api,
                lib,
                renderer,
                track: std::ptr::null_mut(),
                size: (0, 0),
                raise: 0.0,
                showing: false,
            })
        }
    }

    /// Show `track` (or nothing); `video` is the picture's own size.
    pub fn set_track(&mut self, track: Option<&SubTrack>, video: (u32, u32)) {
        // SAFETY: as above; the old track isn't used again.
        unsafe {
            if !self.track.is_null() {
                (self.api.free_track)(self.track);
                self.track = std::ptr::null_mut();
            }
            self.size = (0, 0);
            let Some(t) = track else { return };
            let track = (self.api.new_track)(self.lib);
            if track.is_null() {
                return;
            }
            let ass = matches!(
                t.codec_id.as_str(),
                "S_TEXT/ASS" | "S_TEXT/SSA" | "S_ASS" | "S_SSA"
            );
            let header = if ass {
                t.codec_private.clone()
            } else {
                PLAIN_HEADER.as_bytes().to_vec()
            };
            (self.api.process_codec_private)(track, header.as_ptr().cast(), header.len() as c_int);
            for (i, e) in t.events.iter().enumerate() {
                let chunk = if ass {
                    e.data.clone()
                } else {
                    format!(
                        "{i},0,Default,,0,0,0,,{}",
                        plain_to_ass(&String::from_utf8_lossy(&e.data))
                    )
                    .into_bytes()
                };
                (self.api.process_chunk)(
                    track,
                    chunk.as_ptr().cast(),
                    chunk.len() as c_int,
                    e.start_ms,
                    e.duration_ms,
                );
            }
            if video.0 > 0 && video.1 > 0 {
                (self.api.set_storage_size)(self.renderer, video.0 as c_int, video.1 as c_int);
            }
            self.track = track;
        }
    }

    /// The subtitles at `now_ms`, drawn at `size` pixels, with dialogue
    /// raised by `raise` percent of the height (signs placed by the
    /// subtitles themselves stay put).
    pub fn draw(&mut self, now_ms: i64, size: (u32, u32), raise: f64) -> Drawn {
        if self.track.is_null() || size.0 == 0 || size.1 == 0 {
            return if std::mem::take(&mut self.showing) {
                Drawn::Empty
            } else {
                Drawn::Unchanged
            };
        }
        let (w, h) = (size.0 as i32, size.1 as i32);
        let resized = self.size != (w, h) || self.raise != raise;
        let mut changed: c_int = 0;
        // SAFETY: valid renderer and track; the images live until the next
        // render call, and are only read here.
        unsafe {
            if resized {
                (self.api.set_frame_size)(self.renderer, w, h);
                (self.api.set_line_position)(self.renderer, raise.clamp(0.0, 100.0));
                self.size = (w, h);
                self.raise = raise;
            }
            let mut img = (self.api.render_frame)(self.renderer, self.track, now_ms, &mut changed);
            if changed == 0 && !resized {
                return Drawn::Unchanged;
            }
            if img.is_null() {
                self.showing = false;
                return Drawn::Empty;
            }
            let mut rgba = vec![0u8; size.0 as usize * size.1 as usize * 4];
            while let Some(i) = img.as_ref() {
                blend(&mut rgba, (w, h), i);
                img = i.next;
            }
            self.showing = true;
            Drawn::Image(rgba)
        }
    }
}

/// One libass bitmap over the image (straight alpha, "over").
unsafe fn blend(rgba: &mut [u8], (fw, fh): (i32, i32), i: &AssImage) {
    let (r, g, b) = (i.color >> 24, i.color >> 16 & 0xff, i.color >> 8 & 0xff);
    let opacity = 255 - (i.color & 0xff);
    if opacity == 0 || i.bitmap.is_null() {
        return;
    }
    for y in 0..i.h {
        let fy = i.dst_y + y;
        if fy < 0 || fy >= fh {
            continue;
        }
        for x in 0..i.w {
            let fx = i.dst_x + x;
            if fx < 0 || fx >= fw {
                continue;
            }
            // SAFETY: within the bitmap libass gave (w × h at stride).
            let k = unsafe { *i.bitmap.add((y * i.stride + x) as usize) } as u32;
            let sa = k * opacity / 255;
            if sa == 0 {
                continue;
            }
            let at = ((fy * fw + fx) * 4) as usize;
            let px = &mut rgba[at..at + 4];
            let da = px[3] as u32;
            let oa = sa + da * (255 - sa) / 255;
            for (c, s) in [r, g, b].into_iter().enumerate() {
                px[c] = ((s * sa + px[c] as u32 * da * (255 - sa) / 255) / oa.max(1)) as u8;
            }
            px[3] = oa as u8;
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: made here, not used again.
        unsafe {
            if !self.track.is_null() {
                (self.api.free_track)(self.track);
            }
            (self.api.renderer_done)(self.renderer);
            (self.api.library_done)(self.lib);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mkv::SubEvent;

    /// `PLAYER_SAMPLE=<episode>`: read its text subtitles and draw the
    /// English ones at `PLAYER_AT` (default 122.5 s), to `$COMPARE_DIR`.
    #[test]
    #[ignore = "needs PLAYER_SAMPLE"]
    fn draws_a_real_files_subtitles() {
        let Some(file) = std::env::var_os("PLAYER_SAMPLE") else {
            return;
        };
        let at: f64 = std::env::var("PLAYER_AT")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(122.5);
        let started = std::time::Instant::now();
        let subs = crate::mkv::text_subtitles(std::path::Path::new(&file)).unwrap();
        let info = crate::mkv::probe(std::path::Path::new(&file)).unwrap();
        let english = info
            .tracks
            .iter()
            .find(|t| t.kind == crate::mkv::TrackKind::Subtitles && t.default)
            .or_else(|| {
                info.tracks
                    .iter()
                    .find(|t| t.kind == crate::mkv::TrackKind::Subtitles)
            })
            .unwrap();
        let track = &subs.tracks[&english.number];
        println!(
            "{} text tracks, {} fonts, read in {:?}; track {} ({}): {} lines, first at {} ms",
            subs.tracks.len(),
            subs.fonts.len(),
            started.elapsed(),
            english.number,
            english.language,
            track.events.len(),
            track.events.first().map_or(-1, |e| e.start_ms)
        );
        let mut r = Renderer::new(&subs).unwrap();
        r.set_track(Some(track), (1920, 1080));
        let Drawn::Image(px) = r.draw((at * 1000.0) as i64, (1920, 1080), 0.0) else {
            panic!("nothing at {at}")
        };
        if let Some(dir) = std::env::var_os("COMPARE_DIR") {
            image::RgbaImage::from_raw(1920, 1080, px)
                .unwrap()
                .save(std::path::Path::new(&dir).join("libass-real.png"))
                .unwrap();
        }
    }

    #[test]
    fn plain_text_becomes_ass() {
        assert_eq!(plain_to_ass("Hello\nthere"), "Hello\\Nthere");
        assert_eq!(
            plain_to_ass("<i>so</i> <font color=\"red\">red</font>"),
            "{\\i1}so{\\i0} red"
        );
        assert_eq!(plain_to_ass("a {b}"), "a (b}");
    }

    /// A line drawn from its start to its end, whatever came before (no
    /// seeking to lose it); nothing outside it.
    #[test]
    fn draws_lines_at_their_times() {
        if !available() {
            eprintln!("skipping: no libass");
            return;
        }
        let track = SubTrack {
            codec_id: "S_TEXT/UTF8".into(),
            codec_private: Vec::new(),
            events: vec![SubEvent {
                start_ms: 2000,
                duration_ms: 4000,
                data: b"HELLO <i>THERE</i>".to_vec(),
            }],
        };
        let mut r = Renderer::new(&TextSubtitles::default()).unwrap();
        r.set_track(Some(&track), (1920, 1080));
        let lit = |d: Drawn| match d {
            Drawn::Image(px) => px.chunks(4).filter(|p| p[3] > 128).count(),
            _ => 0,
        };
        assert_eq!(lit(r.draw(1000, (1280, 720), 0.0)), 0, "before");
        let at = lit(r.draw(3500, (1280, 720), 0.0));
        assert!(at > 500, "in the middle, straight away: {at}");
        assert!(matches!(r.draw(3500, (1280, 720), 0.0), Drawn::Unchanged));
        assert!(
            matches!(r.draw(6500, (1280, 720), 0.0), Drawn::Empty),
            "after"
        );
        // Raised by 10% of the height (a little less: libass measures it
        // inside the margins), the same size.
        let rows = |d: Drawn| match d {
            Drawn::Image(px) => {
                let lit: Vec<usize> = px
                    .chunks(1280 * 4)
                    .enumerate()
                    .filter(|(_, row)| row.chunks(4).any(|p| p[3] > 128))
                    .map(|(y, _)| y)
                    .collect();
                (lit[0], lit[lit.len() - 1])
            }
            _ => panic!("nothing drawn"),
        };
        let low = rows(r.draw(3000, (1280, 720), 0.0));
        let high = rows(r.draw(3000, (1280, 720), 10.0));
        let (moved, taller) = (low.1 - high.1, (low.1 - low.0).abs_diff(high.1 - high.0));
        assert!(
            (60..=72).contains(&moved) && taller <= 1,
            "{low:?} → {high:?}"
        );
    }
}
