//! The BitTorrent side, on top of librqbit.
//!
//! This client never uploads. librqbit's `disable-upload` mode never tells
//! peers what we have and refuses their piece requests; on top of that the
//! session skips UPnP and local peer discovery, and each torrent is stopped
//! as soon as its selected files are complete, so nothing lingers announcing
//! itself as a seed. It opens no listening port, except a port a VPN
//! forwards to us: peers that connect in can then send us pieces, but still
//! get none.
//!
//! Over a VPN ([`crate::vpn`]), every socket (peers, DHT, trackers) is bound
//! to the VPN's network interface, so nothing can go out any other way.
//!
//! Files are stored by [`crate::storage`], which only creates the files you
//! selected.

use std::collections::HashSet;
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use librqbit::api::TorrentIdOrHash;
use librqbit::dht::DhtPersistenceConfig;
use librqbit::storage::StorageFactoryExt;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ListenerMode,
    ListenerOptions, ManagedTorrent, Session, SessionOptions, SessionPersistenceConfig,
    TorrentStatsState, ValidatedTorrentMetaV1Info, torrent_from_bytes,
};

use crate::storage::{Registry, SelectiveStorageFactory};
use crate::vpn::Network;

pub type TorrentHandle = Arc<ManagedTorrent>;
pub type TorrentId = usize;

/// How long to wait for peers to send a magnet link's metadata.
const MAGNET_TIMEOUT: Duration = Duration::from_secs(90);

/// Where a torrent's metadata comes from.
pub enum Source {
    TorrentFile(Vec<u8>),
    Magnet(String),
}

/// A torrent's contents, for the file picker.
#[derive(Clone)]
pub struct Listing {
    pub name: String,
    pub torrent_bytes: Vec<u8>,
    /// Real files only (BEP 47 padding files are left out), keeping their
    /// index in the torrent, which is what librqbit's `only_files` refers to.
    pub files: Vec<TorrentFile>,
    pub multi_file: bool,
    /// Set if this torrent is already in the session.
    pub existing: Option<Existing>,
}

#[derive(Clone)]
pub struct TorrentFile {
    pub index: usize,
    pub path: String,
    pub len: u64,
}

#[derive(Clone)]
pub struct Existing {
    /// Files it is set to download (`None` means all of them).
    pub only_files: Option<Vec<usize>>,
    pub output_folder: PathBuf,
}

impl Listing {
    /// Where a fresh download of this torrent goes: multi-file torrents get
    /// their own folder named after the torrent, like most clients do.
    pub fn output_folder(&self, download_dir: &Path) -> PathBuf {
        match &self.existing {
            Some(e) => e.output_folder.clone(),
            None if self.multi_file => download_dir.join(sanitize_folder_name(&self.name)),
            None => download_dir.to_owned(),
        }
    }
}

pub struct Engine {
    session: Arc<Session>,
    registry: Arc<Registry>,
}

