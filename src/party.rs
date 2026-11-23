//! Watch parties: friends watch an episode together, each from their own
//! copy on disk, the host's player leading.
//!
//! Members reach the host over iroh (it finds a way through home routers,
//! through a relay if it must). Only the party's own small messages go that
//! way: who's there, whether they have the episode, and where the host's
//! player is. Never the video: everyone has the episode before it starts,
//! and a member who's downloading it can get it from the party over
//! BitTorrent (`party_seed`), never through iroh.
//!
//! Messages are JSON, a line each, on one QUIC stream per member.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::mpsc;

/// The protocol, in the connection's handshake (a different version doesn't
/// connect).
pub const ALPN: &[u8] = b"ichioshi/party/1";

/// The longest message read.
const MAX_LINE: u64 = 1 << 20;

/// Milliseconds on this app's clock (since it first asked).
pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The characters a party code is made of: Crockford's base 32, which
/// leaves out look-alikes (I, L, O and U) so it's easy to say and type.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// How long a party code is.
pub const CODE_LEN: usize = 4;

/// Joining a party: its code (four characters, about a million of them),
/// and what the code stands for, worked out from it: the host's iroh key
/// (so a member can look the host up by it) and the party's secret.
/// Working them out is slow on purpose, so guessing codes is slow too.
/// Anyone with the code can join, so it's shared only with friends.
#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    pub code: String,
    key: [u8; 32],
    pub secret: [u8; 16],
}

impl Invite {
    /// A new party's: a random code.
    pub fn random() -> Self {
        use ring::rand::SecureRandom;
        let mut bytes = [0u8; CODE_LEN];
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .expect("the system's random numbers");
        let code = bytes
            .iter()
            .map(|b| ALPHABET[(*b & 31) as usize] as char)
            .collect::<String>();
        Self::from_code(&code)
    }

    /// What a code (as `normalize` gives it) stands for. (Slow: a tenth of
    /// a second or so.)
    pub fn from_code(code: &str) -> Self {
        let mut out = [0u8; 48];
        ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            std::num::NonZeroU32::new(100_000).unwrap(),
            b"ichioshi watch party 1",
            code.as_bytes(),
            &mut out,
        );
        Self {
            code: code.to_owned(),
            key: out[..32].try_into().unwrap(),
            secret: out[32..].try_into().unwrap(),
        }
    }

    /// A code as typed: spaces and dashes left out, any case, and the
    /// look-alikes read as what they look like (O as 0, I and L as 1).
    pub fn normalize(text: &str) -> Result<String, String> {
        let code: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| match c.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            })
            .collect();
        if code.chars().count() == CODE_LEN && code.bytes().all(|b| ALPHABET.contains(&b)) {
            Ok(code)
        } else {
            Err(format!(
                "A watch party code is {CODE_LEN} letters and numbers"
            ))
        }
    }

    /// Read a code as typed.
    #[cfg(test)]
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::normalize(text).map(|c| Self::from_code(&c))
    }

    /// The host's iroh key.
    fn host_key(&self) -> iroh::SecretKey {
        iroh::SecretKey::from_bytes(&self.key)
    }

    /// The host's iroh ID: what members look up.
    pub fn host_id(&self) -> iroh::EndpointId {
        self.host_key().public()
    }
}

/// A party member's number (the host is 0).
pub type MemberId = u32;
pub const HOST: MemberId = 0;

/// The episode the party's watching: series (its AniList ID) and number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Episode {
    pub series: u64,
    pub episode: u32,
    /// The series' title, as the host has it.
    pub title: String,
}

/// Whether a member has the episode on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Have {
    #[default]
    Missing,
    /// Downloading it (how much is here, if known: 0 to 1).
    Downloading(Option<f32>),
    Ready,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    pub name: String,
    pub have: Have,
}

/// Where the host's player is: `position` seconds in at `at` (on the
/// host's clock, `now_ms`), playing or paused.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Playback {
    pub position: f64,
    pub playing: bool,
    pub at: u64,
}

impl Playback {
    /// Where it is at `t` (the same clock as `at`).
    pub fn position_at(&self, t: u64) -> f64 {
        if self.playing {
            self.position + t.saturating_sub(self.at) as f64 / 1000.0
        } else {
            self.position
        }
    }
}

