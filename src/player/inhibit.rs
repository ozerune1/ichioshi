//! Keeping the screen on while a video plays, through
//! org.freedesktop.ScreenSaver (hypridle, KDE, GNOME, Xfce...). Best effort:
//! without the service, nothing happens.

use std::sync::mpsc::{Sender, channel};

pub struct Inhibitor {
    tx: Option<Sender<bool>>,
    on: bool,
}

impl Inhibitor {
    pub fn new() -> Self {
        Self {
            tx: None,
            on: false,
        }
    }

    /// Keep the screen on, or let it sleep again.
    pub fn set(&mut self, on: bool) {
        if on == self.on {
            return;
        }
        self.on = on;
        let tx = self.tx.get_or_insert_with(|| {
            let (tx, rx) = channel::<bool>();
            std::thread::Builder::new()
                .name("screensaver".into())
                .spawn(move || {
                    let conn = match zbus::blocking::Connection::session() {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::debug!("no session bus to keep the screen on: {e}");
                            return;
                        }
                    };
                    let mut held: Option<(&str, u32)> = None;
                    for on in rx {
                        match (on, held) {
                            (true, None) => held = inhibit(&conn),
                            (false, Some((path, cookie))) => {
                                let _ = conn.call_method(
                                    Some("org.freedesktop.ScreenSaver"),
                                    path,
                                    Some("org.freedesktop.ScreenSaver"),
                                    "UnInhibit",
                                    &(cookie,),
                                );
                                held = None;
                            }
                            _ => {}
                        }
                    }
                })
                .expect("starting a thread");
            tx
        });
        let _ = tx.send(on);
    }
}

fn inhibit(conn: &zbus::blocking::Connection) -> Option<(&'static str, u32)> {
    for path in ["/org/freedesktop/ScreenSaver", "/ScreenSaver"] {
        let reply = conn.call_method(
            Some("org.freedesktop.ScreenSaver"),
            path,
            Some("org.freedesktop.ScreenSaver"),
            "Inhibit",
            &("SeaDex Downloader", "Playing a video"),
        );
        match reply.and_then(|m| m.body().deserialize::<u32>()) {
            Ok(cookie) => return Some((path, cookie)),
            Err(e) => tracing::debug!("keeping the screen on ({path}): {e}"),
        }
    }
    None
}

impl Drop for Inhibitor {
    fn drop(&mut self) {
        self.set(false);
    }
}
