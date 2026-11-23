//! An episode's discussion: its thread on AniList's forum and its topic on
//! MyAnimeList's, read into plain text (spoilers kept apart, to show when
//! asked); and MyAnimeList's score for each episode (from its episode
//! polls).

use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};

const MAL_URL: &str = "https://myanimelist.net";

/// Where MyAnimeList's pages come from instead (tests).
#[cfg(test)]
pub static MAL_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn mal_url() -> String {
    #[cfg(test)]
    if let Some(base) = MAL_BASE.lock().unwrap().as_ref() {
        return base.clone();
    }
    MAL_URL.to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Site {
    AniList,
    Mal,
}

impl Site {
    pub fn name(self) -> &'static str {
        match self {
            Site::AniList => "AniList",
            Site::Mal => "MyAnimeList",
        }
    }
}

/// A run of a post's text.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text(String),
    /// Someone else's words (and who, when known).
    Quote(Option<String>, String),
    /// Hidden until asked for.
    Spoiler(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Post {
    pub id: u64,
    pub author: String,
    /// Unix time.
    pub at: i64,
    pub likes: Option<u32>,
    pub body: Vec<Piece>,
    /// Replies to it (AniList's comments nest).
    pub replies: Vec<Post>,
}

/// A page of a discussion.
#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub site: Site,
    pub title: String,
    /// On the site, to reply there.
    pub url: String,
    pub replies: u32,
    pub posts: Vec<Post>,
    /// The page (AniList) or offset (MyAnimeList) of what comes next.
    pub next: Option<u32>,
}

// ── AniList ───────────────────────────────────────────────────────────

/// An episode's thread on AniList's forum: one in "Release Discussion"
/// (else any) titled with the episode, e.g. "[Spoilers] Sousou No Frieren
/// - Episode 28 Discussion".
pub async fn anilist_thread_for(
    client: &reqwest::Client,
    media: u64,
    episode: u32,
) -> Result<Option<u64>> {
    const RELEASE_DISCUSSION: u64 = 5;
    for category in [Some(RELEASE_DISCUSSION), None] {
        for page in 1..=4 {
            let data = anilist(
                client,
                "query ($media: Int, $category: Int, $page: Int) { Page(page: $page, perPage: 50) \
                 { pageInfo { hasNextPage } threads(mediaCategoryId: $media, categoryId: $category, \
                 sort: ID_DESC) { id title } } }",
                json!({ "media": media, "category": category, "page": page }),
            )
            .await?;
            let p = &data["Page"];
            let found = p["threads"].as_array().into_iter().flatten().find(|t| {
                t["title"]
                    .as_str()
                    .is_some_and(|title| is_episode_thread(title, episode))
            });
            if let Some(t) = found {
                return Ok(t["id"].as_u64());
            }
            if p["pageInfo"]["hasNextPage"] != true {
                break;
            }
        }
    }
    Ok(None)
}

/// A title about episode `n` alone ("Episode 5 Discussion", "Ep. 05").
pub fn is_episode_thread(title: &str, n: u32) -> bool {
    static EPISODE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:episode|ep\.?)\s*#?0*(\d+)(\s*[-–~&,+]\s*\d+)?\b").unwrap()
    });
    EPISODE
        .captures_iter(title)
        .any(|c| c.get(2).is_none() && c[1].parse::<u32>().ok() == Some(n))
}