/// A member downloading the party's episode with BitTorrent: whoever has
/// it may seed it to them, and only to them (`crate::party_seed`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wanted {
    /// Who wants it (the host says).
    pub member: MemberId,
    pub series: u64,
    pub episode: u32,
    /// The .torrent (base64).
    pub torrent: String,
    /// Its file that's the episode.
    pub file: usize,
    /// Their BitTorrent ID (hex): the seeder lets only it in.
    pub peer_id: String,
}

/// What a member asks the host's player to do (only the host controls it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    Pause,
    Play,
}

/// From the host to a member.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToMember {
    /// You're in (as member `you`).
    Welcome {
        you: MemberId,
    },
    Members(Vec<Member>),
    /// The episode chosen (none yet).
    Episode(Option<Episode>),
    /// The host's player (none: not playing).
    Playback(Option<Playback>),
    /// A member asked the host's player to do something.
    Asked {
        name: String,
        ask: Ask,
    },
    /// A member's downloading the episode with BitTorrent: seed it to
    /// them, if you have it.
    Wanted(Wanted),
    /// Members seeding the episode to you: connect to them.
    Seeders(Vec<std::net::SocketAddr>),
    /// For the clock: the host's time when a ping arrived.
    Pong {
        nonce: u64,
        at: u64,
    },
    /// The party's over.
    Ended(String),
}

/// From a member to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToHost {
    Hello {
        secret: [u8; 16],
        name: String,
    },
    /// Whether you have an episode (the one chosen, unless it's just
    /// changed).
    Have {
        series: u64,
        episode: u32,
        have: Have,
    },
    Ask(Ask),
    Ping {
        nonce: u64,
    },
    /// You're downloading the episode with BitTorrent.
    Torrenting(Wanted),
    /// You're seeding it to member `to`, there.
    Seeding {
        to: MemberId,
        addrs: Vec<std::net::SocketAddr>,
    },
}

/// Tests stay on this machine (unless one says to use the relays).
pub(crate) fn local() -> bool {
    (cfg!(test) && std::env::var_os("ICHIOSHI_PARTY_RELAY").is_none())
        || std::env::var_os("ICHIOSHI_PARTY_LOCAL").is_some()
}

/// Where local parties (tests, the self-test) are found: in this process.
fn local_lookup() -> &'static iroh::address_lookup::MemoryLookup {
    static LOOKUP: OnceLock<iroh::address_lookup::MemoryLookup> = OnceLock::new();
    LOOKUP.get_or_init(Default::default)
}

/// The party's connection: iroh's free public relays (to find a way
/// between home networks) and its public lookup, where a host (`key`,
/// worked out from the code) publishes which relay it's on (`publish`;
/// never its IP addresses) and members look that up. Nothing to set up.
pub async fn endpoint(key: Option<iroh::SecretKey>, publish: bool) -> anyhow::Result<Endpoint> {
    use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver};
    let mut builder = Endpoint::builder(presets::Minimal).alpns(vec![ALPN.to_vec()]);
    if let Some(key) = key {
        builder = builder.secret_key(key);
    }
    let builder = if local() {
        builder
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr("127.0.0.1:0")
            .map_err(|e| anyhow!("{e}"))?
            .address_lookup(local_lookup().clone())
    } else {
        let builder = builder
            .relay_mode(iroh::endpoint::default_relay_mode())
            .address_lookup(PkarrResolver::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns());
        if publish {
            builder.address_lookup(PkarrPublisher::n0_dns())
        } else {
            builder
        }
    };
    builder
        .bind()
        .await
        .map_err(|e| anyhow!("couldn't start the party's connection: {e}"))
}

/// Whether someone's on `id` already (a party with that code).
async fn taken(looker: Option<&Endpoint>, id: iroh::EndpointId) -> bool {
    use n0_future::StreamExt;
    let Some(looker) = looker else {
        return local_lookup().get_endpoint_info(id).is_some();
    };
    let Ok(lookup) = looker.address_lookup() else {
        return false;
    };
    let mut found = lookup.resolve(id);
    matches!(
        tokio::time::timeout(Duration::from_secs(3), found.next()).await,
        Ok(Some(Ok(Ok(_))))
    )
}

/// A new party's code: one nobody's using.
async fn free_code() -> anyhow::Result<Invite> {
    let looker = if local() {
        None
    } else {
        Some(endpoint(None, false).await?)
    };
    for _ in 0..5 {
        let invite = tokio::task::spawn_blocking(Invite::random).await?;
        if !taken(looker.as_ref(), invite.host_id()).await {
            if let Some(l) = looker {
                l.close().await;
            }
            return Ok(invite);
        }
    }
    bail!("couldn't find a free party code")
}

