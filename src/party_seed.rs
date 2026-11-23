//! Seeding an episode to a watch party: the one exception to "never seeds".
//!
//! A member who has the party's episode on disk (from anywhere: BitTorrent,
//! a debrid service, Usenet, their servers) serves it over BitTorrent to
//! the members downloading it with BitTorrent, and to nobody else. It
//! answers only connections that name the torrent and a BitTorrent ID a
//! member told the party, on a port of its own that nothing announces (no
//! trackers, no DHT, no other peers). The app's BitTorrent engine itself
//! still never uploads. Seeding stops once every member has the episode,
//! or the party ends.
//!
//! The video goes over BitTorrent, peer to peer: never through the party's
//! own connection (iroh), which only says where to connect.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The largest block a peer may ask for (BitTorrent clients ask for 16 KiB).
const MAX_BLOCK: u32 = 128 * 1024;
/// At most this many members' connections at once.
const MAX_PEERS: usize = 8;

/// A torrent's piece, in the torrent's bytes laid end to end.
fn piece_range(piece: u32, piece_length: u32, total: u64) -> (u64, u64) {
    let start = piece as u64 * piece_length as u64;
    (start, (start + piece_length as u64).min(total))
}

/// What's served: the pieces of a torrent that are all in one file here
/// (or in padding files, which are zeros), checked against its hashes.
pub struct Served {
    pub info_hash: [u8; 20],
    piece_length: u32,
    total: u64,
    /// The file, in the torrent's bytes.
    file: (u64, u64),
    /// Padding files (zeros), in the torrent's bytes.
    padding: Vec<(u64, u64)>,
    path: PathBuf,
    /// Bytes the file had before the app edited it there (its default
    /// tracks): read in their place, so it's the torrent's again.
    original: Option<crate::mkv::Replaced>,
    /// Which pieces are here.
    pub have: Vec<bool>,
}

impl Served {
    /// How many pieces are here, of how many.
    pub fn count(&self) -> (usize, usize) {
        (self.have.iter().filter(|h| **h).count(), self.have.len())
    }

    /// Where the torrent's bytes from `at` (`len` of them) are in the
    /// file: from where in it, into where in the bytes; the rest is
    /// padding (zeros).
    fn in_file(&self, at: u64, len: usize) -> Option<(u64, std::ops::Range<usize>)> {
        let (fs, fe) = self.file;
        let (from, to) = (at.max(fs), (at + len as u64).min(fe));
        (from < to).then(|| {
            let i = (from - at) as usize;
            (from - fs, i..i + (to - from) as usize)
        })
    }

    /// The file's bytes from `pos`, read into `out`: as it was before the
    /// app edited it.
    fn unedit(&self, pos: u64, out: &mut [u8]) {
        let Some((at, was)) = &self.original else {
            return;
        };
        let end = pos + out.len() as u64;
        let (from, to) = (pos.max(*at), end.min(at + was.len() as u64));
        if from < to {
            out[(from - pos) as usize..(to - pos) as usize]
                .copy_from_slice(&was[(from - at) as usize..(to - at) as usize]);
        }
    }

    /// Fill `out` with the torrent's bytes from `at`.
    async fn read(
        &self,
        file: &mut tokio::fs::File,
        at: u64,
        out: &mut [u8],
    ) -> anyhow::Result<()> {
        out.fill(0);
        if let Some((pos, range)) = self.in_file(at, out.len()) {
            file.seek(std::io::SeekFrom::Start(pos)).await?;
            file.read_exact(&mut out[range.clone()]).await?;
            self.unedit(pos, &mut out[range]);
        }
        Ok(())
    }

    /// The same, blocking.
    fn read_now(&self, file: &mut std::fs::File, at: u64, out: &mut [u8]) -> std::io::Result<()> {
        use std::io::{Read, Seek};
        out.fill(0);
        if let Some((pos, range)) = self.in_file(at, out.len()) {
            file.seek(std::io::SeekFrom::Start(pos))?;
            file.read_exact(&mut out[range.clone()])?;
            self.unedit(pos, &mut out[range]);
        }
        Ok(())
    }
}

