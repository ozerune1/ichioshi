//! The installer's window: setup (where it goes and which shortcuts, then
//! installing, then done) and the uninstaller (what's removed, removing,
//! done), in Ichioshi's look.

use std::path::{Path, PathBuf};

use iced::futures::{SinkExt, StreamExt};
use iced::widget::{column, container, image, row, space, text};
use iced::{Alignment, Element, Fill, Task, Theme};

use crate::files::{self, PROGRAM};
use crate::style::{
    BG, BOLD, FAINT, FONTS, GREEN, INTER, MUTED, RED, SEMI, TEXT, bar, card, check, primary,
    secondary,
};
use crate::system::{self, Installed, Shortcuts};
use crate::work;

const ICON: &[u8] = include_bytes!("../../assets/icon/png/ichioshi-128.png");

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Setup,
    Uninstall,
}

#[derive(Clone, Debug, PartialEq)]
enum Page {
    Start,
    /// How far along (0 to 1).
    Working(f32),
    Done,
    Failed(String),
}

#[derive(Clone)]
pub struct Installer {
    mode: Mode,
    page: Page,
    /// Where it goes (or is).
    dir: PathBuf,
    /// Ichioshi as installed before (setup: it's updated in place).
    existing: Option<Installed>,
    start_menu: bool,
    desktop: bool,
    /// Open Ichioshi when setup's done.
    launch: bool,
    /// Uninstalling: its settings and records too.
    remove_data: bool,
    payload: &'static [u8],
    /// Why it can't go ahead yet (Ichioshi's open).
    notice: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Message {
    StartMenu(bool),
    Desktop(bool),
    Launch(bool),
    RemoveData(bool),
    Pick,
    Picked(Option<PathBuf>),
    Go,
    Progress(f32),
    Finished(Result<(), String>),
    Finish,
    Close,
}

impl Installer {
    /// Setup, carrying the app and its uninstaller (`payload`).
    pub fn setup(payload: &'static [u8]) -> Self {
        let existing = system::installed().filter(|i| i.dir.join(PROGRAM).is_file());
        Self {
            mode: Mode::Setup,
            page: Page::Start,
            dir: existing
                .as_ref()
                .map_or_else(files::default_dir, |i| i.dir.clone()),
            existing,
            start_menu: true,
            desktop: false,
            launch: true,
            remove_data: false,
            payload,
            notice: None,
        }
    }

    /// The uninstaller, for Ichioshi in `dir`.
    pub fn uninstall(dir: PathBuf) -> Self {
        Self {
            mode: Mode::Uninstall,
            existing: system::installed(),
            ..Self::setup(&[])
        }
        .with_dir(dir)
    }

    fn with_dir(mut self, dir: PathBuf) -> Self {
        self.dir = dir;
        self
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::StartMenu(on) => self.start_menu = on,
            Message::Desktop(on) => self.desktop = on,
            Message::Launch(on) => self.launch = on,
            Message::RemoveData(on) => self.remove_data = on,
            Message::Pick => return pick_folder(self.dir.clone()),
            Message::Picked(Some(picked)) => self.dir = files::install_dir(&picked),
            Message::Picked(None) => {}
            Message::Go => {
                if files::running(&self.dir) {
                    self.notice = Some("Ichioshi is open: close it, then try again.".into());
                    return Task::none();
                }
                self.notice = None;
                self.page = Page::Working(0.0);
                return self.work();
            }
            Message::Progress(p) => self.page = Page::Working(p),
            Message::Finished(Ok(())) => self.page = Page::Done,
            Message::Finished(Err(e)) => self.page = Page::Failed(e),
            Message::Finish => {
                if self.mode == Mode::Setup && self.launch {
                    let _ = system::launch(&self.dir.join(PROGRAM));
                }
                return iced::exit();
            }
            Message::Close => return iced::exit(),
        }
        Task::none()
    }

