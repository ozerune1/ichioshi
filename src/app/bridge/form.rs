//! Settings as items the phone app draws: text, choices, fields, switches,
//! buttons, boxes of them.

use serde_json::{Value, json};

use super::ui::Ui;
use crate::app::App;

/// What choosing or tapping something does to the app.
pub(super) type Change = Box<dyn Fn(&mut App)>;

pub(super) fn text(s: impl Into<String>) -> Value {
    json!({"t": "text", "text": s.into()})
}

/// Text in a tone: "muted", "good", "warn", "error".
pub(super) fn said(s: impl Into<String>, tone: &str) -> Value {
    json!({"t": "text", "text": s.into(), "tone": tone})
}

pub(super) fn heading(s: impl Into<String>, sub: Option<String>) -> Value {
    json!({"t": "heading", "text": s.into(), "sub": sub})
}

/// One of a few (the one on, and what each does).
pub(super) fn chips(ui: &mut Ui, options: Vec<(String, bool, Change)>) -> Value {
    let options: Vec<Value> = options
        .into_iter()
        .map(|(label, on, f)| json!({"label": label, "on": on, "id": ui.edit(f)}))
        .collect();
    json!({"t": "chips", "options": options})
}

/// A field: its name, what it hints, its text, and what typing sets.
/// `secret`: hidden as typed.
pub(super) fn field(
    ui: &mut Ui,
    label: &str,
    hint: &str,
    value: &str,
    secret: bool,
    set: impl Fn(&mut App, String) + 'static,
) -> Value {
    json!({
        "t": "field",
        "label": label,
        "hint": hint,
        "value": value,
        "secret": secret,
        "id": ui.input(set),
    })
}

/// A switch.
pub(super) fn check(
    ui: &mut Ui,
    label: &str,
    on: bool,
    set: impl Fn(&mut App, bool) + 'static,
) -> Value {
    json!({"t": "check", "label": label, "on": on, "id": ui.edit(move |app| set(app, !on))})
}

/// A choice from a list: (its name, chosen, what choosing it does).
pub(super) fn select(ui: &mut Ui, label: &str, options: Vec<(String, bool, Change)>) -> Value {
    let options: Vec<Value> = options
        .into_iter()
        .map(|(label, on, f)| json!({"label": label, "on": on, "id": ui.edit(f)}))
        .collect();
    json!({"t": "select", "label": label, "options": options})
}

/// Buttons in a row (from `Ui::button`); the first, `primary`, stands out.
pub(super) fn buttons(list: Vec<Value>, primary: bool) -> Value {
    json!({"t": "buttons", "buttons": list, "primary": primary})
}

/// A link to open (the system's browser).
pub(super) fn link(ui: &mut Ui, label: &str, url: String) -> Value {
    let id = ui.edit(move |app| app.ctx.open_url(&url));
    json!({"t": "link", "label": label, "id": id})
}

/// Text to copy (a URL to give a site).
pub(super) fn copy(label: &str, value: String) -> Value {
    json!({"t": "copy", "label": label, "value": value})
}

/// A short code to read out or copy, shown large (a watch party's).
pub(super) fn big_code(value: String) -> Value {
    json!({"t": "bigcode", "value": value})
}

/// Items in a box of their own.
pub(super) fn boxed(items: Vec<Value>) -> Value {
    json!({"t": "box", "items": items})
}
