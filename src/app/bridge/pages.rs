//! The playlist (and the remuxed copies kept for it), and remuxing: built
//! from the settings' items.

use serde_json::{Value, json};

use super::form::{Change, boxed, buttons, field, said, select, text};
use super::ui::Ui;
use crate::app::remux_page::{FileState, RemuxPage, default_out_dir};
use crate::app::{Action, App, Page, PlaylistPage, fmt_bytes};
use crate::playlist;
use crate::remux::{Kind, language_name};

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn small(list: Vec<Value>) -> Value {
    json!({"t": "buttons", "buttons": list, "primary": false, "small": true})
}

pub(super) fn playlist(ui: &mut Ui, pp: &PlaylistPage) -> Value {
    let app = ui.app;
    let exporting = pp.export.as_ref().is_some_and(|r| r.running());
    let items = &app.playlist.items;
    let names = playlist::export_names(items);
    let sizes: Vec<Option<u64>> = items
        .iter()
        .map(|i| std::fs::metadata(&i.path).ok().map(|m| m.len()))
        .collect();
    let missing = sizes.iter().filter(|s| s.is_none()).count();
    let total: u64 = sizes.iter().flatten().sum();
    let mut list = vec![
        said(
            format!(
                "{} episode{} · {}",
                items.len(),
                plural(items.len()),
                fmt_bytes(total)
            ),
            "muted",
        ),
        said(
            "Episodes in the order you want them. Exporting makes a zip named after the date and \
             time, holding a folder of the same name with the files numbered “1) title”.",
            "faint",
        ),
    ];
    if items.is_empty() {
        list.push(said(
            "Nothing here yet. Add episodes with “+ Playlist” on a series page, or after remuxing.",
            "muted",
        ));
    } else {
        let clear = ui.button("Clear", (!exporting).then_some(Action::PlaylistClear));
        list.push(buttons(vec![clear], false));
    }
    for (i, (_item, name)) in items.iter().zip(&names).enumerate() {
        let shown = name
            .rsplit_once('.')
            .map_or(name.as_str(), |(stem, _)| stem)
            .to_owned();
        let size = match sizes[i] {
            Some(s) => said(fmt_bytes(s), "faint"),
            None => said("File missing", "error"),
        };
        let enabled = !exporting;
        let tools = vec![
            ui.button(
                "↑",
                (enabled && i > 0).then(|| Action::PlaylistMove { from: i, to: i - 1 }),
            ),
            ui.button(
                "↓",
                (enabled && i + 1 < items.len())
                    .then(|| Action::PlaylistMove { from: i, to: i + 1 }),
            ),
            ui.button("Remove", enabled.then_some(Action::PlaylistRemove(i))),
        ];
        list.push(boxed(vec![text(shown), size, small(tools)]));
    }
    // Exporting.
    let mut export = Vec::new();
    if !items.is_empty() {
        let name = playlist::export_name(&chrono::Local::now());
        export.push(field(
            ui,
            "Export to",
            "Folder",
            &pp.out_dir,
            false,
            |app, t| {
                if let Page::Playlist(pp) = &mut app.page {
                    pp.out_dir = t;
                }
            },
        ));
        export.push(said(
            format!(
                "Creates “{name}.zip” ({}) with a “{name}” folder inside; the time is taken when \
                 you export.",
                fmt_bytes(total)
            ),
            "faint",
        ));
        match &pp.export {
            Some(run) if run.running() => {
                export.push(said(
                    format!("Exporting · {:.0}%", run.progress * 100.0),
                    "muted",
                ));
                let cancel = ui.button("Cancel", Some(Action::CancelExport));
                export.push(buttons(vec![cancel], false));
            }
            _ => {
                let can = missing == 0 && !pp.out_dir.trim().is_empty();
                let go = ui.button(
                    format!("Export {} episode{}", items.len(), plural(items.len())),
                    can.then_some(Action::StartExport),
                );
                export.push(buttons(vec![go], true));
                if missing > 0 {
                    export.push(said(
                        format!(
                            "{missing} file{} missing: remove or re-add {}",
                            if missing == 1 { " is" } else { "s are" },
                            if missing == 1 { "it" } else { "them" }
                        ),
                        "error",
                    ));
                }
                match pp.export.as_ref().and_then(|r| r.result.as_ref()) {
                    Some(Ok(path)) => {
                        export.push(said(format!("Saved {}", path.display()), "good"))
                    }
                    Some(Err(e)) if e != "cancelled" => {
                        export.push(said(format!("Export failed: {e}"), "error"))
                    }
                    _ => {}
                }
            }
        }
    }
    // The remuxed copies.
    let root = app.library_dir().join("Remuxed");
    let count: usize = pp.remuxes.iter().map(|r| r.files.len()).sum();
    let bytes: u64 = pp.remuxes.iter().map(|r| r.bytes).sum();
    let mut remuxed = vec![said(
        format!(
            "{count} episode{} · {} · kept in {} until you delete them (the episodes in your \
             library are separate, and stay)",
            plural(count),
            fmt_bytes(bytes),
            root.display()
        ),
        "muted",
    )];
    if pp.remuxes.is_empty() {
        remuxed.push(said(
            "None yet. “Remux…” on a series page makes copies with just the tracks you choose.",
            "faint",
        ));
    }
    for r in &pp.remuxes {
        let mut series = vec![
            text(r.name.clone()),
            said(
                format!(
                    "{} episode{} · {}",
                    r.files.len(),
                    plural(r.files.len()),
                    fmt_bytes(r.bytes)
                ),
                "muted",
            ),
        ];
        let mut all = ui.button("Delete all…", Some(Action::AskDeleteRemux(r.dir.clone())));
        all["kind"] = json!("danger");
        series.push(small(vec![all]));
        for (path, len) in &r.files {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let listed = app.playlist.items.iter().any(|i| &i.path == path);
            let p = path.clone();
            let add = if listed {
                json!({"label": "In playlist", "id": null})
            } else {
                json!({"label": "+ Playlist", "id": ui.edit(move |app: &mut App| app.add_files_to_playlist(vec![p.clone()]))})
            };
            let mut del = ui.button("Delete…", Some(Action::AskDeleteRemux(path.clone())));
            del["kind"] = json!("danger");
            series.push(said(format!("{name} · {}", fmt_bytes(*len)), "plain"));
            series.push(small(vec![add, del]));
        }
        remuxed.push(boxed(series));
    }
    json!({
        "kind": "playlist",
        "sections": [
            {"title": "", "items": list},
            {"title": "Export", "items": export},
            {"title": "Remuxed copies", "items": remuxed},
        ],
    })
}

