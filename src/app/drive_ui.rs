//! Google Drive in the app: setting up the OAuth client, signing in,
//! choosing the upload folder, and uploading exported playlists.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use eframe::egui::{self, RichText, Ui};

use super::{Action, App, GREEN, Msg, fmt_duration, fmt_speed};
use crate::gdrive::{
    self, Account, Drive, Endpoints, Entry, Folder, OAuthClient, Place, SignInExpired, Uploaded,
};

pub(super) struct DriveState {
    pub(super) account: Account,
    file: PathBuf,
    http: reqwest::Client,
    /// Google's servers (a mock one in tests).
    endpoints: Endpoints,
    pub(super) drive: Option<Arc<Drive>>,
    signing_in: Option<SignInJob>,
    pub(super) browser: Option<FolderBrowser>,
    /// The latest upload. It belongs to the app, not a page, so it keeps
    /// going while you browse.
    upload: Option<UploadRun>,
    seq: u64,
}

struct SignInJob {
    seq: u64,
    cancel: Arc<AtomicBool>,
    /// The consent page, to open again if the browser didn't.
    url: Option<String>,
}

pub(super) struct FolderBrowser {
    /// Where we are: ("Google Drive", Top), ("My Drive", root), ("Anime", ...).
    trail: Vec<(String, Place)>,
    entries: Option<Result<Vec<Entry>, String>>,
    seq: u64,
    link: String,
    checking_link: bool,
    link_error: Option<String>,
}

struct UploadRun {
    seq: u64,
    cancel: Arc<AtomicBool>,
    progress: f32,
    file: PathBuf,
    result: Option<Result<Uploaded, String>>,
    /// The file's size, for the time estimate.
    size: u64,
    /// The last progress report and when it came.
    last: (f32, Instant),
    /// Bytes per second, once a chunk has gone up.
    speed: Option<f64>,
}

/// Closing the app stops an upload in progress.
impl Drop for UploadRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl DriveState {
    pub(super) fn load(state_dir: &Path, user_agent: &str) -> Self {
        let file = state_dir.join("google-drive.json");
        let account = Account::load(&file);
        // Big chunks on slow connections take a while: no overall timeout.
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("failed to build the HTTP client");
        let mut state = Self {
            account,
            file,
            http,
            endpoints: Endpoints::default(),
            drive: None,
            signing_in: None,
            browser: None,
            upload: None,
            seq: 0,
        };
        state.connect();
        state
    }

    /// Make the Drive client from the saved sign-in, if there is one.
    fn connect(&mut self) {
        self.drive = match (&self.account.client, &self.account.refresh_token) {
            (Some(client), Some(token)) => Some(Arc::new(Drive::new(
                self.http.clone(),
                self.endpoints.clone(),
                client.clone(),
                token.clone(),
            ))),
            _ => None,
        };
    }

    fn save(&self) {
        if let Err(e) = self.account.save(&self.file) {
            tracing::warn!("could not save {}: {e:#}", self.file.display());
        }
    }

    fn sign_out_locally(&mut self) {
        self.account.refresh_token = None;
        self.account.email = None;
        self.account.folder = None;
        self.drive = None;
        self.browser = None;
        self.save();
    }

    pub(super) fn ready_to_upload(&self) -> bool {
        self.drive.is_some() && self.account.folder.is_some()
    }

    /// How far along a running upload is.
    pub(super) fn upload_progress(&self) -> Option<f32> {
        self.upload
            .as_ref()
            .filter(|u| u.running())
            .map(|u| u.progress)
    }

    /// A running upload's time left (and speed), counting down between
    /// progress reports.
    pub(super) fn upload_time_left(&self) -> Option<String> {
        self.upload
            .as_ref()
            .filter(|u| u.running())
            .map(|u| u.time_left(Instant::now()))
    }
}

impl UploadRun {
    fn new(seq: u64, cancel: Arc<AtomicBool>, file: PathBuf, size: u64) -> Self {
        Self {
            seq,
            cancel,
            progress: 0.0,
            file,
            result: None,
            size,
            last: (0.0, Instant::now()),
            speed: None,
        }
    }

    fn running(&self) -> bool {
        self.result.is_none()
    }

