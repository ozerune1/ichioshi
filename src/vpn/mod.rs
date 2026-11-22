//! Downloading torrents over a VPN.
//!
//! The VPN connection itself is made by the VPN's own app (or a WireGuard or
//! OpenVPN connection set up in the system). This app finds that
//! connection's network interface and the torrent engine binds every socket
//! to it: peers, DHT, trackers. So torrent traffic can only go through the
//! VPN. If the VPN drops, nothing can fall back to the normal connection;
//! the engine stops and torrents wait until it's back.
//!
//! Where the service forwards a port, the engine listens on it, so peers
//! that can't take incoming connections themselves can still connect to us:
//! - PIA: the PIA app gets the port; `piactl` reports it ([`pia`]).
//! - ProtonVPN: NAT-PMP to the VPN gateway, renewed every 45 s ([`natpmp`]).
//! - AirVPN, Windscribe, TorGuard, CyberGhost (dedicated IP) and others:
//!   a fixed port set up on the service's website, entered in Settings.

pub mod natpmp;
pub mod pia;

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Service {
    #[default]
    Pia,
    ProtonVpn,
    Mullvad,
    NordVpn,
    ExpressVpn,
    Surfshark,
    CyberGhost,
    Ivpn,
    Windscribe,
    AirVpn,
    TorGuard,
    Other,
}

/// How a service forwards a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forwarding {
    /// The PIA app does it; `piactl` reports the port.
    PiaApp,
    /// NAT-PMP to this gateway.
    NatPmp(Ipv4Addr),
    /// A fixed port set up on the service's website.
    Manual,
    None,
}

impl Service {
    pub const ALL: [Service; 12] = [
        Service::Pia,
        Service::ProtonVpn,
        Service::Mullvad,
        Service::NordVpn,
        Service::ExpressVpn,
        Service::Surfshark,
        Service::CyberGhost,
        Service::Ivpn,
        Service::Windscribe,
        Service::AirVpn,
        Service::TorGuard,
        Service::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Service::Pia => "PIA",
            Service::ProtonVpn => "ProtonVPN",
            Service::Mullvad => "Mullvad",
            Service::NordVpn => "NordVPN",
            Service::ExpressVpn => "ExpressVPN",
            Service::Surfshark => "Surfshark",
            Service::CyberGhost => "CyberGhost",
            Service::Ivpn => "IVPN",
            Service::Windscribe => "Windscribe",
            Service::AirVpn => "AirVPN",
            Service::TorGuard => "TorGuard",
            Service::Other => "Other",
        }
    }

    pub fn forwarding(self) -> Forwarding {
        match self {
            Service::Pia => Forwarding::PiaApp,
            Service::ProtonVpn => Forwarding::NatPmp(Ipv4Addr::new(10, 2, 0, 1)),
            Service::CyberGhost | Service::Windscribe | Service::AirVpn | Service::TorGuard | Service::Other => {
                Forwarding::Manual
            }
            Service::Mullvad | Service::NordVpn | Service::ExpressVpn | Service::Surfshark | Service::Ivpn => {
                Forwarding::None
            }
        }
    }

    /// How to get a forwarded port with this service.
    pub fn forwarding_help(self) -> String {
        match self {
            Service::Pia => "PIA forwards a port when “Request Port Forwarding” is on in the PIA app \
                             (not in US regions). It's used automatically."
                .to_owned(),
            Service::ProtonVpn => "Connect to a P2P server. With the Proton app, turn on port forwarding; \
                                   with a WireGuard config, turn on NAT-PMP when you create it; with OpenVPN, \
                                   add +pmp to your OpenVPN username. The port is then used automatically."
                .to_owned(),
            Service::CyberGhost => "CyberGhost forwards ports with a dedicated IP. Enter the port here.".to_owned(),
            Service::Windscribe => "Set up port forwarding in your Windscribe account and enter the port here.".to_owned(),
            Service::AirVpn => "Add a port in the Client Area on airvpn.org (Ports) and enter it here.".to_owned(),
            Service::TorGuard => "Request a port in TorGuard's client area and enter it here.".to_owned(),
            Service::Other => "If your VPN forwards a port to you, enter it here.".to_owned(),
            s => format!("{} doesn't offer port forwarding. Downloads still work; fewer peers can connect to you.", s.name()),
        }
    }
}