impl Engine {
    /// Start the session on `network`. Torrents from earlier runs are
    /// restored from `state_dir` and pick up where they left off.
    pub async fn start(
        state_dir: &Path,
        default_download_dir: PathBuf,
        network: &Network,
    ) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("could not create {}", state_dir.display()))?;
        // Torrents restored below need their folders and selections registered
        // before librqbit creates their storage.
        let registry = Arc::new(Registry::default());
        registry.load_librqbit_session(&state_dir.join("session").join("session.json"));
        // Over a VPN the DHT keeps a separate identity (node ID and port), so
        // DHT nodes can't link the VPN's address to the home one.
        let dht_file = if network.interface.is_some() {
            "dht-vpn.json"
        } else {
            "dht.json"
        };
        let opts = SessionOptions {
            // Offline: no DHT and no trackers, so nothing even looks up a name.
            disable_trackers: network.offline,
            dht: (!network.offline).then(|| DhtSessionConfig {
                persistence: Some(DhtPersistenceConfig {
                    config_filename: Some(state_dir.join(dht_file)),
                    ..Default::default()
                }),
                // DHT on the forwarded port too, so other nodes can reach it.
                port: network.port,
                ..Default::default()
            }),
            bind_device_name: network.interface.clone(),
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(state_dir.join("session")),
            }),
            fastresume: true,
            // No seeding: see the module docs.
            disable_upload: true,
            listen: network
                .port
                .filter(|_| !network.offline)
                .map(|port| ListenerOptions {
                    mode: ListenerMode::TcpOnly,
                    listen_addr: (Ipv6Addr::UNSPECIFIED, port).into(),
                    enable_upnp_port_forwarding: false,
                    announce_port: Some(port),
                    ..Default::default()
                }),
            disable_local_service_discovery: true,
            default_storage_factory: Some(SelectiveStorageFactory::new(registry.clone()).boxed()),
            ..Default::default()
        };
        let session = Session::new_with_opts(default_download_dir, opts)
            .await
            .context("could not start the torrent engine")?;
        tokio::spawn(stop_finished_torrents(session.clone()));
        Ok(Self { session, registry })
    }

    /// Read a torrent's file list without downloading anything.
    pub async fn inspect(&self, source: Source) -> Result<Listing> {
        let is_magnet = matches!(source, Source::Magnet(_));
        let add = match source {
            Source::TorrentFile(bytes) => AddTorrent::from_bytes(bytes),
            Source::Magnet(uri) => AddTorrent::from_url(uri),
        };
        let opts = AddTorrentOptions {
            list_only: true,
            ..Default::default()
        };
        let fut = self.session.add_torrent(add, Some(opts));
        let resp = if is_magnet {
            tokio::time::timeout(MAGNET_TIMEOUT, fut)
                .await
                .context("timed out fetching the torrent's metadata from peers")??
        } else {
            fut.await?
        };

        let AddTorrentResponse::ListOnly(r) = resp else {
            bail!("librqbit added the torrent instead of listing it");
        };
        let mut listing = build_listing(&r.info, r.torrent_bytes.to_vec(), r.info_hash.as_string());

        // list_only never reports "already managed", so check ourselves.
        if let Some(handle) = self.session.get(TorrentIdOrHash::Hash(r.info_hash)) {
            listing.existing = Some(Existing {
                only_files: handle.only_files(),
                output_folder: handle.output_folder().to_owned(),
            });
        }
        Ok(listing)
    }

    /// The torrent's selection and folder, if it's in the session.
    pub fn existing(&self, info_hash_hex: &str) -> Option<Existing> {
        let id = TorrentIdOrHash::parse(info_hash_hex).ok()?;
        let handle = self.session.get(id)?;
        Some(Existing {
            only_files: handle.only_files(),
            output_folder: handle.output_folder().to_owned(),
        })
    }

    /// The listing for a torrent already in the session (to change its files).
    pub fn listing_for(&self, handle: &TorrentHandle) -> Result<Listing> {
        let mut listing = handle.with_metadata(|m| {
            build_listing(
                &m.info,
                m.torrent_bytes.to_vec(),
                handle.info_hash().as_string(),
            )
        })?;
        listing.existing = Some(Existing {
            only_files: handle.only_files(),
            output_folder: handle.output_folder().to_owned(),
        });
        Ok(listing)
    }

    /// Download `only_files` of the torrent (`None` for all of them) into
    /// `output_folder`. If the torrent is already in the session, its file
    /// selection is replaced instead.
    pub async fn download(
        &self,
        torrent_bytes: Vec<u8>,
        only_files: Option<Vec<usize>>,
        output_folder: PathBuf,
    ) -> Result<()> {
        if only_files.as_ref().is_some_and(|f| f.is_empty()) {
            bail!("no files selected");
        }
        let info_hash = torrent_from_bytes(&torrent_bytes)
            .map_err(|e| anyhow::anyhow!("invalid .torrent file: {e}"))?
            .info_hash;
        if let Some(handle) = self.session.get(TorrentIdOrHash::Hash(info_hash)) {
            return self.reselect(&handle, only_files).await;
        }
        // Register before adding: librqbit creates the storage while adding.
        self.registry
            .set(info_hash, &output_folder, only_files.as_deref());
        let opts = AddTorrentOptions {
            only_files: only_files.clone(),
            output_folder: Some(output_folder.to_string_lossy().into_owned()),
            // Lets an interrupted download resume on top of its own partial files.
            overwrite: true,
            ..Default::default()
        };
        match self
            .session
            .add_torrent(AddTorrent::from_bytes(torrent_bytes), Some(opts))
            .await?
        {
            AddTorrentResponse::Added(..) => Ok(()),
            AddTorrentResponse::AlreadyManaged(_, handle) => {
                self.reselect(&handle, only_files).await
            }
            AddTorrentResponse::ListOnly(_) => {
                bail!("librqbit listed the torrent instead of adding it")
            }
        }
    }

    /// Change which files a torrent in the session downloads.
    async fn reselect(&self, handle: &TorrentHandle, only_files: Option<Vec<usize>>) -> Result<()> {
        let (piece_ranges, torrent_bytes) = handle.with_metadata(|m| {
            let ranges: Vec<Option<std::ops::Range<u32>>> = m
                .file_infos
                .iter()
                .map(|f| (!f.attrs.padding).then(|| f.piece_range.clone()))
                .collect();
            (ranges, m.torrent_bytes.to_vec())
        })?;
        let all = || (0..piece_ranges.len()).collect::<HashSet<usize>>();
        let new: HashSet<usize> = only_files
            .as_ref()
            .map_or_else(all, |f| f.iter().copied().collect());
        let old: HashSet<usize> = handle
            .only_files()
            .map_or_else(all, |f| f.into_iter().collect());
        let info_hash = handle.info_hash();
        let output_folder = handle.output_folder().to_owned();
        self.registry
            .set(info_hash, &output_folder, only_files.as_deref());

        if needs_recheck(&piece_ranges, &old, &new) {
            // A newly selected file shares a piece with one already selected.
            // librqbit counts that piece as downloaded, but the new file's
            // sliver of it was deleted. Re-adding the torrent drops librqbit's
            // resume data, so it re-checks what's on disk and downloads those
            // pieces again. Files on disk are kept.
            self.session
                .delete(TorrentIdOrHash::Hash(info_hash), false)
                .await
                .context("could not restart the torrent")?;
            let opts = AddTorrentOptions {
                only_files,
                output_folder: Some(output_folder.to_string_lossy().into_owned()),
                overwrite: true,
                ..Default::default()
            };
            self.session
                .add_torrent(AddTorrent::from_bytes(torrent_bytes), Some(opts))
                .await
                .context("could not re-add the torrent")?;
            return Ok(());
        }

        self.session
            .update_only_files(handle, &new)
            .await
            .context("could not change the torrent's files")?;
        if handle.is_paused() {
            self.session.unpause(handle).await?;
        }
        Ok(())
    }

    /// A series' finished video files: those of this torrent, plus those of
    /// other torrents in the same folder (a release downloaded one episode per
    /// torrent), unless the folder is the download folder itself.
    pub fn series_files(&self, handle: &TorrentHandle, download_dir: &Path) -> Result<SeriesFiles> {
        let folder = handle.output_folder().to_owned();
        let shared_folder = folder != download_dir;
        let group: Vec<TorrentHandle> = if shared_folder {
            self.torrents()
                .into_iter()
                .map(|(_, h)| h)
                .filter(|h| h.output_folder() == folder)
                .collect()
        } else {
            vec![handle.clone()]
        };
        let mut files = Vec::new();
        let mut not_ready = 0;
        for h in &group {
            let progress = h.stats().file_progress;
            let only = h.only_files();
            h.with_metadata(|m| {
                for (i, fi) in m.file_infos.iter().enumerate() {
                    let selected = only.as_ref().is_none_or(|o| o.contains(&i));
                    if fi.attrs.padding
                        || !selected
                        || !crate::remux::is_video(&fi.relative_filename)
                    {
                        continue;
                    }
                    if progress.get(i).copied().unwrap_or(0) >= fi.len {
                        files.push(folder.join(&fi.relative_filename));
                    } else {
                        not_ready += 1;
                    }
                }
            })?;
        }
        files.sort();
        files.dedup();
        let name = if shared_folder && group.len() > 1 {
            folder.file_name().map(|n| n.to_string_lossy().into_owned())
        } else {
            handle.name()
        }
        .unwrap_or_else(|| folder.display().to_string());
        Ok(SeriesFiles {
            name,
            files,
            not_ready,
            info_hashes: group.iter().map(|h| h.info_hash().as_string()).collect(),
        })
    }

    /// All torrents in the session, oldest first.
    pub fn torrents(&self) -> Vec<(TorrentId, TorrentHandle)> {
        let mut v: Vec<_> = self
            .session
            .with_torrents(|it| it.map(|(id, h)| (id, h.clone())).collect());
        v.sort_by_key(|(id, _)| *id);
        v
    }

    pub async fn pause(&self, handle: &TorrentHandle) -> Result<()> {
        self.session.pause(handle).await
    }

    pub async fn resume(&self, handle: &TorrentHandle) -> Result<()> {
        self.session.unpause(handle).await
    }

    /// Remove a torrent from the session, optionally deleting its downloaded files.
    pub async fn remove(&self, id: TorrentId, delete_files: bool) -> Result<()> {
        let info_hash = self
            .session
            .get(TorrentIdOrHash::Id(id))
            .map(|h| h.info_hash());
        self.session
            .delete(TorrentIdOrHash::Id(id), delete_files)
            .await?;
        if let Some(hash) = info_hash {
            self.registry.remove(&hash);
        }
        Ok(())
    }

    pub async fn stop(&self) {
        self.session.stop().await;
    }

    #[cfg(test)]
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }
}

