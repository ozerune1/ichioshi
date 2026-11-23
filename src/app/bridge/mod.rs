//! The core, for a front end outside this crate's Rust: the phone app's web
//! UI (mobile/, through Tauri). The app runs on a thread of its own; the UI
//! sends it commands, asks it for the view (the page it's on and what's
//! around it, as JSON), and is told when that changes.

mod view;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use super::{App, Waker};

pub use view::Command;

enum Job {
    /// Look at the app (a view): changes nothing.
    Read(Box<dyn FnOnce(&App) + Send>),
    /// Change it (a command).
    Write(Box<dyn FnOnce(&mut App) + Send>),
    /// Background work finished.
    Wake,
}

/// The running app.
#[derive(Clone)]
pub struct Bridge {
    jobs: mpsc::Sender<Job>,
}

impl Bridge {
    /// Start the app, its state in `state_dir`. `changed` is called when
    /// what it shows has changed (the UI asks for the view again); `open`
    /// opens links (the system's browser).
    pub fn start(
        state_dir: PathBuf,
        changed: impl Fn() + Send + 'static,
        open: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let wake = jobs.clone();
        std::thread::Builder::new()
            .name("seadex-core".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to start the tokio runtime");
                let waker = Waker::new(
                    move || {
                        let _ = wake.send(Job::Wake);
                    },
                    open,
                );
                let mut app = App::create(rt.handle().clone(), state_dir, waker);
                app.duties();
                loop {
                    match inbox.recv_timeout(Duration::from_millis(500)) {
                        Ok(Job::Read(f)) => f(&app),
                        Ok(Job::Write(f)) => {
                            f(&mut app);
                            app.duties();
                            changed();
                        }
                        Ok(Job::Wake) => {
                            app.duties();
                            changed();
                        }
                        // Progress (downloads) moves without messages.
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            app.duties();
                            if view::moving(&app) {
                                changed();
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
        f: impl FnOnce(&App) -> T + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Job::Read(Box::new(move |app| {
                let _ = tx.send(f(app));
            })))
            .map_err(|_| "the app has stopped".to_owned())?;
        rx.await.map_err(|_| "the app has stopped".to_owned())
    }

    async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut App) -> T + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Job::Write(Box::new(move |app| {
                let _ = tx.send(f(app));
            })))
            .map_err(|_| "the app has stopped".to_owned())?;
        rx.await.map_err(|_| "the app has stopped".to_owned())
    }

    /// What to show: the page, and what's around it.
    pub async fn view(&self) -> Result<Value, String> {
        self.read(view::view).await
    }

    /// Do something (go somewhere, change a setting, download…).
    pub async fn command(&self, command: Command) -> Result<(), String> {
        self.write(move |app| view::command(app, command)).await?
    }
}