    /// Note progress reported at `now`. Drive reports once per chunk, so
    /// each report gives the speed over the last chunk.
    fn advance(&mut self, fraction: f32, now: Instant) {
        let (before, at) = self.last;
        let secs = now.saturating_duration_since(at).as_secs_f64();
        let bytes = f64::from((fraction - before).max(0.0)) * self.size as f64;
        if secs > 0.05 && bytes > 0.0 {
            let rate = bytes / secs;
            // Lean on recent chunks so the estimate follows the connection.
            self.speed = Some(self.speed.map_or(rate, |s| s * 0.5 + rate * 0.5));
        }
        self.progress = fraction;
        self.last = (fraction, now);
    }

    /// Seconds left: what's left at the current speed, minus the time since
    /// the last report.
    fn seconds_left(&self, now: Instant) -> Option<f64> {
        let speed = self.speed.filter(|s| *s > 0.0)?;
        let left = f64::from(1.0 - self.progress).max(0.0) * self.size as f64 / speed;
        Some((left - now.saturating_duration_since(self.last.1).as_secs_f64()).max(1.0))
    }

    fn time_left(&self, now: Instant) -> String {
        match (self.seconds_left(now), self.speed) {
            (Some(left), Some(speed)) => format!(
                "about {} left · {}",
                fmt_duration(left),
                fmt_speed(speed / 1_048_576.0)
            ),
            _ => "estimating time left…".to_owned(),
        }
    }
}

impl App {
    pub(super) fn handle_drive_msg(&mut self, msg: Msg) {
        match msg {
            Msg::DriveClientFile(Ok(client)) => {
                self.drive.account.client = Some(client);
                self.drive.sign_out_locally(); // a new client needs a new sign-in
                self.set_info("Google OAuth client loaded. Now sign in.");
            }
            Msg::DriveClientFile(Err(e)) if e == "no file chosen" => {}
            Msg::DriveClientFile(Err(e)) => self.set_error(format!("Couldn't use that file: {e}")),
            Msg::DriveSignInUrl { seq, url } => {
                let Some(job) = &mut self.drive.signing_in else {
                    return;
                };
                if job.seq != seq {
                    return;
                }
                job.url = Some(url.clone());
                self.ctx.open_url(egui::OpenUrl::new_tab(url));
            }
            Msg::DriveSignedIn { seq, result } => {
                // A sign-in that was cancelled (and maybe restarted) is over.
                if self
                    .drive
                    .signing_in
                    .as_ref()
                    .is_none_or(|job| job.seq != seq)
                {
                    return;
                }
                self.drive.signing_in = None;
                match result {
                    Ok((refresh_token, email)) => {
                        self.drive.account.refresh_token = Some(refresh_token);
                        self.drive.account.email = Some(email.clone());
                        self.drive.connect();
                        self.drive.save();
                        self.set_info(format!("Signed in to Google Drive as {email}"));
                        if self.drive.account.folder.is_none() {
                            self.open_folder_browser();
                        }
                    }
                    Err(e) if e == "cancelled" => {}
                    Err(e) => self.set_error(format!("Google sign-in failed: {e}")),
                }
            }
            Msg::DriveFolders {
                seq,
                result,
                expired,
            } => {
                if expired {
                    self.drive_sign_in_expired();
                    return;
                }
                if let Some(b) = &mut self.drive.browser
                    && b.seq == seq
                {
                    b.entries = Some(result);
                }
            }
            Msg::DriveLinkChecked { result, expired } => {
                if expired {
                    self.drive_sign_in_expired();
                    return;
                }
                let Some(b) = &mut self.drive.browser else {
                    return;
                };
                b.checking_link = false;
                match result {
                    Ok(folder) => self.use_drive_folder(folder),
                    Err(e) => b.link_error = Some(e),
                }
            }
            Msg::DriveUploadProgress { seq, fraction } => {
                if let Some(run) = &mut self.drive.upload
                    && run.seq == seq
                {
                    run.advance(fraction, Instant::now());
                }
            }
            Msg::DriveUploadDone {
                seq,
                result,
                expired,
            } => {
                match &mut self.drive.upload {
                    Some(run) if run.seq == seq => run.result = Some(result.clone()),
                    _ => return,
                }
                match result {
                    Ok(file) => self.set_info(format!("Uploaded {} to Google Drive", file.name)),
                    Err(e) if e == "cancelled" => self.set_info("Upload cancelled"),
                    Err(_) if expired => self.drive_sign_in_expired(),
                    Err(e) => self.set_error(format!("Upload failed: {e}")),
                }
            }
            _ => {}
        }
    }

