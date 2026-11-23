//! The VPN in the app: the setting, and keeping the torrent engine on the
//! VPN's connection (stopped while there is none).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{App, EngineState, Msg};
use crate::engine::Engine;
use crate::vpn::{self, Config, Env, Fix, Monitor, Network, Status};

pub(super) struct VpnState {
    pub(super) config: Config,
    file: PathBuf,
    pub(super) monitor: Monitor,
    /// The forwarded-port field's text.
    pub(super) port_text: String,
    /// A PIA command is running.
    pub(super) busy: bool,
    /// The loopback interface, for running torrents offline.
    loopback: Option<String>,
    /// Torrents need the VPN, which is down: why. (The engine runs offline.)
    pub(super) waiting: Option<String>,
}

impl VpnState {
    pub(super) fn load(state_dir: &Path, rt: &tokio::runtime::Handle, ctx: super::Waker) -> Self {
        let file = state_dir.join("vpn.json");
        let config = Config::load(&file);
        let monitor = Monitor::start(rt, config.clone(), Env::default(), move || {
            ctx.request_repaint()
        });
        let port_text = config
            .manual_port()
            .map(|p| p.to_string())
            .unwrap_or_default();
        Self {
            config,
            file,
            monitor,
            port_text,
            busy: false,
            loopback: vpn::loopback(),
            waiting: None,
        }
    }

    pub(super) fn save(&self) {
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
            let port = net
                .port
                .map(|p| format!(", listening on port {p}"))
                .unwrap_or_default();
            tracing::info!("torrents go through {interface}{port}");
        }
        let (current, lock) = (self.engine_seq.clone(), self.engine_lock.clone());
        let (state_dir, default_dir) = (self.state_dir.clone(), self.library_dir());
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
        let Some(piactl) = self.vpn.monitor.piactl() else {
            return;
        };
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

    /// The engine is running without any network (the VPN is down).
    #[cfg(test)]
    pub(super) fn engine_offline(&self) -> bool {
        self.engine_net.as_ref().is_some_and(|n| n.offline)
    }
}

#[cfg(test)]
impl App {
    /// Watch the VPN through `env` instead of the real system.
    pub(super) fn use_vpn_env_for_test(&mut self, env: Env) {
        let ctx = self.ctx.clone();
        self.vpn.monitor = Monitor::start(&self.rt, self.vpn.config.clone(), env, move || {
            ctx.request_repaint()
        });
    }
}
