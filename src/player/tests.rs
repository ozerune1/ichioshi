use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

/// A 6-second episode made with ffmpeg: red over blue, Japanese (default)
/// and English audio, full (default) and signs & songs subtitles, and
/// chapters Opening 0–2 s, Part A 2–4 s, Ending 4–6 s. None without ffmpeg.
pub(crate) fn sample_episode(dir: &Path, name: &str) -> Option<PathBuf> {
    let ass = |text: &str| {
        format!(
            "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n[V4+ Styles]\n\
             Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
             Style: Default,Arial,16,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n\n\
             [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:00.50,0:00:05.50,Default,,0,0,0,,{text}\n"
        )
    };
    std::fs::create_dir_all(dir).unwrap();
    let work = tempfile::tempdir_in(dir).unwrap();
    let full = work.path().join("full.ass");
    let signs = work.path().join("signs.ass");
    let chapters = work.path().join("chapters.txt");
    std::fs::write(&full, ass("Hello")).unwrap();
    std::fs::write(&signs, ass("{\\pos(160,20)}SIGN")).unwrap();
    let mut meta = String::from(";FFMETADATA1\n");
    for (start, title) in [(0, "Opening"), (2000, "Part A"), (4000, "Ending")] {
        meta.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={start}\nEND={}\ntitle={title}\n",
            start + 2000
        ));
    }
    std::fs::write(&chapters, meta).unwrap();
    let out = dir.join(name);
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args(["-f", "lavfi", "-i", "color=c=red:s=320x120:d=6:r=24"])
        .args(["-f", "lavfi", "-i", "color=c=blue:s=320x120:d=6:r=24"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=6",
            "-f",
            "lavfi",
            "-i",
            "sine=f=660:d=6",
        ])
        .arg("-i")
        .arg(&full)
        .arg("-i")
        .arg(&signs)
        .arg("-i")
        .arg(&chapters)
        .args([
            "-filter_complex",
            "[0:v][1:v]vstack[v]",
            "-map",
            "[v]",
            "-map",
            "2:a",
            "-map",
            "3:a",
            "-map",
            "4",
            "-map",
            "5",
        ])
        .args(["-map_metadata", "6", "-map_chapters", "6"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "flac",
            "-c:s",
            "ass",
        ])
        .args([
            "-metadata:s:a:0",
            "language=jpn",
            "-metadata:s:a:1",
            "language=eng",
            "-metadata:s:a:1",
            "title=English Dub",
        ])
        .args([
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:0",
            "title=Full Subtitles",
        ])
        .args([
            "-metadata:s:s:1",
            "language=eng",
            "-metadata:s:s:1",
            "title=Signs & Songs",
        ])
        .args([
            "-disposition:a:0",
            "default",
            "-disposition:a:1",
            "0",
            "-disposition:s:0",
            "default",
            "-disposition:s:1",
            "0",
        ])
        .arg(&out)
        .status();
    match status {
        Ok(s) if s.success() => Some(out),
        _ => {
            eprintln!("skipping: ffmpeg couldn't make a sample episode");
            None
        }
    }
}

/// libmpv is needed; tests skip without it.
fn have_mpv() -> bool {
    match ffi::api() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skipping: {e}");
            false
        }
    }
}