/// The VPN setting, stored in `<state>/vpn.json`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Torrents only run over the VPN.
    pub enabled: bool,
    pub service: Service,
    /// A particular network interface, or `None` to find the VPN's.
    pub interface: Option<String>,
    /// Ports forwarded on services' websites.
    pub ports: BTreeMap<Service, u16>,
}

impl Config {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::fsutil::write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn manual_port(&self) -> Option<u16> {
        (self.service.forwarding() == Forwarding::Manual).then(|| self.ports.get(&self.service).copied()).flatten()
    }
}

/// A network interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub index: u32,
    pub ipv4: Option<Ipv4Addr>,
    pub up: bool,
    /// A point-to-point link, which is what VPN tunnels are.
    pub tunnel: bool,
    pub loopback: bool,
}

impl Interface {
    /// A mesh network (Tailscale, NetBird, ZeroTier…) rather than a privacy
    /// VPN: never picked automatically.
    pub fn mesh(&self) -> bool {
        let n = self.name.as_str();
        n.starts_with("tailscale")
            || n.starts_with("zt")
            || n.starts_with("ham")
            || n.starts_with("nebula")
            || (n.starts_with("wt") && n[2..].chars().all(|c| c.is_ascii_digit()))
    }

    fn usable(&self) -> bool {
        self.up && self.ipv4.is_some()
    }
}

/// The system's network interfaces.
#[cfg(unix)]
pub fn interfaces() -> Vec<Interface> {
    use std::ffi::CStr;
    let mut list: Vec<Interface> = Vec::new();
    let mut addrs: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `addrs` with a list that is freed below.
    if unsafe { libc::getifaddrs(&mut addrs) } != 0 {
        return list;
    }
    let mut cur = addrs;
    while !cur.is_null() {
        // SAFETY: a node of the list from getifaddrs, alive until freeifaddrs.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        // SAFETY: interface names are NUL-terminated.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned();
        let flags = ifa.ifa_flags as libc::c_int;
        let i = match list.iter().position(|i| i.name == name) {
            Some(i) => i,
            None => {
                let c_name = std::ffi::CString::new(name.clone()).unwrap_or_default();
                // SAFETY: a valid C string.
                let index = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
                list.push(Interface { name, index, ipv4: None, up: false, tunnel: false, loopback: false });
                list.len() - 1
            }
        };
        let entry = &mut list[i];
        entry.up |= flags & libc::IFF_UP != 0;
        entry.tunnel |= flags & libc::IFF_POINTOPOINT != 0;
        entry.loopback |= flags & libc::IFF_LOOPBACK != 0;
        // SAFETY: ifa_addr is null or points to a sockaddr of its family.
        if !ifa.ifa_addr.is_null() && i32::from(unsafe { (*ifa.ifa_addr).sa_family }) == libc::AF_INET {
            // SAFETY: an AF_INET address is a sockaddr_in.
            let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
            entry.ipv4.get_or_insert(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
        }
    }
    // SAFETY: the list from getifaddrs, freed once.
    unsafe { libc::freeifaddrs(addrs) };
    // WireGuard and tun devices have no link-layer address (ARPHRD_NONE).
    #[cfg(target_os = "linux")]
    for i in &mut list {
        let kind = std::fs::read_to_string(format!("/sys/class/net/{}/type", i.name)).unwrap_or_default();
        i.tunnel |= kind.trim() == "65534";
    }
    list
}

#[cfg(not(unix))]
pub fn interfaces() -> Vec<Interface> {
    Vec::new()
}

/// Whether torrents can be bound to a network interface here.
pub const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "android", target_os = "macos"));