/// Which pieces of the torrent (`torrent`: its .torrent) the copy at `path`
/// of its file `file_index` has (`original`: what the app replaced in it,
/// and where). Every piece is checked against the torrent's hashes, so
/// another release's file has none. (Slow: it reads the whole file.)
pub fn check(
    torrent: &[u8],
    file_index: usize,
    path: &Path,
    original: Option<crate::mkv::Replaced>,
) -> anyhow::Result<Served> {
    let parsed = librqbit_core::torrent_metainfo::torrent_from_bytes(torrent)
        .map_err(|e| anyhow::anyhow!("not a torrent: {e}"))?;
    let info_hash = parsed.info_hash.0;
    let info = parsed
        .info
        .data
        .validate()
        .map_err(|e| anyhow::anyhow!("not a torrent: {e}"))?;
    let piece_length = info.info().piece_length;
    let mut total = 0u64;
    let mut file = None;
    let mut padding = Vec::new();
    for (i, f) in info.iter_file_details_ext().enumerate() {
        let range = (f.offset, f.offset + f.details.len);
        total = range.1;
        if i == file_index {
            file = Some((range, f.pieces.clone()));
        } else if f.details.attrs().padding {
            padding.push(range);
        }
    }
    let Some((file, pieces)) = file else {
        bail!("the torrent has no file {file_index}");
    };
    let len = std::fs::metadata(path)
        .with_context(|| format!("couldn't read {}", path.display()))?
        .len();
    let count = info.info().pieces.as_ref().len() / 20;
    let mut served = Served {
        info_hash,
        piece_length,
        total,
        file,
        padding,
        path: path.to_owned(),
        original,
        have: vec![false; count],
    };
    // Another size: another release, so nothing to check.
    if len != file.1 - file.0 {
        return Ok(served);
    }
    let covered = |(s, e): (u64, u64)| {
        let mut at = s;
        while at < e {
            let next = [served.file]
                .iter()
                .chain(&served.padding)
                .find(|(fs, fe)| *fs <= at && at < *fe)
                .map(|(_, fe)| *fe);
            match next {
                Some(n) => at = n,
                None => return false,
            }
        }
        true
    };
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; piece_length as usize];
    for p in pieces {
        let range = piece_range(p, piece_length, total);
        if !covered(range) {
            continue;
        }
        let out = &mut buf[..(range.1 - range.0) as usize];
        served.read_now(&mut f, range.0, out)?;
        let hash = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, out);
        if info
            .info()
            .compare_hash(p, hash.as_ref().try_into().unwrap())
            == Some(true)
        {
            served.have[p as usize] = true;
        }
    }
    Ok(served)
}

/// Seeding to the party: stops when dropped.
pub struct Seeder {
    /// Where members connect.
    pub addrs: Vec<SocketAddr>,
    /// Bytes sent so far.
    pub sent: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
    /// The router's port mapping, if it made one (removed when dropped).
    _mapping: Option<portmapper::Client>,
}

impl Drop for Seeder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// This computer's address on its network (none: no network). Nothing is
/// sent: connecting a UDP socket only picks the route.
fn local_ip() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