/// A page (from 1) of an AniList thread: its post first, then 25 comments
/// at a time, with their replies.
pub async fn anilist_thread(client: &reqwest::Client, id: u64, page: u32) -> Result<Thread> {
    let data = anilist(
        client,
        "query ($id: Int, $page: Int) { Thread(id: $id) { id title body replyCount siteUrl \
         createdAt likeCount user { name } } Page(page: $page, perPage: 25) { pageInfo \
         { hasNextPage } threadComments(threadId: $id) { id comment likeCount createdAt \
         user { name } childComments } } }",
        json!({ "id": id, "page": page }),
    )
    .await?;
    let t = &data["Thread"];
    if !t.is_object() {
        bail!("AniList has no thread {id}");
    }
    let mut posts = Vec::new();
    if page == 1 {
        posts.push(Post {
            id,
            author: t["user"]["name"].as_str().unwrap_or("?").to_owned(),
            at: t["createdAt"].as_i64().unwrap_or(0),
            likes: t["likeCount"].as_u64().map(|n| n as u32),
            body: anilist_text(t["body"].as_str().unwrap_or("")),
            replies: Vec::new(),
        });
    }
    posts.extend(
        data["Page"]["threadComments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(anilist_comment),
    );
    Ok(Thread {
        site: Site::AniList,
        title: t["title"].as_str().unwrap_or("").trim().to_owned(),
        url: t["siteUrl"].as_str().map_or_else(
            || format!("https://anilist.co/forum/thread/{id}"),
            str::to_owned,
        ),
        replies: t["replyCount"].as_u64().unwrap_or(0) as u32,
        posts,
        next: (data["Page"]["pageInfo"]["hasNextPage"] == true).then_some(page + 1),
    })
}

fn anilist_comment(c: &Value) -> Post {
    Post {
        id: c["id"].as_u64().unwrap_or(0),
        author: c["user"]["name"].as_str().unwrap_or("?").to_owned(),
        at: c["createdAt"].as_i64().unwrap_or(0),
        likes: c["likeCount"].as_u64().map(|n| n as u32),
        body: anilist_text(c["comment"].as_str().unwrap_or("")),
        replies: c["childComments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(anilist_comment)
            .collect(),
    }
}

async fn anilist(client: &reqwest::Client, q: &str, variables: Value) -> Result<Value> {
    let resp = client
        .post(crate::seadex::anilist_url())
        .json(&json!({ "query": q, "variables": variables }))
        .send()
        .await
        .context("could not reach AniList")?;
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("AniList is busy; try again in a minute");
    }
    let mut body: Value = resp
        .json()
        .await
        .with_context(|| format!("unexpected response from AniList (HTTP {status})"))?;
    if let Some(e) = body["errors"].get(0) {
        bail!(
            "AniList error: {}",
            e["message"].as_str().unwrap_or("unknown")
        );
    }
    Ok(body["data"].take())
}

/// AniList's markup (markdown, a little HTML, `~!spoilers!~`, `img(…)`) as
/// text.
pub fn anilist_text(markup: &str) -> Vec<Piece> {
    static MEDIA: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)!\[[^\]]*\]\([^)]*\)|img\d*%?\([^)]*\)|<img[^>]*>").unwrap()
    });
    static VIDEO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(?:youtube|webm)\([^)]*\)").unwrap());
    static LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)]*)\)").unwrap());
    static BR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>\r?\n?").unwrap());
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
    static MARKS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"~~~|\*\*|__|(?m)^#{1,6}\s*|~~").unwrap());
    static SPOILER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)~!(.*?)!~").unwrap());
    let text = MEDIA.replace_all(markup, "[image]");
    let text = VIDEO.replace_all(&text, "[video]");
    let text = LINK.replace_all(&text, |c: &regex::Captures| {
        if c[1].trim().is_empty() {
            c[2].to_owned()
        } else {
            c[1].to_owned()
        }
    });
    let text = BR.replace_all(&text, "\n");
    let text = TAG.replace_all(&text, "");
    let mut pieces = Vec::new();
    let mut rest: &str = &text;
    while let Some(m) = SPOILER.captures(rest) {
        let whole = m.get(0).unwrap();
        plain_and_quotes(&rest[..whole.start()], &mut pieces);
        let hidden = tidy(&MARKS.replace_all(&unescape(&m[1]), ""));
        if !hidden.is_empty() {
            pieces.push(Piece::Spoiler(hidden));
        }
        rest = &rest[whole.end()..];
    }
    plain_and_quotes(rest, &mut pieces);
    for p in &mut pieces {
        if let Piece::Text(t) | Piece::Quote(_, t) = p {
            *t = tidy(&MARKS.replace_all(t, ""));
        }
    }
    pieces.retain(|p| !matches!(p, Piece::Text(t) | Piece::Quote(_, t) if t.is_empty()));
    pieces
}

