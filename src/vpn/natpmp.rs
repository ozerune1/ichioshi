//! NAT-PMP (RFC 6886), which ProtonVPN uses for port forwarding: ask the
//! VPN's gateway for a public port, then renew it before it lapses.
//!
//! See https://protonvpn.com/support/port-forwarding-manual-setup: the
//! gateway is 10.2.0.1, mappings last 60 seconds and are renewed every 45,
//! and the public port it gives is the port to listen on.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::UdpSocket;

/// NAT-PMP's port on the gateway.
pub const PORT: u16 = 5351;
/// How long each mapping is asked for.
pub const LIFETIME: u32 = 60;
/// How often mappings are renewed.
pub const RENEW_EVERY: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Udp = 1,
    Tcp = 2,
}

/// A port the gateway forwards to us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub public_port: u16,
    pub lifetime: u32,
}

/// Map a port for both TCP and UDP (BitTorrent uses both) and return the
/// public port, which is the one to listen on.
pub async fn map_both(gateway: SocketAddr, device: Option<&str>) -> Result<Mapping> {
    let udp = map(gateway, device, Protocol::Udp).await?;
    let tcp = map(gateway, device, Protocol::Tcp).await?;
    if udp.public_port != tcp.public_port {
        tracing::warn!(
            "NAT-PMP gave UDP port {} but TCP port {}",
            udp.public_port,
            tcp.public_port
        );
    }
    Ok(tcp)
}

/// Ask the gateway for a mapping, the way Proton's guide does with
/// `natpmpc -a 1 0 <protocol> 60`: no particular private port, any public one.
pub async fn map(gateway: SocketAddr, device: Option<&str>, protocol: Protocol) -> Result<Mapping> {
    let socket = socket(device)?;
    socket
        .connect(gateway)
        .await
        .with_context(|| format!("could not reach {gateway}"))?;
    let request = request(protocol, 0, 1, LIFETIME);
    // RFC 6886: resend after 250 ms, doubling each time.
    let mut wait = Duration::from_millis(250);
    for _ in 0..4 {
        socket
            .send(&request)
            .await
            .context("could not send the NAT-PMP request")?;
        let mut buf = [0u8; 16];
        let deadline = tokio::time::Instant::now() + wait;
        while let Ok(got) = tokio::time::timeout_at(deadline, socket.recv(&mut buf)).await {
            let n = got.context("NAT-PMP reply failed")?;
            if let Some(reply) = parse(&buf[..n], protocol) {
                return reply;
            }
        }
        wait *= 2;
    }
    bail!("the VPN gateway didn't answer NAT-PMP requests")
}