    /// Install or uninstall on a thread of its own, saying how far it is.
    fn work(&self) -> Task<Message> {
        let (mode, dir, remove_data) = (self.mode, self.dir.clone(), self.remove_data);
        let payload = self.payload;
        let shortcuts = Shortcuts {
            start_menu: self.start_menu,
            desktop: self.desktop,
        };
        let stream = iced::stream::channel(64, async move |mut out| {
            let (tx, mut rx) = iced::futures::channel::mpsc::unbounded();
            std::thread::spawn(move || {
                let progress = |p| {
                    let _ = tx.unbounded_send(Message::Progress(p));
                };
                let result = match mode {
                    Mode::Setup => work::install(payload, &dir, shortcuts, progress),
                    Mode::Uninstall => work::uninstall(&dir, remove_data, progress),
                };
                let _ = tx.unbounded_send(Message::Finished(result.map_err(|e| format!("{e:#}"))));
            });
            while let Some(m) = rx.next().await {
                let _ = out.send(m).await;
            }
        });
        Task::run(stream, |m| m)
    }

    pub fn title(&self) -> String {
        match self.mode {
            Mode::Setup => "Ichioshi Setup".into(),
            Mode::Uninstall => "Uninstall Ichioshi".into(),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let setup = self.mode == Mode::Setup;
        let (title, subtitle, body, buttons) =
            match &self.page {
                Page::Start if setup => self.setup_start(),
                Page::Start => self.uninstall_start(),
                Page::Working(p) => (
                    if setup {
                        "Installing Ichioshi…"
                    } else {
                        "Removing Ichioshi…"
                    }
                    .to_owned(),
                    self.dir.display().to_string(),
                    column![
                        bar(*p),
                        text(format!("{:.0}%", p * 100.0)).size(13).color(MUTED),
                    ]
                    .spacing(10)
                    .into(),
                    row![].into(),
                ),
                Page::Done if setup => (
                    "Ichioshi is installed".to_owned(),
                    format!("Version {}", crate::VERSION),
                    card(
                        column![
                            text(format!(
                                "✓ {}",
                                where_to_find(self.start_menu, self.desktop)
                            ))
                            .size(14)
                            .color(GREEN),
                            check("Open Ichioshi now", self.launch, Message::Launch),
                        ]
                        .spacing(16),
                    ),
                    row![primary("Finish", Message::Finish, false)].into(),
                ),
                Page::Done => (
                    "Ichioshi is removed".to_owned(),
                    String::new(),
                    card(
                        column![
                        text("✓ Ichioshi, its shortcuts and its entry in Settings › Apps are gone.")
                            .size(14)
                            .color(GREEN),
                        text(if self.remove_data {
                            "Its settings went too. Your videos are still in your library folder."
                        } else {
                            "Your settings are kept, for if you install it again, and your \
                             videos are still in your library folder."
                        })
                        .size(13)
                        .color(MUTED),
                    ]
                        .spacing(10),
                    ),
                    row![primary("Close", Message::Close, false)].into(),
                ),
                Page::Failed(why) => (
                    if setup {
                        "Ichioshi couldn't be installed"
                    } else {
                        "Ichioshi couldn't be removed"
                    }
                    .to_owned(),
                    self.dir.display().to_string(),
                    card(text(why.clone()).size(14).color(RED)),
                    row![
                        secondary("Close", Message::Close, false),
                        primary("Try again", Message::Go, false),
                    ]
                    .spacing(10)
                    .into(),
                ),
            };
        let mut heading = column![text(title).size(24).font(BOLD).color(TEXT)].spacing(4);
        if !subtitle.is_empty() {
            heading = heading.push(text(subtitle).size(13).color(MUTED));
        }
        let header = row![
            image(image::Handle::from_bytes(ICON)).width(56).height(56),
            heading,
        ]
        .spacing(16)
        .align_y(Alignment::Center);
        let mut page = column![header, body].spacing(24);
        if let Some(notice) = &self.notice {
            page = page.push(text(notice.clone()).size(13).color(RED));
        }
        let page = page
            .push(space::vertical())
            .push(row![space::horizontal(), buttons]);
        container(page)
            .padding(28)
            .width(Fill)
            .height(Fill)
            .style(|_| container::Style {
                background: Some(BG.into()),
                text_color: Some(TEXT),
                ..Default::default()
            })
            .into()
    }

    /// Setup's first page: where, which shortcuts.
    fn setup_start(&self) -> (String, String, Element<'_, Message>, Element<'_, Message>) {
        let version = crate::VERSION;
        let (title, subtitle, place): (_, _, Element<'_, Message>) = match &self.existing {
            Some(old) => (
                if old.version == version {
                    "Reinstall Ichioshi"
                } else {
                    "Update Ichioshi"
                },
                if old.version.is_empty() || old.version == version {
                    format!("Version {version}")
                } else {
                    format!("{} → {version}", old.version)
                },
                column![
                    label("Installed in"),
                    text(self.dir.display().to_string()).size(14).color(TEXT),
                    text("Your library, settings and sign-ins stay as they are.")
                        .size(12)
                        .color(FAINT),
                ]
                .spacing(6)
                .into(),
            ),
            None => (
                "Install Ichioshi",
                format!("Version {version} · Download, organize and watch anime"),
                column![
                    label("Install in"),
                    row![
                        text(self.dir.display().to_string())
                            .size(14)
                            .color(TEXT)
                            .width(Fill),
                        secondary("Change…", Message::Pick, true),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                    text("Just for you, so it needs no administrator.")
                        .size(12)
                        .color(FAINT),
                ]
                .spacing(6)
                .into(),
            ),
        };
        let body = card(
            column![
                place,
                column![
                    check(
                        "Add Ichioshi to the Start menu",
                        self.start_menu,
                        Message::StartMenu
                    ),
                    check(
                        "Put a shortcut on the desktop",
                        self.desktop,
                        Message::Desktop
                    ),
                ]
                .spacing(12),
            ]
            .spacing(20),
        );
        let go = match &self.existing {
            Some(old) if old.version != version => "Update",
            Some(_) => "Reinstall",
            None => "Install",
        };
        let buttons = row![
            secondary("Cancel", Message::Close, false),
            primary(go, Message::Go, false),
        ]
        .spacing(10)
        .into();
        (title.to_owned(), subtitle, body, buttons)
    }

    /// The uninstaller's first page: what goes.
    fn uninstall_start(&self) -> (String, String, Element<'_, Message>, Element<'_, Message>) {
        let version = self
            .existing
            .as_ref()
            .map(|i| i.version.clone())
            .filter(|v| !v.is_empty());
        let body =
            card(
                column![
                text("This removes Ichioshi, its Start menu and desktop shortcuts, and its entry \
                      in Settings › Apps, from")
                .size(14)
                .color(TEXT),
                text(self.dir.display().to_string()).size(14).font(SEMI).color(TEXT),
                check(
                    "Also remove its settings, sign-ins and library records",
                    self.remove_data,
                    Message::RemoveData
                ),
                text("Your videos stay in your library folder either way.")
                    .size(12)
                    .color(FAINT),
            ]
                .spacing(12),
            );
        let buttons = row![
            secondary("Cancel", Message::Close, false),
            primary("Uninstall", Message::Go, true),
        ]
        .spacing(10)
        .into();
        (
            "Uninstall Ichioshi".to_owned(),
            version.map(|v| format!("Version {v}")).unwrap_or_default(),
            body,
            buttons,
        )
    }
}

fn label(s: &str) -> iced::widget::Text<'_> {
    text(s).size(12).font(SEMI).color(MUTED)
}

/// Where the installed app is found.
fn where_to_find(start_menu: bool, desktop: bool) -> &'static str {
    match (start_menu, desktop) {
        (true, true) => "Find it in the Start menu and on the desktop.",
        (true, false) => "Find it in the Start menu.",
        (false, true) => "Find it on the desktop.",
        (false, false) => "Find it in the folder it was installed in.",
    }
}