fn same_secret(a: &[u8; 16], b: &[u8; 16]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn send_line<T: Serialize>(send: &mut SendStream, msg: &T) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    send.write_all(&line).await?;
    Ok(())
}

/// The next message (none: the stream ended).
async fn read_line<T: for<'de> Deserialize<'de>>(
    recv: &mut BufReader<RecvStream>,
) -> anyhow::Result<Option<T>> {
    let mut line = Vec::new();
    let n = recv.take(MAX_LINE).read_until(b'\n', &mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        bail!("a message too long, or cut off");
    }
    Ok(Some(
        serde_json::from_slice(&line).context("a message it couldn't read")?,
    ))
}

/// What happens at the host's end.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    Joined { id: MemberId, name: String },
    Said { id: MemberId, msg: ToHost },
    Left { id: MemberId },
}

/// A party being hosted.
pub struct Hosting {
    pub invite: Invite,
    endpoint: Endpoint,
    members: Arc<Mutex<HashMap<MemberId, mpsc::UnboundedSender<ToMember>>>>,
}

impl Hosting {
    /// Start a party: members join with `invite.code`; what they do comes
    /// through `events`.
    pub async fn start(events: impl Fn(HostEvent) + Send + Sync + 'static) -> anyhow::Result<Self> {
        let invite = free_code().await?;
        let endpoint = endpoint(Some(invite.host_key()), true).await?;
        if local() {
            local_lookup().add_endpoint_info(endpoint.addr());
        } else {
            // On a relay, and published, before the code's given out.
            let _ = tokio::time::timeout(Duration::from_secs(8), endpoint.online()).await;
        }
        let members: Arc<Mutex<HashMap<MemberId, mpsc::UnboundedSender<ToMember>>>> =
            Arc::default();
        let events = Arc::new(events);
        let next = Arc::new(AtomicU32::new(HOST + 1));
        let accepting = endpoint.clone();
        let (secret, list) = (invite.secret, members.clone());
        tokio::spawn(async move {
            while let Some(incoming) = accepting.accept().await {
                let (events, next, list) = (events.clone(), next.clone(), list.clone());
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    if let Err(e) = serve(conn, secret, &next, &list, events.as_ref()).await {
                        tracing::info!("a party member: {e:#}");
                    }
                });
            }
        });
        Ok(Self {
            invite,
            endpoint,
            members,
        })
    }

    pub fn send(&self, to: MemberId, msg: ToMember) {
        if let Some(tx) = self.members.lock().unwrap().get(&to) {
            let _ = tx.send(msg);
        }
    }

    pub fn send_all(&self, msg: ToMember) {
        for tx in self.members.lock().unwrap().values() {
            let _ = tx.send(msg.clone());
        }
    }

    /// End the party: members are told why.
    pub async fn end(self, why: &str) {
        self.send_all(ToMember::Ended(why.to_owned()));
        self.members.lock().unwrap().clear();
        // What's queued goes out before the connection closes.
        tokio::time::sleep(Duration::from_millis(300)).await;
        if local() {
            local_lookup().remove_endpoint_info(self.endpoint.id());
        }
        self.endpoint.close().await;
    }
}

/// One member's connection, at the host's end.
async fn serve(
    conn: Connection,
    secret: [u8; 16],
    next: &AtomicU32,
    members: &Mutex<HashMap<MemberId, mpsc::UnboundedSender<ToMember>>>,
    events: &(dyn Fn(HostEvent) + Send + Sync),
) -> anyhow::Result<()> {
    let (mut send, recv) = conn.accept_bi().await?;
    let mut recv = BufReader::new(recv);
    let hello = tokio::time::timeout(Duration::from_secs(15), read_line::<ToHost>(&mut recv))
        .await
        .map_err(|_| anyhow!("said nothing"))??;
    let name = match hello {
        Some(ToHost::Hello { secret: s, name }) if same_secret(&s, &secret) => name,
        _ => {
            conn.close(1u32.into(), b"not invited");
            bail!("wasn't invited");
        }
    };
    let id = next.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<ToMember>();
    let _ = tx.send(ToMember::Welcome { you: id });
    members.lock().unwrap().insert(id, tx.clone());
    events(HostEvent::Joined {
        id,
        name: clean_name(&name),
    });
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let ended = matches!(msg, ToMember::Ended(_));
            if send_line(&mut send, &msg).await.is_err() {
                break;
            }
            if ended {
                let _ = send.finish();
                break;
            }
        }
    });
    let read = async {
        loop {
            match read_line::<ToHost>(&mut recv).await? {
                None => return anyhow::Ok(()),
                Some(ToHost::Ping { nonce }) => {
                    let _ = tx.send(ToMember::Pong {
                        nonce,
                        at: now_ms(),
                    });
                }
                Some(ToHost::Hello { .. }) => {}
                Some(msg) => events(HostEvent::Said { id, msg }),
            }
        }
    };
    let result = read.await;
    members.lock().unwrap().remove(&id);
    writer.abort();
    events(HostEvent::Left { id });
    result
}