    fn drive_sign_in_expired(&mut self) {
        self.drive.sign_out_locally();
        self.set_error("Google no longer accepts the saved sign-in; sign in to Google Drive again");
    }

    pub(super) fn drive_load_client(&mut self) {
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Google OAuth client file")
                .add_filter("JSON", &["json"])
                .pick_file()
                .await;
            let Some(file) = picked else {
                return Msg::DriveClientFile(Err("no file chosen".into()));
            };
            let result = match tokio::fs::read_to_string(file.path()).await {
                Ok(text) => OAuthClient::from_json(&text).map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            Msg::DriveClientFile(result)
        });
    }

    pub(super) fn drive_sign_in(&mut self) {
        let Some(client) = self.drive.account.client.clone() else {
            return;
        };
        if self.drive.signing_in.is_some() {
            return;
        }
        self.drive.seq += 1;
        let seq = self.drive.seq;
        let cancel = Arc::new(AtomicBool::new(false));
        self.drive.signing_in = Some(SignInJob {
            seq,
            cancel: cancel.clone(),
            url: None,
        });
        let (tx, ctx, http, endpoints) = (
            self.tx.clone(),
            self.ctx.clone(),
            self.drive.http.clone(),
            self.drive.endpoints.clone(),
        );
        self.rt.spawn(async move {
            let (url_tx, url_ctx) = (tx.clone(), ctx.clone());
            let result = gdrive::sign_in(&http, &endpoints, &client, &cancel, move |url| {
                let _ = url_tx.send(Msg::DriveSignInUrl { seq, url });
                url_ctx.request_repaint();
            })
            .await;
            let result = result
                .map(|s| (s.refresh_token, s.email))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::DriveSignedIn { seq, result });
            ctx.request_repaint();
        });
    }

    pub(super) fn drive_cancel_sign_in(&mut self) {
        if let Some(job) = self.drive.signing_in.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn drive_sign_out(&mut self) {
        if let Some(drive) = self.drive.drive.clone() {
            self.rt.spawn(async move { drive.revoke().await });
        }
        self.drive.sign_out_locally();
        self.set_info("Signed out of Google Drive");
    }

    pub(super) fn open_folder_browser(&mut self) {
        self.drive.browser = Some(FolderBrowser {
            trail: vec![("Google Drive".into(), Place::Top)],
            entries: None,
            seq: 0,
            link: String::new(),
            checking_link: false,
            link_error: None,
        });
        self.load_folders();
    }

    /// List the folders where the browser is.
    fn load_folders(&mut self) {
        let Some(drive) = self.drive.drive.clone() else {
            return;
        };
        self.drive.seq += 1;
        let seq = self.drive.seq;
        let Some(b) = &mut self.drive.browser else {
            return;
        };
        b.seq = seq;
        b.entries = None;
        let place = b.trail.last().map(|(_, p)| p.clone()).unwrap_or(Place::Top);
        self.spawn(async move {
            let result = drive.folders(&place).await;
            let expired = result
                .as_ref()
                .is_err_and(|e| e.downcast_ref::<SignInExpired>().is_some());
            Msg::DriveFolders {
                seq,
                result: result.map_err(|e| format!("{e:#}")),
                expired,
            }
        });
    }

    pub(super) fn browse_into(&mut self, entry: Entry) {
        if let Some(b) = &mut self.drive.browser {
            b.trail.push((entry.name, entry.place));
        }
        self.load_folders();
    }

    pub(super) fn browse_back(&mut self, depth: usize) {
        if let Some(b) = &mut self.drive.browser {
            b.trail.truncate(depth + 1);
        }
        self.load_folders();
    }

    pub(super) fn use_current_drive_folder(&mut self) {
        let Some(b) = &self.drive.browser else { return };
        let Some((_, Place::Folder(id))) = b.trail.last() else {
            return;
        };
        let path = b
            .trail
            .iter()
            .skip(1)
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let folder = Folder {
            id: id.clone(),
            path,
        };
        self.use_drive_folder(folder);
    }

    fn use_drive_folder(&mut self, folder: Folder) {
        self.set_info(format!("Uploads will go to {}", folder.path));
        self.drive.account.folder = Some(folder);
        self.drive.browser = None;
        self.drive.save();
    }

    pub(super) fn use_drive_link(&mut self) {
        let Some(drive) = self.drive.drive.clone() else {
            return;
        };
        let Some(b) = &mut self.drive.browser else {
            return;
        };
        let link = b.link.trim().to_owned();
        if link.is_empty() {
            return;
        }
        b.checking_link = true;
        b.link_error = None;
        self.spawn(async move {
            let result = drive.folder_from_link(&link).await;
            let expired = result
                .as_ref()
                .is_err_and(|e| e.downcast_ref::<SignInExpired>().is_some());
            Msg::DriveLinkChecked {
                result: result.map_err(|e| format!("{e:#}")),
                expired,
            }
        });
    }

    pub(super) fn set_auto_upload(&mut self, on: bool) {
        self.drive.account.upload_after_export = on;
        self.drive.save();
    }

    /// Upload a file to the chosen folder in the background.
    pub(super) fn start_upload(&mut self, path: PathBuf) {
        let (Some(drive), Some(folder)) =
            (self.drive.drive.clone(), self.drive.account.folder.clone())
        else {
            return;
        };
        if self.drive.upload.as_ref().is_some_and(UploadRun::running) {
            self.set_error("An upload is already running; wait for it or cancel it first");
            return;
        }
        self.drive.seq += 1;
        let seq = self.drive.seq;
        let cancel = Arc::new(AtomicBool::new(false));
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.drive.upload = Some(UploadRun::new(seq, cancel.clone(), path.clone(), size));
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let send = |msg| {
                let _ = tx.send(msg);
                ctx.request_repaint();
            };
            let mut shown = -1.0f32;
            let result = drive
                .upload(&path, &folder.id, &cancel, |fraction| {
                    if fraction - shown >= 0.005 || fraction >= 1.0 {
                        shown = fraction;
                        send(Msg::DriveUploadProgress { seq, fraction });
                    }
                })
                .await;
            let expired = result
                .as_ref()
                .is_err_and(|e| e.downcast_ref::<SignInExpired>().is_some());
            send(Msg::DriveUploadDone {
                seq,
                result: result.map_err(|e| format!("{e:#}")),
                expired,
            });
        });
    }

    pub(super) fn cancel_upload(&mut self) {
        if let Some(run) = &self.drive.upload {
            run.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The Google Drive part of the playlist page. `export` is the latest
    /// exported zip, if any.
    pub(super) fn drive_section(
        &self,
        ui: &mut Ui,
        export: Option<&PathBuf>,
        actions: &mut Vec<Action>,
    ) {
        let d = &self.drive;
        let upload = d.upload.as_ref();
        ui.add_space(4.0);
        ui.strong("Google Drive");
        let Some(client) = &d.account.client else {
            ui.label("To upload exports, the app needs an OAuth client from your own Google Cloud project (one-time setup).");
            ui.horizontal(|ui| {
                if ui.button("Load client file…").clicked() {
                    actions.push(Action::DriveLoadClient);
                }
                ui.hyperlink_to(
                    "Google Cloud Console ↗",
                    "https://console.cloud.google.com/apis/credentials",
                );
            });
            egui::CollapsingHeader::new("How to get the client file").show(ui, |ui| {
                // Short lines read better than ones spanning a wide window.
                ui.set_max_width(ui.available_width().min(720.0));
                for step in SETUP_STEPS {
                    ui.label(step);
                }
            });
            return;
        };

        if let Some(job) = &d.signing_in {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Finish signing in in your browser…");
                if ui.button("Cancel").clicked() {
                    actions.push(Action::DriveCancelSignIn);
                }
                if let Some(url) = &job.url {
                    ui.hyperlink_to("Open the sign-in page again ↗", url);
                }
            });
            return;
        }

        let Some(email) = d.account.email.as_ref().filter(|_| d.drive.is_some()) else {
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Sign in with Google").strong())
                    .clicked()
                {
                    actions.push(Action::DriveSignIn);
                }
                ui.weak(format!("Client {}", short_client_id(&client.id)));
                if ui.small_button("Change client file…").clicked() {
                    actions.push(Action::DriveLoadClient);
                }
            });
            return;
        };

        let uploading = upload.is_some_and(UploadRun::running);
        ui.add_enabled_ui(!uploading, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Signed in as {email}"));
                if ui.small_button("Sign out").clicked() {
                    actions.push(Action::DriveSignOut);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Upload to");
                match &d.account.folder {
                    Some(folder) => ui.label(RichText::new(&folder.path).strong()),
                    None => ui.colored_label(ui.visuals().warn_fg_color, "no folder chosen"),
                };
                if ui.button("Choose folder…").clicked() {
                    actions.push(Action::DriveChooseFolder);
                }
            });
            let mut auto = d.account.upload_after_export;
            if ui
                .checkbox(&mut auto, "Upload each export to this folder")
                .changed()
            {
                actions.push(Action::DriveSetAutoUpload(auto));
            }
        });

        ui.horizontal(|ui| match upload {
            Some(run) if run.running() => {
                ui.label(format!("Uploading {}", file_name(&run.file)));
                ui.add(
                    egui::ProgressBar::new(run.progress)
                        .desired_width(220.0)
                        .show_percentage(),
                );
                ui.weak(run.time_left(Instant::now()));
                if ui.button("Cancel").clicked() {
                    actions.push(Action::DriveCancelUpload);
                }
                // The estimate counts down between Drive's progress reports.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs(1));
            }
            _ => {
                if let Some(path) = export {
                    let already =
                        upload.is_some_and(|r| r.file == *path && matches!(r.result, Some(Ok(_))));
                    if !already {
                        let button = egui::Button::new(format!("⬆ Upload {}", file_name(path)));
                        if ui.add_enabled(d.account.folder.is_some(), button).clicked() {
                            actions.push(Action::DriveUpload(path.clone()));
                        }
                    }
                }
                match upload.and_then(|r| r.result.as_ref()) {
                    Some(Ok(file)) => {
                        ui.colored_label(GREEN, format!("Uploaded {}", file.name));
                        if let Some(link) = &file.link {
                            ui.hyperlink_to("Open in Drive ↗", link);
                        }
                    }
                    Some(Err(e)) if e != "cancelled" => {
                        ui.colored_label(ui.visuals().error_fg_color, "Upload failed")
                            .on_hover_text(e);
                    }
                    _ if export.is_none() => {
                        ui.weak("Export the playlist to upload it.");
                    }
                    _ => {}
                }
            }
        });
    }

    /// The folder chooser, as a modal window.
    pub(super) fn folder_browser_modal(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(b) = &mut self.drive.browser else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::unique("drive_folders")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Choose the upload folder");
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let last = b.trail.len() - 1;
                for (depth, (name, _)) in b.trail.iter().enumerate() {
                    if depth == last {
                        ui.strong(name);
                    } else {
                        if ui.link(name).clicked() {
                            actions.push(Action::DriveBrowseBack(depth));
                        }
                        ui.weak("›");
                    }
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, true])
                .show(ui, |ui| match &b.entries {
                    None => {
                        ui.spinner();
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    Some(Ok(entries)) if entries.is_empty() => {
                        ui.weak("No folders in here.");
                    }
                    Some(Ok(entries)) => {
                        for entry in entries {
                            if ui
                                .selectable_label(false, format!("🗁 {}", entry.name))
                                .clicked()
                            {
                                actions.push(Action::DriveBrowseInto(entry.clone()));
                            }
                        }
                    }
                });
            ui.separator();
            let here = matches!(b.trail.last(), Some((_, Place::Folder(_))));
            ui.horizontal(|ui| {
                let label = match b.trail.last() {
                    Some((name, Place::Folder(_))) => format!("Upload to “{name}”"),
                    _ => "Open a folder to upload to it".to_owned(),
                };
                if ui
                    .add_enabled(here, egui::Button::new(RichText::new(label).strong()))
                    .clicked()
                {
                    actions.push(Action::DriveUseFolder);
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::DriveCloseBrowser);
                }
            });
            ui.add_space(6.0);
            ui.label("Or paste a folder's link (e.g. one shared with you):");
            ui.horizontal(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut b.link)
                        .hint_text("https://drive.google.com/drive/folders/…")
                        .desired_width(380.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui
                    .add_enabled(!b.checking_link, egui::Button::new("Use link"))
                    .clicked()
                    || enter)
                    && !b.link.trim().is_empty()
                {
                    actions.push(Action::DriveUseLink);
                }
                if b.checking_link {
                    ui.spinner();
                }
            });
            if let Some(e) = &b.link_error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        });
        if modal.should_close() {
            actions.push(Action::DriveCloseBrowser);
        }
    }
}