/// The local address the system uses to reach the internet. With a
/// full-tunnel VPN, it's the VPN's. No packet is sent.
pub fn route_ip() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(1, 1, 1, 1), 53)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// Find the VPN's interface among `interfaces`, or say why there isn't one.
pub fn pick<'a>(config: &Config, interfaces: &'a [Interface], route: Option<Ipv4Addr>) -> Result<&'a Interface, String> {
    let service = config.service.name();
    if let Some(name) = &config.interface {
        return match interfaces.iter().find(|i| &i.name == name) {
            Some(i) if i.usable() => Ok(i),
            _ => Err(format!("{name} isn't connected")),
        };
    }
    let candidates: Vec<&Interface> = interfaces
        .iter()
        .filter(|i| i.tunnel && !i.loopback && !i.mesh() && i.usable())
        .filter(|i| match config.service {
            // The PIA app's tunnel: wgpia0 for WireGuard, tunN for OpenVPN.
            Service::Pia => i.name.starts_with("wgpia") || i.name.starts_with("tun"),
            _ => true,
        })
        .collect();
    if let Some(i) = candidates.iter().find(|i| i.name.starts_with("wgpia")) {
        return Ok(i);
    }
    if let Some(i) = candidates.iter().find(|i| i.ipv4 == route && route.is_some()) {
        return Ok(i);
    }
    match candidates.as_slice() {
        [] if config.service == Service::Other => Err("No VPN connection found. Connect your VPN first.".to_owned()),
        [] => Err(format!("No {service} connection found. Connect {service} first.")),
        [one] => Ok(one),
        several => Err(format!(
            "Found several VPN connections ({}). Choose one under Network interface in Settings.",
            several.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Where torrents stand with the VPN.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Torrents don't need a VPN.
    Off,
    /// The first check isn't done yet.
    Checking,
    /// No usable VPN connection: torrents wait. The text says why.
    Down { reason: String, fix: Option<Fix> },
    Up { interface: String, index: u32, ip: Ipv4Addr, port: Port, fix: Option<Fix> },
}

/// A forwarded port.
#[derive(Clone, Debug, PartialEq)]
pub enum Port {
    /// None to be had; the text says why.
    None(String),
    /// Being set up; torrents wait a moment for it.
    Requesting,
    /// None for now; torrents run without one. The text says why.
    Unavailable(String),
    Open(u16),
}

/// Something the app can do about the VPN for you.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    PiaConnect,
    PiaForwardPorts,
}

/// Where torrent traffic goes: what the torrent engine needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    /// Bind every torrent socket to this interface.
    pub interface: Option<String>,
    /// The interface's index. A VPN that reconnects comes back as a new
    /// interface (a new index), and the engine must restart to use it.
    pub index: u32,
    /// A forwarded port to listen on.
    pub port: Option<u16>,
    /// No network at all (the VPN is down): bound to loopback, without DHT
    /// or trackers, so torrents from before can be seen but nothing goes out.
    pub offline: bool,
}

impl Network {
    pub fn offline(loopback: &str) -> Self {
        Network { interface: Some(loopback.to_owned()), index: 0, port: None, offline: true }
    }
}

/// The loopback interface's name (`lo`, or `lo0` on macOS).
pub fn loopback() -> Option<String> {
    interfaces().into_iter().find(|i| i.loopback).map(|i| i.name)
}

impl Status {
    /// The network the torrent engine should run on, or why it must wait.
    pub fn network(&self) -> Result<Network, String> {
        match self {
            Status::Off => Ok(Network::default()),
            Status::Checking => Err("Looking for the VPN…".to_owned()),
            Status::Down { reason, .. } => Err(reason.clone()),
            Status::Up { port: Port::Requesting, .. } => Err("Waiting for the forwarded port…".to_owned()),
            Status::Up { interface, index, port, .. } => Ok(Network {
                interface: Some(interface.clone()),
                index: *index,
                port: match port {
                    Port::Open(p) => Some(*p),
                    _ => None,
                },
                offline: false,
            }),
        }
    }

    pub fn fix(&self) -> Option<Fix> {
        match self {
            Status::Down { fix, .. } | Status::Up { fix, .. } => *fix,
            _ => None,
        }
    }
}

/// What the monitor looks at; tests swap in their own.
pub trait Probe: Send + Sync {
    fn interfaces(&self) -> Vec<Interface>;
    fn route_ip(&self) -> Option<Ipv4Addr>;
}

struct System;

impl Probe for System {
    fn interfaces(&self) -> Vec<Interface> {
        interfaces()
    }

    fn route_ip(&self) -> Option<Ipv4Addr> {
        route_ip()
    }
}

