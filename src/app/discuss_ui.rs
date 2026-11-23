//! Episodes' scores (MyAnimeList's episode polls) on series pages, and
//! reading an episode's discussion on AniList and MyAnimeList.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::discover_ui::Loaded;
use super::{App, Msg, Page};
use crate::discuss::{self, MalEpisode, Site, Thread};

/// MyAnimeList's episode lists, by AniList ID.
#[derive(Default)]
pub(super) struct EpisodeScores {
    pub(super) lists: HashMap<u64, (Loaded<Vec<MalEpisode>>, Instant)>,
}

impl EpisodeScores {
    /// MyAnimeList's episode `n` of an anime, if its list is in.
    pub(super) fn episode(&self, id: u64, n: u32) -> Option<&MalEpisode> {
        match self.lists.get(&id) {
            Some((Loaded::Ready(eps), _)) => eps.iter().find(|e| e.number == n),
            _ => None,
        }
    }
}

/// An episode's discussion, open over its series' page.
pub(super) struct Discussion {
    pub(super) anilist_id: u64,
    pub(super) episode: u32,
    pub(super) title: String,
    pub(super) site: Site,
    /// The thread on each site: `None` when there's none for the episode.
    pub(super) threads: HashMap<Site, Loaded<Option<Thread>>>,
    /// The next page of a site's thread is being read.
    pub(super) more: HashSet<Site>,
    /// Posts whose spoilers are shown (by site and post).
    pub(super) shown: HashSet<(Site, u64)>,
    seq: u64,
}

impl App {
    /// MyAnimeList's ID for an anime, if known.
    pub(super) fn mal_id(&self, id: u64) -> Option<u64> {
        self.details(id)
            .and_then(|d| d.id_mal)
            .or_else(|| self.tracking.config.mal_ids.get(&id).copied())
    }

    /// Read MyAnimeList's episode list (scores, discussion topics) for the
    /// series page showing.
    pub(super) fn request_episode_scores(&mut self) {
        let Page::Anime(ap) = &self.page else { return };
        let id = ap.anime.anilist_id;
        let fresh = |(loaded, at): &(Loaded<Vec<MalEpisode>>, Instant)| match loaded {
            Loaded::Loading => true,
            Loaded::Ready(_) => at.elapsed() < Duration::from_secs(6 * 3600),
            Loaded::Failed(_) => at.elapsed() < Duration::from_secs(300),
        };
        if self.episode_scores.lists.get(&id).is_some_and(fresh) {
            return;
        }
        // Wait for its details (which say its MyAnimeList ID).
        if self.details(id).is_none() && self.pages.details_loading.contains(&id) {
            return;
        }
        let known = self.mal_id(id);
        self.episode_scores
            .lists
            .insert(id, (Loaded::Loading, Instant::now()));
        let (client, api) = (
            self.http.clone(),
            self.tracking.endpoints.anilist_api.clone(),
        );
        self.spawn(async move {
            let result = async {
                let mal = match known {
                    Some(m) => Some(m),
                    None => crate::tracking::anilist::mal_ids(&client, &api, &[id])
                        .await?
                        .get(&id)
                        .copied(),
                };
                let Some(mal) = mal else {
                    return anyhow::Ok((None, Vec::new()));
                };
                Ok((Some(mal), discuss::mal_episodes(&client, mal).await?))
            }
            .await
            .map_err(|e| format!("{e:#}"));
            Msg::MalEpisodes { id, result }
        });
    }

    pub(super) fn handle_mal_episodes(
        &mut self,
        id: u64,
        result: Result<(Option<u64>, Vec<MalEpisode>), String>,
    ) {
        let loaded = match result {
            Ok((mal, eps)) => {
                if let Some(mal) = mal
                    && self.tracking.config.mal_ids.insert(id, mal).is_none()
                {
                    self.tracking.save();
                }
                Loaded::Ready(eps)
            }
            Err(e) => {
                tracing::warn!("MyAnimeList's episodes of {id}: {e}");
                Loaded::Failed(e)
            }
        };
        self.episode_scores
            .lists
            .insert(id, (loaded, Instant::now()));
    }