impl Seeder {
    /// Serve what's `served` to the BitTorrent IDs `allowed` (the members
    /// downloading it), on a port of its own: on this network, and from
    /// outside if the router maps it (UPnP, NAT-PMP or PCP).
    pub async fn start(served: Served, allowed: Vec<[u8; 20]>) -> anyhow::Result<Self> {
        let local = cfg!(test) || std::env::var_os("ICHIOSHI_PARTY_LOCAL").is_some();
        let bind: SocketAddr = if local {
            (Ipv4Addr::LOCALHOST, 0).into()
        } else {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        };
        let listener = TcpListener::bind(bind)
            .await
            .context("couldn't open a port to share the episode on")?;
        let port = listener.local_addr()?.port();
        let mut addrs = Vec::new();
        let mut mapping = None;
        if local {
            addrs.push((Ipv4Addr::LOCALHOST, port).into());
        } else {
            if let Some(ip) = local_ip() {
                addrs.push((ip, port).into());
            }
            let client = portmapper::Client::new(portmapper::Config {
                enable_upnp: true,
                enable_pcp: true,
                enable_nat_pmp: true,
                protocol: portmapper::Protocol::Tcp,
            });
            if let Some(p) = std::num::NonZeroU16::new(port) {
                client.update_local_port(p);
                client.procure_mapping();
                let mut watch = client.watch_external_address();
                let mapped = tokio::time::timeout(Duration::from_secs(4), async {
                    loop {
                        if let Some(a) = *watch.borrow_and_update() {
                            return Some(a);
                        }
                        if watch.changed().await.is_err() {
                            return None;
                        }
                    }
                })
                .await
                .ok()
                .flatten();
                if let Some(a) = mapped {
                    addrs.push(a.into());
                }
            }
            mapping = Some(client);
        }
        let served = Arc::new(served);
        let allowed = Arc::new(allowed);
        let sent = Arc::new(AtomicU64::new(0));
        let peers = Arc::new(tokio::sync::Semaphore::new(MAX_PEERS));
        let counted = sent.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, from)) = listener.accept().await else {
                    continue;
                };
                let Ok(permit) = peers.clone().try_acquire_owned() else {
                    continue;
                };
                let (served, allowed, sent) = (served.clone(), allowed.clone(), counted.clone());
                tokio::spawn(async move {
                    if let Err(e) = serve(stream, &served, &allowed, &sent).await {
                        tracing::info!("party seeding to {from}: {e:#}");
                    }
                    drop(permit);
                });
            }
        });
        Ok(Self {
            addrs,
            sent,
            task,
            _mapping: mapping,
        })
    }
}

const PROTOCOL: &[u8; 19] = b"BitTorrent protocol";

/// One member's BitTorrent connection: the handshake (it must name the
/// torrent, and be a member's), what's here, and the blocks it asks for.
async fn serve(
    mut s: TcpStream,
    served: &Served,
    allowed: &[[u8; 20]],
    sent: &AtomicU64,
) -> anyhow::Result<()> {
    let mut hs = [0u8; 68];
    tokio::time::timeout(Duration::from_secs(15), s.read_exact(&mut hs))
        .await
        .context("no handshake")??;
    if hs[0] != 19 || &hs[1..20] != PROTOCOL {
        bail!("not BitTorrent");
    }
    if hs[28..48] != served.info_hash {
        bail!("another torrent");
    }
    if !allowed.iter().any(|id| hs[48..68] == id[..]) {
        bail!("not a party member's");
    }
    let me = librqbit_core::peer_id::generate_azereus_style(*b"rQ", (9, 1, 0, 0));
    let mut out = Vec::with_capacity(68);
    out.push(19);
    out.extend_from_slice(PROTOCOL);
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&served.info_hash);
    out.extend_from_slice(&me.0);
    s.write_all(&out).await?;
    // What's here, and they may ask for it.
    let mut bits = vec![0u8; served.have.len().div_ceil(8)];
    for (i, h) in served.have.iter().enumerate() {
        if *h {
            bits[i / 8] |= 0x80 >> (i % 8);
        }
    }
    send(&mut s, 5, &bits).await?;
    send(&mut s, 1, &[]).await?;
    let mut file = tokio::fs::File::open(&served.path).await?;
    let mut block = Vec::new();
    loop {
        let mut len = [0u8; 4];
        // Members ask while they're downloading; a long silence, they're done.
        tokio::time::timeout(Duration::from_secs(300), s.read_exact(&mut len))
            .await
            .context("quiet too long")??;
        let len = u32::from_be_bytes(len);
        if len == 0 {
            continue;
        }
        if len > 1 << 16 {
            bail!("a message too long");
        }
        let mut msg = vec![0u8; len as usize];
        s.read_exact(&mut msg).await?;
        match msg[0] {
            // Interested: they may ask.
            2 => send(&mut s, 1, &[]).await?,
            // A block.
            6 if msg.len() == 13 => {
                let num = |i: usize| u32::from_be_bytes(msg[i..i + 4].try_into().unwrap());
                let (index, begin, length) = (num(1), num(5), num(9));
                if !served.have.get(index as usize).copied().unwrap_or(false) {
                    continue;
                }
                let (start, end) = piece_range(index, served.piece_length, served.total);
                if length == 0 || length > MAX_BLOCK || start + begin as u64 + length as u64 > end {
                    bail!("asked for something that isn't there");
                }
                block.resize(8 + length as usize, 0);
                block[..4].copy_from_slice(&index.to_be_bytes());
                block[4..8].copy_from_slice(&begin.to_be_bytes());
                served
                    .read(&mut file, start + begin as u64, &mut block[8..])
                    .await?;
                send(&mut s, 7, &block).await?;
                sent.fetch_add(length as u64, Ordering::Relaxed);
            }
            // Anything else (have, not interested, cancel, extensions):
            // nothing to do.
            _ => {}
        }
    }
}