/// A UDP socket that can only send through `device`.
fn socket(device: Option<&str>) -> Result<UdpSocket> {
    use socket2::{Domain, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(socket2::Protocol::UDP))?;
    if let Some(device) = device {
        bind_device(&socket, device)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
    Ok(UdpSocket::from_std(socket.into())?)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn bind_device(socket: &socket2::Socket, device: &str) -> Result<()> {
    socket
        .bind_device(Some(device.as_bytes()))
        .with_context(|| format!("could not use {device}"))
}

#[cfg(target_os = "macos")]
fn bind_device(socket: &socket2::Socket, device: &str) -> Result<()> {
    let name = std::ffi::CString::new(device)?;
    // SAFETY: a valid C string.
    let index = std::num::NonZeroU32::new(unsafe { libc::if_nametoindex(name.as_ptr()) })
        .with_context(|| format!("no network interface {device}"))?;
    socket
        .bind_device_by_index_v4(Some(index))
        .with_context(|| format!("could not use {device}"))
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn bind_device(_socket: &socket2::Socket, _device: &str) -> Result<()> {
    bail!("binding to a network interface isn't supported on this system")
}

fn request(protocol: Protocol, private_port: u16, public_port: u16, lifetime: u32) -> [u8; 12] {
    let mut r = [0u8; 12];
    r[1] = protocol as u8;
    r[4..6].copy_from_slice(&private_port.to_be_bytes());
    r[6..8].copy_from_slice(&public_port.to_be_bytes());
    r[8..12].copy_from_slice(&lifetime.to_be_bytes());
    r
}

/// A reply to our request, or `None` if the packet is something else.
fn parse(reply: &[u8], protocol: Protocol) -> Option<Result<Mapping>> {
    if reply.len() < 16 || reply[0] != 0 || reply[1] != 128 + protocol as u8 {
        return None;
    }
    let code = u16::from_be_bytes([reply[2], reply[3]]);
    let public_port = u16::from_be_bytes([reply[10], reply[11]]);
    let lifetime = u32::from_be_bytes([reply[12], reply[13], reply[14], reply[15]]);
    Some(match code {
        0 if public_port != 0 => Ok(Mapping {
            public_port,
            lifetime,
        }),
        0 => Err(anyhow::anyhow!("the VPN gateway gave no port")),
        2 => Err(anyhow::anyhow!("the VPN gateway refused to forward a port")),
        3 => Err(anyhow::anyhow!("the VPN gateway has a network problem")),
        4 => Err(anyhow::anyhow!("the VPN gateway has no ports left")),
        code => Err(anyhow::anyhow!(
            "the VPN gateway answered with error {code}"
        )),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A NAT-PMP gateway on localhost that hands out `port` and counts requests.
    /// It ignores the first `drop_first` requests, like a lossy link.
    pub(crate) async fn gateway(port: u16, drop_first: u32) -> (SocketAddr, Arc<AtomicU32>) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let count = Arc::new(AtomicU32::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            while let Ok((n, from)) = socket.recv_from(&mut buf).await {
                let nth = seen.fetch_add(1, Ordering::SeqCst);
                assert_eq!(n, 12, "request size");
                assert_eq!(
                    &buf[4..8],
                    &[0, 0, 0, 1],
                    "private port 0, public port 1, as natpmpc -a 1 0"
                );
                assert_eq!(&buf[8..12], &60u32.to_be_bytes(), "60 s lifetime");
                if nth < drop_first {
                    continue;
                }
                let mut r = [0u8; 16];
                r[1] = 128 + buf[1];
                r[4..8].copy_from_slice(&1234u32.to_be_bytes());
                r[10..12].copy_from_slice(&port.to_be_bytes());
                r[12..16].copy_from_slice(&60u32.to_be_bytes());
                // A stray packet first, which must be ignored.
                socket.send_to(&[0, 129, 0, 0], from).await.unwrap();
                socket.send_to(&r, from).await.unwrap();
            }
        });
        (addr, count)
    }

    #[tokio::test]
    async fn maps_tcp_and_udp() {
        let (gw, count) = gateway(51_234, 0).await;
        let m = map_both(gw, None).await.unwrap();
        assert_eq!(
            m,
            Mapping {
                public_port: 51_234,
                lifetime: 60
            }
        );
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn resends_when_a_request_is_lost() {
        let (gw, count) = gateway(40_000, 2).await;
        assert_eq!(
            map(gw, None, Protocol::Tcp).await.unwrap().public_port,
            40_000
        );
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn binds_to_the_interface() {
        // Bound to loopback, the local gateway is reachable.
        let (gw, _) = gateway(40_001, 0).await;
        if cfg!(target_os = "linux") {
            assert_eq!(
                map(gw, Some("lo"), Protocol::Udp)
                    .await
                    .unwrap()
                    .public_port,
                40_001
            );
            assert!(map(gw, Some("no-such-if0"), Protocol::Udp).await.is_err());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_when_nothing_answers() {
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let err = map(silent.local_addr().unwrap(), None, Protocol::Udp)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the VPN gateway didn't answer NAT-PMP requests"
        );
    }

    #[test]
    fn reads_errors() {
        let mut r = [0u8; 16];
        r[1] = 130;
        r[3] = 2;
        assert_eq!(
            parse(&r, Protocol::Tcp).unwrap().unwrap_err().to_string(),
            "the VPN gateway refused to forward a port"
        );
        assert!(
            parse(&r, Protocol::Udp).is_none(),
            "a reply to the other protocol"
        );
        assert!(parse(&r[..8], Protocol::Tcp).is_none());
    }
}
