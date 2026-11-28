//! Finding anime: Discover (AniList's shelves), the calendar, searching,
//! and choosing a release's files to download.

use chrono::{Days, Local};
use serde_json::{Value, json};

use super::ui::Ui;
use crate::app::discover_ui::{LIST_CHOICES, Loaded, More, list_choice_name, midnight, monday_of};
use crate::app::{Action, App, FilesPage, Origin, Page, Picker, fmt_bytes, is_video};
use crate::browse::{self, Choice, FORMATS, Filter, GENRES, Media, Season, Shelf, Sort};
use crate::tracking::{Service, Status};

/// A list's status for an anime (AniList's, else MyAnimeList's).
fn list_status(ui: &Ui, m: &Media) -> Option<Status> {
    let t = &ui.app.tracking;
    t.lists
        .get(&Service::AniList)
        .and_then(|l| l.get(&m.id))
        .or_else(|| m.id_mal.and_then(|k| t.lists.get(&Service::Mal)?.get(&k)))
        .map(|e| e.status)
}

fn status(s: Status) -> Value {
    json!({"name": s.name(), "key": format!("{s:?}").to_lowercase()})
}

/// A poster: score, format, what's airing; on your lists, in the library.
fn media(ui: &mut Ui, m: &Media, origin: Origin) -> Value {
    let mut facts = Vec::new();
    if let Some(format) = &m.format {
        facts.push(format.replace('_', " "));
    }
    if let Some(n) = m.episodes {
        facts.push(format!("{n} eps"));
    }
    if let (Some(season), Some(year)) = (&m.season, m.year) {
        facts.push(browse::season_name((season.as_str(), year)));
    }
    let airing = m.next_airing.map(|(ep, at)| {
        let when = chrono::DateTime::from_timestamp(at, 0)
            .map(|t| t.with_timezone(&Local).format("%a %-d %b").to_string())
            .unwrap_or_default();
        format!("Ep {ep} · {when}")
    });
    json!({
        "id": m.id,
        "title": m.title(),
        "cover": m.cover,
        "score": m.score,
        "facts": facts.join(" · "),
        "airing": airing,
        "genres": m.genres.iter().take(3).cloned().collect::<Vec<_>>().join(" · "),
        "status": list_status(ui, m).map(status),
        "in_library": ui.app.library.find(m.id).is_some(),
        "open": ui.act(Action::OpenMedia(Box::new(m.clone()), origin)),
    })
}

/// A three-way chip: off, wanted ("in"), not wanted ("out").
fn tri(ui: &mut Ui, label: &str, state: Option<Choice>, f: impl Fn(&mut App) + 'static) -> Value {
    json!({
        "label": label,
        "state": match state {
            None => Value::Null,
            Some(Choice::Include) => json!("in"),
            Some(Choice::Exclude) => json!("out"),
        },
        "id": ui.edit(f),
    })
}

/// The earliest year Discover offers.
const FIRST_YEAR: i32 = 1960;

