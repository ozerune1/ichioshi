//! Watch party members sharing the episode with each other over BitTorrent
//! (`crate::party_seed`): the one time anything is seeded.
//!
//! Someone downloading the episode with BitTorrent tells the party (its
//! .torrent, which file, and their BitTorrent ID), but only if BitTorrent
//! is all they have: with a debrid service, Usenet or their own servers set
//! up, they get it that way, and nobody seeds. The host passes that on
//! to those who have the episode. Each checks their copy against the
//! torrent, seeds it to that member only, and says where; the member's
//! engine connects there too. Seeding to a member stops once they have the
//! episode, and all of it when the party ends or moves on to another
//! episode. Only where to connect goes through the party's connection; the
//! video goes over BitTorrent.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use base64::Engine as _;

use super::party_ui::{PartyMsg, Role};
use super::watching::episode_files;
use super::{App, Msg};
use crate::party::{HOST, Have, MemberId, ToHost, ToMember, Wanted};
use crate::party_seed::{self, Seeder};

#[derive(Default)]
pub(crate) struct Sharing {
    /// Seeding to these members.
    pub(crate) seeding: HashMap<MemberId, Seeder>,
    /// Checking your copy, to seed it to these.
    preparing: HashSet<MemberId>,
    /// Your copy isn't theirs (another release): not tried again.
    refused: HashSet<MemberId>,
    /// You told the party you're downloading it: series, episode, and the
    /// torrent's info hash.
    asked: Option<(u64, u32, String)>,
    /// Members seeding it to you, given to the engine.
    pub(crate) peers: Vec<SocketAddr>,
    /// The host: who's downloading it, and who's been told.
    wanted: BTreeMap<MemberId, Wanted>,
    told: HashSet<(MemberId, MemberId)>,
}