    /// Open an episode's discussion: find its thread on each site, and read
    /// the first page.
    pub(super) fn open_discussion(&mut self, id: u64, episode: u32) {
        let title = match &self.page {
            Page::Anime(ap) if ap.anime.anilist_id == id => ap.anime.title.clone(),
            _ => self
                .library
                .find(id)
                .map(|s| s.title().to_owned())
                .unwrap_or_default(),
        };
        self.discussion_seq += 1;
        let seq = self.discussion_seq;
        let site = self.discussion.as_ref().map_or(Site::AniList, |d| d.site);
        self.discussion = Some(Discussion {
            anilist_id: id,
            episode,
            title,
            site,
            threads: [Site::AniList, Site::Mal]
                .into_iter()
                .map(|s| (s, Loaded::Loading))
                .collect(),
            more: HashSet::new(),
            shown: HashSet::new(),
            seq,
        });
        let client = self.http.clone();
        self.spawn(async move {
            let result = async {
                match discuss::anilist_thread_for(&client, id, episode).await? {
                    Some(thread) => Ok(Some(Box::new(
                        discuss::anilist_thread(&client, thread, 1).await?,
                    ))),
                    None => anyhow::Ok(None),
                }
            }
            .await
            .map_err(|e| format!("{e:#}"));
            Msg::Discussion {
                seq,
                site: Site::AniList,
                more: false,
                result,
            }
        });
        let topic = self
            .episode_scores
            .episode(id, episode)
            .and_then(|e| e.topic);
        let (known, api) = (self.mal_id(id), self.tracking.endpoints.anilist_api.clone());
        let client = self.http.clone();
        self.spawn(async move {
            let result = async {
                let topic = match topic {
                    Some(t) => Some(t),
                    None => {
                        let mal = match known {
                            Some(m) => Some(m),
                            None => crate::tracking::anilist::mal_ids(&client, &api, &[id])
                                .await?
                                .get(&id)
                                .copied(),
                        };
                        let Some(mal) = mal else {
                            return anyhow::Ok(None);
                        };
                        discuss::mal_episodes(&client, mal)
                            .await?
                            .into_iter()
                            .find(|e| e.number == episode)
                            .and_then(|e| e.topic)
                    }
                };
                match topic {
                    Some(t) => Ok(Some(Box::new(discuss::mal_topic(&client, t, 0).await?))),
                    None => Ok(None),
                }
            }
            .await
            .map_err(|e| format!("{e:#}"));
            Msg::Discussion {
                seq,
                site: Site::Mal,
                more: false,
                result,
            }
        });
    }

    /// The next page of the thread showing.
    pub(super) fn discussion_more(&mut self) {
        let Some(d) = &mut self.discussion else {
            return;
        };
        let site = d.site;
        let Some(Loaded::Ready(Some(thread))) = d.threads.get(&site) else {
            return;
        };
        let Some(next) = thread.next else { return };
        if !d.more.insert(site) {
            return;
        }
        let seq = d.seq;
        let client = self.http.clone();
        // The thread's ID, from its URL (both sites end it with the ID).
        let id: Option<u64> = thread
            .url
            .rsplit(['/', '='])
            .find_map(|part| part.parse().ok());
        let Some(id) = id else { return };
        self.spawn(async move {
            let result = match site {
                Site::AniList => discuss::anilist_thread(&client, id, next).await,
                Site::Mal => discuss::mal_topic(&client, id, next).await,
            }
            .map(|t| Some(Box::new(t)))
            .map_err(|e| format!("{e:#}"));
            Msg::Discussion {
                seq,
                site,
                more: true,
                result,
            }
        });
    }

    pub(super) fn handle_discussion(
        &mut self,
        seq: u64,
        site: Site,
        more: bool,
        result: Result<Option<Box<Thread>>, String>,
    ) {
        let Some(d) = &mut self.discussion else {
            return;
        };
        if d.seq != seq {
            return;
        }
        if more {
            d.more.remove(&site);
            match (result, d.threads.get_mut(&site)) {
                (Ok(Some(page)), Some(Loaded::Ready(Some(thread)))) => {
                    thread.posts.extend(page.posts);
                    thread.next = page.next;
                }
                (Err(e), _) => {
                    let e = format!("Couldn't read more of it: {e}");
                    self.set_error(e);
                }
                _ => {}
            }
            return;
        }
        // MyAnimeList's topic page doesn't say how many replies; its
        // episode list does.
        let replies = self
            .episode_scores
            .episode(d.anilist_id, d.episode)
            .map(|e| e.replies);
        let loaded = match result {
            Ok(Some(mut thread)) => {
                if site == Site::Mal
                    && let Some(r) = replies
                {
                    thread.replies = r;
                }
                Loaded::Ready(Some(*thread))
            }
            Ok(None) => Loaded::Ready(None),
            Err(e) => Loaded::Failed(e),
        };
        d.threads.insert(site, loaded);
    }
}