/// Text, with markdown's "> " lines as quotes.
fn plain_and_quotes(text: &str, out: &mut Vec<Piece>) {
    let text = unescape(text);
    let mut plain = String::new();
    let mut quote = String::new();
    let flush = |plain: &mut String, quote: &mut String, out: &mut Vec<Piece>| {
        if !plain.trim().is_empty() {
            out.push(Piece::Text(std::mem::take(plain)));
        }
        plain.clear();
        if !quote.trim().is_empty() {
            out.push(Piece::Quote(None, std::mem::take(quote)));
        }
        quote.clear();
    };
    for line in text.split('\n') {
        match line.trim_start().strip_prefix('>') {
            Some(q) => {
                if !plain.trim().is_empty() {
                    flush(&mut plain, &mut String::new(), out);
                }
                quote.push_str(q.trim_start());
                quote.push('\n');
            }
            None => {
                if !quote.trim().is_empty() {
                    flush(&mut String::new(), &mut quote, out);
                }
                plain.push_str(line);
                plain.push('\n');
            }
        }
    }
    flush(&mut plain, &mut quote, out);
}

// ── MyAnimeList ───────────────────────────────────────────────────────

/// An episode, as MyAnimeList's episode list has it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MalEpisode {
    pub number: u32,
    pub title: Option<String>,
    /// The episode poll's average, out of 5.
    pub score: Option<f32>,
    /// Its discussion topic, and how many replies it has.
    pub topic: Option<u64>,
    pub replies: u32,
    pub filler: bool,
    pub recap: bool,
}

/// Every episode on MyAnimeList's episode list (100 to a page).
pub async fn mal_episodes(client: &reqwest::Client, mal_id: u64) -> Result<Vec<MalEpisode>> {
    let mut out = Vec::new();
    for page in 0..20 {
        let url = format!(
            "{}/anime/{mal_id}/_/episode?offset={}",
            mal_url(),
            page * 100
        );
        let html = page_text(client, &url).await?;
        let found = parse_episode_list(&html);
        let full = found.len() >= 100;
        out.extend(found);
        if !full {
            break;
        }
    }
    Ok(out)
}

pub fn parse_episode_list(html: &str) -> Vec<MalEpisode> {
    static NUMBER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"episode-number[^>]*data-raw="(\d+)""#).unwrap());
    static TITLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)class="episode-title[^"]*">\s*<a[^>]*>([^<]*)</a>"#).unwrap()
    });
    static SCORE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"episode-poll[^>]*data-raw="([\d.]+)""#).unwrap());
    static FORUM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)episode-forum[^>]*data-raw="(\d+)".*?topicid=(\d+)"#).unwrap()
    });
    html.split(r#"<tr class="episode-list-data">"#)
        .skip(1)
        .filter_map(|row| {
            let row = row.split("</tr>").next().unwrap_or(row);
            let number = NUMBER.captures(row)?[1].parse().ok()?;
            let forum = FORUM.captures(row);
            Some(MalEpisode {
                number,
                title: TITLE
                    .captures(row)
                    .map(|c| unescape(c[1].trim()))
                    .filter(|t| !t.is_empty()),
                score: SCORE
                    .captures(row)
                    .and_then(|c| c[1].parse::<f32>().ok())
                    .filter(|s| *s > 0.0),
                topic: forum.as_ref().and_then(|c| c[2].parse().ok()),
                replies: forum.as_ref().and_then(|c| c[1].parse().ok()).unwrap_or(0),
                filler: row.contains(">Filler<"),
                recap: row.contains(">Recap<"),
            })
        })
        .collect()
}

/// A page of a MyAnimeList topic: 50 posts from `offset`.
pub async fn mal_topic(client: &reqwest::Client, topic: u64, offset: u32) -> Result<Thread> {
    let url = format!("{}/forum/?topicid={topic}&show={offset}", mal_url());
    let html = page_text(client, &url).await?;
    let mut thread = parse_topic(&html, offset);
    thread.url = format!("{MAL_URL}/forum/?topicid={topic}");
    if thread.posts.is_empty() && thread.title.is_empty() {
        bail!("MyAnimeList has no topic {topic}");
    }
    Ok(thread)
}