fn hex(id: &[u8; 20]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 20]> {
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect::<Option<_>>()?;
    bytes.try_into().ok()
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

impl App {
    /// Stop sharing (the party's over, or on to another episode).
    pub(super) fn share_reset(&mut self) {
        self.party.share = Sharing::default();
    }

    /// The engine's torrent downloading the episode: its info hash, its
    /// .torrent, and the episode's file in it.
    fn party_torrent(&self, series: u64, episode: u32) -> Option<(String, Vec<u8>, usize)> {
        let engine = self.engine()?;
        let s = self.library.find(series)?;
        for (_, h) in engine.torrents() {
            let hash = h.info_hash().as_string().to_ascii_lowercase();
            if h.stats().finished || !s.downloads.contains(&hash) {
                continue;
            }
            let Ok(listing) = engine.listing_for(&h) else {
                continue;
            };
            let only = h.only_files();
            let files: Vec<_> = listing
                .files
                .iter()
                .filter(|f| only.as_ref().is_none_or(|o| o.contains(&f.index)))
                .collect();
            let named: Vec<(&str, u64)> = files.iter().map(|f| (f.path.as_str(), f.len)).collect();
            let eps = crate::title::episodes_of_files(&named, s.one_episode());
            if let Some((f, _)) = files.iter().zip(eps).find(|(_, ep)| *ep == Some(episode)) {
                return Some((hash, listing.torrent_bytes.clone(), f.index));
            }
        }
        None
    }

    /// Whether BitTorrent is your only way to get episodes (no debrid
    /// service, Usenet or servers set up): only then do you ask the party
    /// to seed you the episode.
    pub(crate) fn torrent_only(&self) -> bool {
        !self.servers_mode() && !self.debrid.config.active() && !self.usenet.active()
    }

    /// Your copy of the episode, and what the app changed in it (its
    /// default tracks), as it was.
    fn party_file(
        &self,
        series: u64,
        episode: u32,
    ) -> Option<(PathBuf, Option<crate::mkv::Replaced>)> {
        if self.on_servers(series) {
            return Some((self.offline_copy(series, episode)?.clone(), None));
        }
        let s = self.library.find(series)?;
        let path = episode_files(s, &self.busy_files()).remove(&episode)?;
        let original = path
            .strip_prefix(&s.folder)
            .ok()
            .and_then(|rel| s.applied.get(&*rel.to_string_lossy()))
            .and_then(|a| a.original.as_ref()?.decoded());
        Some((path, original))
    }

    /// A member's name.
    pub(crate) fn party_member_name(&self, id: MemberId) -> String {
        self.party_people()
            .into_iter()
            .find(|p| p.0 == id)
            .map_or("someone".into(), |p| p.1)
    }

    /// Now and then (with the party's tick): say you're downloading it,
    /// pass on who is (the host), and stop seeding to those who have it.
    pub(super) fn share_tick(&mut self) {
        let Some(e) = self.party_episode().cloned() else {
            return;
        };
        let (me, you) = match &self.party.role {
            Some(Role::Host(h)) => (h.me, HOST),
            Some(Role::Member(m)) => (m.me, m.you),
            None => return,
        };
        // Downloading it with BitTorrent: say so, once.
        let asked = self
            .party
            .share
            .asked
            .as_ref()
            .is_some_and(|(s, ep, _)| (*s, *ep) == (e.series, e.episode));
        if matches!(me, Have::Downloading(_))
            && !asked
            && self.torrent_only()
            && let (Some((hash, torrent, file)), Some(engine)) =
                (self.party_torrent(e.series, e.episode), self.engine())
        {
            let w = Wanted {
                member: you,
                series: e.series,
                episode: e.episode,
                torrent: b64().encode(torrent),
                file,
                peer_id: hex(&engine.peer_id()),
            };
            self.party.share.asked = Some((e.series, e.episode, hash));
            match &self.party.role {
                Some(Role::Host(_)) => {
                    self.party.share.wanted.insert(HOST, w);
                }
                Some(Role::Member(m)) => {
                    if let Some(j) = m.joined() {
                        j.send(ToHost::Torrenting(w));
                    }
                }
                None => {}
            }
        }
        // Everyone who has it now (and who's still here).
        let (have_it, here): (HashSet<MemberId>, HashSet<MemberId>) = match &self.party.role {
            Some(Role::Host(h)) => {
                let mut have: HashSet<MemberId> = h
                    .members
                    .values()
                    .filter(|m| m.have == Have::Ready)
                    .map(|m| m.id)
                    .collect();
                if h.me == Have::Ready {
                    have.insert(HOST);
                }
                let mut here: HashSet<MemberId> = h.members.keys().copied().collect();
                here.insert(HOST);
                (have, here)
            }
            Some(Role::Member(m)) => (
                m.members
                    .iter()
                    .filter(|p| p.have == Have::Ready)
                    .map(|p| p.id)
                    .collect(),
                m.members.iter().map(|p| p.id).collect(),
            ),
            None => return,
        };
        // The host: tell those who have it who's downloading it, and seed it
        // yourself if you have it.
        let mut seed = Vec::new();
        if let Some(Role::Host(h)) = &self.party.role {
            let share = &mut self.party.share;
            share.wanted.retain(|from, w| {
                here.contains(from)
                    && !have_it.contains(from)
                    && (w.series, w.episode) == (e.series, e.episode)
            });
            for (from, w) in &share.wanted {
                for to in have_it.iter().filter(|to| **to != *from && **to != HOST) {
                    if share.told.insert((*from, *to))
                        && let Some(hosting) = h.hosting()
                    {
                        hosting.send(*to, ToMember::Wanted(w.clone()));
                    }
                }
                if *from != HOST && h.me == Have::Ready {
                    seed.push(w.clone());
                }
            }
        }
        for w in seed {
            self.seed_for(w);
        }
        // Seeding to someone who has it now, or who's left: it stops.
        let done: Vec<MemberId> = self
            .party
            .share
            .seeding
            .keys()
            .filter(|id| have_it.contains(id) || !here.contains(id))
            .copied()
            .collect();
        for id in done {
            if let Some(s) = self.party.share.seeding.remove(&id) {
                let sent = s.sent.load(Ordering::Relaxed);
                self.set_info(format!(
                    "{} has the episode now: stopped sharing it ({} sent)",
                    self.party_member_name(id),
                    super::fmt_bytes(sent)
                ));
            }
        }
    }

    /// A member's downloading the episode with BitTorrent: seed it to them,
    /// if your copy's the same (checked against the torrent first).
    pub(super) fn seed_for(&mut self, w: Wanted) {
        let share = &self.party.share;
        if share.seeding.contains_key(&w.member)
            || share.preparing.contains(&w.member)
            || share.refused.contains(&w.member)
            || self
                .party_episode()
                .is_none_or(|e| (e.series, e.episode) != (w.series, w.episode))
        {
            return;
        }
        let Some((path, original)) = self.party_file(w.series, w.episode) else {
            return;
        };
        let (Ok(torrent), Some(peer)) = (b64().decode(&w.torrent), unhex(&w.peer_id)) else {
            return;
        };
        self.party.share.preparing.insert(w.member);
        let (seq, to, file) = (self.party.seq, w.member, w.file);
        self.spawn(async move {
            let served = tokio::task::spawn_blocking(move || {
                party_seed::check(&torrent, file, &path, original)
            })
            .await;
            let result = match served {
                Ok(Ok(s)) if s.count().0 > 0 => Seeder::start(s, vec![peer])
                    .await
                    .map_err(|e| format!("{e:#}")),
                Ok(Ok(_)) => Err("your copy isn't from the same release".into()),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            Msg::Party(PartyMsg::Seeding { seq, to, result })
        });
    }

    /// Seeding to member `to` (or not): say where.
    pub(super) fn share_started(&mut self, to: MemberId, result: Result<Seeder, String>) {
        self.party.share.preparing.remove(&to);
        let seeder = match result {
            Ok(s) => s,
            Err(e) => {
                tracing::info!("not sharing the episode: {e}");
                self.party.share.refused.insert(to);
                return;
            }
        };
        let addrs = seeder.addrs.clone();
        self.party.share.seeding.insert(to, seeder);
        match &self.party.role {
            Some(Role::Host(h)) => {
                if let Some(hosting) = h.hosting() {
                    hosting.send(to, ToMember::Seeders(addrs));
                }
            }
            Some(Role::Member(m)) => {
                if let Some(j) = m.joined() {
                    j.send(ToHost::Seeding { to, addrs });
                }
            }
            None => {}
        }
        self.set_info(format!(
            "Sharing the episode with {} (only with them, until they have it)",
            self.party_member_name(to)
        ));
    }

    /// The host: a member's downloading the episode with BitTorrent.
    pub(super) fn share_wanted(&mut self, from: MemberId, w: Wanted) {
        self.party
            .share
            .wanted
            .insert(from, Wanted { member: from, ..w });
        // Told those who have it on the next tick.
        self.share_tick();
    }

    /// The host: member `by` is seeding it to `to`, there.
    pub(super) fn share_seeding(&mut self, to: MemberId, addrs: Vec<SocketAddr>) {
        if to == HOST {
            self.share_peers(addrs);
        } else if let Some(Role::Host(h)) = &self.party.role
            && let Some(hosting) = h.hosting()
        {
            hosting.send(to, ToMember::Seeders(addrs));
        }
    }

    /// Members seeding the episode to you: the engine connects to them too.
    pub(super) fn share_peers(&mut self, addrs: Vec<SocketAddr>) {
        let Some((_, _, hash)) = self.party.share.asked.clone() else {
            return;
        };
        let new: Vec<SocketAddr> = addrs
            .into_iter()
            .filter(|a| !self.party.share.peers.contains(a))
            .collect();
        if new.is_empty() {
            return;
        }
        self.party.share.peers.extend(new);
        let Some(engine) = self.engine() else { return };
        let peers = self.party.share.peers.clone();
        self.rt.spawn(async move {
            if let Err(e) = engine.add_peers(&hash, peers).await {
                tracing::warn!("couldn't get it from the party: {e:#}");
            }
        });
        self.set_info("Getting the episode from the party too");
    }

    /// What you're sharing, said: "Sharing it with Mai (12 MB sent)".
    pub(crate) fn share_lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .party
            .share
            .seeding
            .iter()
            .map(|(id, s)| {
                format!(
                    "Sharing it with {} ({} sent); it stops once they have it.",
                    self.party_member_name(*id),
                    super::fmt_bytes(s.sent.load(Ordering::Relaxed))
                )
            })
            .collect();
        if !self.party.share.peers.is_empty() {
            out.push("Getting it from the party, and from BitTorrent.".into());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_go_both_ways() {
        let id = [0xab; 20];
        assert_eq!(unhex(&hex(&id)), Some(id));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex(&"zz".repeat(20)), None);
    }
}