async fn send(s: &mut TcpStream, id: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut msg = Vec::with_capacity(5 + payload.len());
    msg.extend_from_slice(&(1 + payload.len() as u32).to_be_bytes());
    msg.push(id);
    msg.extend_from_slice(payload);
    s.write_all(&msg).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Bencode, enough for a .torrent.
    enum B<'a> {
        Int(u64),
        Bytes(&'a [u8]),
        List(Vec<B<'a>>),
        Dict(Vec<(&'a str, B<'a>)>),
    }

    fn bencode(b: &B, out: &mut Vec<u8>) {
        match b {
            B::Int(n) => out.extend(format!("i{n}e").bytes()),
            B::Bytes(s) => {
                out.extend(format!("{}:", s.len()).bytes());
                out.extend_from_slice(s);
            }
            B::List(l) => {
                out.push(b'l');
                l.iter().for_each(|x| bencode(x, out));
                out.push(b'e');
            }
            B::Dict(d) => {
                out.push(b'd');
                for (k, v) in d {
                    bencode(&B::Bytes(k.as_bytes()), out);
                    bencode(v, out);
                }
                out.push(b'e');
            }
        }
    }

    /// A .torrent for `files` (name, contents), `piece` bytes a piece.
    pub(crate) fn torrent(files: &[(&str, &[u8])], piece: u32) -> Vec<u8> {
        let all: Vec<u8> = files.iter().flat_map(|(_, d)| d.iter().copied()).collect();
        let hashes: Vec<u8> = all
            .chunks(piece as usize)
            .flat_map(|c| {
                ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, c)
                    .as_ref()
                    .to_vec()
            })
            .collect();
        let entries: Vec<B> = files
            .iter()
            .map(|(n, d)| {
                B::Dict(vec![
                    ("length", B::Int(d.len() as u64)),
                    ("path", B::List(vec![B::Bytes(n.as_bytes())])),
                ])
            })
            .collect();
        let info = B::Dict(vec![
            ("files", B::List(entries)),
            ("name", B::Bytes(b"Show")),
            ("piece length", B::Int(piece as u64)),
            ("pieces", B::Bytes(&hashes)),
        ]);
        let mut out = Vec::new();
        bencode(&B::Dict(vec![("info", info)]), &mut out);
        out
    }

    fn walkdir(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    fn data(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[0] ^ seed)
            .collect()
    }

    #[test]
    fn serves_only_the_pieces_its_file_has() {
        let dir = tempfile::tempdir().unwrap();
        // Three episodes; pieces cross from one into the next.
        let (a, b, c) = (data(50_000, 1), data(100_000, 2), data(30_000, 3));
        let t = torrent(&[("01.mkv", &a), ("02.mkv", &b), ("03.mkv", &c)], 16_384);
        let path = dir.path().join("Show - 02.mkv");
        std::fs::write(&path, &b).unwrap();
        let served = check(&t, 1, &path, None).unwrap();
        // Episode 2 is bytes 50000..150000: pieces 4 to 8 are all in it
        // (3 and 9 cross into episodes 1 and 3).
        let have: Vec<usize> = (0..served.have.len()).filter(|i| served.have[*i]).collect();
        assert_eq!(have, vec![4, 5, 6, 7, 8]);

        // The app changed a few bytes of it (its default tracks): that
        // piece isn't the torrent's, unless it's read as it was.
        let mut edited = b.clone();
        edited[20_000..20_010].copy_from_slice(&[0xee; 10]);
        std::fs::write(&path, &edited).unwrap();
        let without = check(&t, 1, &path, None).unwrap();
        assert!(!without.have[4] && without.have[5]);
        let was = Some((20_000, b[20_000..20_010].to_vec()));
        assert_eq!(check(&t, 1, &path, was).unwrap().count().0, 5);

        // A different file of the same size (another release): nothing.
        std::fs::write(&path, data(100_000, 9)).unwrap();
        assert_eq!(check(&t, 1, &path, None).unwrap().count().0, 0);
        // Another size: nothing, without reading it.
        std::fs::write(&path, b"short").unwrap();
        assert_eq!(check(&t, 1, &path, None).unwrap().count().0, 0);
    }

    /// The app's own BitTorrent engine (which never uploads) downloads an
    /// episode from a party member's seeder, and only from it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_member_downloads_it_from_the_party() {
        let dir = tempfile::tempdir().unwrap();
        let episode = data(300_000, 4);
        let t = torrent(&[("Show - 01.mkv", &episode)], 32_768);
        let have = dir.path().join("have.mkv");
        std::fs::write(&have, &episode).unwrap();

        // The member's engine: no DHT, no trackers, so the party's seeder
        // is the only peer there is.
        let offline = crate::vpn::Network {
            offline: true,
            ..Default::default()
        };
        let engine = crate::engine::Engine::start(
            &dir.path().join("state"),
            dir.path().join("dl"),
            &offline,
        )
        .await
        .unwrap();
        let served = tokio::task::spawn_blocking({
            let (t, have) = (t.clone(), have.clone());
            move || check(&t, 0, &have, None)
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(served.count(), (10, 10));
        let seeder = Seeder::start(served, vec![engine.peer_id()]).await.unwrap();

        let out = dir.path().join("dl").join("Show");
        engine.download(t.clone(), None, out.clone()).await.unwrap();
        let hash = crate::engine::info_hash_of(&t).unwrap();
        engine.add_peers(&hash, seeder.addrs.clone()).await.unwrap();
        let find = || {
            walkdir(&dir.path().join("dl"))
                .into_iter()
                .find(|p| p.file_name().is_some_and(|n| n == "Show - 01.mkv"))
        };
        for _ in 0..300 {
            if find().and_then(|f| std::fs::read(f).ok()).as_deref() == Some(&episode[..]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let file = find().expect("nothing downloaded");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            episode,
            "downloaded from the party"
        );
        assert_eq!(seeder.sent.load(Ordering::Relaxed), episode.len() as u64);
        engine.stop().await;
    }

    /// Someone who isn't in the party (another BitTorrent ID) gets nothing.
    #[tokio::test]
    async fn outsiders_get_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let episode = data(40_000, 5);
        let t = torrent(&[("Show - 01.mkv", &episode)], 16_384);
        let have = dir.path().join("have.mkv");
        std::fs::write(&have, &episode).unwrap();
        let served = check(&t, 0, &have, None).unwrap();
        let info_hash = served.info_hash;
        let seeder = Seeder::start(served, vec![[7; 20]]).await.unwrap();
        for (peer, hash, answered) in [
            ([8; 20], info_hash, false),
            ([7; 20], [0; 20], false),
            ([7; 20], info_hash, true),
        ] {
            let mut s = TcpStream::connect(seeder.addrs[0]).await.unwrap();
            let mut hs = vec![19];
            hs.extend_from_slice(PROTOCOL);
            hs.extend_from_slice(&[0; 8]);
            hs.extend_from_slice(&hash);
            hs.extend_from_slice(&peer);
            s.write_all(&hs).await.unwrap();
            let mut back = [0u8; 68];
            let got = tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut back))
                .await
                .unwrap();
            assert_eq!(got.is_ok(), answered, "{peer:?}");
        }
    }
}