pub(super) fn discover(ui: &mut Ui) -> Value {
    let app = ui.app;
    let d = &app.discover;
    let f = d.filter.clone();
    let today = Local::now().date_naive();
    let season = browse::season_of(today);
    let shelves: Vec<Value> = Shelf::ALL
        .into_iter()
        .map(|shelf| {
            let label = match shelf {
                Shelf::ThisSeason => format!("This season · {}", browse::season_name(season)),
                Shelf::NextSeason => format!(
                    "Next season · {}",
                    browse::season_name(browse::next_season(season))
                ),
                other => other.name().to_owned(),
            };
            json!({
                "label": label,
                "on": shelf.is(&f, today),
                "id": ui.edit(move |app| app.discover.show(shelf)),
            })
        })
        .collect();
    let sorts: Vec<Value> = Sort::ALL
        .into_iter()
        .map(|s| {
            json!({
                "label": s.name(),
                "on": f.sort == s,
                "id": ui.edit(move |app| app.discover.set_sort(s)),
            })
        })
        .collect();
    let mut seasons = vec![json!({
        "label": "Any season",
        "on": f.season.is_none(),
        "id": ui.edit(|app| app.discover.filter.season = None),
    })];
    for s in Season::ALL {
        seasons.push(json!({
            "label": s.name(),
            "on": f.season == Some(s),
            "id": ui.edit(move |app| app.discover.filter.season = Some(s)),
        }));
    }
    let mut years = vec![json!({
        "label": "Any year",
        "on": f.year.is_none(),
        "id": ui.edit(|app| app.discover.filter.year = None),
    })];
    for y in (FIRST_YEAR..=season.1 + 1).rev() {
        years.push(json!({
            "label": y.to_string(),
            "on": f.year == Some(y),
            "id": ui.edit(move |app| app.discover.filter.year = Some(y)),
        }));
    }
    let mut formats = vec![json!({
        "label": "Any format",
        "on": f.format.is_none(),
        "id": ui.edit(|app| app.discover.filter.format = None),
    })];
    for (label, value) in FORMATS {
        formats.push(json!({
            "label": label,
            "on": f.format.as_deref() == Some(value),
            "id": ui.edit(move |app| app.discover.filter.format = Some(value.to_owned())),
        }));
    }
    let adult = app.adult_on();
    let genres: Vec<Value> = GENRES
        .into_iter()
        .chain(adult.then_some(browse::ADULT_GENRE))
        .map(|g| {
            tri(ui, g, f.tags.get(g).copied(), move |app| {
                app.discover.cycle_tag(g)
            })
        })
        .collect();
    let chosen_tags: Vec<Value> = f
        .tags
        .iter()
        .filter(|(n, _)| !Filter::is_genre(n))
        .map(|(name, choice)| {
            let n = name.clone();
            tri(ui, name, Some(*choice), move |app| {
                app.discover.cycle_tag(&n)
            })
        })
        .collect();
    let found_tags: Vec<Value> = d
        .tag_matches(adult)
        .into_iter()
        .map(|t| {
            let n = t.name.clone();
            tri(ui, &t.name, None, move |app| {
                app.discover.cycle_tag(&n);
                app.discover.tag_query.clear();
            })
        })
        .collect();
    let tag_note = match &d.tags {
        Some((Loaded::Failed(e), _)) => Some(format!("Couldn't read AniList's tags: {e}")),
        _ if !d.tag_query.trim().is_empty() && found_tags.is_empty() => {
            Some("No tag by that name.".to_owned())
        }
        _ => None,
    };
    let lists: Vec<Value> = LIST_CHOICES
        .into_iter()
        .map(|s| {
            tri(ui, list_choice_name(s), d.list_choice(s), move |app| {
                app.discover.cycle_list(s)
            })
        })
        .collect();
    let (list, state) = match d.shelves.get(&f).map(|(l, _)| l) {
        None | Some(Loaded::Loading) => (Vec::new(), json!({"text": "Asking AniList…"})),
        Some(Loaded::Failed(e)) => (
            Vec::new(),
            json!({
                "text": format!("Couldn't read AniList's list: {e}"),
                "error": true,
                "retry": ui.edit(move |app| { app.discover.shelves.remove(&app.discover.filter.clone()); }),
            }),
        ),
        Some(Loaded::Ready(shelf)) if shelf.items.is_empty() => {
            (Vec::new(), json!({"text": "Nothing on AniList for that."}))
        }
        Some(Loaded::Ready(shelf)) => (
            shelf
                .items
                .iter()
                .map(|m| media(ui, m, Origin::Discover))
                .collect(),
            Value::Null,
        ),
    };
    let more = match d.shelves.get(&f).map(|(l, _)| l) {
        Some(Loaded::Ready(shelf)) => {
            list_end(ui, &shelf.more, shelf.next.is_some(), Action::MoreDiscover)
        }
        _ => Value::Null,
    };
    json!({
        "kind": "discover",
        "calendar": false,
        "shelves": shelves,
        "sorts": sorts,
        "seasons": seasons,
        "years": years,
        "formats": formats,
        "shuffle": (f.sort == Sort::Random).then(|| ui.edit(|app| app.discover.reshuffle())),
        "genres": genres,
        "tags": chosen_tags,
        "tag_query": ui.field(&app.discover.tag_query, |app, q| app.discover.tag_query = q),
        "found_tags": found_tags,
        "tag_note": tag_note,
        "lists": lists,
        "lists_note": (!app.tracking.any())
            .then_some("Sign in to AniList or MyAnimeList (More › Lists) to choose by your lists."),
        "clear": app.discover.narrowed().then(|| ui.edit(|app| app.discover.clear_choices())),
        "list": list,
        "state": state,
        "more": more,
        "to_calendar": ui.act(Action::GoCalendar),
    })
}