/// A name as shown: one line, not too long, never empty.
pub fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(32).collect();
    match name.trim() {
        "" => "Someone".to_owned(),
        n => n.to_owned(),
    }
}

/// What happens at a member's end.
#[derive(Debug, Clone, PartialEq)]
pub enum MemberEvent {
    Said(ToMember),
    /// The host's clock, less this app's (ms): the best guess so far.
    Clock(i64),
    /// The connection's gone (why).
    Lost(String),
}

/// In someone's party.
pub struct Joined {
    pub you: MemberId,
    endpoint: Endpoint,
    out: mpsc::UnboundedSender<ToHost>,
}

impl Joined {
    /// Join the party `invite` is for, as `name`; what the host says comes
    /// through `events`.
    pub async fn join(
        invite: &Invite,
        name: &str,
        events: impl Fn(MemberEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let endpoint = endpoint(None, false).await?;
        // The host's published where it is (a party that's just started may
        // not be findable for a moment): try for a while.
        let host = EndpointAddr::new(invite.host_id());
        let patience = Duration::from_secs(if local() { 2 } else { 20 });
        let until = Instant::now() + patience;
        let conn = loop {
            match tokio::time::timeout(
                Duration::from_secs(20),
                endpoint.connect(host.clone(), ALPN),
            )
            .await
            {
                Ok(Ok(conn)) => break conn,
                Ok(Err(e)) if Instant::now() >= until => {
                    tracing::info!("joining a party: {e}");
                    bail!("No party with that code (is it right, and is the party still on?)");
                }
                Ok(Err(_)) => tokio::time::sleep(Duration::from_secs(2)).await,
                Err(_) => bail!("Couldn't reach the host (is the party still on?)"),
            }
        };
        let (mut send, recv) = conn.open_bi().await?;
        let mut recv = BufReader::new(recv);
        send_line(
            &mut send,
            &ToHost::Hello {
                secret: invite.secret,
                name: name.to_owned(),
            },
        )
        .await?;
        let you = match tokio::time::timeout(Duration::from_secs(15), read_line(&mut recv)).await {
            Ok(Ok(Some(ToMember::Welcome { you }))) => you,
            Ok(Ok(Some(ToMember::Ended(why)))) => bail!("{why}"),
            Ok(Err(_) | Ok(None)) => bail!("the host turned it away (an old code?)"),
            Ok(Ok(Some(_))) => bail!("the host said something unexpected"),
            Err(_) => bail!("the host didn't answer"),
        };
        let (out, mut rx) = mpsc::unbounded_channel::<ToHost>();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if send_line(&mut send, &msg).await.is_err() {
                    break;
                }
            }
        });
        // Pings, for the host's clock: often at first, then now and then.
        let pinged: Arc<Mutex<HashMap<u64, u64>>> = Arc::default();
        let nonce = Arc::new(AtomicU64::new(1));
        {
            let (out, pinged) = (out.clone(), pinged.clone());
            tokio::spawn(async move {
                for i in 0u32.. {
                    let n = nonce.fetch_add(1, Ordering::Relaxed);
                    pinged.lock().unwrap().insert(n, now_ms());
                    if out.send(ToHost::Ping { nonce: n }).is_err() {
                        break;
                    }
                    let wait = if i < 8 { 300 } else { 5000 };
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                }
            });
        }
        let events = Arc::new(events);
        tokio::spawn(async move {
            // The offset from the quickest round trip lately is the best.
            let mut samples: Vec<(u64, i64)> = Vec::new();
            let why = loop {
                match read_line::<ToMember>(&mut recv).await {
                    Ok(Some(ToMember::Pong { nonce, at })) => {
                        let Some(sent) = pinged.lock().unwrap().remove(&nonce) else {
                            continue;
                        };
                        let back = now_ms();
                        let rtt = back.saturating_sub(sent);
                        let offset = at as i64 - (sent + rtt / 2) as i64;
                        samples.push((rtt, offset));
                        if samples.len() > 12 {
                            samples.remove(0);
                        }
                        let best = samples.iter().min_by_key(|(r, _)| *r).unwrap().1;
                        events(MemberEvent::Clock(best));
                    }
                    Ok(Some(ToMember::Ended(why))) => break why,
                    Ok(Some(msg)) => events(MemberEvent::Said(msg)),
                    Ok(None) => break "The host left".to_owned(),
                    Err(e) => break format!("Lost the host: {e:#}"),
                }
            };
            events(MemberEvent::Lost(why));
        });
        Ok(Self { you, endpoint, out })
    }

    pub fn send(&self, msg: ToHost) {
        let _ = self.out.send(msg);
    }

    pub async fn leave(self) {
        self.endpoint.close().await;
    }
}