fn wait(
    player: &mut Player,
    what: &str,
    mut done: impl FnMut(&Player, &[Happened]) -> bool,
) -> Vec<Happened> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = Vec::new();
    loop {
        seen.extend(player.poll());
        if done(player, &seen) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {:?}",
            player.now
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn names_episode_parts_by_chapter() {
    for t in [
        "Opening",
        "OP",
        "OP1",
        "Intro",
        "opening song",
        "Opening Credits",
    ] {
        assert_eq!(part_of(t), Part::Opening, "{t}");
    }
    for t in [
        "Ending",
        "ED",
        "ED 2",
        "Credits",
        "Outro",
        "Ending Theme",
        "End Credits",
    ] {
        assert_eq!(part_of(t), Part::Ending, "{t}");
    }
    assert_eq!(part_of("Next Episode Preview"), Part::Preview);
    for t in [
        "Part A",
        "Chapter 01",
        "Prologue",
        "Episode",
        "Opening Scene",
        "Edward",
    ] {
        assert_eq!(part_of(t), Part::Other, "{t}");
    }
}

#[test]
fn labels_tracks() {
    let t = |kind, lang: Option<&str>, title: Option<&str>, codec: &str, channels| Track {
        id: 1,
        kind,
        title: title.map(Into::into),
        lang: lang.map(Into::into),
        codec: Some(codec.into()),
        channels,
        default: false,
        forced: false,
        external: false,
        selected: false,
    };
    assert_eq!(
        t(TrackKind::Audio, Some("jpn"), None, "flac", Some(2)).label(),
        "Japanese · FLAC 2.0"
    );
    assert_eq!(
        t(
            TrackKind::Audio,
            Some("eng"),
            Some("English"),
            "eac3",
            Some(6)
        )
        .label(),
        "English · E-AC-3 5.1"
    );
    assert_eq!(
        t(
            TrackKind::Sub,
            Some("eng"),
            Some("Signs & Songs"),
            "ass",
            None
        )
        .label(),
        "English · Signs & Songs"
    );
    assert_eq!(
        t(TrackKind::Sub, None, None, "hdmv_pgs_subtitle", None).label(),
        "PGS"
    );
}

#[test]
fn plays_an_episode() {
    if !have_mpv() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(file) = sample_episode(dir.path(), "Show - 01.mkv") else {
        return;
    };
    let mut player = Player::new(Output::Headless, &Settings::default(), || {}).unwrap();
    player.load(&file, None).unwrap();
    wait(&mut player, "the file", |_, h| {
        h.contains(&Happened::Loaded)
    });
    wait(&mut player, "tracks", |p, _| {
        p.tracks.len() == 5 && p.now.duration > 5.0
    });

    // The defaults: Japanese audio, full subtitles.
    assert_eq!(
        player.selected(TrackKind::Audio).unwrap().lang.as_deref(),
        Some("jpn")
    );
    assert_eq!(
        player.selected(TrackKind::Sub).unwrap().title.as_deref(),
        Some("Full Subtitles")
    );
    let audio: Vec<String> = player.tracks(TrackKind::Audio).map(Track::label).collect();
    assert_eq!(
        audio,
        ["Japanese · FLAC 1.0", "English · English Dub · FLAC 1.0"],
        "mono"
    );

    // Chapters: in the opening, which can be skipped.
    let parts: Vec<Part> = player.chapters.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Opening, Part::Other, Part::Ending]);
    wait(&mut player, "the opening", |p, _| {
        p.current_part()
            .is_some_and(|(c, _)| c.part == Part::Opening)
    });
    player.skip_part();
    wait(&mut player, "past the opening", |p, _| {
        p.now.pos >= 2.0 && p.current_part().is_some_and(|(c, _)| c.part == Part::Other)
    });
    assert!(!player.watched_enough());

    // Switching tracks.
    let english = player
        .tracks(TrackKind::Audio)
        .find(|t| t.lang.as_deref() == Some("eng"))
        .unwrap()
        .id;
    player.set_audio(english);
    let signs = player
        .tracks(TrackKind::Sub)
        .find(|t| t.title.as_deref() == Some("Signs & Songs"))
        .unwrap()
        .id;
    player.set_subtitles(Some(signs));
    wait(&mut player, "English audio, signs", |p, _| {
        p.selected(TrackKind::Audio)
            .is_some_and(|t| t.id == english)
            && p.selected(TrackKind::Sub).is_some_and(|t| t.id == signs)
    });
    player.set_subtitles(None);
    wait(&mut player, "no subtitles", |p, _| {
        p.selected(TrackKind::Sub).is_none()
    });

    // Pause, then on to the end.
    player.set_paused(true);
    wait(&mut player, "paused", |p, _| p.now.paused);
    player.seek_to(4.5);
    player.set_paused(false);
    wait(&mut player, "the ending", |p, _| p.now.pos >= 4.5);
    assert!(player.watched_enough(), "in the ending counts as watched");
    let seen = wait(&mut player, "the end", |_, h| h.contains(&Happened::Ended));
    assert_eq!(seen.iter().filter(|h| **h == Happened::Ended).count(), 1);
    assert!(player.now.eof);

    // Play again from the end: from the top.
    player.toggle_pause();
    wait(&mut player, "playing again", |p, _| {
        !p.now.eof && p.now.pos < 2.0 && !p.now.paused
    });

    // Resuming another file part way through.
    player.load(&file, Some(3.0)).unwrap();
    wait(&mut player, "resumed", |p, h| {
        h.contains(&Happened::Loaded) && p.now.pos >= 2.9
    });
    assert_eq!(
        player.mpv().get("start").as_deref(),
        Some("none"),
        "the next file starts at the top"
    );
}

