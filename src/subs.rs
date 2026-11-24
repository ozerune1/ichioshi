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
        let mut names: Vec<std::ffi::OsString> =
            ["libass.so.9", "libass.so", "libass-9.dll", "libass.9.dylib"]
                .into_iter()
                .map(Into::into)
                .collect();
        // macOS: in the app bundle's GStreamer, else GStreamer.framework's
        // or Homebrew's (a bare name isn't looked for there).
        if cfg!(target_os = "macos") {
            let bundled = std::env::current_exe().ok().and_then(|e| {
                Some(
                    e.parent()?
                        .parent()?
                        .join("Frameworks/GStreamer/lib/libass.9.dylib"),
                )
            });
            names.splice(
                0..0,
                bundled.into_iter().map(Into::into).chain(
                    [
                        "/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/libass.9.dylib",
                        "/opt/homebrew/lib/libass.9.dylib",
                        "/usr/local/lib/libass.9.dylib",
                    ]
                    .into_iter()
                    .map(Into::into),
                ),
            );
        }
        for name in &names {
            let name = name.to_string_lossy();
            // SAFETY: loading libass runs its initializers only.
            match unsafe { Library::new(&*name) } {
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

/// The video's YCbCr matrix: subtitles made for VSFilter get their colours
/// adjusted to it, as mpv does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoMatrix {
    Bt601,
    Bt709,
    Other,
}

impl VideoMatrix {
    /// mpv's guess when the video doesn't say: HD is BT.709.
    pub fn guess((w, h): (u32, u32)) -> Self {
        if w >= 1280 || h > 576 {
            Self::Bt709
        } else {
            Self::Bt601
        }
    }
}

/// Whether VSFilter would show a script's colours shifted on this video:
/// it turns them into YCbCr by BT.601 (TV range), and BT.709 video turns
/// them back differently. Scripts written for that say "YCbCr Matrix:
/// TV.601", or nothing (older ones). mpv copies this (its
/// `sub-ass-vsfilter-color-compat=basic`), so typesetting matched to the
/// picture's colours matches there.
fn vsfilter_shifts(header: &[u8], matrix: VideoMatrix) -> bool {
    if matrix != VideoMatrix::Bt709 {
        return false;
    }
    let header = String::from_utf8_lossy(header);
    let declared = header.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("YCbCr Matrix")
            .then(|| v.trim().to_owned())
    });
    match declared.as_deref() {
        None | Some("") => true,
        Some(m) => m.eq_ignore_ascii_case("TV.601"),
    }
}

/// A libass colour (RGBA, A inverted) as VSFilter shows it on BT.709
/// video: to 8-bit YCbCr by BT.601, back to RGB by BT.709 (both TV range).
fn vsfilter_color(color: u32) -> u32 {
    let c = |shift: u32| f64::from(color >> shift & 0xff) / 255.0;
    let (r, g, b) = (c(24), c(16), c(8));
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let q = |v: f64, scale: f64, zero: f64| ((zero + scale * v).round() - zero) / scale;
    let (y, cb, cr) = (
        q(y, 219.0, 16.0),
        q((b - y) / 1.772, 224.0, 128.0),
        q((r - y) / 1.402, 224.0, 128.0),
    );
    let out = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    out(y + 1.5748 * cr) << 24
        | out(y - 0.187_324 * cb - 0.468_124 * cr) << 16
        | out(y + 1.8556 * cb) << 8
        | (color & 0xff)
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
    /// Colours shifted as VSFilter shows them ([`vsfilter_shifts`]).
    shift_colors: bool,
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
                shift_colors: false,
            })
        }
    }

    /// Show `track` (or nothing); `video` is the picture's own size and
    /// `matrix` its YCbCr matrix.
    pub fn set_track(&mut self, track: Option<&SubTrack>, video: (u32, u32), matrix: VideoMatrix) {
        // SAFETY: as above; the old track isn't used again.
        unsafe {
            if !self.track.is_null() {
                (self.api.free_track)(self.track);
                self.track = std::ptr::null_mut();
            }
            self.size = (0, 0);
            self.shift_colors = false;
            let Some(t) = track else { return };
            let track = (self.api.new_track)(self.lib);
            if track.is_null() {
                return;
            }
            let ass = matches!(
                t.codec_id.as_str(),
                "S_TEXT/ASS" | "S_TEXT/SSA" | "S_ASS" | "S_SSA"
            );
            self.shift_colors = ass && vsfilter_shifts(&t.codec_private, matrix);
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
                let color = if self.shift_colors {
                    vsfilter_color(i.color)
                } else {
                    i.color
                };
                blend(&mut rgba, (w, h), i, color);
                img = i.next;
            }
            self.showing = true;
            Drawn::Image(rgba)
        }
    }
}

