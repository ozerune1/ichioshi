//! The VPN in the app: the setting, and keeping the torrent engine on the
//! VPN's connection (stopped while there is none).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{self, RichText, Ui};

use super::{Action, App, EngineState, GREEN, Msg};
use crate::engine::Engine;
use crate::vpn::{self, Config, Env, Fix, Forwarding, Monitor, Network, Port, Service, Status};

pub(super) struct VpnState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) monitor: Monitor,
    /// The forwarded-port field's text.
    port_text: String,
    /// A PIA command is running.
    busy: bool,
    /// The loopback interface, for running torrents offline.
    loopback: Option<String>,
    /// Torrents need the VPN, which is down: why. (The engine runs offline.)
    pub(super) waiting: Option<String>,
}

impl VpnState {
    pub(super) fn load(state_dir: &Path, rt: &tokio::runtime::Handle, ctx: egui::Context) -> Self {
        let file = state_dir.join("vpn.json");
        let config = Config::load(&file);
        let monitor = Monitor::start(rt, config.clone(), Env::default(), move || ctx.request_repaint());
        let port_text = config.manual_port().map(|p| p.to_string()).unwrap_or_default();
        Self { config, file, monitor, port_text, busy: false, loopback: vpn::loopback(), waiting: None }
    }

    fn save(&self) {
        if let Err(e) = self.config.save(&self.file) {
            tracing::warn!("could not save {}: {e:#}", self.file.display());
        }
        self.monitor.set_config(self.config.clone());
    }
}

impl App {
    /// Start the torrent engine, once the VPN allows (right away without one).
    pub(super) fn start_engine(&mut self) {
        self.engine = EngineState::WaitingForVpn(String::new());
        self.engine_net = None;
        self.sync_engine();
    }

    /// Keep the engine in step with the VPN: stop it when the VPN goes, and
    /// (re)start it on the VPN's current connection and port.
    pub(super) fn sync_engine(&mut self) {
        if matches!(self.engine, EngineState::Off) {
            return;
        }
        let status = self.vpn.monitor.status();
        let wanted = match &status {
            // The VPN is down: torrents stay visible with no network at all.
            Status::Down { reason, .. } => {
                if self.vpn.waiting.as_deref() != Some(reason) {
                    self.vpn.waiting = Some(reason.clone());
                }
                match &self.vpn.loopback {
                    Some(lo) => Ok(Network::offline(lo)),
                    None => Err(reason.clone()),
                }
            }
            // Still looking, or about to get a port: wait rather than start twice.
            _ => {
                self.vpn.waiting = None;
                status.network()
            }
        };
        match wanted {
            Err(reason) => match &mut self.engine {
                EngineState::WaitingForVpn(r) => {
                    if *r != reason {
                        *r = reason;
                    }
                }
                _ => self.halt_engine(reason),
            },
            Ok(net) => {
                let waiting = matches!(self.engine, EngineState::WaitingForVpn(_));
                if waiting || self.engine_net.as_ref() != Some(&net) {
                    self.launch_engine(net);
                }
            }
        }
    }