/// The one-time Google Cloud setup, shown until a client file is loaded.
pub(super) const SETUP_STEPS: [&str; 7] = [
    "1. In Google Cloud Console, create a project (any name).",
    "2. APIs & Services → Library: search for “Google Drive API” and click Enable.",
    "3. Google Auth Platform → Branding: enter an app name (anything) and your email.",
    "4. Google Auth Platform → Audience: choose External, then click “Publish app”. \
     Without publishing, Google signs you out every 7 days.",
    "5. Google Auth Platform → Clients → Create client: choose “Desktop app”, create it, and download its JSON file.",
    "6. Click “Load client file…” above, choose that JSON file, then sign in.",
    "7. Google will say it hasn't verified the app. That's expected for an app you set up yourself: \
     click Advanced, then “Go to (your app name) (unsafe)”, and allow access to Drive.",
];

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// "1234…apps.googleusercontent.com" → "1234…".
fn short_client_id(id: &str) -> String {
    let head: String = id.chars().take(12).collect();
    format!("{head}…")
}

#[cfg(test)]
pub(super) fn browser_for_test(
    state: &mut DriveState,
    trail: &[(&str, Place)],
    entries: Vec<Entry>,
) {
    state.browser = Some(FolderBrowser {
        trail: trail
            .iter()
            .map(|(n, p)| ((*n).to_owned(), p.clone()))
            .collect(),
        entries: Some(Ok(entries)),
        seq: 0,
        link: String::new(),
        checking_link: false,
        link_error: None,
    });
}

