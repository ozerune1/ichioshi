//! What the streaming sites share: a [`Session`] that sends back the
//! cookies a site sets (their players check them, as a browser's would),
//! pages told apart from bot checks ([`Challenge`]) and missing pages, and
//! matching a show's names against the titles a site gives it.

use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{Context, Result, ensure};
use regex::Regex;
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, COOKIE, HeaderMap, HeaderValue, SET_COOKIE, USER_AGENT,
};
use reqwest::{RequestBuilder, StatusCode};
use url::Url;

/// The site answered with a bot check (a Cloudflare challenge, a captcha),
/// which isn't something to get past.
#[derive(Debug)]
pub struct Challenge(pub &'static str);

impl std::fmt::Display for Challenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is showing a bot check (a browser challenge); try again later",
            self.0
        )
    }
}

impl std::error::Error for Challenge {}

/// Where tests send a site's requests instead: (its real address, the mock's).
#[cfg(test)]
pub static BASES: Mutex<Vec<(&'static str, String)>> = Mutex::new(Vec::new());

/// `real` (a site's address, like `https://hanime.tv`), or the mock server
/// a test put in its place.
pub fn base(real: &'static str) -> String {
    #[cfg(test)]
    if let Some((_, mock)) = BASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(r, _)| *r == real)
    {
        return mock.clone();
    }
    real.to_owned()
}

/// A cookie a site set: for its domain and the hosts under it, or (set
/// without a Domain) for that one host.
#[derive(Debug, Clone)]
struct Cookie {
    domain: String,
    host_only: bool,
    name: String,
    value: String,
}

/// The headers yt-dlp sends with every request unless told otherwise: the
/// hanime-plugin's extractors were written for its requests (a player page
/// may turn others away).
const YT_DLP_HEADERS: [(reqwest::header::HeaderName, &str); 4] = [
    (
        USER_AGENT,
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36",
    ),
    (
        ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    ),
    (ACCEPT_LANGUAGE, "en-us,en;q=0.5"),
    (
        reqwest::header::HeaderName::from_static("sec-fetch-mode"),
        "navigate",
    ),
];

/// A client sending yt-dlp's headers (and `referer`, if any) with every
/// request: for downloads that don't go through a [`Session`].
pub fn client_like_yt_dlp(referer: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    for (name, value) in YT_DLP_HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if let Some(r) = referer {
        headers.insert(reqwest::header::REFERER, HeaderValue::from_str(r)?);
    }
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(90))
        .build()?)
}

/// One visit to a site: its requests share the cookies its answers set
/// (clones too), and carry yt-dlp's headers.
#[derive(Clone)]
pub struct Session {
    pub client: reqwest::Client,
    /// The site's name, for messages ("hanime.tv").
    pub site: &'static str,
    jar: Arc<Mutex<Vec<Cookie>>>,
}

impl Session {
    pub fn new(client: &reqwest::Client, site: &'static str) -> Self {
        Session {
            client: client.clone(),
            site,
            jar: Arc::default(),
        }
    }