    /// Start the engine on `net`, after stopping the one running.
    fn launch_engine(&mut self, net: Network) {
        let old = self.take_engine();
        let seq = self.engine_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.engine = EngineState::Starting;
        self.engine_net = Some(net.clone());
        if let Some(interface) = &net.interface {
            let port = net.port.map(|p| format!(", listening on port {p}")).unwrap_or_default();
            tracing::info!("torrents go through {interface}{port}");
        }
        let (current, lock) = (self.engine_seq.clone(), self.engine_lock.clone());
        let (state_dir, default_dir) = (self.state_dir.clone(), self.download_dir());
        self.spawn(async move {
            // One start or stop at a time, in order: sessions share files and ports.
            let _turn = lock.lock().await;
            if let Some(old) = old {
                old.stop().await;
            }
            let mut result = Err("superseded".to_owned());
            for attempt in 0..4 {
                if current.load(Ordering::SeqCst) != seq {
                    break;
                }
                match Engine::start(&state_dir, default_dir.clone(), &net).await {
                    Ok(engine) => {
                        result = Ok(Arc::new(engine));
                        break;
                    }
                    Err(e) => {
                        result = Err(format!("{e:#}"));
                        // After a restart, the old session's ports can take a moment to close.
                        if attempt < 3 {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }
            // Superseded while starting (the VPN went, say): stop it again
            // before anything else starts.
            if current.load(Ordering::SeqCst) != seq
                && let Ok(engine) = &result
            {
                engine.stop().await;
            }
            Msg::Engine { seq, result }
        });
    }

    /// Stop the engine until the VPN is back.
    fn halt_engine(&mut self, reason: String) {
        self.engine_seq.fetch_add(1, Ordering::SeqCst);
        self.engine_net = None;
        if let Some(old) = self.take_engine() {
            tracing::info!("VPN gone ({reason}): stopping torrents");
            let lock = self.engine_lock.clone();
            self.rt.spawn(async move {
                let _turn = lock.lock().await;
                old.stop().await;
            });
        }
        self.engine = EngineState::WaitingForVpn(reason);
    }

    fn take_engine(&mut self) -> Option<Arc<Engine>> {
        match std::mem::replace(&mut self.engine, EngineState::Starting) {
            EngineState::Ready(engine) => Some(engine),
            _ => None,
        }
    }

    pub(super) fn handle_engine_msg(&mut self, seq: u64, result: Result<Arc<Engine>, String>) {
        if seq != self.engine_seq.load(Ordering::SeqCst) {
            return; // superseded, and already stopped
        }
        match result {
            Ok(engine) => self.engine = EngineState::Ready(engine),
            Err(e) => {
                self.set_error(format!("Torrent engine failed to start: {e}"));
                self.engine = EngineState::Failed(e);
            }
        }
    }

    pub(super) fn vpn_fix(&mut self, fix: Fix) {
        let Some(piactl) = self.vpn.monitor.piactl() else { return };
        self.vpn.busy = true;
        self.spawn(async move {
            let result = match fix {
                Fix::PiaConnect => vpn::pia::connect(&piactl).await,
                Fix::PiaForwardPorts => vpn::pia::forward_ports(&piactl).await,
            };
            Msg::VpnFixDone(result.map_err(|e| format!("{e:#}")))
        });
    }

    pub(super) fn handle_vpn_fix_done(&mut self, result: Result<(), String>) {
        self.vpn.busy = false;
        match result {
            Ok(()) => self.set_info("Asked PIA to connect"),
            Err(e) => self.set_error(format!("{e}. Open the PIA app and connect there.")),
        }
        self.vpn.monitor.check_now();
    }

    /// The VPN part of the settings menu.
    pub(super) fn vpn_settings(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.strong("VPN");
        if !vpn::SUPPORTED {
            ui.weak("Downloading over a VPN isn't supported on this system yet.");
            return;
        }
        if ui.checkbox(&mut self.vpn.config.enabled, "Only download torrents over a VPN").changed() {
            self.vpn.save();
        }
        if !self.vpn.config.enabled {
            ui.weak("Torrents use your normal connection, or whatever VPN the whole computer is on.");
            return;
        }
        ui.indent("vpn", |ui| {
            ui.horizontal_wrapped(|ui| {
                for s in Service::ALL {
                    if ui.radio_value(&mut self.vpn.config.service, s, s.name()).changed() {
                        self.vpn.port_text = self.vpn.config.manual_port().map(|p| p.to_string()).unwrap_or_default();
                        self.vpn.save();
                    }
                }
            });
            let service = self.vpn.config.service;
            let status = self.vpn.monitor.status();
            vpn_status_line(ui, &status);
            if let Some(fix) = status.fix() {
                let label = match fix {
                    Fix::PiaConnect => "Connect PIA",
                    Fix::PiaForwardPorts => "Turn on port forwarding in PIA (reconnects)",
                };
                if ui.add_enabled(!self.vpn.busy, egui::Button::new(label)).clicked() {
                    actions.push(Action::VpnFix(fix));
                }
            }

            ui.add_space(4.0);
            ui.label(RichText::new("Port forwarding").strong());
            ui.weak(service.forwarding_help());
            if service.forwarding() == Forwarding::Manual {
                ui.horizontal(|ui| {
                    ui.label("Forwarded port");
                    let edit = ui.add(egui::TextEdit::singleline(&mut self.vpn.port_text).desired_width(70.0).hint_text("none"));
                    if edit.changed() {
                        let port = self.vpn.port_text.trim().parse::<u16>().ok().filter(|p| *p > 0);
                        match port {
                            Some(p) => self.vpn.config.ports.insert(service, p),
                            None => self.vpn.config.ports.remove(&service),
                        };
                        self.vpn.save();
                    }
                    if !self.vpn.port_text.trim().is_empty() && self.vpn.config.manual_port().is_none() {
                        ui.colored_label(ui.visuals().error_fg_color, "not a port number");
                    }
                });
            }

            egui::CollapsingHeader::new("Network interface").id_salt("vpn_interface").show(ui, |ui| {
                ui.weak("Torrents are bound to this connection. Automatic finds your VPN's.");
                let mut chosen = self.vpn.config.interface.clone();
                ui.radio_value(&mut chosen, None, "Automatic");
                let mut list = vpn::interfaces();
                list.retain(|i| !i.loopback && i.up && i.ipv4.is_some());
                list.sort_by_key(|i| (!i.tunnel, i.name.clone()));
                if let Some(name) = chosen.clone()
                    && !list.iter().any(|i| i.name == name)
                {
                    ui.radio_value(&mut chosen, Some(name.clone()), format!("{name} (not connected)"));
                }
                for i in &list {
                    let ip = i.ipv4.map(|ip| ip.to_string()).unwrap_or_default();
                    let kind = if i.mesh() {
                        " · mesh network"
                    } else if i.tunnel {
                        " · tunnel"
                    } else {
                        ""
                    };
                    ui.radio_value(&mut chosen, Some(i.name.clone()), format!("{} · {ip}{kind}", i.name));
                }
                if chosen != self.vpn.config.interface {
                    self.vpn.config.interface = chosen;
                    self.vpn.save();
                }
            });
            ui.weak(
                "Every torrent connection (peers, DHT, trackers) is tied to the VPN, so nothing can go out \
                 over your normal connection. If the VPN drops, torrents stop until it's back.",
            );
        });
    }

    /// The VPN's part of the status bar.
    pub(super) fn vpn_status(&self, ui: &mut Ui) {
        let (color, text) = match self.vpn.monitor.status() {
            Status::Off => return,
            Status::Checking => (ui.visuals().text_color(), "VPN: checking…".to_owned()),
            Status::Down { .. } => (ui.visuals().warn_fg_color, "VPN: not connected".to_owned()),
            Status::Up { interface, port: Port::Open(p), .. } => (GREEN, format!("VPN: {interface} · port {p}")),
            Status::Up { interface, .. } => (GREEN, format!("VPN: {interface}")),
        };
        super::status_dot(ui, color);
        ui.label(RichText::new(text).color(color));
    }

    /// The engine is running without any network (the VPN is down).
    #[cfg(test)]
    pub(super) fn engine_offline(&self) -> bool {
        self.engine_net.as_ref().is_some_and(|n| n.offline)
    }

    /// Shown in the downloads panel while torrents wait for the VPN.
    pub(super) fn vpn_banner(&self, ui: &mut Ui, reason: &str, actions: &mut Vec<Action>) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("⏸ Torrents are waiting for the VPN").strong().color(ui.visuals().warn_fg_color));
            if !reason.is_empty() {
                ui.label(reason);
            }
            if self.vpn.monitor.status().fix() == Some(Fix::PiaConnect)
                && ui.add_enabled(!self.vpn.busy, egui::Button::new("Connect PIA")).clicked()
            {
                actions.push(Action::VpnFix(Fix::PiaConnect));
            }
            ui.weak("They continue as soon as it connects. Nothing goes out without it.");
        });
    }
}

fn vpn_status_line(ui: &mut Ui, status: &Status) {
    match status {
        Status::Off => {}
        Status::Checking => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for the VPN…");
            });
        }
        Status::Down { reason, .. } => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Not connected: {reason}"));
        }
        Status::Up { interface, ip, port, .. } => {
            ui.colored_label(GREEN, format!("✔ Connected through {interface} ({ip})"));
            let (color, text) = match port {
                Port::Open(p) => (GREEN, format!("✔ Port {p} forwarded")),
                Port::Requesting => (ui.visuals().text_color(), "Getting a forwarded port…".to_owned()),
                Port::Unavailable(why) | Port::None(why) => (ui.visuals().weak_text_color(), format!("No forwarded port: {why}")),
            };
            ui.colored_label(color, text);
        }
    }
}

#[cfg(test)]
impl App {
    /// Watch the VPN through `env` instead of the real system.
    pub(super) fn use_vpn_env_for_test(&mut self, env: Env) {
        let ctx = self.ctx.clone();
        self.vpn.monitor = Monitor::start(&self.rt, self.vpn.config.clone(), env, move || ctx.request_repaint());
    }
}