pub(super) fn calendar(ui: &mut Ui) -> Value {
    let app = ui.app;
    let c = &app.calendar;
    let today = Local::now().date_naive();
    let week = c.week;
    let last = week + Days::new(6);
    let range = if week.format("%b").to_string() == last.format("%b").to_string() {
        format!("{} – {}", week.format("%-d"), last.format("%-d %B %Y"))
    } else {
        format!("{} – {}", week.format("%-d %b"), last.format("%-d %b %Y"))
    };
    let whose = if c.full {
        "everything airing"
    } else if app.tracking.any() {
        "the anime on your lists (but not dropped)"
    } else {
        "your library's anime (sign in to AniList or MyAnimeList for your lists)"
    };
    let now = chrono::Utc::now().timestamp();
    let (days, state) = match c.weeks.get(&week).map(|(l, _)| l) {
        None | Some(Loaded::Loading) => {
            (Vec::new(), json!({"text": "Reading AniList's schedule…"}))
        }
        Some(Loaded::Failed(e)) => (
            Vec::new(),
            json!({
                "text": format!("Couldn't read the schedule: {e}"),
                "error": true,
                "retry": ui.edit(move |app| { app.calendar.weeks.remove(&week); }),
            }),
        ),
        Some(Loaded::Ready(airing)) => {
            let shown: Vec<_> = airing
                .iter()
                .filter(|a| c.full || app.on_your_lists(&a.media))
                .collect();
            let days = (0..7)
                .map(|i| {
                    let day = week + Days::new(i);
                    let (from, to) = (midnight(day), midnight(day + Days::new(1)));
                    let items: Vec<Value> = shown
                        .iter()
                        .filter(|a| a.at >= from && a.at < to)
                        .map(|a| {
                            let m = &a.media;
                            json!({
                                "time": chrono::DateTime::from_timestamp(a.at, 0)
                                    .map(|t| t.with_timezone(&Local).format("%H:%M").to_string()),
                                "title": m.title(),
                                "cover": m.cover,
                                "episode": match m.episodes {
                                    Some(n) => format!("Ep {} of {n}", a.episode),
                                    None => format!("Ep {}", a.episode),
                                },
                                "aired": a.at < now,
                                "status": list_status(ui, m).map(status),
                                "open": ui.act(Action::OpenMedia(Box::new(m.clone()), Origin::Calendar)),
                            })
                        })
                        .collect();
                    json!({
                        "date": day.to_string(),
                        "weekday": day.format("%a").to_string(),
                        "day": day.format("%-d").to_string(),
                        "long": day.format("%A %-d %B").to_string(),
                        "today": day == today,
                        "items": items,
                    })
                })
                .collect();
            (days, Value::Null)
        }
    };
    json!({
        "kind": "discover",
        "calendar": true,
        "week": week.to_string(),
        "range": range,
        "whose": whose,
        "full": {"on": c.full, "id": ui.edit(|app| app.calendar.full = !app.calendar.full)},
        "previous": ui.edit(|app| app.calendar.week = app.calendar.week - Days::new(7)),
        "next": ui.edit(|app| app.calendar.week = app.calendar.week + Days::new(7)),
        "this_week": (week != monday_of(today)).then(|| ui.edit(|app| {
            app.calendar.week = monday_of(Local::now().date_naive())
        })),
        "days": days,
        "state": state,
        "to_browse": ui.act(Action::GoDiscover),
    })
}

/// The end of a list read a page at a time (null at the very end): the
/// phone asks for the next page (`id`) when this comes into view, unless
/// it's `loading`; after an `error`, a button asks again.
fn list_end(ui: &mut Ui, more: &More, has_more: bool, action: Action) -> Value {
    match more {
        More::Idle if !has_more => Value::Null,
        More::Loading => json!({"loading": true, "text": "Loading more…", "id": ui.act(action)}),
        More::Failed(e) => json!({
            "loading": false,
            "error": format!("Couldn't load more: {e}"),
            "id": ui.act(action),
        }),
        More::Idle => json!({"loading": false, "id": ui.act(action)}),
    }
}