    /// The `Cookie` header for a request to `url`.
    fn cookies_for(&self, url: &Url) -> Option<String> {
        let host = url.host_str()?.to_ascii_lowercase();
        let jar = self.jar.lock().unwrap();
        let pairs: Vec<String> = jar
            .iter()
            .filter(|c| {
                host == c.domain || (!c.host_only && host.ends_with(&format!(".{}", c.domain)))
            })
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    /// Keeps the cookies an answer from `url` sets (and forgets the ones it
    /// clears).
    fn keep(&self, url: &Url, headers: &HeaderMap) {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return;
        };
        let mut jar = self.jar.lock().unwrap();
        for line in headers.get_all(SET_COOKIE).iter() {
            let Ok(line) = line.to_str() else { continue };
            let mut parts = line.split(';');
            let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
                continue;
            };
            let (name, value) = (name.trim().to_owned(), value.trim().to_owned());
            let mut domain = None;
            let mut foreign = false;
            let mut expired = value.is_empty();
            for attr in parts {
                let (k, v) = attr.split_once('=').unwrap_or((attr, ""));
                match k.trim().to_ascii_lowercase().as_str() {
                    "domain" => {
                        let d = v.trim().trim_start_matches('.').to_ascii_lowercase();
                        // Only the host's own domain or one it's under: a
                        // browser drops a cookie for any other.
                        if host == d || host.ends_with(&format!(".{d}")) {
                            domain = Some(d);
                        } else if !d.is_empty() {
                            foreign = true;
                        }
                    }
                    "max-age" => expired |= v.trim().parse::<i64>().is_ok_and(|s| s <= 0),
                    _ => {}
                }
            }
            if foreign {
                continue;
            }
            let host_only = domain.is_none();
            let domain = domain.unwrap_or_else(|| host.clone());
            jar.retain(|c| !(c.name == name && c.domain == domain));
            if !expired {
                jar.push(Cookie {
                    domain,
                    host_only,
                    name,
                    value,
                });
            }
        }
    }

    /// Sends `req` with the cookies it should carry, keeping the ones the
    /// answer sets. A Cloudflare check is a [`Challenge`].
    pub async fn send(&self, req: RequestBuilder) -> Result<reqwest::Response> {
        let mut req = req
            .build()
            .with_context(|| format!("couldn't make a request to {}", self.site))?;
        if !req.headers().contains_key(COOKIE)
            && let Some(cookies) = self.cookies_for(req.url())
            && let Ok(v) = HeaderValue::from_str(&cookies)
        {
            req.headers_mut().insert(COOKIE, v);
        }
        for (name, value) in YT_DLP_HEADERS {
            if !req.headers().contains_key(&name) {
                req.headers_mut()
                    .insert(name, HeaderValue::from_static(value));
            }
        }
        let url = req.url().clone();
        let resp = self
            .client
            .execute(req)
            .await
            .with_context(|| format!("couldn't reach {} ({url})", self.site))?;
        self.keep(resp.url(), resp.headers());
        if resp.headers().contains_key("cf-mitigated") {
            return Err(Challenge(self.site).into());
        }
        Ok(resp)
    }

    /// What the site sends for `req` as text; None if it hasn't got it
    /// (404, 410).
    pub async fn text(&self, req: RequestBuilder) -> Result<Option<String>> {
        let resp = self.send(req).await?;
        let status = resp.status();
        let url = resp.url().clone();
        let body = resp
            .text()
            .await
            .with_context(|| format!("reading {url}"))?;
        if matches!(status.as_u16(), 403 | 429 | 503) && looks_like_challenge(&body) {
            return Err(Challenge(self.site).into());
        }
        if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
            return Ok(None);
        }
        ensure!(
            status.is_success(),
            "{} answered {status} for {url}",
            self.site
        );
        Ok(Some(body))
    }

    /// A web page; None if the site hasn't got it.
    pub async fn page(&self, url: &str) -> Result<Option<String>> {
        self.text(
            self.client
                .get(url)
                .header(ACCEPT, "text/html,application/xhtml+xml"),
        )
        .await
    }

    /// A web page that has to be there.
    pub async fn need(&self, url: &str) -> Result<String> {
        self.page(url)
            .await?
            .with_context(|| format!("{} has no page at {url}", self.site))
    }
}

pub fn looks_like_challenge(body: &str) -> bool {
    body.contains("challenge-platform")
        || body.contains("<title>Just a moment")
        || body.contains("cf-turnstile")
}

/// `href` (from a page at `page`) as an absolute URL.
pub fn absolute(page: &str, href: &str) -> Result<Url> {
    let href = unescape(href.trim());
    Url::parse(page)
        .and_then(|p| p.join(&href))
        .with_context(|| format!("not a link: {href}"))
}

/// The value of the attribute `name` in an HTML tag.
pub fn attr(tag: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r#"(?is)\b{}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#,
        regex::escape(name)
    ))
    .ok()?;
    let c = re.captures(tag)?;
    let v = c.get(1).or(c.get(2)).or(c.get(3))?.as_str();
    Some(unescape(v))
}

/// The `content` of `<meta name="…">` (or `property="…"`).
pub fn meta(html: &str, name: &str) -> Option<String> {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<meta\b[^>]*>").unwrap());
    TAG.find_iter(html).find_map(|m| {
        let tag = m.as_str();
        let named = attr(tag, "name").or_else(|| attr(tag, "property"))?;
        if named.eq_ignore_ascii_case(name) {
            attr(tag, "content")
        } else {
            None
        }
    })
}

/// The `{...}` that starts right after the first match of `before` (a
/// JavaScript object literal: braces inside strings don't count).
pub fn object_after<'a>(html: &'a str, before: &Regex) -> Option<&'a str> {
    let m = before.find(html)?;
    let rest = &html[m.end()..];
    let open = rest.find(|c: char| !c.is_whitespace())?;
    if !rest[open..].starts_with('{') {
        return None;
    }
    balanced(&rest[open..])
}