/// Ask for a folder (Windows' own dialog).
fn pick_folder(start: PathBuf) -> Task<Message> {
    #[cfg(windows)]
    {
        let from = start
            .ancestors()
            .find(|p| p.is_dir())
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .set_title("Install Ichioshi in")
                    .set_directory(from)
                    .pick_folder()
                    .await
                    .map(|f| f.path().to_path_buf())
            },
            Message::Picked,
        )
    }
    #[cfg(not(windows))]
    {
        let _: &Path = &start;
        Task::none()
    }
}

/// The window's settings: fixed size, the app's icon.
pub fn window() -> iced::window::Settings {
    let icon = ::image::load_from_memory(ICON).ok().and_then(|i| {
        let i = i.into_rgba8();
        let (w, h) = i.dimensions();
        iced::window::icon::from_rgba(i.into_raw(), w, h).ok()
    });
    iced::window::Settings {
        size: iced::Size::new(620.0, 420.0),
        resizable: false,
        icon,
        ..Default::default()
    }
}

/// Run the window until it's closed.
pub fn run(installer: Installer) -> iced::Result {
    let app = iced::application(
        move || (installer.clone(), Task::none()),
        Installer::update,
        Installer::view,
    )
    .title(Installer::title)
    .theme(|_: &Installer| Theme::Dark)
    .default_font(INTER)
    .window(window());
    FONTS.iter().fold(app, |app, f| app.font(*f)).run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_test::Simulator;

    fn settings() -> iced::Settings {
        iced::Settings {
            default_font: INTER,
            fonts: FONTS
                .iter()
                .map(|f| std::borrow::Cow::Borrowed(*f))
                .collect(),
            ..Default::default()
        }
    }

    /// Each page, drawn (`SETUP_SHOTS=<folder>` keeps the pictures).
    #[test]
    fn draws_every_page() {
        let out = std::env::var_os("SETUP_SHOTS").map_or_else(
            || std::env::temp_dir().join(format!("ichioshi-setup-shots-{}", std::process::id())),
            PathBuf::from,
        );
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let dir = PathBuf::from(r"C:\Users\you\AppData\Local\Programs\Ichioshi");
        let fresh = Installer::setup(b"x").with_dir(dir.clone());
        let update = Installer {
            existing: Some(Installed {
                dir: dir.clone(),
                version: "0.3.3".into(),
            }),
            ..fresh.clone()
        };
        let pages = [
            ("setup", fresh.clone()),
            ("setup-update", update),
            (
                "setup-installing",
                Installer {
                    page: Page::Working(0.42),
                    ..fresh.clone()
                },
            ),
            (
                "setup-done",
                Installer {
                    page: Page::Done,
                    ..fresh.clone()
                },
            ),
            (
                "setup-failed",
                Installer {
                    page: Page::Failed(
                        "couldn't write lib\\gstreamer-1.0\\libgstplayback.dll: \
                                        Access is denied. (os error 5)"
                            .into(),
                    ),
                    ..fresh.clone()
                },
            ),
            (
                "setup-open",
                Installer {
                    notice: Some("Ichioshi is open: close it, then try again.".into()),
                    ..fresh.clone()
                },
            ),
            ("uninstall", Installer::uninstall(dir.clone())),
            (
                "uninstall-removing",
                Installer {
                    page: Page::Working(0.8),
                    ..Installer::uninstall(dir.clone())
                },
            ),
            (
                "uninstall-done",
                Installer {
                    page: Page::Done,
                    ..Installer::uninstall(dir)
                },
            ),
        ];
        for (name, installer) in pages {
            let mut ui =
                Simulator::with_size(settings(), iced::Size::new(620.0, 420.0), installer.view());
            let shot = ui.snapshot(&Theme::Dark).unwrap();
            assert!(shot.matches_image(out.join(name)).unwrap(), "{name}");
        }
    }

    /// Checkboxes and the folder: as clicked.
    #[test]
    fn choices_change_what_it_does() {
        let mut s = Installer::setup(b"x");
        let _ = s.update(Message::Desktop(true));
        let _ = s.update(Message::StartMenu(false));
        assert!(s.desktop && !s.start_menu);
        let picked = std::env::temp_dir().join(format!("ichioshi-pick-{}", std::process::id()));
        std::fs::create_dir_all(&picked).unwrap();
        std::fs::write(picked.join("other.txt"), "x").unwrap();
        let _ = s.update(Message::Picked(Some(picked.clone())));
        assert_eq!(s.dir, picked.join("Ichioshi"));
        let _ = s.update(Message::Finished(Err("no".into())));
        assert_eq!(s.page, Page::Failed("no".into()));
        let _ = std::fs::remove_dir_all(&picked);
    }
}