/// Change the remux page, if it's still showing.
fn on_remux(f: impl Fn(&mut RemuxPage) + 'static) -> impl Fn(&mut App) + 'static {
    move |app| {
        if let Page::Remux(rp) = &mut app.page {
            f(rp);
        }
    }
}

pub(super) fn remux(ui: &mut Ui, rp: &RemuxPage) -> Value {
    let locked = rp.run.as_ref().is_some_and(|r| !r.finished);
    let ready = rp.series.files.len();
    let mut summary = format!("{ready} finished episode{}", plural(ready));
    if rp.series.not_ready > 0 {
        summary.push_str(&format!(
            " · {} still downloading (not included)",
            rp.series.not_ready
        ));
    }
    let mut items = vec![
        said(format!("{} · {summary}", rp.series.name), "muted"),
        said(
            "Copies keep only the audio and subtitles you pick; video and chapters are kept. The \
             downloads stay as they are.",
            "faint",
        ),
    ];
    if ready == 0 {
        items.push(text(
            "Nothing to remux yet: wait for an episode to finish downloading.",
        ));
        return json!({"kind": "remux", "sections": [{"title": "", "items": items}]});
    }
    let note = rp.title_note.trim_end_matches('.').to_owned();
    items.push(field(
        ui,
        "Title",
        "English title",
        &rp.title,
        false,
        move |app, t| {
            let dir = app.library_dir();
            if let Page::Remux(rp) = &mut app.page {
                rp.title = t;
                if !rp.out_dir_edited {
                    rp.out_dir = default_out_dir(&dir, &rp.title);
                }
            }
        },
    ));
    items.push(said(
        format!("{note}. Files are named “<title> - 01.mkv” and titled the same."),
        "faint",
    ));
    items.push(field(
        ui,
        "Save to",
        "Folder",
        &rp.out_dir,
        false,
        |app, t| {
            on_remux(move |rp| {
                rp.out_dir = t.clone();
                rp.out_dir_edited = true;
            })(app)
        },
    ));
    if let Some(e) = &rp.error {
        items.push(said(
            format!("Couldn't read the episodes' tracks: {e}"),
            "error",
        ));
        return json!({"kind": "remux", "sections": [{"title": "", "items": items}]});
    }
    let Some(analysis) = &rp.analysis else {
        items.push(said("Reading the episodes' tracks…", "muted"));
        return json!({"kind": "remux", "sections": [{"title": "", "items": items}]});
    };
    if !analysis.unreadable.is_empty() {
        let names: Vec<String> = analysis
            .unreadable
            .iter()
            .map(|(p, _)| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        items.push(said(
            format!(
                "Can't remux {} (only MKV files are supported): {}",
                names.len(),
                names.join(", ")
            ),
            "warn",
        ));
    }
    // The tracks: a language, and which of its tracks.
    let mut tracks = Vec::new();
    for kind in [Kind::Audio, Kind::Subtitles] {
        tracks.push(text(if kind == Kind::Audio {
            "Audio"
        } else {
            "Subtitles"
        }));
        let total = analysis.files.len();
        let langs = analysis.languages(kind);
        let selected = match kind {
            Kind::Audio => rp.audio_lang.clone(),
            _ => rp.sub_lang.clone(),
        };
        let mut options: Vec<(String, bool, Change)> = Vec::new();
        let pick = move |code: Option<String>| -> Change {
            Box::new(on_remux(move |rp| match kind {
                Kind::Audio => {
                    rp.audio_lang = code.clone();
                    rp.refresh_audio_track();
                }
                _ => {
                    rp.sub_lang = code.clone();
                    rp.refresh_subtitle_track();
                }
            }))
        };
        if kind == Kind::Subtitles {
            options.push(("None (no subtitles)".into(), selected.is_none(), pick(None)));
        }
        for l in &langs {
            let label = if l.files == total {
                language_name(&l.code)
            } else {
                format!(
                    "{} ({} of {total} episodes)",
                    language_name(&l.code),
                    l.files
                )
            };
            options.push((
                label,
                selected.as_deref() == Some(l.code.as_str()),
                pick(Some(l.code.clone())),
            ));
        }
        if !locked {
            tracks.push(select(ui, "Language", options));
        }
        // Several tracks in it: which to keep.
        let chosen = match kind {
            Kind::Audio => rp.audio.as_ref(),
            _ => rp.subs.as_ref(),
        };
        if let Some(lang) = selected.as_deref() {
            let candidates = analysis.candidates(kind, lang);
            if candidates.len() > 1 {
                tracks.push(said(
                    format!(
                        "{} has {} tracks; keep:",
                        language_name(lang),
                        candidates.len()
                    ),
                    "muted",
                ));
                let mut radios = Vec::new();
                for cand in candidates {
                    let mut label = cand.label();
                    let sample = rp.samples.get(&cand.sig);
                    if let Some(s) = sample {
                        label.push_str(&format!(" · {} line{}", s.events, plural(s.events)));
                    }
                    if cand.files < total {
                        label.push_str(&format!(" · in {} of {total} episodes", cand.files));
                    }
                    let preview = sample.filter(|s| !s.lines.is_empty()).map(|s| {
                        s.lines
                            .iter()
                            .map(|l| format!("“{l}”"))
                            .collect::<Vec<_>>()
                            .join("  ")
                    });
                    let sig = cand.sig.clone();
                    radios.push(json!({
                        "label": label,
                        "note": preview,
                        "on": chosen == Some(&cand.sig),
                        "id": (!locked).then(|| ui.edit(on_remux(move |rp| match kind {
                            Kind::Audio => rp.audio = Some(sig.clone()),
                            _ => rp.subs = Some(sig.clone()),
                        }))),
                    }));
                }
                tracks.push(json!({"t": "radios", "options": radios}));
                if kind == Kind::Subtitles && rp.samples.is_empty() {
                    tracks.push(said("Reading what each track contains…", "faint"));
                }
            }
        }
    }
    // Remuxing.
    let plan = rp.plan();
    let runnable = plan.iter().filter(|p| p.problem().is_none()).count();
    let mut run = Vec::new();
    match &rp.run {
        Some(r) if !r.finished => {
            let progress: f32 = r
                .states
                .iter()
                .map(|s| match s {
                    FileState::Done => 1.0,
                    FileState::Running(f) => *f,
                    _ => 0.0,
                })
                .sum::<f32>()
                / runnable.max(1) as f32;
            run.push(said(
                format!("Remuxing · {:.0}%", progress * 100.0),
                "muted",
            ));
            let cancel = ui.button("Cancel", Some(Action::CancelRemux));
            run.push(buttons(vec![cancel], false));
        }
        _ => {
            let mut list = vec![ui.button(
                format!("Remux {runnable} episode{}", plural(runnable)),
                (runnable > 0).then_some(Action::StartRemux),
            )];
            if let Some(r) = rp.run.as_ref().filter(|r| r.finished) {
                let done = r
                    .states
                    .iter()
                    .filter(|s| matches!(s, FileState::Done))
                    .count();
                if done > 0 {
                    list.push(ui.button(
                        format!("Add {done} to playlist"),
                        Some(Action::AddRemuxesToPlaylist),
                    ));
                }
            }
            run.push(buttons(list, true));
            if rp.title.trim().is_empty() {
                run.push(said("Enter a title first.", "warn"));
            }
        }
    }
    let mut episodes = Vec::new();
    for (i, item) in plan.iter().enumerate() {
        let name = item
            .output
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let audio = item
            .audio
            .as_ref()
            .map_or("—".to_owned(), |t| t.short_label());
        let subtitles = item
            .subtitles
            .as_ref()
            .map_or("None".to_owned(), |t| t.short_label());
        let state = match rp.run.as_ref().and_then(|r| r.states.get(i)) {
            Some(FileState::Waiting) => said("Waiting", "muted"),
            Some(FileState::Running(f)) => said(format!("{:.0}%", f * 100.0), "accent"),
            Some(FileState::Done) => said("Done", "good"),
            Some(FileState::Failed(e)) => said(format!("Failed: {e}"), "error"),
            Some(FileState::Skipped(why)) => said(format!("Skipped: {why}"), "warn"),
            Some(FileState::Cancelled) => said("Cancelled", "muted"),
            None => match item.problem() {
                Some(p) => said(format!("Will skip: {p}"), "warn"),
                None if !item.notes.is_empty() => said(item.notes.join("; "), "warn"),
                None => json!({"t": "text", "text": ""}),
            },
        };
        episodes.push(boxed(vec![
            text(name),
            said(format!("Audio {audio} · subtitles {subtitles}"), "muted"),
            state,
        ]));
    }
    json!({
        "kind": "remux",
        "sections": [
            {"title": "", "items": items},
            {"title": "Tracks", "items": tracks},
            {"title": "", "items": run},
            {"title": "Episodes", "items": episodes},
        ],
    })
}