#[test]
fn reports_files_it_cant_play() {
    if !have_mpv() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.mkv");
    std::fs::write(&junk, b"not a video").unwrap();
    let mut player = Player::new(Output::Headless, &Settings::default(), || {}).unwrap();
    player.load(&junk, None).unwrap();
    let seen = wait(&mut player, "an error", |_, h| {
        h.iter().any(|h| matches!(h, Happened::Failed(_)))
    });
    assert!(!seen.contains(&Happened::Loaded));
}

/// A headless OpenGL context (EGL, no window): Mesa's surfaceless platform.
fn headless_gl() -> Option<(std::sync::Arc<eframe::glow::Context>, video::GetProc)> {
    use khronos_egl as egl;
    // SAFETY: loading libEGL.
    let lib = match unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() } {
        Ok(l) => std::sync::Arc::new(l),
        Err(e) => {
            eprintln!("skipping: no EGL 1.5 ({e})");
            return None;
        }
    };
    const PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;
    // SAFETY: the default display of the surfaceless platform.
    let display = unsafe {
        lib.get_platform_display(
            PLATFORM_SURFACELESS_MESA,
            egl::DEFAULT_DISPLAY,
            &[egl::ATTRIB_NONE],
        )
    };
    let Ok(display) = display else {
        eprintln!("skipping: no surfaceless EGL display");
        return None;
    };
    lib.initialize(display).ok()?;
    lib.bind_api(egl::OPENGL_API).ok()?;
    let config = lib
        .choose_first_config(display, &[egl::RENDERABLE_TYPE, egl::OPENGL_BIT, egl::NONE])
        .ok()??;
    let attrs = [
        egl::CONTEXT_MAJOR_VERSION,
        3,
        egl::CONTEXT_MINOR_VERSION,
        3,
        egl::CONTEXT_OPENGL_PROFILE_MASK,
        egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
        egl::NONE,
    ];
    let context = lib.create_context(display, config, None, &attrs).ok()?;
    lib.make_current(display, None, None, Some(context)).ok()?;
    let get_proc: video::GetProc = std::sync::Arc::new(move |name: &std::ffi::CStr| {
        name.to_str()
            .ok()
            .and_then(|n| lib.get_proc_address(n))
            .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
    });
    let loader = get_proc.clone();
    // SAFETY: the context is current on this thread.
    let gl = unsafe { eframe::glow::Context::from_loader_function_cstr(|name| loader(name)) };
    Some((std::sync::Arc::new(gl), get_proc))
}

/// mpv draws into the texture right side up: the top row is the picture's
/// top (red), as egui shows row 0 at the top.
#[test]
fn draws_frames_right_side_up() {
    if !have_mpv() {
        return;
    }
    let Some((gl, get_proc)) = headless_gl() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(file) = sample_episode(dir.path(), "Show - 01.mkv") else {
        return;
    };
    let mut player = Player::new(Output::SilentWindow, &Settings::default(), || {}).unwrap();
    // No subtitles over the colors.
    player.mpv().set("sid", "no").unwrap();
    player.video = Some(video::Video::new(player.mpv(), gl, get_proc, || {}).unwrap());
    player.load(&file, Some(1.0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut drawn = 0;
    while drawn < 5 {
        assert!(
            Instant::now() < deadline,
            "no frames drawn: {:?}",
            player.now
        );
        player.poll();
        if player.now.loaded && player.video.as_mut().unwrap().render([320, 240]) {
            drawn += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let video = player.video.as_ref().unwrap();
    assert_eq!(video.size(), [320, 240]);
    let px = video.pixels();
    let at = |x: usize, y: usize| {
        let i = (y * 320 + x) * 4;
        [px[i], px[i + 1], px[i + 2]]
    };
    let (top, bottom) = (at(160, 20), at(160, 220));
    assert!(top[0] > 200 && top[2] < 60, "top is red: {top:?}");
    assert!(
        bottom[2] > 200 && bottom[0] < 60,
        "bottom is blue: {bottom:?}"
    );
    player.video = None;
}