/// Whether earlier runs left torrents in the session (so the engine has
/// something to show even when downloads go through a debrid service).
pub fn has_saved_torrents(state_dir: &Path) -> bool {
    std::fs::read_to_string(state_dir.join("session").join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["torrents"].as_object().is_some_and(|m| !m.is_empty()))
}

/// The finished video files of a downloaded series.
#[derive(Clone)]
pub struct SeriesFiles {
    /// The torrent's or the folder's name.
    pub name: String,
    pub files: Vec<PathBuf>,
    /// Selected video files still downloading.
    pub not_ready: usize,
    pub info_hashes: Vec<String>,
}

/// A .torrent file's info hash, as lowercase hex.
pub fn info_hash_of(torrent_bytes: &[u8]) -> Option<String> {
    torrent_from_bytes(torrent_bytes)
        .ok()
        .map(|t| t.info_hash.as_string().to_ascii_lowercase())
}

/// Whether any newly selected file shares a piece with a file that was
/// already selected (`piece_ranges` is `None` for padding files).
fn needs_recheck(
    piece_ranges: &[Option<std::ops::Range<u32>>],
    old: &HashSet<usize>,
    new: &HashSet<usize>,
) -> bool {
    let overlaps =
        |a: &std::ops::Range<u32>, b: &std::ops::Range<u32>| a.start < b.end && b.start < a.end;
    new.difference(old).any(|&added| {
        let Some(Some(a)) = piece_ranges.get(added) else {
            return false;
        };
        old.iter().any(|&kept| {
            kept != added && matches!(piece_ranges.get(kept), Some(Some(b)) if overlaps(a, b))
        })
    })
}