pub fn parse_topic(html: &str, offset: u32) -> Thread {
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<title>\s*(.*?)\s*-\s*Forums - MyAnimeList").unwrap());
    static POST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?s)<div class="forum-topic-message message" id="msg(\d+)"[^>]*data-user="([^"]*)">.*?data-time="(\d+)".*?class="body clearfix"><tr><td>(.*?)</td></tr></table>"#,
        )
        .unwrap()
    });
    static SHOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"show=(\d+)").unwrap());
    let posts: Vec<Post> = POST
        .captures_iter(html)
        .map(|c| Post {
            id: c[1].parse().unwrap_or(0),
            author: unescape(&c[2]),
            at: c[3].parse().unwrap_or(0),
            likes: None,
            body: mal_text(&c[4]),
            replies: Vec::new(),
        })
        .collect();
    let last = SHOW
        .captures_iter(html)
        .filter_map(|c| c[1].parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    let next = (last > offset && !posts.is_empty()).then_some(offset + 50);
    Thread {
        site: Site::Mal,
        title: TITLE
            .captures(html)
            .map(|c| unescape(&c[1]))
            .unwrap_or_default(),
        url: String::new(),
        replies: 0,
        posts,
        next,
    }
}

/// A MyAnimeList post's HTML as text: quotes and spoilers kept apart.
pub fn mal_text(html: &str) -> Vec<Piece> {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>|[^<]+").unwrap());
    static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^</?\s*([a-zA-Z0-9]+)").unwrap());
    static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"data-user="([^"]*)""#).unwrap());
    #[derive(Clone, PartialEq)]
    enum Mode {
        Plain,
        Quote(Option<String>),
        Spoiler,
    }
    // Open elements, and what each makes their content.
    let mut stack: Vec<(String, Mode)> = Vec::new();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut current: Option<(Mode, String)> = None;
    let mut spoiler_next = false;
    let mode_of = |stack: &[(String, Mode)]| {
        if stack.iter().any(|(_, m)| *m == Mode::Spoiler) {
            return Mode::Spoiler;
        }
        stack
            .iter()
            .rev()
            .find_map(|(_, m)| matches!(m, Mode::Quote(_)).then(|| m.clone()))
            .unwrap_or(Mode::Plain)
    };
    let push =
        |pieces: &mut Vec<Piece>, current: &mut Option<(Mode, String)>, mode: Mode, s: &str| {
            match current {
                Some((m, text)) if *m == mode => text.push_str(s),
                _ => {
                    if let Some((m, text)) = current.take() {
                        pieces.push(match m {
                            Mode::Plain => Piece::Text(text),
                            Mode::Quote(who) => Piece::Quote(who, text),
                            Mode::Spoiler => Piece::Spoiler(text),
                        });
                    }
                    *current = Some((mode, s.to_owned()));
                }
            }
        };
    for token in TOKEN.find_iter(html).map(|m| m.as_str()) {
        if !token.starts_with('<') {
            let mode = mode_of(&stack);
            push(&mut pieces, &mut current, mode, &unescape(token));
            continue;
        }
        let lower = token.to_ascii_lowercase();
        let Some(name) = NAME.captures(&lower).map(|c| c[1].to_owned()) else {
            continue;
        };
        let closing = lower.starts_with("</");
        match name.as_str() {
            "br" => push(&mut pieces, &mut current, mode_of(&stack), "\n"),
            "img" => push(&mut pieces, &mut current, mode_of(&stack), "[image]"),
            "input" => spoiler_next = lower.contains("show spoiler"),
            "div" | "span" if !closing => {
                let mode = if lower.contains("quotetext") {
                    Mode::Quote(USER.captures(token).map(|c| unescape(&c[1])))
                } else if lower.contains("spoiler_content") && std::mem::take(&mut spoiler_next) {
                    Mode::Spoiler
                } else {
                    Mode::Plain
                };
                if name == "div" {
                    push(&mut pieces, &mut current, mode_of(&stack), "\n");
                }
                stack.push((name, mode));
            }
            "div" | "span" => {
                if let Some(i) = stack.iter().rposition(|(n, _)| *n == name) {
                    stack.truncate(i);
                }
                if name == "div" {
                    push(&mut pieces, &mut current, mode_of(&stack), "\n");
                }
            }
            _ => {}
        }
    }
    if let Some((m, text)) = current {
        pieces.push(match m {
            Mode::Plain => Piece::Text(text),
            Mode::Quote(who) => Piece::Quote(who, text),
            Mode::Spoiler => Piece::Spoiler(text),
        });
    }
    // "Someone said:" opens each quote; the quote says who already.
    static SAID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\S+ said:\s*").unwrap());
    pieces
        .into_iter()
        .map(|p| match p {
            Piece::Text(t) => Piece::Text(tidy(&t)),
            Piece::Quote(who, t) => Piece::Quote(who, tidy(&SAID.replace(&t, ""))),
            Piece::Spoiler(t) => Piece::Spoiler(tidy(&t)),
        })
        .filter(|p| !matches!(p, Piece::Text(t) | Piece::Quote(_, t) | Piece::Spoiler(t) if t.is_empty()))
        .collect()
}

async fn page_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .send()
        .await
        .context("could not reach MyAnimeList")?;
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("MyAnimeList is busy; try again in a minute");
    }
    if !status.is_success() {
        bail!("MyAnimeList answered HTTP {status}");
    }
    resp.text()
        .await
        .context("couldn't read MyAnimeList's page")
}

