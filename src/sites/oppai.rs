//! oppai.stream.
//!
//! An episode's page (`/watch?e=…`) builds its stream's address in its
//! script: `'<base>'+startsource+'/<manifest>'`, where `startsource` is a
//! resolution (`720`, `1080` or `4k`). Each is a DASH presentation, fetched
//! with the site as the Referer.
//!
//! Its search answers only its page's own script, which nothing here
//! reads, so shows aren't found on it yet (see [`super::episodes`]).

use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use url::Url;

use super::web::{Session, text};
use super::{Media, Resolved, Stream};

const REFERER: &str = "https://oppai.stream/";
/// The resolutions the player picks from, as its addresses name them.
const RESOLUTIONS: [(u32, &str); 3] = [(720, "720"), (1080, "1080"), (2160, "4k")];

pub(super) async fn resolve(s: &Session, url: &str) -> Result<Resolved> {
    let page = s.need(url).await?;
    let (stream_base, manifest) =
        stream_base(&page).context("no stream on oppai.stream's episode page")?;
    let mut mpds = Vec::new();
    for (res, name) in RESOLUTIONS {
        mpds.push((res, join_path(&stream_base, name, &manifest)?));
    }
    Ok(Resolved {
        title: episode_title(&page),
        streams: vec![Stream {
            media: Media::Dash(mpds),
            referer: Some(REFERER.to_owned()),
            resolution: None,
            label: "DASH".into(),
        }],
    })
}

/// The stream's base address and its manifest's name.
fn stream_base(page: &str) -> Option<(String, String)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"'(https?://[^']+)'\s*\+\s*startsource\s*\+\s*'/([^']+)'").unwrap()
    });
    let c = RE.captures(page)?;
    Some((c[1].to_owned(), c[2].to_owned()))
}

/// `base` with `res` and `manifest` added to its path (yarl's `joinpath`).
fn join_path(base: &str, res: &str, manifest: &str) -> Result<Url> {
    let mut u = Url::parse(base).with_context(|| format!("not an address: {base}"))?;
    let path = format!(
        "{}/{res}/{}",
        u.path().trim_end_matches('/'),
        manifest.trim_start_matches('/')
    );
    u.set_path(&path);
    Ok(u)
}

fn episode_title(page: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<h1.*line-2">(.*)</h1>"#).unwrap());
    Some(text(&RE.captures(page)?[1])).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_episode_page() {
        let page = r#"<h1 class="title line-2">Overflow 1</h1>
        <script>var startsource = '1080'; player.src('https://s2.myspacecat.pictures/v/over-1/'+startsource+'/manifest.mpd');</script>"#;
        let (b, m) = stream_base(page).unwrap();
        assert_eq!(b, "https://s2.myspacecat.pictures/v/over-1/");
        assert_eq!(m, "manifest.mpd");
        assert_eq!(
            join_path(&b, "4k", &m).unwrap().as_str(),
            "https://s2.myspacecat.pictures/v/over-1/4k/manifest.mpd"
        );
        assert_eq!(
            join_path("https://h/x", "720", "/m.mpd").unwrap().as_str(),
            "https://h/x/720/m.mpd"
        );
        assert_eq!(episode_title(page).as_deref(), Some("Overflow 1"));
        assert!(stream_base("<p>nothing</p>").is_none());
    }
}