/// The monitor's surroundings.
#[derive(Clone)]
pub struct Env {
    pub probe: Arc<dyn Probe>,
    /// The PIA app's `piactl`, if it's installed.
    pub piactl: Option<PathBuf>,
    /// Where NAT-PMP requests go, instead of the service's gateway (tests).
    pub natpmp: Option<SocketAddr>,
    pub poll: Duration,
    /// How long torrents wait for a port being requested before starting
    /// without one.
    pub port_wait: Duration,
}

impl Default for Env {
    fn default() -> Self {
        Self {
            probe: Arc::new(System),
            piactl: pia::find_piactl(),
            natpmp: None,
            poll: Duration::from_secs(3),
            port_wait: Duration::from_secs(20),
        }
    }
}

/// Watches the VPN connection in the background and keeps a [`Status`].
pub struct Monitor {
    config: watch::Sender<Config>,
    status: watch::Receiver<Status>,
    kick: Arc<Notify>,
    env: Env,
}

impl Monitor {
    /// Start watching. `notify` is called whenever the status changes.
    pub fn start(rt: &tokio::runtime::Handle, config: Config, env: Env, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let first = if config.enabled { Status::Checking } else { Status::Off };
        let (status_tx, status) = watch::channel(first);
        let (config, config_rx) = watch::channel(config);
        let kick = Arc::new(Notify::new());
        rt.spawn(run(env.clone(), config_rx, status_tx, kick.clone(), Box::new(notify)));
        Self { config, status, kick, env }
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn set_config(&self, config: Config) {
        self.config.send_replace(config);
    }

    /// Look again now (after connecting, say).
    pub fn check_now(&self) {
        self.kick.notify_one();
    }

    pub fn piactl(&self) -> Option<PathBuf> {
        self.env.piactl.clone()
    }
}

async fn run(
    env: Env,
    mut config_rx: watch::Receiver<Config>,
    status_tx: watch::Sender<Status>,
    kick: Arc<Notify>,
    notify: Box<dyn Fn() + Send + Sync>,
) {
    let mut watch = Watch::default();
    loop {
        let config = config_rx.borrow_and_update().clone();
        let status = if config.enabled { watch.check(&env, &config).await } else { Status::Off };
        // Settings may have changed during the check; look again with them.
        if config_rx.has_changed().unwrap_or(false) {
            continue;
        }
        if *status_tx.borrow() != status {
            tracing::info!("VPN: {status:?}");
            status_tx.send_replace(status);
            notify();
        }
        tokio::select! {
            _ = tokio::time::sleep(env.poll) => {}
            changed = config_rx.changed() => if changed.is_err() { return },
            _ = kick.notified() => {}
        }
    }
}

/// What the monitor remembers between checks.
#[derive(Default)]
struct Watch {
    /// The interface in use, and since when.
    link: Option<(u32, Instant)>,
    natpmp: NatPmp,
}

#[derive(Default)]
struct NatPmp {
    index: u32,
    port: Option<u16>,
    expires: Option<Instant>,
    error: Option<String>,
    next: Option<Instant>,
}

impl Watch {
    async fn check(&mut self, env: &Env, config: &Config) -> Status {
        let service = config.service;
        // PIA: its app says whether it's connected.
        let pia = match (&env.piactl, service) {
            (Some(piactl), Service::Pia) => Some(pia::state(piactl).await),
            _ => None,
        };
        match &pia {
            Some(Ok(state)) if !state.connected() => {
                self.link = None;
                let fix = (state.connection == "Disconnected").then_some(Fix::PiaConnect);
                return Status::Down { reason: format!("PIA is {}", state.connection.to_lowercase()), fix };
            }
            Some(Err(e)) => {
                self.link = None;
                return Status::Down { reason: format!("Couldn't check the PIA app ({e:#})"), fix: None };
            }
            _ => {}
        }

        let interfaces = env.probe.interfaces();
        let interface = match pick(config, &interfaces, env.probe.route_ip()) {
            Ok(i) => i.clone(),
            Err(reason) => {
                self.link = None;
                return Status::Down { reason, fix: None };
            }
        };
        let since = match self.link {
            Some((index, since)) if index == interface.index => since,
            _ => self.link.insert((interface.index, Instant::now())).1,
        };
        let waiting = since.elapsed() < env.port_wait;

        let mut fix = None;
        let port = match service.forwarding() {
            Forwarding::PiaApp => match pia {
                None => Port::None("Port forwarding needs the PIA app".to_owned()),
                Some(Ok(state)) => match state.port {
                    pia::PortForward::Port(p) => Port::Open(p),
                    pia::PortForward::Attempting if waiting => Port::Requesting,
                    pia::PortForward::Attempting => Port::Unavailable("PIA is still asking for a port".to_owned()),
                    pia::PortForward::Inactive => {
                        fix = Some(Fix::PiaForwardPorts);
                        Port::None(if state.request_forwarding {
                            "PIA asks for a port when it next connects".to_owned()
                        } else {
                            "Port forwarding is off in the PIA app".to_owned()
                        })
                    }
                    pia::PortForward::Failed => Port::Unavailable("PIA couldn't get a forwarded port".to_owned()),
                    pia::PortForward::Unavailable => {
                        Port::Unavailable("This PIA region doesn't forward ports (US regions don't)".to_owned())
                    }
                    pia::PortForward::Other(s) => Port::Unavailable(format!("PIA says “{s}”")),
                },
                Some(Err(_)) => unreachable!("handled above"),
            },
            Forwarding::NatPmp(gateway) => {
                let gateway = env.natpmp.unwrap_or(SocketAddr::from((gateway, natpmp::PORT)));
                self.natpmp(gateway, &interface).await
            }
            Forwarding::Manual => match config.manual_port() {
                Some(p) => Port::Open(p),
                None => Port::None("No forwarded port entered".to_owned()),
            },
            Forwarding::None => Port::None(format!("{} doesn't offer port forwarding", service.name())),
        };
        let ip = interface.ipv4.unwrap_or(Ipv4Addr::UNSPECIFIED);
        Status::Up { interface: interface.name, index: interface.index, ip, port, fix }
    }

