//! Discover (AniList's lists of anime) and the airing calendar: what's
//! airing each day of a week, for the anime on your lists (or everything).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate, TimeZone};

use super::{App, Msg, Origin, Page};
use crate::browse::{self, Airing, Choice, Filter, Media, Shelf, Sort, Tag};
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
    /// How many pages there are, when that's been asked (a random order's).
    pub(crate) last: Option<u32>,
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
            last: None,
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

#[derive(Default)]
pub(super) struct DiscoverState {
    pub(super) filter: Filter,
    /// Each shelf as fetched (the pages so far), and when its first was.
    pub(super) shelves: HashMap<Filter, (Loaded<Paged<Media>>, Instant)>,
    /// AniList's tags, once asked for.
    pub(super) tags: Option<(Loaded<Vec<Tag>>, Instant)>,
    /// What's typed in the tag search.
    pub(super) tag_query: String,
}

/// The statuses Discover filters by: your lists', and on neither.
pub(crate) const LIST_CHOICES: [Option<Status>; 7] = [
    Some(Status::Watching),
    Some(Status::Planning),
    Some(Status::Completed),
    Some(Status::Rewatching),
    Some(Status::Paused),
    Some(Status::Dropped),
    None,
];

/// What a list status is called in Discover's filter.
pub(crate) fn list_choice_name(s: Option<Status>) -> &'static str {
    s.map_or("Not on your lists", Status::name)
}

impl DiscoverState {
    /// How `status` is chosen (None: neither way).
    pub(crate) fn list_choice(&self, status: Option<Status>) -> Option<Choice> {
        self.filter
            .lists
            .iter()
            .find(|(s, _)| *s == status)
            .map(|(_, c)| *c)
    }

    /// `status` chosen the next way round (off, wanted, not wanted).
    pub(crate) fn cycle_list(&mut self, status: Option<Status>) {
        let next = Choice::cycle(self.list_choice(status));
        let lists = &mut self.filter.lists;
        lists.retain(|(s, _)| *s != status);
        if let Some(c) = next {
            lists.push((status, c));
        }
        // One order, so the same choices are the same filter.
        let at = |s: &Option<Status>| LIST_CHOICES.iter().position(|c| c == s);
        lists.sort_by_key(|(s, _)| at(s));
    }

    /// A genre or tag chosen the next way round (off, wanted, not wanted).
    pub(crate) fn cycle_tag(&mut self, name: &str) {
        let next = Choice::cycle(self.filter.tags.get(name).copied());
        match next {
            Some(c) => self.filter.tags.insert(name.to_owned(), c),
            None => self.filter.tags.remove(name),
        };
    }

    /// The sort: a random one is shuffled anew each time it's picked.
    pub(crate) fn set_sort(&mut self, sort: Sort) {
        self.filter.sort = sort;
        if sort == Sort::Random {
            self.reshuffle();
        }
    }

    /// Another random order.
    pub(crate) fn reshuffle(&mut self) {
        self.filter.shuffle = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
            | 1;
    }

    /// A ready-made view (its genres, tags and format kept).
    pub(crate) fn show(&mut self, shelf: Shelf) {
        self.filter = shelf.apply(&self.filter, Local::now().date_naive());
    }

    /// Whether any genre, tag, list status or format is chosen.
    pub(crate) fn narrowed(&self) -> bool {
        !self.filter.tags.is_empty()
            || !self.filter.lists.is_empty()
            || self.filter.format.is_some()
    }

    /// Genres, tags, list statuses and format all unchosen.
    pub(crate) fn clear_choices(&mut self) {
        self.filter.tags.clear();
        self.filter.format = None;
        self.filter.lists.clear();
    }