pub(super) fn search(ui: &mut Ui) -> Value {
    let app = ui.app;
    let note = if app.last_query.is_empty() {
        "Anime from AniList, with the releases SeaDex (releases.moe) rates best.".to_owned()
    } else {
        let with_entry = app.results.iter().filter(|r| r.entry.is_some()).count();
        format!(
            "{} results for “{}”, {with_entry} with a SeaDex entry",
            app.results.len(),
            app.last_query
        )
    };
    let dual = app.settings.prefer_dual_audio || app.settings.prefs.wants_dub();
    let results: Vec<Value> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, anime)| {
            let (best, good) = match &anime.entry {
                None => (
                    "Not on SeaDex · Nyaa releases ranked by group".to_owned(),
                    false,
                ),
                Some(entry) => match entry.best_nyaa(dual) {
                    Some(b) => {
                        let r = &entry.releases[b];
                        let kind = if r.is_best { "Best" } else { "Alt" };
                        let audio = if r.dual_audio { " · dual audio" } else { "" };
                        (format!("★ {kind} on Nyaa: {}{audio}", r.group), true)
                    }
                    None => (
                        "On SeaDex, but not on Nyaa · Nyaa releases ranked by group".to_owned(),
                        false,
                    ),
                },
            };
            json!({
                "title": anime.title,
                "alt": anime.alt_title,
                "cover": anime.cover_small.clone().or(anime.cover_large.clone()),
                "summary": anime.summary(),
                "best": best,
                "good": good,
                "in_library": app.library.find(anime.anilist_id).is_some(),
                "open": ui.act(Action::OpenAnime(i)),
            })
        })
        .collect();
    json!({
        "kind": "search",
        "query": ui.field(&app.query, |app, q| app.query = q),
        "go": ui.button(
            if app.searching { "Searching…" } else { "Search" },
            (!app.searching && !app.query.trim().is_empty()).then_some(Action::Search),
        ),
        "note": note,
        "nothing": (!app.last_query.is_empty() && app.results.is_empty() && !app.searching)
            .then(|| format!("No anime found for “{}”.", app.last_query)),
        "results": results,
        "more": if app.results.is_empty() {
            Value::Null
        } else {
            let (more, has) = (app.results_more.clone(), app.results_next.is_some());
            list_end(ui, &more, has, Action::MoreResults)
        },
    })
}

