//! Discover (AniList's lists of anime) and the airing calendar: what's
//! airing each day of a week, for the anime on your lists (or everything).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate, TimeZone};

use super::{App, Msg, Origin, Page};
use crate::browse::{self, Airing, Filter, Media, Shelf};
use crate::tracking::{Service, Status};

/// Something fetched in the background.
pub(super) enum Loaded<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// How long fetched lists are kept before being fetched again.
const KEEP: Duration = Duration::from_secs(3600);

/// A list fetched a page at a time, for endless scrolling: what's here so
/// far, and the page after it.
pub(crate) struct Paged<T> {
    pub(crate) items: Vec<T>,
    /// The next page to ask for (pages count from 1), if there's more.
    pub(crate) next: Option<u32>,
    pub(crate) more: More,
}

/// Where the next page is.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum More {
    #[default]
    Idle,
    Loading,
    Failed(String),
}

impl<T> Paged<T> {
    /// The first page; `more` if there are others after it.
    pub(crate) fn first(items: Vec<T>, more: bool) -> Self {
        Paged {
            items,
            next: more.then_some(2),
            more: More::Idle,
        }
    }

    /// Page `page`'s items, after those here (ones already here are left
    /// out: a list can shift while it's read). A page that isn't the next
    /// one (an old answer) changes nothing.
    pub(crate) fn add(&mut self, page: u32, items: Vec<T>, more: bool, id: impl Fn(&T) -> u64) {
        if self.next != Some(page) {
            return;
        }
        let have: HashSet<u64> = self.items.iter().map(&id).collect();
        self.items
            .extend(items.into_iter().filter(|i| !have.contains(&id(i))));
        self.next = more.then_some(page + 1);
        self.more = More::Idle;
    }

    /// The page to ask for now, if there's more and it isn't being fetched.
    pub(crate) fn wanted(&self) -> Option<u32> {
        if self.more == More::Loading {
            None
        } else {
            self.next
        }
    }
}

pub(super) struct DiscoverState {
    pub(super) filter: Filter,
    /// Each shelf as fetched (the pages so far), and when its first was.
    pub(super) shelves: HashMap<Filter, (Loaded<Paged<Media>>, Instant)>,
}

impl Default for DiscoverState {
    fn default() -> Self {
        Self {
            filter: Filter {
                shelf: Shelf::Trending,
                genre: None,
                format: None,
            },
            shelves: HashMap::new(),
        }
    }
}

pub(super) struct CalendarState {
    /// The Monday of the week shown.
    pub(super) week: NaiveDate,
    /// Everything airing, not just your lists'.
    pub(super) full: bool,
    pub(super) weeks: HashMap<NaiveDate, (Loaded<Vec<Airing>>, Instant)>,
}

impl Default for CalendarState {
    fn default() -> Self {
        Self {
            week: monday_of(Local::now().date_naive()),
            full: false,
            weeks: HashMap::new(),
        }
    }
}

pub(super) fn monday_of(day: NaiveDate) -> NaiveDate {
    day - chrono::Days::new(u64::from(day.weekday().num_days_from_monday()))
}

/// Midnight starting `day`, here (Unix time).
pub(super) fn midnight(day: NaiveDate) -> i64 {
    let at = day.and_hms_opt(0, 0, 0).expect("midnight");
    Local
        .from_local_datetime(&at)
        .earliest()
        .map_or_else(|| at.and_utc().timestamp(), |t| t.timestamp())
}

impl App {
    /// Fetch what Discover or the calendar shows, if it isn't here (or is
    /// old).
    pub(super) fn request_browse(&mut self) {
        match self.page {
            Page::Discover => {
                let filter = self.discover.filter.clone();
                let stale = |(loaded, at): &(Loaded<Paged<Media>>, Instant)| match loaded {
                    Loaded::Loading => false,
                    Loaded::Ready(_) => at.elapsed() > KEEP,
                    Loaded::Failed(_) => at.elapsed() > Duration::from_secs(60),
                };
                if self
                    .discover
                    .shelves
                    .get(&filter)
                    .is_some_and(|s| !stale(s))
                {
                    return;
                }
                self.discover
                    .shelves
                    .insert(filter.clone(), (Loaded::Loading, Instant::now()));
                let client = self.http.clone();
                let today = Local::now().date_naive();
                let adult = self.adult_on();
                self.spawn(async move {
                    let result = browse::browse(&client, &filter, today, adult, 1)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    Msg::Discovered {
                        filter,
                        page: 1,
                        result,
                    }
                });
            }
            Page::Calendar => {
                let week = self.calendar.week;
                let stale = |(loaded, at): &(Loaded<Vec<Airing>>, Instant)| match loaded {
                    Loaded::Loading => false,
                    Loaded::Ready(_) => at.elapsed() > KEEP,
                    Loaded::Failed(_) => at.elapsed() > Duration::from_secs(60),
                };
                if self.calendar.weeks.get(&week).is_some_and(|w| !stale(w)) {
                    return;
                }
                self.calendar
                    .weeks
                    .insert(week, (Loaded::Loading, Instant::now()));
                let client = self.http.clone();
                let (from, to) = (midnight(week), midnight(week + chrono::Days::new(7)));
                let adult = self.adult_on();
                self.spawn(async move {
                    let result = browse::schedule(&client, from, to, adult)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    Msg::Schedule { week, result }
                });
            }
            _ => {}
        }
    }

