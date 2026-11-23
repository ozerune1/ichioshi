//! The core, for a front end outside this crate's Rust: the phone app's web
//! UI (mobile/, through Tauri). The app runs on a thread of its own; the UI
//! asks it for the view (the page it's on and what's around it, as JSON)
//! and sends commands back, and is told when the view changes.
//!
//! A view's buttons and fields carry handles (`ui::Handles`): the action a
//! button does, or what a field sets, kept here until the UI uses it. So
//! the view says what's there and the core keeps what each thing does, as
//! the desktop GUI does with its messages.

mod browse;
mod dialogs;
mod discussion;
mod downloads;
mod form;
mod library;
mod manage;
mod play;
mod series;
mod settings;
mod ui;
mod view;

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;

use super::{App, Waker};

pub use view::Command;

/// Word to the UI: "changed" (ask for the view again), "play" (a video to
/// play, and where from).
pub(super) type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// What the core's thread keeps besides the app.
struct Core {
    handles: ui::Handles,
    emit: Emit,
    rt: tokio::runtime::Handle,
}

enum Job {
    /// Look at the app (a view): changes nothing in it.
    Read(Box<dyn FnOnce(&App, &mut Core) + Send>),
    /// Change it (a command).
    Write(Box<dyn FnOnce(&mut App, &mut Core) + Send>),
    /// Background work finished.
    Wake,
}

/// The running app.
#[derive(Clone)]
pub struct Bridge {
    jobs: mpsc::Sender<Job>,
}

impl Bridge {
    /// Start the app, its state in `state_dir`. `emit` tells the UI
    /// things: "changed" (ask for the view again), "play"; `open` opens
    /// links (the system's browser).
    pub fn start(
        state_dir: PathBuf,
        emit: impl Fn(&str, Value) + Send + Sync + 'static,
        open: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let wake = jobs.clone();
        let emit: Emit = Arc::new(emit);
        std::thread::Builder::new()
            .name("seadex-core".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to start the tokio runtime");
                let waker = Waker::new(
                    move || {
                        let _ = wake.send(Job::Wake);
                    },
                    open,
                );
                let mut app = App::create(runtime.handle().clone(), state_dir, waker);
                let mut core = Core {
                    handles: ui::Handles::default(),
                    emit,
                    rt: runtime.handle().clone(),
                };
                let changed = |core: &Core| (core.emit)("changed", Value::Null);
                app.duties();
                loop {
                    match inbox.recv_timeout(Duration::from_millis(500)) {
                        Ok(Job::Read(f)) => f(&app, &mut core),
                        Ok(Job::Write(f)) => {
                            f(&mut app, &mut core);
                            app.duties();
                            changed(&core);
                        }
                        Ok(Job::Wake) => {
                            app.duties();
                            changed(&core);
                        }
                        // Progress (downloads) moves without messages.
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            app.duties();
                            if view::moving(&app) {
                                changed(&core);
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                app.save_settings();
                app.shut_down();
            })
            .expect("failed to start the core's thread");
        Self { jobs }
    }

    async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&App, &mut Core) -> T + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Job::Read(Box::new(move |app, core| {
                let _ = tx.send(f(app, core));
            })))
            .map_err(|_| "the app has stopped".to_owned())?;
        rx.await.map_err(|_| "the app has stopped".to_owned())
    }

    async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut App, &mut Core) -> T + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Job::Write(Box::new(move |app, core| {
                let _ = tx.send(f(app, core));
            })))
            .map_err(|_| "the app has stopped".to_owned())?;
        rx.await.map_err(|_| "the app has stopped".to_owned())
    }

    /// What to show: the page, and what's around it.
    pub async fn view(&self) -> Result<Value, String> {
        self.read(|app, core| view::view(app, &mut core.handles))
            .await
    }

    /// Do something (press a button, type into a field, go somewhere…).
    /// What it says back (`{"exit": true}`: back from the first page).
    pub async fn command(&self, command: Command) -> Result<Value, String> {
        self.write(move |app, core| view::command(app, core, command))
            .await
    }
}
