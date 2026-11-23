//! A watch party in the app (`crate::party`): hosting one or joining one,
//! who has the episode, and this app's player kept with the host's.
//!
//! Only the host controls playback. The host's player says where it is;
//! a member's player follows it (opened, paused, played and moved by the
//! party), and a member's pause asks the host instead. Nothing starts until
//! everyone has the episode on disk.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::watching::episode_files;
use super::{App, Msg};
use crate::party::{
    self, Ask, Episode, HOST, Have, HostEvent, Hosting, Joined, Member, MemberEvent, MemberId,
    Playback, ToHost, ToMember,
};

/// How far (seconds) a member's player may be from the host's before
/// it's moved.
const DRIFT: f64 = 1.0;
/// The host says where its player is at least this often.
const HEARTBEAT: Duration = Duration::from_secs(3);
/// After telling the player something, give it this long to do it.
const SETTLE: Duration = Duration::from_millis(1500);
/// A player opening (the phone's takes a moment).
const OPENING: Duration = Duration::from_secs(6);
/// How often whether you have the episode is looked at.
const LOOK: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct PartyState {
    pub(crate) role: Option<Role>,
    /// The code typed in, to join with.
    pub(crate) code: String,
    /// Starting a party, or joining one.
    pub(crate) busy: bool,
    /// Why the last try didn't work.
    pub(crate) error: Option<String>,
    pub(super) seq: u64,
    looked: Option<Instant>,
    /// Sharing the episode with members (and getting it from them).
    pub(crate) share: super::party_share::Sharing,
}

pub(crate) enum Role {
    Host(HostSide),
    Member(MemberSide),
}

pub(crate) struct HostSide {
    hosting: Option<Hosting>,
    pub(crate) code: String,
    /// Everyone else, by number.
    pub(crate) members: BTreeMap<MemberId, Member>,
    /// Whether you have the episode.
    pub(crate) me: Have,
    pub(crate) episode: Option<Episode>,
    /// It starts once everyone has the episode.
    pub(crate) starting: bool,
    sent: Option<Playback>,
    sent_at: Instant,
}

pub(crate) struct MemberSide {
    joined: Option<Joined>,
    pub(crate) you: MemberId,
    /// Everyone, the host first.
    pub(crate) members: Vec<Member>,
    pub(crate) episode: Option<Episode>,
    /// The host's player (on the host's clock).
    playback: Option<Playback>,
    /// The host's clock, less this app's (ms).
    offset: i64,
    pub(crate) me: Have,
    sent_have: Option<(u64, u32, Have)>,
    /// Your player follows the host's (unless you closed it).
    pub(crate) following: bool,
    /// Your player has the party's episode open (the party opened it).
    shown: bool,
    told: Option<Instant>,
    opened: Option<Instant>,
}

impl HostSide {
    pub(super) fn hosting(&self) -> Option<&Hosting> {
        self.hosting.as_ref()
    }
}

impl MemberSide {
    pub(super) fn joined(&self) -> Option<&Joined> {
        self.joined.as_ref()
    }

    /// The host's player is playing (or paused) an episode.
    pub(crate) fn host_playing(&self) -> bool {
        self.playback.is_some() && self.episode.is_some()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum PartyAction {
    Host,
    /// Join with the code typed in.
    Join,
    SetCode(String),
    SetName(String),
    /// Leave (or, the host: end it for everyone).
    Leave,
    /// The host: start the episode for everyone once they all have it.
    Start,
    /// The host: don't start it after all.
    Cancel,
    /// A member: ask the host's player to pause or play.
    Ask(Ask),
    /// A member: follow the host's player again (after closing yours).
    Follow,
}

pub(super) enum PartyMsg {
    Hosted {
        seq: u64,
        result: Result<Hosting, String>,
    },
    Joined {
        seq: u64,
        result: Result<Joined, String>,
    },
    Host(u64, HostEvent),
    Member(u64, MemberEvent),
    /// Seeding the episode to member `to` (or why not).
    Seeding {
        seq: u64,
        to: MemberId,
        result: Result<crate::party_seed::Seeder, String>,
    },
}

/// What this app's player is playing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Watching {
    pub(crate) series: u64,
    pub(crate) episode: u32,
    pub(crate) position: f64,
    pub(crate) playing: bool,
}

/// What the party wants this app's player to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PlayerCmd {
    /// Play an episode (`at`: from there; none: from where you left it).
    Open {
        series: u64,
        episode: u32,
        at: Option<f64>,
        playing: bool,
    },
    Seek(f64),
    Pause,
    Play,
    Close,
}