/// The balanced `{...}` at the start of `s` (JavaScript: braces in strings
/// and comments don't count).
pub fn balanced(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let (mut depth, mut quote, mut i) = (0usize, None::<u8>, 0);
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(_) if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            // A comment ("JW's" in one isn't a quote).
            None if c == b'/' && bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            None if c == b'/' && bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            None => match c {
                b'"' | b'\'' | b'`' => quote = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(&s[..=i]);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Base64 (standard or URL-safe alphabet), padded or not, spaces ignored:
/// as leniently as Python decodes it.
pub fn b64(s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let clean = clean.trim_end_matches('=');
    let url_safe = clean.contains(['-', '_']);
    let engine = if url_safe {
        GeneralPurpose::new(&base64::alphabet::URL_SAFE, config)
    } else {
        GeneralPurpose::new(&base64::alphabet::STANDARD, config)
    };
    engine.decode(clean).context("not base64")
}

/// A JSON object's fields in the order they're written (`serde_json`'s
/// map may sort them; Python keeps them as they come).
pub fn ordered_fields(json: &str) -> Result<Vec<(String, serde_json::Value)>> {
    struct Fields(Vec<(String, serde_json::Value)>);
    impl<'de> serde::Deserialize<'de> for Fields {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visit;
            impl<'de> serde::de::Visitor<'de> for Visit {
                type Value = Fields;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("an object")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Fields, A::Error> {
                    let mut out = Vec::new();
                    while let Some(kv) = map.next_entry()? {
                        out.push(kv);
                    }
                    Ok(Fields(out))
                }
            }
            d.deserialize_map(Visit)
        }
    }
    Ok(serde_json::from_str::<Fields>(json)
        .context("not a JSON object")?
        .0)
}

/// A JSON value as Python's `str()` writes it, which is what yt-dlp sends
/// when one goes in a URL or a form: strings as they are, `True`, `None`.
pub fn python_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(true) => "True".into(),
        serde_json::Value::Bool(false) => "False".into(),
        serde_json::Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// `v` as Python's `json.dumps` writes it: ", " and ": " between items,
/// keys in order, anything not ASCII escaped.
pub fn py_json(v: &serde_json::Value) -> String {
    use serde_json::Value;
    fn string(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    let mut units = [0u16; 2];
                    for u in c.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{u:04x}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write(item, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    string(k, out);
                    out.push_str(": ");
                    write(item, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

/// An object of `fields` as Python's `json.dumps` writes a dict: in this
/// order (a `serde_json` map may sort its keys).
pub fn py_object(fields: &[(&str, serde_json::Value)]) -> String {
    let items: Vec<String> = fields
        .iter()
        .map(|(k, v)| {
            format!(
                "{}: {}",
                py_json(&serde_json::Value::String((*k).to_owned())),
                py_json(v)
            )
        })
        .collect();
    format!("{{{}}}", items.join(", "))
}

// ---------------------------------------------------------------------------
// Names

/// Lowercase letters and digits only.
pub fn normalize(s: &str) -> String {
    unescape(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether `found` (normalized) is one of `wanted` (normalized), give or
/// take a closing "The Animation".
pub fn matches(wanted: &[String], found: &str) -> bool {
    if found.is_empty() {
        return false;
    }
    let bare = |s: &str| s.strip_suffix("theanimation").unwrap_or(s).to_owned();
    let found = bare(found);
    wanted.iter().any(|w| !w.is_empty() && bare(w) == found)
}

/// `names` normalized, once each, empty ones left out.
pub fn wanted(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names.iter().map(|n| normalize(n)) {
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// What to search for: each name, then without punctuation (sites drop
/// apostrophes: "Hon'i" is "Honi"), then each name's first words.
pub fn queries(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |q: String| {
        let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
        if !q.is_empty() && !out.contains(&q) {
            out.push(q);
        }
    };
    for name in names {
        push(name.clone());
        push(plain(name));
    }
    for name in names {
        let plain = plain(name);
        let words: Vec<&str> = plain.split_whitespace().collect();
        if words.len() > 3 {
            push(words[..3].join(" "));
        }
    }
    out
}

/// `s` with only letters, digits, spaces and hyphens; apostrophes dropped,
/// other punctuation made spaces.
pub fn plain(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\'' | '’' | '`'))
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect()
}

/// A name as sites put it in their addresses: "Kuroinu: Kedakaki" is
/// `kuroinu-kedakaki` (ASCII letters and digits, hyphens between words).
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !matches!(c, '\'' | '’' | '`') && !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// The episode number in a title or address: "Episode 3", `…-episode-3/`,
/// else a number at the end ("Overflow 3", `overflow-3`).
pub fn episode_number(s: &str) -> Option<u32> {
    static EPISODE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(?:episode|ep)[\s._-]*0*(\d{1,4})\b").unwrap());
    static END: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|[\s_-])0*(\d{1,4})\s*/?\s*$").unwrap());
    EPISODE
        .captures_iter(s)
        .last()
        .or_else(|| END.captures(s))
        .and_then(|c| c[1].parse().ok())
        .filter(|&n| n > 0)
}

/// A title without its episode number: "Overflow - Episode 3" and
/// "Overflow 3" are "Overflow".
pub fn without_episode(title: &str) -> String {
    static EPISODE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)[\s:–-]*\b(?:episode|ep)[\s._-]*\d{1,4}\b.*$|[\s:–-]+\d{1,4}\s*$").unwrap()
    });
    EPISODE.replace(title.trim(), "").trim().to_owned()
}

/// An HTML fragment's text, entities decoded and spaces collapsed.
pub fn text(html: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
    unescape(&TAG.replace_all(html, " "))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());
    ENTITY
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
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
                    _ => None,
                }
            };
            ch.map_or_else(|| c[0].to_owned(), String::from)
        })
        .into_owned()
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_cookies_per_domain() {
        let s = Session::new(&reqwest::Client::new(), "x");
        let mut h = HeaderMap::new();
        h.append(SET_COOKIE, "a=1; Path=/; HttpOnly".parse().unwrap());
        h.append(SET_COOKIE, "b=2; Domain=.example.com".parse().unwrap());
        h.append(SET_COOKIE, "evil=3; Domain=other.net".parse().unwrap());
        s.keep(&Url::parse("https://www.example.com/x").unwrap(), &h);
        let at = |u: &str| s.cookies_for(&Url::parse(u).unwrap());
        assert_eq!(at("https://www.example.com/y").as_deref(), Some("a=1; b=2"));
        assert_eq!(at("https://cdn.example.com/").as_deref(), Some("b=2"));
        assert_eq!(at("https://other.net/"), None);
        let mut h = HeaderMap::new();
        h.append(SET_COOKIE, "a=; Max-Age=0".parse().unwrap());
        s.keep(&Url::parse("https://www.example.com/").unwrap(), &h);
        assert_eq!(at("https://www.example.com/").as_deref(), Some("b=2"));
    }

    #[test]
    fn reads_tags_and_objects() {
        let tag = r#"<input type="hidden" name="title" value="Overflow &amp; more">"#;
        assert_eq!(attr(tag, "value").as_deref(), Some("Overflow & more"));
        assert_eq!(attr(tag, "name").as_deref(), Some("title"));
        let html =
            r#"<meta content="sha512-abc" name="x-secure-token"><meta name="other" content="no">"#;
        assert_eq!(meta(html, "x-secure-token").as_deref(), Some("sha512-abc"));
        let re = Regex::new(r"var data\s*=\s*").unwrap();
        let js = r#"var data = { a: "}", b: {c: 1} }; var x = {};"#;
        assert_eq!(object_after(js, &re), Some(r#"{ a: "}", b: {c: 1} }"#));
        assert_eq!(balanced("}{"), None);
        let commented = "{a: 1, // JW's {default}\nb: '}' /* c's */}; rest";
        assert_eq!(
            balanced(commented),
            Some("{a: 1, // JW's {default}\nb: '}' /* c's */}")
        );
        assert_eq!(python_str(&serde_json::json!(true)), "True");
        assert_eq!(python_str(&serde_json::json!(12)), "12");
        assert_eq!(python_str(&serde_json::json!("x y")), "x y");
        assert_eq!(b64("aGk").unwrap(), b"hi");
        assert_eq!(b64("aGk=\n").unwrap(), b"hi");
        assert_eq!(b64("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(
            py_object(&[
                ("b", serde_json::json!([1, "é\"\n"])),
                ("a", serde_json::json!(null)),
                ("c", serde_json::json!(false)),
            ]),
            r#"{"b": [1, "\u00e9\"\n"], "a": null, "c": false}"#
        );
    }

    #[test]
    fn reads_names_and_numbers() {
        assert_eq!(slug("Kuroinu: Kedakaki Seijo"), "kuroinu-kedakaki-seijo");
        assert_eq!(slug("Hon'i no Tame"), "honi-no-tame");
        assert_eq!(episode_number("Episode 3"), Some(3));
        assert_eq!(episode_number("overflow-episode-12/"), Some(12));
        assert_eq!(episode_number("Overflow Season Episode 1"), Some(1));
        assert_eq!(episode_number("Overflow 4"), Some(4));
        assert_eq!(episode_number("overflow-2"), Some(2));
        assert_eq!(episode_number("Overflow"), None);
        assert_eq!(without_episode("Overflow - Episode 3"), "Overflow");
        assert_eq!(without_episode("Overflow 3"), "Overflow");
        assert_eq!(
            without_episode("Overflow Season Episode 1"),
            "Overflow Season"
        );
        assert!(matches(
            &wanted(&["Overflow".into()]),
            &normalize("OVERFLOW the animation")
        ));
        assert_eq!(percent_decode("eyJ%3D%3D+"), "eyJ==+");
    }
}
