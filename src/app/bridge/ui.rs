//! Building a view: its buttons' and fields' handles, kept for when the UI
//! uses them.

use std::collections::VecDeque;

use serde_json::{Value, json};

use crate::app::{Action, App};

/// What a handle does.
pub(super) enum Handler {
    /// An action, as the desktop GUI's buttons do.
    Act(Action),
    /// A change to the app.
    Edit(Box<dyn Fn(&mut App)>),
    /// What a field sets, with its text.
    Input(Box<dyn Fn(&mut App, String)>),
}

/// The handles of the last views (a tap on a view just replaced, or text
/// typed as views change underneath, still counts).
#[derive(Default)]
pub(super) struct Handles {
    generation: u64,
    views: VecDeque<(u64, Vec<Handler>)>,
}

impl Handles {
    fn start(&mut self) {
        self.generation += 1;
        self.views.push_back((self.generation, Vec::new()));
        while self.views.len() > 64 {
            self.views.pop_front();
        }
    }

    fn add(&mut self, h: Handler) -> String {
        let (generation, list) = self.views.back_mut().expect("a view started");
        list.push(h);
        format!("{generation}.{}", list.len() - 1)
    }

    pub(super) fn get(&self, id: &str) -> Option<&Handler> {
        let (generation, n) = id.split_once('.')?;
        let (generation, n): (u64, usize) = (generation.parse().ok()?, n.parse().ok()?);
        self.views
            .iter()
            .find(|(g, _)| *g == generation)
            .and_then(|(_, list)| list.get(n))
    }
}

/// A view being built: the app to look at, and where its handles go.
pub(super) struct Ui<'a> {
    pub(super) app: &'a App,
    handles: &'a mut Handles,
}

impl<'a> Ui<'a> {
    pub(super) fn new(app: &'a App, handles: &'a mut Handles) -> Self {
        handles.start();
        Self { app, handles }
    }

    /// A handle for an action.
    pub(super) fn act(&mut self, action: Action) -> String {
        self.handles.add(Handler::Act(action))
    }

    /// A handle for a change.
    pub(super) fn edit(&mut self, f: impl Fn(&mut App) + 'static) -> String {
        self.handles.add(Handler::Edit(Box::new(f)))
    }

    /// A field's handle: what it sets with its text.
    pub(super) fn input(&mut self, f: impl Fn(&mut App, String) + 'static) -> String {
        self.handles.add(Handler::Input(Box::new(f)))
    }

    /// A button: its label, and its action (`None`: greyed out).
    pub(super) fn button(&mut self, label: impl Into<String>, action: Option<Action>) -> Value {
        let id = action.map(|a| self.act(a));
        json!({"label": label.into(), "id": id})
    }

    /// A button that's greyed out for a reason (said when it's tapped).
    pub(super) fn button_why(
        &mut self,
        label: impl Into<String>,
        action: Option<Action>,
        why: Option<String>,
    ) -> Value {
        let mut b = self.button(label, action);
        if let Some(w) = why {
            b["why"] = json!(w);
        }
        b
    }

    /// A field: its text, and what typing into it sets.
    pub(super) fn field(&mut self, value: &str, set: impl Fn(&mut App, String) + 'static) -> Value {
        json!({"value": value, "id": self.input(set)})
    }
}