    /// Get or renew the NAT-PMP mapping when it's due.
    async fn natpmp(&mut self, gateway: SocketAddr, interface: &Interface) -> Port {
        let now = Instant::now();
        let n = &mut self.natpmp;
        if n.index != interface.index {
            *n = NatPmp { index: interface.index, ..Default::default() };
        }
        if n.next.is_none_or(|t| now >= t) {
            match natpmp::map_both(gateway, Some(&interface.name)).await {
                Ok(m) => {
                    n.port = Some(m.public_port);
                    n.expires = Some(now + Duration::from_secs(m.lifetime.into()));
                    n.error = None;
                    n.next = Some(now + natpmp::RENEW_EVERY);
                }
                Err(e) => {
                    n.error = Some(format!("{e:#}"));
                    // Keep a port that hasn't lapsed yet, and try again soon.
                    if n.expires.is_none_or(|t| now >= t) {
                        n.port = None;
                    }
                    n.next = Some(now + Duration::from_secs(if n.port.is_some() { 5 } else { 30 }));
                }
            }
        }
        match (n.port, &n.error) {
            (Some(p), _) => Port::Open(p),
            (None, Some(e)) => Port::Unavailable(format!("No forwarded port ({e}). Use a P2P server with NAT-PMP on")),
            (None, None) => Port::Requesting,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Interfaces a test controls.
    #[derive(Default)]
    pub(crate) struct FakeProbe {
        pub(crate) interfaces: Mutex<Vec<Interface>>,
        pub(crate) route: Mutex<Option<Ipv4Addr>>,
    }

    impl Probe for FakeProbe {
        fn interfaces(&self) -> Vec<Interface> {
            self.interfaces.lock().unwrap().clone()
        }

        fn route_ip(&self) -> Option<Ipv4Addr> {
            *self.route.lock().unwrap()
        }
    }

    pub(crate) fn iface(name: &str, index: u32, ip: [u8; 4], tunnel: bool) -> Interface {
        Interface { name: name.into(), index, ipv4: Some(ip.into()), up: true, tunnel, loopback: name == "lo" }
    }

    fn this_machine() -> Vec<Interface> {
        vec![
            iface("lo", 1, [127, 0, 0, 1], false),
            iface("enp15s0", 2, [192, 168, 1, 20], false),
            iface("tailscale0", 9, [100, 101, 1, 2], true),
            iface("wt0", 10, [100, 90, 3, 4], true),
        ]
    }

    #[test]
    fn picks_the_vpn_interface() {
        let pia = Config { enabled: true, service: Service::Pia, ..Default::default() };
        let other = Config { enabled: true, service: Service::Other, ..Default::default() };
        let home = Some(Ipv4Addr::new(192, 168, 1, 20));

        // Mesh networks are not VPNs for this.
        let mut list = this_machine();
        assert_eq!(pick(&pia, &list, home).unwrap_err(), "No PIA connection found. Connect PIA first.");
        assert_eq!(pick(&other, &list, home).unwrap_err(), "No VPN connection found. Connect your VPN first.");

        // PIA over OpenVPN.
        list.push(iface("tun0", 20, [10, 13, 112, 6], true));
        assert_eq!(pick(&pia, &list, Some(Ipv4Addr::new(10, 13, 112, 6))).unwrap().name, "tun0");
        // PIA over WireGuard wins over another tunnel.
        list.push(iface("wgpia0", 21, [10, 5, 1, 2], true));
        assert_eq!(pick(&pia, &list, home).unwrap().name, "wgpia0");

        // Two tunnels: the one carrying the internet traffic.
        let mut list = this_machine();
        list.push(iface("wg0", 30, [10, 2, 0, 2], true));
        list.push(iface("tun1", 31, [10, 8, 0, 6], true));
        assert_eq!(pick(&other, &list, Some(Ipv4Addr::new(10, 8, 0, 6))).unwrap().name, "tun1");
        assert!(pick(&other, &list, home).unwrap_err().starts_with("Found several VPN connections (wg0, tun1)"));
        // A tunnel without an address, or down, isn't connected.
        list.retain(|i| i.name != "tun1");
        list[4].ipv4 = None;
        assert!(pick(&other, &list, home).is_err());

        // A chosen interface is used as is (even a mesh one), if connected.
        let chosen = Config { interface: Some("tailscale0".into()), ..other.clone() };
        assert_eq!(pick(&chosen, &list, home).unwrap().name, "tailscale0");
        let gone = Config { interface: Some("wg9".into()), ..other };
        assert_eq!(pick(&gone, &list, home).unwrap_err(), "wg9 isn't connected");
    }

    #[test]
    fn lists_this_computers_interfaces() {
        let list = interfaces();
        let lo = list.iter().find(|i| i.loopback).expect("a loopback interface");
        assert_eq!(lo.ipv4, Some(Ipv4Addr::LOCALHOST));
        assert!(lo.index > 0 && lo.up && !lo.tunnel);
    }

    fn env(probe: &Arc<FakeProbe>, piactl: Option<PathBuf>) -> Env {
        Env {
            probe: probe.clone(),
            piactl,
            natpmp: None,
            poll: Duration::from_millis(20),
            port_wait: Duration::from_millis(300),
        }
    }

    async fn until(m: &Monitor, what: &str, done: impl Fn(&Status) -> bool) -> Status {
        for _ in 0..300 {
            let s = m.status();
            if done(&s) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}; status {:?}", m.status());
    }

    #[tokio::test]
    async fn follows_the_pia_app() {
        let dir = tempfile::tempdir().unwrap();
        let piactl = pia::tests::fake_piactl(dir.path(), "Disconnected", "Inactive", false);
        let probe = Arc::new(FakeProbe::default());
        *probe.interfaces.lock().unwrap() = this_machine();
        let config = Config { enabled: true, service: Service::Pia, ..Default::default() };
        let m = Monitor::start(&tokio::runtime::Handle::current(), config.clone(), env(&probe, Some(piactl)), || {});

        let s = until(&m, "down", |s| matches!(s, Status::Down { .. })).await;
        assert_eq!(s, Status::Down { reason: "PIA is disconnected".into(), fix: Some(Fix::PiaConnect) });
        assert_eq!(s.network().unwrap_err(), "PIA is disconnected");

        // Connected, asking for a port: torrents wait for it a moment…
        pia::tests::set(dir.path(), "connectionstate", "Connected");
        pia::tests::set(dir.path(), "portforward", "Attempting");
        probe.interfaces.lock().unwrap().push(iface("tun0", 40, [10, 13, 112, 6], true));
        let s = until(&m, "requesting", |s| matches!(s, Status::Up { .. })).await;
        assert!(matches!(&s, Status::Up { port: Port::Requesting, .. }), "{s:?}");
        assert!(s.network().is_err());
        // …then go ahead without it.
        let s = until(&m, "gave up waiting", |s| matches!(s, Status::Up { port: Port::Unavailable(_), .. })).await;
        assert_eq!(s.network().unwrap(), Network { interface: Some("tun0".into()), index: 40, port: None, offline: false });

        // The port arrives.
        pia::tests::set(dir.path(), "portforward", "47001");
        let s = until(&m, "port", |s| matches!(s, Status::Up { port: Port::Open(_), .. })).await;
        assert_eq!(s.network().unwrap(), Network { interface: Some("tun0".into()), index: 40, port: Some(47001), offline: false });

        // PIA reconnects: a new tunnel.
        probe.interfaces.lock().unwrap().retain(|i| i.name != "tun0");
        pia::tests::set(dir.path(), "connectionstate", "Reconnecting");
        until(&m, "reconnecting", |s| *s == Status::Down { reason: "PIA is reconnecting".into(), fix: None }).await;
        probe.interfaces.lock().unwrap().push(iface("tun0", 41, [10, 13, 9, 9], true));
        pia::tests::set(dir.path(), "connectionstate", "Connected");
        let s = until(&m, "back", |s| matches!(s, Status::Up { index: 41, .. })).await;
        assert_eq!(s.network().unwrap().port, Some(47001));

        // Forwarding off in PIA: offered as a fix.
        pia::tests::set(dir.path(), "portforward", "Inactive");
        let s = until(&m, "no port", |s| matches!(s, Status::Up { port: Port::None(_), .. })).await;
        assert_eq!(s.fix(), Some(Fix::PiaForwardPorts));

        // Turning the VPN requirement off.
        m.set_config(Config { enabled: false, ..config });
        until(&m, "off", |s| *s == Status::Off).await;
    }

    #[tokio::test]
    async fn keeps_a_protonvpn_port_with_nat_pmp() {
        let (gateway, count) = natpmp::tests::gateway(36_000, 0).await;
        let probe = Arc::new(FakeProbe::default());
        // Loopback stands in for the tunnel, so the local gateway is reachable through it.
        let mut lo = iface("lo", 1, [127, 0, 0, 1], false);
        lo.loopback = false;
        lo.tunnel = true;
        *probe.interfaces.lock().unwrap() = vec![lo];
        let mut env = env(&probe, None);
        env.natpmp = Some(gateway);
        let config = Config { enabled: true, service: Service::ProtonVpn, ..Default::default() };
        let m = Monitor::start(&tokio::runtime::Handle::current(), config, env, || {});
        let s = until(&m, "port", |s| matches!(s, Status::Up { .. })).await;
        assert_eq!(s.network().unwrap(), Network { interface: Some("lo".into()), index: 1, port: Some(36_000), offline: false });
        // Not renewed on every check, only every 45 s.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2, "one TCP and one UDP request");
    }

    /// What this computer looks like to the monitor (read-only: interfaces,
    /// the route, and `piactl get`).
    #[tokio::test]
    #[ignore = "depends on this computer's network"]
    async fn this_computer() {
        for i in interfaces() {
            println!("{i:?} mesh={}", i.mesh());
        }
        println!("route: {:?}", route_ip());
        for service in [Service::Pia, Service::Other] {
            let config = Config { enabled: true, service, ..Default::default() };
            let status = Watch::default().check(&Env::default(), &config).await;
            println!("{}: {status:?}", service.name());
        }
    }

    #[test]
    fn manual_ports_only_for_services_that_have_them() {
        let mut c = Config { service: Service::AirVpn, ..Default::default() };
        c.ports.insert(Service::AirVpn, 41_000);
        c.ports.insert(Service::Mullvad, 1);
        assert_eq!(c.manual_port(), Some(41_000));
        c.service = Service::Mullvad;
        assert_eq!(c.manual_port(), None);
    }

    #[test]
    fn config_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpn.json");
        assert_eq!(Config::load(&path), Config::default());
        let mut c = Config { enabled: true, service: Service::ProtonVpn, interface: Some("wg0".into()), ..Default::default() };
        c.ports.insert(Service::AirVpn, 41_000);
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path), c);
    }
}