#[cfg(test)]
impl DriveState {
    pub(super) fn use_endpoints_for_test(&mut self, endpoints: Endpoints) {
        self.endpoints = endpoints;
        self.connect();
    }

    pub(super) fn upload_result_for_test(&self) -> Option<&Result<Uploaded, String>> {
        self.upload.as_ref().and_then(|u| u.result.as_ref())
    }

    /// Upload in `chunk`-byte pieces (Drive reports progress per chunk).
    pub(super) fn use_chunk_for_test(&mut self, chunk: usize) {
        self.connect();
        let drive = self.drive.take().map(|d| {
            Arc::try_unwrap(d)
                .ok()
                .expect("just made")
                .with_chunk(chunk)
        });
        self.drive = drive.map(Arc::new);
    }
}

#[cfg(test)]
pub(super) fn signed_in_for_test(state: &mut DriveState, email: &str, folder: Option<Folder>) {
    state.account.client = Some(OAuthClient {
        id: "test.apps.googleusercontent.com".into(),
        secret: "s".into(),
    });
    state.account.refresh_token = Some("rt".into());
    state.account.email = Some(email.into());
    state.account.folder = folder;
    state.connect();
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn estimates_time_left_from_each_chunk() {
        let start = Instant::now();
        let mut run = UploadRun::new(1, Arc::default(), "x.zip".into(), 100 << 20);
        run.last.1 = start;
        assert_eq!(run.time_left(start), "estimating time left…");

        // 10 MiB in 5 s: 2 MiB/s, 90 MiB to go.
        run.advance(0.1, start + Duration::from_secs(5));
        assert_eq!(
            run.seconds_left(start + Duration::from_secs(5))
                .map(f64::round),
            Some(45.0)
        );
        assert_eq!(
            run.time_left(start + Duration::from_secs(5)),
            "about 45 s left · 2.0 MiB/s"
        );
        // It counts down until the next report…
        assert_eq!(
            run.seconds_left(start + Duration::from_secs(15))
                .map(f64::round),
            Some(35.0)
        );
        // …and never claims to be done before Drive says so.
        assert_eq!(
            run.seconds_left(start + Duration::from_secs(500)),
            Some(1.0)
        );

        // A slower chunk (10 MiB in 10 s) pulls the speed down: 1.5 MiB/s.
        run.advance(0.2, start + Duration::from_secs(15));
        assert_eq!(
            run.speed.map(|s| (s / 1_048_576.0 * 100.0).round() / 100.0),
            Some(1.5)
        );
        // 80 MiB at 1.5 MiB/s.
        assert_eq!(
            run.time_left(start + Duration::from_secs(15)),
            "about 53 s left · 1.5 MiB/s"
        );
    }
}