// ── Text ──────────────────────────────────────────────────────────────

/// HTML entities as characters.
pub fn unescape(s: &str) -> String {
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(#x[0-9a-fA-F]+|#\d+|[a-zA-Z]+);").unwrap());
    ENTITY
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x") {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = e.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match e {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "hellip" => Some('…'),
                    "rsquo" => Some('’'),
                    "lsquo" => Some('‘'),
                    "rdquo" => Some('”'),
                    "ldquo" => Some('“'),
                    "middot" => Some('·'),
                    "times" => Some('×'),
                    "eacute" => Some('é'),
                    "deg" => Some('°'),
                    "mdash" => Some('—'),
                    "ndash" => Some('–'),
                    _ => None,
                }
            };
            ch.map_or_else(|| c[0].to_owned(), String::from)
        })
        .into_owned()
}

/// Lines trimmed, at most one blank line between paragraphs.
fn tidy(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in s.lines().map(str::trim) {
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The real sites: Frieren's episode 28 thread on AniList, its episode
    /// list and episode 1's topic on MyAnimeList; Discover's lists and this
    /// week's schedule on AniList.
    #[test]
    #[ignore = "network"]
    fn reads_the_real_sites() {
        let _net = crate::mock_http::real();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .user_agent("seadex-downloader-test")
            .build()
            .unwrap();
        rt.block_on(async {
            let thread = anilist_thread_for(&client, 154587, 28).await.unwrap();
            println!("AniList thread for episode 28: {thread:?}");
            let t = anilist_thread(&client, thread.expect("a thread"), 1)
                .await
                .unwrap();
            println!(
                "{:?}: {} replies, {} posts here, next {:?}",
                t.title,
                t.replies,
                t.posts.len(),
                t.next
            );
            assert!(t.posts.len() > 1);
            let eps = mal_episodes(&client, 52991).await.unwrap();
            println!("MyAnimeList: {} episodes; 1: {:?}", eps.len(), eps.first());
            assert_eq!(eps.len(), 28);
            let topic = mal_topic(&client, eps[0].topic.unwrap(), 0).await.unwrap();
            println!(
                "{:?}: {} posts, next {:?}",
                topic.title,
                topic.posts.len(),
                topic.next
            );
            assert_eq!(topic.posts.len(), 50);
            let today = chrono::Local::now().date_naive();
            for shelf in crate::browse::Shelf::ALL {
                let filter = crate::browse::Filter {
                    shelf,
                    genre: None,
                    format: None,
                };
                let list = crate::browse::browse(&client, &filter, today)
                    .await
                    .unwrap();
                println!(
                    "{}: {} anime, first {:?}",
                    shelf.name(),
                    list.len(),
                    list.first().map(|m| m.title())
                );
                assert!(!list.is_empty());
            }
            let filter = crate::browse::Filter {
                shelf: crate::browse::Shelf::Popular,
                genre: Some("Mecha".into()),
                format: Some("MOVIE".into()),
            };
            let list = crate::browse::browse(&client, &filter, today)
                .await
                .unwrap();
            println!(
                "Mecha movies: {} first {:?}",
                list.len(),
                list.first().map(|m| m.title())
            );
            let now = chrono::Utc::now().timestamp();
            let week = crate::browse::schedule(&client, now - 3 * 86_400, now + 4 * 86_400)
                .await
                .unwrap();
            println!(
                "this week: {} episodes airing; e.g. {:?}",
                week.len(),
                week.first().map(|a| (a.media.title(), a.episode))
            );
            assert!(week.len() > 20);
        });
    }

    #[test]
    fn finds_episode_threads_by_title() {
        assert!(is_episode_thread(
            "[Spoilers] Sousou No Frieren - Episode 28 Discussion",
            28
        ));
        assert!(is_episode_thread("Frieren Ep. 05 discussion", 5));
        assert!(is_episode_thread("Episode #3", 3));
        assert!(!is_episode_thread(
            "Sousou No Frieren - Episode 28 Discussion",
            2
        ));
        assert!(!is_episode_thread("Episodes 1-3 Discussion", 1), "a range");
        assert!(!is_episode_thread("Season 2 announced", 2));
    }

    #[test]
    fn reads_anilist_markup() {
        let body = "~~~__Great__ ending!~~~<br>\nimg220(https://x/y.png) see [this](https://a.b)\n\
                    > what they said\n> more\nmy reply ~!he lives!~ &amp; then";
        assert_eq!(
            anilist_text(body),
            [
                Piece::Text("Great ending!\n[image] see this".into()),
                Piece::Quote(None, "what they said\nmore".into()),
                Piece::Text("my reply".into()),
                Piece::Spoiler("he lives".into()),
                Piece::Text("& then".into()),
            ]
        );
    }

    #[test]
    fn reads_mal_posts() {
        let html = r#"Very cinematic episode.<br />
<br />
<div class="quotetext" data-id="1" data-user="Izana"><strong><a href="/x">Izana said:</a></strong><br>The premiere is split &quot;in four&quot;.<br />
</div>I remember that.<div class="spoiler"><input type="button" class="button show_button" value="Show spoiler"><span class="spoiler_content" style="display:none"><input type="button" class="button hide_button" value="Hide spoiler"><br>Himmel dies.</span></div> <img src="x.gif"> Done"#;
        assert_eq!(
            mal_text(html),
            [
                Piece::Text("Very cinematic episode.".into()),
                Piece::Quote(
                    Some("Izana".into()),
                    "The premiere is split \"in four\".".into()
                ),
                Piece::Text("I remember that.".into()),
                Piece::Spoiler("Himmel dies.".into()),
                Piece::Text("[image] Done".into()),
            ]
        );
    }

    /// Rows as MyAnimeList's episode list has them.
    pub(crate) fn episode_row(
        n: u32,
        score: &str,
        topic: u64,
        replies: u32,
        extra: &str,
    ) -> String {
        format!(
            r#"<tr class="episode-list-data">
          <td class="episode-number nowrap" data-raw="{n}">{n}</td>
          <td class="episode-title fs12"><a href="https://myanimelist.net/anime/1/x/episode/{n}" class="fl-l fw-b ">Title &#039;{n}&#039;</a>{extra}
          </td>
          <td class="episode-aired nowrap">Sep 29, 2023</td><td  class="episode-poll ac nowrap scored" data-raw="{score}" ><div class="average">average <span class="value">4.3</span></div></td>
            <td class="episode-forum ac nowrap" data-raw="{replies}">
              <a href="https://myanimelist.net/forum/?topicid={topic}"><i></i>{replies}</a></td></tr>"#
        )
    }

    #[test]
    fn reads_mal_episode_lists() {
        let html = format!(
            "<table>{}{}</table>",
            episode_row(1, "4.28", 2120196, 791, ""),
            episode_row(
                2,
                "0",
                2120201,
                4,
                r#"<span class="icon-episode-type-bg">Filler</span>"#
            )
        );
        let eps = parse_episode_list(&html);
        assert_eq!(
            eps,
            [
                MalEpisode {
                    number: 1,
                    title: Some("Title '1'".into()),
                    score: Some(4.28),
                    topic: Some(2120196),
                    replies: 791,
                    filler: false,
                    recap: false,
                },
                MalEpisode {
                    number: 2,
                    title: Some("Title '2'".into()),
                    score: None,
                    topic: Some(2120201),
                    replies: 4,
                    filler: true,
                    recap: false,
                },
            ]
        );
    }

    /// A topic page as MyAnimeList serves it.
    pub(crate) fn topic_page(title: &str, posts: &[(u64, &str, &str)], last_show: u32) -> String {
        let mut html = format!("<html><title>\n{title} - Forums - MyAnimeList.net\n</title><body>");
        if last_show > 0 {
            html += &format!(r#"<a href="?topicid=1&show={last_show}">Last</a>"#);
        }
        html += r#"<div class="messages">"#;
        for (id, user, body) in posts {
            html += &format!(
                r#"<div class="forum-topic-message message" id="msg{id}" data-id="{id}" data-user="{user}"><div class="message-header"><div class="date" data-time="1695990601">Sep 29</div></div><div class="message-wrapper"><div class="content"><table id="message{id}" class="body clearfix"><tr><td>{body}</td></tr></table></div></div></div>"#
            );
        }
        html + "</div></body></html>"
    }

    /// `MAL_EPISODES=<saved episode list page> MAL_TOPIC=<saved topic page>`:
    /// what the parsers make of real pages.
    #[test]
    #[ignore = "needs saved MyAnimeList pages"]
    fn reads_real_mal_pages() {
        if let Some(path) = std::env::var_os("MAL_EPISODES") {
            let eps = parse_episode_list(&std::fs::read_to_string(path).unwrap());
            println!("{} episodes; first: {:?}", eps.len(), eps.first());
            assert!(!eps.is_empty() && eps.iter().all(|e| e.topic.is_some()));
            assert!(eps.iter().any(|e| e.score.is_some()));
        }
        if let Some(path) = std::env::var_os("MAL_TOPIC") {
            let t = parse_topic(&std::fs::read_to_string(path).unwrap(), 0);
            println!("{:?}: {} posts, next {:?}", t.title, t.posts.len(), t.next);
            for p in t.posts.iter().take(6) {
                println!("--- {} at {}: {:?}", p.author, p.at, p.body);
            }
            assert_eq!(t.posts.len(), 50);
        }
    }

    #[test]
    fn reads_mal_topics() {
        let html = topic_page(
            "Sousou no Frieren Episode 1 Discussion",
            &[(11, "KANLen09", "<b>Hyped</b>!"), (12, "Other", "Agreed")],
            100,
        );
        let t = parse_topic(&html, 50);
        assert_eq!(t.title, "Sousou no Frieren Episode 1 Discussion");
        assert_eq!(
            t.posts
                .iter()
                .map(|p| (p.id, p.author.as_str(), p.at, p.body.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    11,
                    "KANLen09",
                    1695990601,
                    vec![Piece::Text("Hyped!".into())]
                ),
                (12, "Other", 1695990601, vec![Piece::Text("Agreed".into())]),
            ]
        );
        assert_eq!(t.next, Some(100), "one more page");
        assert_eq!(parse_topic(&html, 100).next, None, "the last page");
    }
}