/// Stop each torrent as soon as its selected files are complete, so it
/// never sits there as a seed. Runs until the session stops.
async fn stop_finished_torrents(session: Arc<Session>) {
    let cancelled = session.cancellation_token().clone();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = cancelled.cancelled() => return,
            _ = tick.tick() => {}
        }
        let torrents: Vec<TorrentHandle> =
            session.with_torrents(|it| it.map(|(_, h)| h.clone()).collect());
        for handle in torrents {
            let stats = handle.stats();
            if matches!(stats.state, TorrentStatsState::Live)
                && stats.finished
                && let Err(e) = session.pause(&handle).await
            {
                tracing::warn!("could not stop finished torrent: {e:#}");
            }
        }
    }
}

/// Read a .torrent file's contents without the torrent engine (Debrid
/// downloads use this; no BitTorrent session needed).
pub fn listing_from_bytes(torrent_bytes: Vec<u8>) -> Result<Listing> {
    let torrent = torrent_from_bytes(&torrent_bytes)
        .map_err(|e| anyhow::anyhow!("invalid .torrent file: {e}"))?;
    let info_hash = torrent.info_hash.as_string();
    let info = torrent
        .info
        .data
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid .torrent file: {e}"))?;
    Ok(build_listing(&info, torrent_bytes.clone(), info_hash))
}

fn build_listing<B: AsRef<[u8]>>(
    info: &ValidatedTorrentMetaV1Info<B>,
    torrent_bytes: Vec<u8>,
    info_hash: String,
) -> Listing {
    let files = info
        .iter_file_details()
        .enumerate()
        .filter(|(_, d)| !d.attrs().padding)
        .map(|(index, d)| TorrentFile {
            index,
            path: d.filename.iter_components().collect::<Vec<_>>().join("/"),
            len: d.len,
        })
        .collect();
    Listing {
        name: info.name().map(|n| n.into_owned()).unwrap_or(info_hash),
        torrent_bytes,
        files,
        // Single-file torrents set `length`; multi-file ones list `files` instead.
        multi_file: info.info().length.is_none(),
        existing: None,
    }
}

/// Make a torrent name safe to use as one folder name.
pub fn sanitize_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "torrent".to_owned()
    } else {
        cleaned.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recheck_only_when_a_new_file_shares_a_piece_with_a_selected_one() {
        // E00 ends in piece 10, which E01 starts in; E02 starts cleanly at 20.
        let ranges = vec![Some(0..11), Some(10..20), Some(20..30), None, Some(30..31)];
        let set = |v: &[usize]| v.iter().copied().collect::<HashSet<usize>>();
        assert!(needs_recheck(&ranges, &set(&[1]), &set(&[0, 1])));
        assert!(!needs_recheck(&ranges, &set(&[1]), &set(&[1, 2])));
        assert!(
            !needs_recheck(&ranges, &set(&[0, 1]), &set(&[1])),
            "deselecting never needs it"
        );
        assert!(
            !needs_recheck(&ranges, &set(&[0]), &set(&[0, 3])),
            "padding files don't count"
        );
    }

    #[test]
    fn folder_names_cannot_escape_the_download_dir() {
        assert_eq!(sanitize_folder_name("[PMR] Frieren"), "[PMR] Frieren");
        assert_eq!(sanitize_folder_name("../../etc"), ".._.._etc");
        assert_eq!(sanitize_folder_name(".."), "torrent");
        assert_eq!(sanitize_folder_name("  "), "torrent");
        assert_eq!(sanitize_folder_name("a/b\\c"), "a_b_c");
    }
}