/// "Has it", "Downloading (45%)", "Doesn't have it".
pub(crate) fn have_text(have: Have) -> String {
    match have {
        Have::Ready => "Has it".into(),
        Have::Downloading(Some(f)) => format!("Downloading ({:.0}%)", f * 100.0),
        Have::Downloading(None) => "Downloading".into(),
        Have::Missing => "Doesn't have it".into(),
    }
}

/// Your name, unless you've given one: your account's on this computer.
fn default_name() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let mut chars = user.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Someone".into(),
    }
}

impl App {
    /// Your name in parties.
    pub(crate) fn party_name(&self) -> String {
        match self.settings.party_name.trim() {
            "" => default_name(),
            name => party::clean_name(name),
        }
    }

    /// The network's news, as messages to the app.
    fn party_events<E: Send + 'static>(
        &self,
        wrap: impl Fn(E) -> PartyMsg + Send + Sync + 'static,
    ) -> impl Fn(E) + Send + Sync + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |e| {
            let _ = tx.send(Msg::Party(wrap(e)));
            ctx.request_repaint();
        }
    }

    pub(crate) fn party_action(&mut self, action: PartyAction) {
        match action {
            PartyAction::SetCode(code) => self.party.code = code,
            PartyAction::SetName(name) => self.settings.party_name = name,
            PartyAction::Host => {
                if self.party.role.is_some() || self.party.busy {
                    return;
                }
                self.party.busy = true;
                self.party.error = None;
                self.party.seq += 1;
                let seq = self.party.seq;
                let events = self.party_events(move |e| PartyMsg::Host(seq, e));
                self.spawn(async move {
                    let result = Hosting::start(events).await.map_err(|e| format!("{e:#}"));
                    Msg::Party(PartyMsg::Hosted { seq, result })
                });
            }
            PartyAction::Join => {
                if self.party.role.is_some() || self.party.busy {
                    return;
                }
                let invite = match party::Invite::parse(&self.party.code) {
                    Ok(i) => i,
                    Err(e) => {
                        self.party.error = Some(e);
                        return;
                    }
                };
                self.party.busy = true;
                self.party.error = None;
                self.party.seq += 1;
                let seq = self.party.seq;
                let name = self.party_name();
                let events = self.party_events(move |e| PartyMsg::Member(seq, e));
                self.spawn(async move {
                    let result = Joined::join(&invite, &name, events)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    Msg::Party(PartyMsg::Joined { seq, result })
                });
            }
            PartyAction::Leave => self.leave_party(),
            PartyAction::Start => {
                if let Some(Role::Host(h)) = &mut self.party.role
                    && h.episode.is_some()
                {
                    h.starting = true;
                }
            }
            PartyAction::Cancel => {
                if let Some(Role::Host(h)) = &mut self.party.role {
                    h.starting = false;
                }
            }
            PartyAction::Ask(ask) => {
                if let Some(Role::Member(m)) = &self.party.role
                    && let Some(j) = &m.joined
                {
                    j.send(ToHost::Ask(ask));
                    self.set_info(match ask {
                        Ask::Pause => "Asked the host to pause",
                        Ask::Play => "Asked the host to play",
                    });
                }
            }
            PartyAction::Follow => {
                if let Some(Role::Member(m)) = &mut self.party.role {
                    m.following = true;
                    m.told = None;
                    m.opened = None;
                }
            }
        }
    }

    fn leave_party(&mut self) {
        // News from this party is old news now.
        self.party.seq += 1;
        self.share_reset();
        match self.party.role.take() {
            Some(Role::Host(mut h)) => {
                if let Some(hosting) = h.hosting.take() {
                    self.rt
                        .spawn(async move { hosting.end("The host ended the party").await });
                }
                self.set_info("The party's over");
            }
            Some(Role::Member(mut m)) => {
                if let Some(j) = m.joined.take() {
                    self.rt.spawn(async move { j.leave().await });
                }
                self.set_info("You left the party");
            }
            None => {}
        }
    }

    /// The app's closing: the party ends (or you leave it), said before
    /// the app goes.
    pub(super) fn party_shut_down(&mut self) {
        self.share_reset();
        let end = match self.party.role.take() {
            Some(Role::Host(mut h)) => h.hosting.take().map(|hosting| {
                Box::pin(async move { hosting.end("The host closed Ichioshi").await })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            }),
            Some(Role::Member(mut m)) => m
                .joined
                .take()
                .map(|j| Box::pin(async move { j.leave().await }) as _),
            None => None,
        };
        let Some(end) = end else { return };
        let end = async {
            let _ = tokio::time::timeout(Duration::from_secs(2), end).await;
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.rt.block_on(end));
        } else {
            self.rt.block_on(end);
        }
    }

    pub(super) fn handle_party_msg(&mut self, msg: PartyMsg) {
        match msg {
            PartyMsg::Hosted { seq, result } => {
                if seq != self.party.seq {
                    if let Ok(h) = result {
                        self.rt
                            .spawn(async move { h.end("The party was called off").await });
                    }
                    return;
                }
                self.party.busy = false;
                match result {
                    Ok(hosting) => {
                        let code = hosting.invite.code();
                        self.party.role = Some(Role::Host(HostSide {
                            hosting: Some(hosting),
                            code,
                            members: BTreeMap::new(),
                            me: Have::Missing,
                            episode: None,
                            starting: false,
                            sent: None,
                            sent_at: Instant::now(),
                        }));
                        self.party.looked = None;
                        self.set_info("Your party's on: send your friends its code");
                    }
                    Err(e) => self.party.error = Some(e),
                }
            }
            PartyMsg::Joined { seq, result } => {
                if seq != self.party.seq {
                    if let Ok(j) = result {
                        self.rt.spawn(async move { j.leave().await });
                    }
                    return;
                }
                self.party.busy = false;
                match result {
                    Ok(joined) => {
                        self.party.role = Some(Role::Member(MemberSide {
                            you: joined.you,
                            joined: Some(joined),
                            members: Vec::new(),
                            episode: None,
                            playback: None,
                            offset: 0,
                            me: Have::Missing,
                            sent_have: None,
                            following: true,
                            shown: false,
                            told: None,
                            opened: None,
                        }));
                        self.party.code.clear();
                        self.party.looked = None;
                        self.set_info("You're in the party");
                    }
                    Err(e) => self.party.error = Some(e),
                }
            }
            PartyMsg::Host(seq, event) if seq == self.party.seq => self.host_event(event),
            PartyMsg::Member(seq, event) if seq == self.party.seq => self.member_event(event),
            PartyMsg::Host(..) | PartyMsg::Member(..) => {}
            PartyMsg::Seeding { seq, to, result } => {
                // (An old party's seeder stops here, dropped.)
                if seq == self.party.seq {
                    self.share_started(to, result);
                }
            }
        }
    }

    fn host_event(&mut self, event: HostEvent) {
        let event = match event {
            HostEvent::Said {
                id,
                msg: ToHost::Torrenting(w),
            } => return self.share_wanted(id, w),
            HostEvent::Said {
                msg: ToHost::Seeding { to, addrs },
                ..
            } => return self.share_seeding(to, addrs),
            other => other,
        };
        let Some(Role::Host(h)) = &mut self.party.role else {
            return;
        };
        let note = match event {
            HostEvent::Joined { id, name } => {
                let note = format!("{name} joined the party");
                h.members.insert(
                    id,
                    Member {
                        id,
                        name,
                        have: Have::Missing,
                    },
                );
                if let Some(hosting) = &h.hosting {
                    hosting.send(id, ToMember::Episode(h.episode.clone()));
                    hosting.send(id, ToMember::Playback(h.sent));
                }
                Some(note)
            }
            HostEvent::Said {
                id,
                msg:
                    ToHost::Have {
                        series,
                        episode,
                        have,
                    },
            } => {
                let current = h
                    .episode
                    .as_ref()
                    .is_some_and(|e| e.series == series && e.episode == episode);
                if let Some(m) = h.members.get_mut(&id)
                    && current
                {
                    m.have = have;
                }
                None
            }
            HostEvent::Said {
                id,
                msg: ToHost::Ask(ask),
            } => {
                let name = h
                    .members
                    .get(&id)
                    .map_or("Someone".into(), |m| m.name.clone());
                Some(match ask {
                    Ask::Pause => format!("{name} asks you to pause"),
                    Ask::Play => format!("{name} asks you to play"),
                })
            }
            HostEvent::Said { .. } => None,
            HostEvent::Left { id } => h
                .members
                .remove(&id)
                .map(|m| format!("{} left the party", m.name)),
        };
        self.send_members();
        if let Some(note) = note {
            self.set_info(note);
        }
    }

    fn member_event(&mut self, event: MemberEvent) {
        let event = match event {
            MemberEvent::Said(ToMember::Wanted(w)) => return self.seed_for(w),
            MemberEvent::Said(ToMember::Seeders(addrs)) => return self.share_peers(addrs),
            other => other,
        };
        let Some(Role::Member(m)) = &mut self.party.role else {
            return;
        };
        match event {
            MemberEvent::Said(ToMember::Members(members)) => m.members = members,
            MemberEvent::Said(ToMember::Episode(episode)) => {
                if episode != m.episode {
                    m.episode = episode;
                    self.party.share = Default::default();
                    m.me = Have::Missing;
                    m.sent_have = None;
                    m.following = true;
                    self.party.looked = None;
                }
            }
            MemberEvent::Said(ToMember::Playback(playback)) => m.playback = playback,
            MemberEvent::Said(_) => {}
            MemberEvent::Clock(offset) => m.offset = offset,
            MemberEvent::Lost(why) => {
                self.party.role = None;
                self.party.seq += 1;
                self.party.share = Default::default();
                self.set_info(why);
            }
        }
    }

    /// The host: tell everyone who's there, and who has the episode.
    fn send_members(&self) {
        let Some(Role::Host(h)) = &self.party.role else {
            return;
        };
        let Some(hosting) = &h.hosting else { return };
        let mut all = vec![Member {
            id: HOST,
            name: self.party_name(),
            have: h.me,
        }];
        all.extend(h.members.values().cloned());
        hosting.send_all(ToMember::Members(all));
    }

    /// Whether you have an episode on disk (or are getting it).
    fn party_have(&self, series: u64, episode: u32) -> Have {
        let Some(s) = self.library.find(series) else {
            return Have::Missing;
        };
        if self.servers_mode() {
            if self.offline_copy(series, episode).is_some() {
                return Have::Ready;
            }
            return match self.offline_job(series, episode) {
                Some(j) => Have::Downloading(Some(j.fraction())),
                None => Have::Missing,
            };
        }
        if episode_files(s, &self.busy_files()).contains_key(&episode) {
            return Have::Ready;
        }
        match self
            .pages
            .stats
            .get(&series)
            .and_then(|st| st.progress.get(&episode))
        {
            Some(f) => Have::Downloading(Some(*f)),
            None => Have::Missing,
        }
    }

    /// Now and then (with the app's duties): whether you have the episode,
    /// told to the party when it changes.
    pub(super) fn party_tick(&mut self) {
        if self.party.role.is_none() || self.party.looked.is_some_and(|t| t.elapsed() < LOOK) {
            return;
        }
        self.party.looked = Some(Instant::now());
        let chosen = match &self.party.role {
            Some(Role::Host(h)) => h.episode.as_ref(),
            Some(Role::Member(m)) => m.episode.as_ref(),
            None => None,
        };
        let have = chosen.map(|e| (e.series, e.episode, self.party_have(e.series, e.episode)));
        match &mut self.party.role {
            Some(Role::Host(h)) => {
                let me = have.map_or(Have::Missing, |(_, _, h)| h);
                if me != h.me {
                    h.me = me;
                    self.send_members();
                }
            }
            Some(Role::Member(m)) => {
                m.me = have.map_or(Have::Missing, |(_, _, h)| h);
                if let Some(have) = have
                    && m.sent_have != Some(have)
                    && let Some(j) = &m.joined
                {
                    j.send(ToHost::Have {
                        series: have.0,
                        episode: have.1,
                        have: have.2,
                    });
                    m.sent_have = Some(have);
                }
            }
            None => {}
        }
        self.share_tick();
    }

    /// The host: everyone else who doesn't have the episode yet.
    pub(crate) fn party_waiting_for(&self) -> Vec<String> {
        match &self.party.role {
            Some(Role::Host(h)) => h
                .members
                .values()
                .filter(|m| m.have != Have::Ready)
                .map(|m| m.name.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Everyone in the party: number, name, whether they have the episode
    /// (you included).
    pub(crate) fn party_people(&self) -> Vec<(MemberId, String, Have, bool)> {
        match &self.party.role {
            Some(Role::Host(h)) => std::iter::once((HOST, self.party_name(), h.me, true))
                .chain(
                    h.members
                        .values()
                        .map(|m| (m.id, m.name.clone(), m.have, false)),
                )
                .collect(),
            Some(Role::Member(m)) => m
                .members
                .iter()
                .map(|p| {
                    let you = p.id == m.you;
                    (p.id, p.name.clone(), if you { m.me } else { p.have }, you)
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// How many are in your party (none: you're not in one).
    pub(crate) fn party_size(&self) -> Option<usize> {
        match &self.party.role {
            Some(Role::Host(h)) => Some(h.members.len() + 1),
            Some(Role::Member(m)) => Some(m.members.len().max(1)),
            None => None,
        }
    }

    /// The host's name (you're a member).
    pub(crate) fn party_host_name(&self) -> Option<String> {
        match &self.party.role {
            Some(Role::Member(m)) => m
                .members
                .iter()
                .find(|p| p.id == HOST)
                .map(|p| p.name.clone()),
            _ => None,
        }
    }

    pub(crate) fn party_episode(&self) -> Option<&Episode> {
        match &self.party.role {
            Some(Role::Host(h)) => h.episode.as_ref(),
            Some(Role::Member(m)) => m.episode.as_ref(),
            None => None,
        }
    }

    /// The party's episode, as you'd say it.
    pub(crate) fn episode_label(&self, e: &Episode) -> String {
        let name = self
            .library
            .find(e.series)
            .and_then(|s| s.details.as_ref()?.episode(e.episode)?.title.clone())
            .filter(|t| !t.starts_with("Episode "));
        match name {
            Some(n) => format!("{} · Episode {}: {n}", e.title, e.episode),
            None => format!("{} · Episode {}", e.title, e.episode),
        }
    }

    /// A member whose player follows the host's: only the host controls it.
    pub(crate) fn party_locked(&self) -> bool {
        matches!(&self.party.role, Some(Role::Member(m)) if m.following && m.host_playing())
    }

    /// Something's to play. The host: it's the party's episode now, and it
    /// waits until everyone has it (`true`: don't play it yet). A member:
    /// not while the host's player is going.
    pub(crate) fn party_play(&mut self, series: u64, episode: u32) -> bool {
        let title = self
            .library
            .find(series)
            .map(|s| s.title().to_owned())
            .unwrap_or_default();
        match &mut self.party.role {
            None => false,
            Some(Role::Member(m)) => {
                if m.following && m.host_playing() {
                    self.set_info("The host chooses what plays");
                    return true;
                }
                false
            }
            Some(Role::Host(h)) => {
                let new = Episode {
                    series,
                    episode,
                    title,
                };
                let changed = h.episode.as_ref() != Some(&new);
                if changed {
                    h.episode = Some(new.clone());
                    self.party.share = Default::default();
                    for m in h.members.values_mut() {
                        m.have = Have::Missing;
                    }
                    if let Some(hosting) = &h.hosting {
                        hosting.send_all(ToMember::Episode(Some(new.clone())));
                    }
                    self.party.looked = None;
                }
                let waiting: Vec<String> = h
                    .members
                    .values()
                    .filter(|m| m.have != Have::Ready)
                    .map(|m| m.name.clone())
                    .collect();
                if waiting.is_empty() {
                    h.starting = false;
                    if changed {
                        self.send_members();
                    }
                    return false;
                }
                h.starting = true;
                let label = self.episode_label(&new);
                self.send_members();
                self.set_info(format!(
                    "{label} starts once {} {} it",
                    join_names(&waiting),
                    if waiting.len() == 1 { "has" } else { "have" }
                ));
                true
            }
        }
    }

    /// The party and this app's player, often (every quarter second or
    /// so): what the player's doing (`watching`), and what the party wants
    /// it to do.
    pub(crate) fn party_player(&mut self, watching: Option<Watching>) -> Option<PlayerCmd> {
        match &mut self.party.role {
            None => None,
            Some(Role::Host(h)) => {
                // Everyone has it now: it starts.
                if h.starting
                    && h.members.values().all(|m| m.have == Have::Ready)
                    && let Some(e) = &h.episode
                {
                    h.starting = false;
                    return Some(PlayerCmd::Open {
                        series: e.series,
                        episode: e.episode,
                        at: None,
                        playing: true,
                    });
                }
                let Some(hosting) = &h.hosting else {
                    return None;
                };
                match watching {
                    Some(w) => {
                        let on = h
                            .episode
                            .as_ref()
                            .is_some_and(|e| e.series == w.series && e.episode == w.episode);
                        if !on {
                            // Playing something else: that's the episode now.
                            let title = self
                                .library
                                .find(w.series)
                                .map(|s| s.title().to_owned())
                                .unwrap_or_default();
                            let e = Episode {
                                series: w.series,
                                episode: w.episode,
                                title,
                            };
                            hosting.send_all(ToMember::Episode(Some(e.clone())));
                            h.episode = Some(e);
                            self.party.share = Default::default();
                            for m in h.members.values_mut() {
                                m.have = Have::Missing;
                            }
                            self.party.looked = None;
                        }
                        let now = Playback {
                            position: w.position,
                            playing: w.playing,
                            at: party::now_ms(),
                        };
                        let moved = h.sent.is_none_or(|s| {
                            s.playing != now.playing
                                || (s.position_at(now.at) - now.position).abs() > DRIFT
                        });
                        if moved || h.sent_at.elapsed() >= HEARTBEAT {
                            hosting.send_all(ToMember::Playback(Some(now)));
                            h.sent = Some(now);
                            h.sent_at = Instant::now();
                        }
                    }
                    None => {
                        if h.sent.take().is_some() {
                            hosting.send_all(ToMember::Playback(None));
                        }
                    }
                }
                None
            }
            Some(Role::Member(m)) => {
                let on_episode =
                    |w: &Watching, e: &Episode| w.series == e.series && w.episode == e.episode;
                let (Some(pb), Some(e)) = (m.playback, m.episode.clone()) else {
                    // The host stopped: so does yours, if the party opened it.
                    if m.shown {
                        m.shown = false;
                        if let (Some(w), Some(e)) = (watching, &m.episode)
                            && on_episode(&w, e)
                        {
                            return Some(PlayerCmd::Close);
                        }
                    }
                    return None;
                };
                if m.me != Have::Ready || !m.following {
                    return None;
                }
                if m.told.is_some_and(|t| t.elapsed() < SETTLE) {
                    return None;
                }
                let host_now = (party::now_ms() as i64 + m.offset).max(0) as u64;
                let expected = pb.position_at(host_now);
                let open = PlayerCmd::Open {
                    series: e.series,
                    episode: e.episode,
                    at: Some(expected),
                    playing: pb.playing,
                };
                match watching {
                    None if m.shown => {
                        // You closed it: it stays closed until you follow again.
                        m.shown = false;
                        m.following = false;
                        None
                    }
                    Some(w) if !on_episode(&w, &e) => {
                        if m.opened.is_some_and(|t| t.elapsed() < OPENING) {
                            return None;
                        }
                        m.opened = Some(Instant::now());
                        m.told = Some(Instant::now());
                        Some(open)
                    }
                    None => {
                        if m.opened.is_some_and(|t| t.elapsed() < OPENING) {
                            return None;
                        }
                        m.opened = Some(Instant::now());
                        m.told = Some(Instant::now());
                        Some(open)
                    }
                    Some(w) => {
                        m.shown = true;
                        m.opened = None;
                        if w.playing != pb.playing {
                            m.told = Some(Instant::now());
                            return Some(if pb.playing {
                                PlayerCmd::Play
                            } else {
                                PlayerCmd::Pause
                            });
                        }
                        if (w.position - expected).abs() > DRIFT {
                            m.told = Some(Instant::now());
                            return Some(PlayerCmd::Seek(expected));
                        }
                        None
                    }
                }
            }
        }
    }
}

/// "Mai", "Mai and Ren", "Mai, Ren and Sora".
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_join_as_said() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(join_names(&n(&["Mai"])), "Mai");
        assert_eq!(join_names(&n(&["Mai", "Ren"])), "Mai and Ren");
        assert_eq!(join_names(&n(&["Mai", "Ren", "Sora"])), "Mai, Ren and Sora");
    }
}