    /// AniList's tags matching what's typed (none if nothing is), not
    /// those already chosen; adult ones only with `adult`.
    pub(crate) fn tag_matches(&self, adult: bool) -> Vec<&Tag> {
        let q = self.tag_query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let Some((Loaded::Ready(tags), _)) = &self.tags else {
            return Vec::new();
        };
        let mut found: Vec<&Tag> = tags
            .iter()
            .filter(|t| adult || !t.adult)
            .filter(|t| !self.filter.tags.contains_key(&t.name))
            .filter(|t| t.name.to_lowercase().contains(&q))
            .collect();
        // Those starting with it first.
        found.sort_by_key(|t| !t.name.to_lowercase().starts_with(&q));
        found.truncate(30);
        found
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
                self.request_tags();
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
                let adult = self.adult_on();
                let ids = self.list_ids(&filter);
                self.spawn(async move {
                    let result = browse::browse(&client, &filter, &ids, adult, 1, None)
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
        let last = shelf.last;
        let client = self.http.clone();
        let adult = self.adult_on();
        let ids = self.list_ids(&filter);
        self.spawn(async move {
            let result = browse::browse(&client, &filter, &ids, adult, page, last)
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
        result: Result<browse::Found, String>,
    ) {
        if page > 1 {
            let Some((Loaded::Ready(shelf), _)) = self.discover.shelves.get_mut(&filter) else {
                return;
            };
            match result {
                Ok(found) => shelf.add(page, found.items, found.more, |m| m.id),
                Err(e) if shelf.next == Some(page) => shelf.more = More::Failed(e),
                Err(_) => {}
            }
            return;
        }
        let loaded = match result {
            Ok(found) => {
                let mut pages = Paged::first(found.items, found.more);
                pages.last = found.last_page;
                Loaded::Ready(pages)
            }
            Err(e) => Loaded::Failed(e),
        };
        self.discover
            .shelves
            .insert(filter, (loaded, Instant::now()));
    }

    /// AniList's tags for Discover's filter, once (again after a failure).
    fn request_tags(&mut self) {
        let stale = match &self.discover.tags {
            None => true,
            Some((Loaded::Failed(_), at)) => at.elapsed() > Duration::from_secs(60),
            Some(_) => false,
        };
        if !stale {
            return;
        }
        self.discover.tags = Some((Loaded::Loading, Instant::now()));
        let client = self.http.clone();
        self.spawn(async move {
            Msg::DiscoverTags(browse::tags(&client).await.map_err(|e| format!("{e:#}")))
        });
    }

    pub(super) fn handle_discover_tags(&mut self, result: Result<Vec<Tag>, String>) {
        let loaded = match result {
            Ok(tags) => Loaded::Ready(tags),
            Err(e) => Loaded::Failed(e),
        };
        self.discover.tags = Some((loaded, Instant::now()));
    }

    /// The anime your lists' statuses chosen in `filter` leave, as IDs for
    /// AniList: those of the statuses wanted, or (on neither list wanted)
    /// all but those of the others. By your AniList list, else your
    /// MyAnimeList one; with neither, every anime is on neither.
    pub(crate) fn list_ids(&self, filter: &Filter) -> browse::ListIds {
        let t = &self.tracking;
        let entries = match (t.lists.get(&Service::AniList), t.lists.get(&Service::Mal)) {
            (Some(l), _) => Some((l, false)),
            (None, Some(l)) => Some((l, true)),
            (None, None) => None,
        };
        list_ids(&filter.lists, entries)
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

    /// An anime the anime page shown recommends: its page, which goes back
    /// to this one (and this one where it would have).
    pub(super) fn open_recommended(&mut self, media: Media) {
        let from = match &self.page {
            Page::Anime(ap) => Some(Box::new(super::BackTo {
                anime: ap.anime.clone(),
                origin: ap.origin,
                back: ap.back.clone(),
            })),
            _ => None,
        };
        let origin = from.as_ref().map_or(Origin::Library, |b| b.origin);
        self.open_media(media, origin);
        if let Page::Anime(ap) = &mut self.page {
            ap.back = from;
        }
    }

    /// An anime page gone back to.
    pub(super) fn reopen(&mut self, to: super::BackTo) {
        let id = to.anime.anilist_id;
        if self.library.find(id).is_some() {
            self.open_series(id);
        } else {
            self.look_up_and_show(to.anime);
        }
        if let Page::Anime(ap) = &mut self.page {
            ap.origin = to.origin;
            ap.back = to.back;
        }
    }
}

/// The anime `lists`' choices leave of a list's `entries` (by AniList ID,
/// or `true`: MyAnimeList's): see [`App::list_ids`].
fn list_ids(
    lists: &[(Option<Status>, Choice)],
    entries: Option<(&HashMap<u64, crate::tracking::Entry>, bool)>,
) -> browse::ListIds {
    if lists.is_empty() {
        return browse::ListIds::Any;
    }
    let wanted: Vec<Option<Status>> = lists
        .iter()
        .filter(|(_, c)| *c == Choice::Include)
        .map(|(s, _)| *s)
        .collect();
    let allowed = |s: Option<Status>| {
        (wanted.is_empty() || wanted.contains(&s))
            && !lists.iter().any(|(x, c)| *c == Choice::Exclude && *x == s)
    };
    let Some((entries, mal)) = entries else {
        return if allowed(None) {
            browse::ListIds::Any
        } else {
            browse::ListIds::Nothing
        };
    };
    // On neither list allowed: all but the lists' others. Else only theirs.
    let not = allowed(None);
    let mut ids: Vec<u64> = entries
        .iter()
        .filter(|(_, e)| allowed(Some(e.status)) != not)
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    match (ids.is_empty(), not) {
        (true, true) => browse::ListIds::Any,
        (true, false) => browse::ListIds::Nothing,
        (false, _) => browse::ListIds::Ids { ids, not, mal },
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
    fn list_choices_become_anilist_ids() {
        use crate::tracking::Entry;
        use browse::ListIds;
        let entry = |status| Entry {
            status,
            score: 0.0,
            progress: 0,
        };
        let list: HashMap<u64, Entry> = HashMap::from([
            (1, entry(Status::Watching)),
            (2, entry(Status::Completed)),
            (3, entry(Status::Dropped)),
            (4, entry(Status::Watching)),
        ]);
        let ids = |lists: &[(Option<Status>, Choice)], mal| list_ids(lists, Some((&list, mal)));
        use Choice::*;
        assert_eq!(ids(&[], false), ListIds::Any);
        assert_eq!(
            ids(&[(Some(Status::Watching), Include)], false),
            ListIds::Ids {
                ids: vec![1, 4],
                not: false,
                mal: false
            }
        );
        // Not dropped: everything but the dropped ones.
        assert_eq!(
            ids(&[(Some(Status::Dropped), Exclude)], true),
            ListIds::Ids {
                ids: vec![3],
                not: true,
                mal: true
            }
        );
        // Not on your lists: everything but all of them.
        assert_eq!(
            ids(&[(None, Include)], false),
            ListIds::Ids {
                ids: vec![1, 2, 3, 4],
                not: true,
                mal: false
            }
        );
        // On neither, or completed: all but the others.
        assert_eq!(
            ids(
                &[(Some(Status::Completed), Include), (None, Include)],
                false
            ),
            ListIds::Ids {
                ids: vec![1, 3, 4],
                not: true,
                mal: false
            }
        );
        assert_eq!(
            ids(&[(Some(Status::Paused), Include)], false),
            ListIds::Nothing
        );
        // Without lists, every anime is on neither.
        assert_eq!(
            list_ids(&[(Some(Status::Watching), Include)], None),
            ListIds::Nothing
        );
        assert_eq!(list_ids(&[(None, Include)], None), ListIds::Any);
        assert_eq!(
            list_ids(&[(Some(Status::Dropped), Exclude)], None),
            ListIds::Any
        );
    }

    #[test]
    fn three_way_choices_cycle_and_keep_one_order() {
        let mut d = DiscoverState::default();
        d.cycle_list(None);
        d.cycle_list(Some(Status::Watching));
        assert_eq!(
            d.filter.lists,
            [
                (Some(Status::Watching), Choice::Include),
                (None, Choice::Include)
            ],
            "LIST_CHOICES' order, whatever the taps' order"
        );
        d.cycle_list(Some(Status::Watching));
        assert_eq!(d.list_choice(Some(Status::Watching)), Some(Choice::Exclude));
        d.cycle_list(Some(Status::Watching));
        assert_eq!(d.list_choice(Some(Status::Watching)), None);
        d.cycle_tag("Drama");
        d.cycle_tag("Drama");
        assert_eq!(d.filter.tags.get("Drama"), Some(&Choice::Exclude));
        assert!(d.narrowed());
        d.set_sort(Sort::Random);
        assert_ne!(d.filter.shuffle, 0, "a random order is shuffled");
        d.clear_choices();
        assert!(!d.narrowed());
        d.show(Shelf::TopRated);
        assert_eq!(d.filter.sort, Sort::Score);
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
