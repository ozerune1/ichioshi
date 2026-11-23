//! An episode's discussion: AniList's thread and MyAnimeList's, their
//! posts (with replies, quotes, spoilers, pictures); commenting on AniList.

use serde_json::{Value, json};

use super::ui::Ui;
use crate::app::Action;
use crate::app::discover_ui::Loaded;
use crate::app::discuss_ui::Discussion;
use crate::discuss::{Piece, Post, Site, Thread};

pub(super) fn view(ui: &mut Ui) -> Value {
    let app = ui.app;
    let Some(d) = &app.discussion else {
        return Value::Null;
    };
    let tabs: Vec<Value> = [Site::AniList, Site::Mal]
        .into_iter()
        .map(|site| {
            let label = match d.threads.get(&site) {
                Some(Loaded::Ready(Some(t))) => format!("{} · {} replies", site.name(), t.replies),
                Some(Loaded::Ready(None)) => format!("{} · none", site.name()),
                _ => site.name().to_owned(),
            };
            json!({
                "label": label,
                "on": d.site == site,
                "id": ui.edit(move |app| if let Some(d) = &mut app.discussion { d.site = site }),
            })
        })
        .collect();
    let mut out = json!({
        "title": format!("Episode {} discussion", d.episode),
        "series": d.title,
        "tabs": tabs,
        "close": ui.edit(|app| app.discussion = None),
    });
    match d.threads.get(&d.site) {
        None | Some(Loaded::Loading) => {
            out["state"] = json!(format!(
                "Finding episode {}'s thread on {}…",
                d.episode,
                d.site.name()
            ));
        }
        Some(Loaded::Failed(e)) => out["error"] = json!(format!("Couldn't read it: {e}")),
        Some(Loaded::Ready(None)) => {
            out["state"] = json!(format!(
                "{} has no thread for episode {} (none titled with it).",
                d.site.name(),
                d.episode
            ));
            let url = match d.site {
                Site::AniList => format!("https://anilist.co/anime/{}", d.anilist_id),
                Site::Mal => match app.mal_id(d.anilist_id) {
                    Some(m) => format!("https://myanimelist.net/forum/?animeid={m}&topic=episode"),
                    None => "https://myanimelist.net/forum/".into(),
                },
            };
            out["look"] = json!({
                "label": format!("Look on {} ↗", d.site.name()),
                "id": ui.edit(move |app| app.ctx.open_url(&url)),
            });
        }
        Some(Loaded::Ready(Some(t))) => out["thread"] = thread(ui, d, t),
    }
    out
}

fn thread(ui: &mut Ui, d: &Discussion, t: &Thread) -> Value {
    let url = t.url.clone();
    let mut out = json!({
        "title": t.title,
        "reply_there": {
            "label": format!("Reply on {} ↗", t.site.name()),
            "id": ui.edit(move |app| app.ctx.open_url(&url)),
        },
        "posts": t.posts.iter().map(|p| post(ui, t, p)).collect::<Vec<_>>(),
    });
    if t.site == Site::AniList {
        out["composer"] = if ui.app.tracking.anilist.is_none() {
            json!({"signin": "Sign in to AniList (More → Lists) to comment here."})
        } else {
            json!({
                "replying": d.reply_to.as_ref().map(|(_, who)| format!("Replying to {who}")),
                "cancel": ui.edit(|app| if let Some(d) = &mut app.discussion { d.reply_to = None }),
                "draft": ui.field(&d.draft, |app, v| if let Some(d) = &mut app.discussion { d.draft = v }),
                "hint": if d.reply_to.is_some() {
                    "Your reply (Markdown works; ~!spoilers!~ hide spoilers)"
                } else {
                    "Add a comment (Markdown works; ~!spoilers!~ hide spoilers)"
                },
                "post": ui.button(
                    if d.posting { "Posting…" } else { "Post" },
                    (!d.draft.trim().is_empty() && !d.posting).then_some(Action::DiscussionPost),
                ),
            })
        };
    }
    if t.next.is_some() {
        let loading = d.more.contains(&t.site);
        out["more"] = ui.button(
            if loading { "Loading…" } else { "Load more" },
            (!loading).then_some(Action::DiscussionMore),
        );
    }
    out
}

/// A post: who and when, its pieces, and the replies to it.
fn post(ui: &mut Ui, t: &Thread, p: &Post) -> Value {
    // The thread's own post (its ID is the thread's) is commented on with
    // the box above; comments can be answered.
    let opening = t
        .url
        .rsplit(['/', '='])
        .find_map(|part| part.parse::<u64>().ok())
        == Some(p.id);
    let reply = (t.site == Site::AniList && !opening).then(|| {
        let (pid, who) = (p.id, p.author.clone());
        ui.edit(move |app| {
            if let Some(d) = &mut app.discussion {
                d.reply_to = Some((pid, who.clone()));
            }
        })
    });
    let body: Vec<Value> = p
        .body
        .iter()
        .map(|piece| match piece {
            Piece::Text(t) => json!({"t": "text", "text": t}),
            Piece::Quote(who, t) => json!({"t": "quote", "who": who, "text": t}),
            Piece::Spoiler(t) => json!({"t": "spoiler", "text": t}),
            Piece::Image(url) => json!({"t": "image", "url": url}),
        })
        .collect();
    json!({
        "id": p.id,
        "author": p.author,
        "when": chrono::DateTime::from_timestamp(p.at, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("%-d %b %Y, %H:%M").to_string()),
        "likes": p.likes.filter(|n| *n > 0),
        "reply": reply,
        "body": body,
        "replies": p.replies.iter().map(|r| post(ui, t, r)).collect::<Vec<_>>(),
    })
}