    /// The shown shelf's next page, as you scroll to its end (if there's
    /// one, and it isn't being fetched already).
    pub(super) fn discover_more(&mut self) {
        let filter = self.discover.filter.clone();
        let Some((Loaded::Ready(shelf), _)) = self.discover.shelves.get_mut(&filter) else {
            return;
        };
        let Some(page) = shelf.wanted() else {
            return;
        };
        shelf.more = More::Loading;
        let client = self.http.clone();
        let today = Local::now().date_naive();
        let adult = self.adult_on();
        self.spawn(async move {
            let result = browse::browse(&client, &filter, today, adult, page)
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::Discovered {
                filter,
                page,
                result,
            }
        });
    }

    pub(super) fn handle_discovered(
        &mut self,
        filter: Filter,
        page: u32,
        result: Result<(Vec<Media>, bool), String>,
    ) {
        if page > 1 {
            let Some((Loaded::Ready(shelf), _)) = self.discover.shelves.get_mut(&filter) else {
                return;
            };
            match result {
                Ok((list, more)) => shelf.add(page, list, more, |m| m.id),
                Err(e) if shelf.next == Some(page) => shelf.more = More::Failed(e),
                Err(_) => {}
            }
            return;
        }
        let loaded = match result {
            Ok((list, more)) => Loaded::Ready(Paged::first(list, more)),
            Err(e) => Loaded::Failed(e),
        };
        self.discover
            .shelves
            .insert(filter, (loaded, Instant::now()));
    }

    pub(super) fn handle_schedule(&mut self, week: NaiveDate, result: Result<Vec<Airing>, String>) {
        let loaded = match result {
            Ok(list) => Loaded::Ready(list),
            Err(e) => Loaded::Failed(e),
        };
        self.calendar.weeks.insert(week, (loaded, Instant::now()));
    }

    /// Whether an anime is on your lists, and not dropped on any of them.
    /// Without lists, whether it's in the library.
    pub(super) fn on_your_lists(&self, m: &Media) -> bool {
        let t = &self.tracking;
        if !t.any() {
            return self.library.find(m.id).is_some();
        }
        let anilist = t.lists.get(&Service::AniList).and_then(|l| l.get(&m.id));
        let mal = m
            .id_mal
            .or_else(|| t.key(Service::Mal, m.id))
            .and_then(|k| t.lists.get(&Service::Mal)?.get(&k));
        still_yours([anilist, mal].into_iter().flatten().map(|e| e.status))
    }

    /// An anime from Discover or the calendar: its page (the library's, if
    /// it's there), its SeaDex entry looked up.
    pub(super) fn open_media(&mut self, media: Media, origin: Origin) {
        if self.library.find(media.id).is_some() {
            self.open_series(media.id);
        } else {
            self.look_up_and_show(media.to_anime());
        }
        if let Page::Anime(ap) = &mut self.page {
            ap.origin = origin;
        }
    }
}

/// On a list, and dropped on none: a list you've dropped it on is the one
/// you keep up to date (the other may not be).
fn still_yours(statuses: impl Iterator<Item = Status>) -> bool {
    let mut any = false;
    for s in statuses {
        if s == Status::Dropped {
            return false;
        }
        any = true;
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_on_either_list_is_not_yours() {
        use Status::*;
        assert!(still_yours([Watching].into_iter()));
        assert!(still_yours([Watching, Planning].into_iter()));
        assert!(!still_yours([Watching, Dropped].into_iter()));
        assert!(!still_yours([Dropped, Watching].into_iter()));
        assert!(!still_yours([].into_iter()), "on no list");
    }

    #[test]
    fn pages_add_up_once_each() {
        let mut p = Paged::first(vec![1u64, 2, 3], true);
        assert_eq!(p.wanted(), Some(2));
        p.more = More::Loading;
        assert_eq!(p.wanted(), None, "not asked for twice");
        p.add(2, vec![3, 4, 5], true, |i| *i);
        assert_eq!(p.items, [1, 2, 3, 4, 5], "3 moved onto page 2: kept once");
        assert_eq!((p.next, &p.more), (Some(3), &More::Idle));
        p.add(2, vec![9], false, |i| *i);
        assert_eq!(p.items.len(), 5, "an old answer changes nothing");
        p.add(3, vec![6], false, |i| *i);
        assert_eq!((p.items.len(), p.wanted()), (6, None), "the last page");
        assert_eq!(Paged::first(vec![1u64], false).wanted(), None);
    }

    #[test]
    fn weeks_start_on_monday() {
        let day = |d| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(monday_of(day(22)), day(16), "a Sunday");
        assert_eq!(monday_of(day(16)), day(16));
        assert_eq!(midnight(day(17)) - midnight(day(16)), 86_400);
    }
}