/// Change the file picker, if it's still showing.
fn on_files(f: impl Fn(&mut FilesPage) + 'static) -> impl Fn(&mut crate::app::App) + 'static {
    move |app| {
        if let Page::Files(fp) = &mut app.page {
            f(fp);
        }
    }
}

pub(super) fn files(ui: &mut Ui, fp: &FilesPage) -> Value {
    let app = ui.app;
    let mut notes = Vec::new();
    if fp.existing() {
        notes.push(json!({"text": "This torrent is already in your downloads. Saving changes which files it fetches.", "tone": "warn"}));
    } else if app.via_usenet(fp) {
        let names: Vec<&str> = fp.nzbs.iter().map(|l| l.indexer.name()).collect();
        notes.push(json!({"text": format!("On Usenet ({}): downloads from your Usenet providers", names.join(", ")), "tone": "good"}));
    } else if let Some(nzbs) = app.episode_nzbs(fp) {
        let on = nzbs
            .iter()
            .filter(|l| l.is_some_and(|l| !l.is_empty()))
            .count();
        if nzbs.iter().any(Option::is_none) {
            notes.push(json!({"text": "Checking which episodes are on Usenet…", "tone": "muted"}));
        } else if on > 0 {
            notes.push(json!({"text": format!("On Usenet: {on} of {} episodes download from your Usenet providers", nzbs.len()), "tone": "good"}));
        }
    }
    let visible = fp.visible();
    let mut tools = vec![
        json!({"label": "All", "id": ui.edit(on_files(|fp| fp.checked.fill(true)))}),
        json!({"label": "None", "id": ui.edit(on_files(|fp| fp.checked.fill(false)))}),
        json!({"label": "Invert", "id": ui.edit(on_files(|fp| fp.checked.iter_mut().for_each(|c| *c = !*c)))}),
    ];
    if matches!(fp.picker, Picker::Files(_)) {
        tools.push(json!({"label": "Videos only", "id": ui.edit(on_files(|fp| {
            let videos: Vec<bool> = fp.rows.iter().map(|r| is_video(&r.label)).collect();
            fp.checked = videos;
        }))}));
    }
    if !fp.filter.trim().is_empty() {
        let shown = visible.clone();
        let hide = visible.clone();
        tools.push(json!({"label": format!("Select {} shown", visible.len()), "id": ui.edit(on_files(move |fp| shown.iter().for_each(|&i| fp.checked[i] = true)))}));
        tools.push(json!({"label": "Deselect shown", "id": ui.edit(on_files(move |fp| hide.iter().for_each(|&i| fp.checked[i] = false)))}));
    }
    let usenet_hashes: Option<Vec<String>> = match &fp.picker {
        Picker::Episodes { items, .. } if app.usenet.active() => {
            Some(items.iter().map(|(_, i)| i.info_hash.clone()).collect())
        }
        _ => None,
    };
    let rows: Vec<Value> = visible
        .iter()
        .map(|&i| {
            let r = &fp.rows[i];
            let mut meta = vec![fmt_bytes(r.len)];
            if let Some(n) = r
                .seeders
                .filter(|_| matches!(fp.picker, Picker::Episodes { .. }))
            {
                meta.push(format!("{n} seeders"));
            }
            if let Some(h) = &usenet_hashes {
                meta.push(match app.usenet.links(&h[i]) {
                    None => "Usenet: checking…".to_owned(),
                    Some(l) if !l.is_empty() => "On Usenet".to_owned(),
                    Some(_) => "Not on Usenet".to_owned(),
                });
            }
            json!({
                "label": r.label,
                "on": fp.checked[i],
                "meta": meta.join(" · "),
                "id": ui.edit(on_files(move |fp| fp.checked[i] = !fp.checked[i])),
            })
        })
        .collect();
    // Downloading.
    let (count, size) = fp
        .rows
        .iter()
        .zip(&fp.checked)
        .filter(|(_, c)| **c)
        .fold((0usize, 0u64), |(n, s), (r, _)| (n + 1, s + r.len));
    let existing = fp.existing();
    let (on, off) = match app.episode_nzbs(fp) {
        Some(nzbs) => fp.selected().iter().fold((0, 0), |(on, off), &i| {
            if nzbs[i].is_some_and(|l| !l.is_empty()) {
                (on + 1, off)
            } else {
                (on, off + 1)
            }
        }),
        None => (0, count),
    };
    let usenet = app.via_usenet(fp) || (on > 0 && off == 0);
    let ready = existing || usenet || app.download_ready().is_ok();
    let label = match app.debrid.service() {
        _ if existing => "Update download".to_owned(),
        _ if usenet => "↓  Download from Usenet".to_owned(),
        service if on > 0 => {
            let rest = service.map_or("as torrents".to_owned(), |s| format!("via {s}"));
            format!("↓  Download: {on} from Usenet, {off} {rest}")
        }
        Some(service) => format!("↓  Download via {service}"),
        None => "↓  Download".to_owned(),
    };
    let folder = fp.output_folder(&app.library_dir()).display().to_string();
    json!({
        "kind": "files",
        "title": fp.title(),
        "subtitle": fp.subtitle,
        "notes": notes,
        "tools": tools,
        "filter": ui.field(&fp.filter, |app, f| if let Page::Files(fp) = &mut app.page { fp.filter = f }),
        "heading": format!("{} ({} shown)", match fp.picker { Picker::Files(_) => "Files", Picker::Episodes { .. } => "Episodes" }, visible.len()),
        "rows": rows,
        "summary": format!("{count} of {} {} · {}", fp.rows.len(), fp.unit(), fmt_bytes(size)),
        "folder": format!("Saves to {folder}"),
        "why": if ready { None } else { app.download_ready().err() },
        "go": ui.button(
            if app.starting_download { "Starting…".to_owned() } else { label },
            (count > 0 && ready && !app.starting_download).then_some(Action::StartDownload),
        ),
    })
}