/// A party on this machine: a host, a member joining it, a message each
/// way (the self-test's check that a build's connection works).
pub async fn loopback() -> anyhow::Result<()> {
    let (tx, mut hosted) = mpsc::unbounded_channel();
    let hosting = Hosting::start(move |e| {
        let _ = tx.send(e);
    })
    .await?;
    let (tx, mut heard) = mpsc::unbounded_channel();
    let joined = Joined::join(&hosting.invite, "self-test", move |e| {
        let _ = tx.send(e);
    })
    .await?;
    let have = ToHost::Have {
        series: 1,
        episode: 1,
        have: Have::Ready,
    };
    joined.send(have.clone());
    let both = async {
        while let Some(e) = hosted.recv().await {
            if matches!(&e, HostEvent::Said { msg, .. } if *msg == have) {
                break;
            }
        }
        hosting.send_all(ToMember::Episode(None));
        while let Some(e) = heard.recv().await {
            if e == MemberEvent::Said(ToMember::Episode(None)) {
                break;
            }
        }
    };
    let answered = tokio::time::timeout(Duration::from_secs(15), both).await;
    joined.leave().await;
    hosting.end("Done").await;
    answered.map_err(|_| anyhow!("no answer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect<T: Send + 'static>() -> (Arc<Mutex<Vec<T>>>, impl Fn(T) + Send + Sync + 'static) {
        let got: Arc<Mutex<Vec<T>>> = Arc::default();
        let g = got.clone();
        (got, move |e| g.lock().unwrap().push(e))
    }

    async fn until<T: Clone>(got: &Mutex<Vec<T>>, what: impl Fn(&T) -> bool) -> T {
        for _ in 0..200 {
            if let Some(e) = got.lock().unwrap().iter().find(|e| what(e)) {
                return e.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("it never came");
    }

    #[test]
    fn codes_are_four_easy_characters() {
        let invite = Invite::random();
        assert_eq!(invite.code.len(), CODE_LEN);
        assert!(invite.code.bytes().all(|b| ALPHABET.contains(&b)));
        // Typed any way: the same party.
        assert_eq!(Invite::parse(&invite.code).unwrap(), invite);
        let messy = format!(" {}-{} ", &invite.code[..2], &invite.code[2..]).to_lowercase();
        assert_eq!(Invite::parse(&messy).unwrap(), invite);
        // Look-alikes read as what they look like.
        assert_eq!(Invite::normalize("oIl9").unwrap(), "0119");
        // Not a code.
        for bad in ["K7Q", "K7QXZ", "K7Q!", "UUUU", ""] {
            assert!(Invite::parse(bad).is_err(), "{bad}");
        }
        // Each code its own host and secret, the same every time.
        let a = Invite::from_code("K7QX");
        let b = Invite::from_code("K7QY");
        assert_eq!(a, Invite::from_code("K7QX"));
        assert_ne!(a.host_id(), b.host_id());
        assert_ne!(a.secret, b.secret);
    }

    #[test]
    fn playback_moves_while_playing() {
        let p = Playback {
            position: 10.0,
            playing: true,
            at: 1000,
        };
        assert_eq!(p.position_at(3500), 12.5);
        assert_eq!(p.position_at(500), 10.0);
        let paused = Playback {
            playing: false,
            ..p
        };
        assert_eq!(paused.position_at(3500), 10.0);
    }

    #[test]
    fn names_are_one_short_line() {
        assert_eq!(clean_name("  Mai\n"), "Mai");
        assert_eq!(clean_name(" \t "), "Someone");
        assert_eq!(clean_name(&"a".repeat(50)).len(), 32);
    }

    #[tokio::test]
    async fn members_join_talk_and_leave() {
        let (host_got, host_events) = collect::<HostEvent>();
        let hosting = Hosting::start(host_events).await.unwrap();
        let (got, events) = collect::<MemberEvent>();
        let joined = Joined::join(&hosting.invite, "Mai", events).await.unwrap();
        assert_eq!(joined.you, 1);
        let ev = until(&host_got, |e| matches!(e, HostEvent::Joined { .. })).await;
        assert_eq!(
            ev,
            HostEvent::Joined {
                id: 1,
                name: "Mai".into()
            }
        );

        // Both ways.
        let have = ToHost::Have {
            series: 5,
            episode: 2,
            have: Have::Downloading(Some(0.5)),
        };
        joined.send(have.clone());
        until(&host_got, |e| {
            *e == HostEvent::Said {
                id: 1,
                msg: have.clone(),
            }
        })
        .await;
        let playback = Playback {
            position: 61.0,
            playing: true,
            at: now_ms(),
        };
        hosting.send_all(ToMember::Playback(Some(playback)));
        until(&got, |e| {
            *e == MemberEvent::Said(ToMember::Playback(Some(playback)))
        })
        .await;

        // The clock: both ends are this machine's, so near enough the same.
        let MemberEvent::Clock(offset) = until(&got, |e| matches!(e, MemberEvent::Clock(_))).await
        else {
            unreachable!()
        };
        assert!(offset.abs() < 50, "{offset}");

        // Pings are answered by the connection, not passed on.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!host_got.lock().unwrap().iter().any(|e| matches!(
            e,
            HostEvent::Said {
                msg: ToHost::Ping { .. },
                ..
            }
        )));

        joined.leave().await;
        until(&host_got, |e| *e == HostEvent::Left { id: 1 }).await;
        hosting.end("Bye").await;
    }

    #[tokio::test]
    async fn the_self_test_connects() {
        loopback().await.unwrap();
    }

    #[tokio::test]
    async fn the_host_ending_it_says_why() {
        let (_host_got, host_events) = collect::<HostEvent>();
        let hosting = Hosting::start(host_events).await.unwrap();
        let (got, events) = collect::<MemberEvent>();
        let _joined = Joined::join(&hosting.invite, "Mai", events).await.unwrap();
        hosting.end("The host ended the party").await;
        until(&got, |e| {
            *e == MemberEvent::Lost("The host ended the party".into())
        })
        .await;
    }

    /// Join a party by its code (by hand: `ICHIOSHI_PARTY_RELAY=1
    /// PARTY_CODE=…`, a phone's, say), and see who's there.
    #[tokio::test]
    #[ignore = "by hand, with PARTY_CODE"]
    async fn joins_a_party_by_hand() {
        let Some(code) = std::env::var("PARTY_CODE").ok() else {
            eprintln!("skipping: by hand, with PARTY_CODE");
            return;
        };
        let invite = Invite::parse(&code).unwrap();
        let (got, events) = collect::<MemberEvent>();
        let joined = Joined::join(&invite, "Desk", events).await.unwrap();
        let members = until(&got, |e| {
            matches!(e, MemberEvent::Said(ToMember::Members(_)))
        })
        .await;
        eprintln!("in the party: {members:?}");
        joined.leave().await;
    }

    #[tokio::test]
    async fn a_code_nobody_hosts_finds_no_party() {
        let (_got, events) = collect::<MemberEvent>();
        let e = Joined::join(&Invite::random(), "Mai", events)
            .await
            .err()
            .unwrap();
        assert!(e.to_string().contains("No party with that code"), "{e}");
    }

    #[tokio::test]
    async fn a_wrong_secret_is_turned_away() {
        let (host_got, host_events) = collect::<HostEvent>();
        let hosting = Hosting::start(host_events).await.unwrap();
        let forged = Invite {
            secret: [0; 16],
            ..hosting.invite.clone()
        };
        let (_got, events) = collect::<MemberEvent>();
        assert!(Joined::join(&forged, "Eve", events).await.is_err());
        assert!(host_got.lock().unwrap().is_empty());
        hosting.end("Bye").await;
    }
}