/// One libass bitmap, in `color`, over the image (straight alpha, "over").
unsafe fn blend(rgba: &mut [u8], (fw, fh): (i32, i32), i: &AssImage, color: u32) {
    let (r, g, b) = (color >> 24, color >> 16 & 0xff, color >> 8 & 0xff);
    let opacity = 255 - (color & 0xff);
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
            let sa = (k * opacity + 127) / 255;
            if sa == 0 {
                continue;
            }
            let at = ((fy * fw + fx) * 4) as usize;
            let px = &mut rgba[at..at + 4];
            // What shows through, and the result's alpha, both ×255: the
            // colour is their weighted average, so it can't pass 255 (it
            // used to, and wrapped to black where faint layers overlap).
            let under = px[3] as u32 * (255 - sa);
            let total = sa * 255 + under;
            for (c, s) in [r, g, b].into_iter().enumerate() {
                px[c] = ((s * sa * 255 + px[c] as u32 * under + total / 2) / total) as u8;
            }
            px[3] = ((total + 127) / 255) as u8;
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
        r.set_track(Some(track), (1920, 1080), VideoMatrix::Bt709);
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

    /// Faint white layers over each other (a fading line's shadow, outline
    /// and fill) blend to light grey: the colour used to pass 255 and wrap
    /// to black.
    #[test]
    fn faint_layers_stay_light() {
        let bitmap = [255u8; 4];
        let mut rgba = vec![0u8; 4 * 4];
        for color in [0xf3f3_f3fb_u32, 0xf3f3_f3f3, 0xffff_fff3] {
            let img = AssImage {
                w: 2,
                h: 2,
                stride: 2,
                bitmap: bitmap.as_ptr(),
                color,
                dst_x: 0,
                dst_y: 0,
                next: std::ptr::null(),
                kind: 0,
            };
            // SAFETY: the bitmap is w × h at stride.
            unsafe { blend(&mut rgba, (2, 2), &img, color) };
        }
        for p in rgba.chunks(4) {
            assert!(p[0] >= 240 && (24..=30).contains(&p[3]), "{p:?}");
        }
    }

    /// A TV.601 script's colours on BT.709 video come out as mpv shows them
    /// (measured: &H00C04080& as 132, 78, 197); greys don't move; scripts
    /// saying TV.709 or None, and BT.601 video, are left alone.
    #[test]
    fn colors_shift_as_vsfilter_shows_them() {
        assert_eq!(vsfilter_color(0x8040_c000), 0x844e_c500);
        assert_eq!(vsfilter_color(0x8040_c07f) & 0xff, 0x7f, "alpha kept");
        for grey in [0, 0x80, 0xff] {
            let c = grey << 24 | grey << 16 | grey << 8;
            assert_eq!(vsfilter_color(c), c);
        }
        let h = |m: &str| format!("[Script Info]\nScriptType: v4.00+\n{m}\n").into_bytes();
        let hd = VideoMatrix::Bt709;
        assert!(vsfilter_shifts(&h("YCbCr Matrix: TV.601"), hd));
        assert!(vsfilter_shifts(&h(""), hd), "VSFilter-era: says nothing");
        assert!(!vsfilter_shifts(&h("YCbCr Matrix: TV.709"), hd));
        assert!(!vsfilter_shifts(&h("YCbCr Matrix: None"), hd));
        assert!(!vsfilter_shifts(&h("YCbCr Matrix: PC.601"), hd));
        assert!(!vsfilter_shifts(
            &h("YCbCr Matrix: TV.601"),
            VideoMatrix::Bt601
        ));
        assert_eq!(VideoMatrix::guess((1920, 1080)), hd);
        assert_eq!(VideoMatrix::guess((720, 480)), VideoMatrix::Bt601);
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
        r.set_track(Some(&track), (1920, 1080), VideoMatrix::Bt709);
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
