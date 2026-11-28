//! Each site's whole way through, against a mock server playing its pages
//! and player APIs as the hanime-plugin reads them.
// The mock lock is held for the whole test on purpose (the sites'
// addresses are global).
#![allow(clippy::await_holding_lock)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::mock_http::{Req, Resp, mock};

/// Points sites' addresses at a mock until dropped.
struct Bases(Vec<&'static str>);

impl Bases {
    fn set(pairs: &[(&'static str, &str)]) -> Self {
        let mut b = web::BASES.lock().unwrap();
        for (real, mock) in pairs {
            b.retain(|(r, _)| r != real);
            b.push((real, (*mock).to_owned()));
        }
        Bases(pairs.iter().map(|(r, _)| *r).collect())
    }
}

impl Drop for Bases {
    fn drop(&mut self) {
        web::BASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(r, _)| !self.0.contains(r));
    }
}

/// A mock server whose handler knows its own address.
async fn server(handler: impl Fn(&Req, &str) -> Resp + Send + Sync + 'static) -> String {
    let me: Arc<OnceLock<String>> = Arc::default();
    let at = me.clone();
    let base =
        mock(move |req| handler(req, at.get().map(String::as_str).unwrap_or_default())).await;
    me.set(base.clone()).unwrap();
    base
}

fn page(html: &str, base: &str) -> Resp {
    Resp::html(200, &html.replace("{base}", base))
}

fn missing() -> Resp {
    Resp::html(404, "<h1>Not found</h1>")
}

fn path(req: &Req) -> &str {
    req.target.split('?').next().unwrap()
}

/// A file served with Range requests, as video servers do.
fn ranged(file: &[u8], req: &Req) -> Resp {
    let range = req
        .headers
        .get("range")
        .and_then(|r| r.strip_prefix("bytes="));
    match range {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap();
            let a: usize = a.parse().unwrap();
            let b = b
                .parse::<usize>()
                .unwrap_or(file.len() - 1)
                .min(file.len() - 1);
            Resp {
                status: 206,
                headers: vec![
                    ("Content-Type".into(), "video/mp4".into()),
                    (
                        "Content-Range".into(),
                        format!("bytes {a}-{b}/{}", file.len()),
                    ),
                ],
                body: file[a..=b].to_vec(),
            }
        }
        None => Resp::binary(200, file.to_vec()),
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("ichioshi-test")
        .build()
        .unwrap()
}

#[tokio::test]
async fn hentaimama_finds_a_show_and_its_mp4() {
    let video: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let served = video.clone();
    let posted: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let seen = posted.clone();
    let base = server(move |req, base| match path(req) {
        "/" if req.query().contains_key("s") => {
            assert_eq!(req.query()["s"], "Overflow");
            page(super::hentaimama::SEARCH, base)
        }
        "/tvshows/overflow/" => page(
            r#"<h1 class="dsc-title">Overflow</h1><div class="dsc-alt">Overflow</div>
            <a class="dt-se-item" href="{base}/episodes/overflow-episode-1/"><b class="dt-se-title">Episode 1</b></a>
            <a class="dt-se-item" href="{base}/episodes/overflow-episode-2/"><b class="dt-se-title">Episode 2</b></a>"#,
            base,
        ),
        "/episodes/overflow-episode-1/" => page(
            r##"<input type="hidden" name="title" value="Overflow Episode 1">
            <a href="#option-2">mi-1</a> <a href="#option-1">mi-2</a>
            <script>function dtLoadOption(n) {$.post("{base}/wp-admin/admin-ajax.php", {action: 'get_player_contents',a: '7353',i: n}, function(r) {});}</script>"##,
            base,
        ),
        "/wp-admin/admin-ajax.php" => {
            assert_eq!(req.method, "POST");
            let form = req.form();
            seen.lock().unwrap().push((form["a"].clone(), form["i"].clone()));
            // Mirror n's player at n - 1; mirror 1's is gone.
            let list = match form["i"].as_str() {
                "2" => json!(["", format!(r#"<iframe src="{base}/?dt_embed=r2-mp4&#038;p=x" frameborder="0"></iframe>"#)]),
                _ => json!(["", ""]),
            };
            Resp {
                status: 200,
                headers: vec![],
                body: list.to_string().into_bytes(),
            }
        }
        "/" if req.query().contains_key("dt_embed") => page(
            r#"<script>var jwp = jwplayer('playercizEiGhEYxCW').setup({sources: [{"file":"\/files\/ep1.mp4","label":"720p","type":"mp4","default":true},{"file":"\/files\/ep1-480.mp4","label":"480p","type":"mp4"}],advertising: {client:"vast",skipoffset:5},image: "x.jpg"});</script>"#,
            base,
        ),
        "/files/ep1.mp4" => ranged(&served, req),
        _ => missing(),
    })
    .await;
    let _net = crate::mock_http::mocked();
    let _bases = Bases::set(&[("https://hentaimama.io", &base)]);
    let c = client();
    let eps = episodes(&c, StreamSite::Hentaimama, &["Overflow".into()])
        .await
        .unwrap();
    assert_eq!(eps.len(), 2);
    assert_eq!(eps[0].url, format!("{base}/episodes/overflow-episode-1/"));
    assert_eq!(eps[1].title, "Overflow - Episode 2");
    assert!(
        episodes(&c, StreamSite::Hentaimama, &["Not There".into()])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        resolution(&c, StreamSite::Hentaimama, &eps[0].url)
            .await
            .unwrap(),
        Some(720)
    );
    assert_eq!(
        *posted.lock().unwrap(),
        [
            ("7353".to_owned(), "2".to_owned()),
            ("7353".to_owned(), "1".to_owned())
        ],
        "each mirror, in the page's order"
    );

    let dir = tempfile::tempdir().unwrap();
    let stem = dir.path().join("Overflow - 01");
    let file = download(
        &c,
        StreamSite::Hentaimama,
        &eps[0].url,
        &stem,
        &AtomicBool::new(false),
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(file, dir.path().join("Overflow - 01.mp4"));
    assert_eq!(std::fs::read(&file).unwrap(), video);
}

#[tokio::test]
async fn hentaihaven_finds_a_show_and_asks_its_player() {
    let octopus = Arc::new(AtomicBool::new(false));
    let vp9 = octopus.clone();
    let base = server(move |req, base| match path(req) {
        "/" => {
            let q = req.query();
            assert_eq!(q["post_type"], "wp-manga");
            page(
                r#"<div class="item"> <a href="{base}/video/overflow-hentai/"> <div class="title">Overflow Hentai</div> </a> </div>
                <div class="item"> <a href="{base}/video/overflow/"> <img alt="Overflow cover"> <div class="title">Overflow</div> </a> </div>"#,
                base,
            )
        }
        "/video/overflow/" => page(
            r#"<h1 class="htitle"> Overflow </h1> <h3 class="h4">Overflow - おーばーふろぉ</h3>
            <li class="hentai__chapter"> <a href="{base}/video/overflow/episode-1"> <span class="title">Episode 1</span> </a> </li>"#,
            base,
        ),
        "/video/overflow/episode-1" => page(
            r#"<h1 id="chapter-heading" class="h3">Overflow Episode 1</h1>
            <iframe src="{base}/wp-content/plugins/player-logic/player.php?data=q1&amp;n=1"></iframe>"#,
            base,
        ),
        "/wp-content/plugins/player-logic/player.php" => {
            assert_eq!(req.query()["n"], "1", "the &amp; was decoded");
            let token = super::hentaihaven::encipher(
                &json!({"en": "EN==", "iv": "IV==", "uri": format!("{base}/zarat/")}).to_string(),
            );
            page(
                &format!(r#"<html><head><meta name="x-secure-token" content="{token}"></head></html>"#),
                base,
            )
        }
        "/zarat/api.php" => {
            assert_eq!(req.method, "POST");
            let body = String::from_utf8_lossy(&req.body);
            assert!(req.headers["content-type"].starts_with("multipart/form-data"));
            for (field, value) in [("action", "zarat_get_data_player_ajax"), ("a", "EN=="), ("b", "IV==")] {
                assert!(
                    body.contains(&format!("name=\"{field}\"\r\n\r\n{value}\r\n")),
                    "{field} in {body}"
                );
            }
            Resp {
                status: 200,
                headers: vec![],
                body: json!({"status": true, "data": {
                    "sources": [{"src": format!("{base}/hls/ep1/playlist.m3u8"), "type": "application/x-mpegURL"}],
                    "isOctopus": vp9.load(Ordering::SeqCst),
                }})
                .to_string()
                .into_bytes(),
            }
        }
        _ => missing(),
    })
    .await;
    let _net = crate::mock_http::mocked();
    let _bases = Bases::set(&[("https://hentaihaven.com", &base)]);
    let c = client();
    let eps = episodes(
        &c,
        StreamSite::HentaiHaven,
        &["Overflow".into(), "おーばーふろぉ".into()],
    )
    .await
    .unwrap();
    assert_eq!(eps.len(), 1);
    let s = Session::new(&c, "hentaihaven.com");
    let r = resolve(&s, StreamSite::HentaiHaven, &eps[0].url)
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("Overflow Episode 1"));
    assert_eq!(
        r.streams
            .iter()
            .map(|s| s.media.clone())
            .collect::<Vec<_>>(),
        [Media::Hls(
            format!("{base}/hls/ep1/playlist.m3u8").parse().unwrap()
        )]
    );
    octopus.store(true, Ordering::SeqCst);
    let r = resolve(&s, StreamSite::HentaiHaven, &eps[0].url)
        .await
        .unwrap();
    assert_eq!(
        r.streams[1].media,
        Media::Hls(format!("{base}/hls/ep1/playlist_vp9.m3u8").parse().unwrap())
    );
}

#[tokio::test]
async fn hanime_red_does_the_players_challenge() {
    let asked: Arc<Mutex<Option<std::collections::HashMap<String, String>>>> = Arc::default();
    let query = asked.clone();
    let base = server(move |req, base| match path(req) {
        // One show over two pages of the same name, as hanime.red has some.
        "/search" => page(
            r#"<a class="chip" href="/serie/sister-breeder"> <!-- -->Sister Breeder<!-- --> <span class="tnum">1</span></a><a class="chip" href="/serie/sister-breeder-new"> <!-- -->Sister Breeder<!-- --> <span class="tnum">2</span></a><a class="chip" href="/serie/sister-other"> <!-- -->Sister Other<!-- --></a>"#,
            base,
        ),
        "/serie/sister-breeder" => page(
            r#"<h1 class="display">Sister Breeder</h1><div class="grid-wide"><a class="card group block " title="Sister Breeder Episode 3" href="/sister-breeder-episode-3"></a></div>"#,
            base,
        ),
        "/serie/sister-breeder-new" => page(
            r#"<h1 class="display">Sister Breeder</h1><div class="grid-wide"><a class="card group block " title="Sister Breeder Episode 1" href="/sister-breeder-episode-1"></a><a class="card group block " title="Sister Breeder Episode 2" href="/sister-breeder-ep-2"></a></div>"#,
            base,
        ),
        "/sister-breeder-episode-1" => page(
            r#"<h1 duration="" class="title">Sister Breeder Episode 1</h1><iframe src="/embed/sb-1" allowfullscreen></iframe>"#,
            base,
        ),
        "/embed/sb-1" => page(super::hanimered::PLAYER_PAGE, base),
        "/v/abc123" => page(super::hanimered::REAL_PLAYER, base),
        "/player-core.min.js" => Resp {
            status: 200,
            headers: vec![("Content-Type".into(), "text/javascript".into())],
            body: super::hanimered::SCRIPT.as_bytes().to_vec(),
        },
        "/get-video-url-v2.php" => {
            assert_eq!(req.headers["x-requested-with"], "XMLHttpRequest");
            *query.lock().unwrap() = Some(req.query());
            Resp {
                status: 200,
                headers: vec![],
                body: json!({"url": format!("{base}/media/sb-1.mp4")}).to_string().into_bytes(),
            }
        }
        _ => missing(),
    })
    .await;
    let _net = crate::mock_http::mocked();
    let _bases = Bases::set(&[
        ("https://hanime.red", &base),
        ("https://nhplayer.com", &base),
    ]);
    let c = client();
    let eps = episodes(&c, StreamSite::HanimeRed, &["Sister Breeder".into()])
        .await
        .unwrap();
    assert_eq!(
        eps.iter().map(|e| e.episode).collect::<Vec<_>>(),
        [1, 2, 3],
        "both pages' episodes"
    );
    assert_eq!(eps[1].url, format!("{base}/sister-breeder-ep-2"));
    let s = Session::new(&c, "hanime.red");
    let r = resolve(&s, StreamSite::HanimeRed, &eps[0].url)
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("Sister Breeder Episode 1"));
    assert_eq!(
        r.streams[0].media,
        Media::File(format!("{base}/media/sb-1.mp4").parse().unwrap())
    );
    let q = asked.lock().unwrap().clone().unwrap();
    assert_eq!(q["vid"], "4821");
    assert_eq!(q["c"], "c7f");
    assert_eq!((q["p1"].as_str(), q["t"].as_str()), ("part-one", "tee"));
    assert_eq!(
        (q["sc"].as_str(), q["rid"].as_str()),
        ("tok-one", "tok-two")
    );
    assert_eq!(q["st"], "True");
    let h = ring::digest::digest(
        &ring::digest::SHA256,
        format!("part-onepart-twopart-threepart-fourtee{}", q["pow"]).as_bytes(),
    );
    assert_eq!(&h.as_ref()[..2], &[0, 0], "a proof of work that works");
    let fp: Value = serde_json::from_slice(&web::b64(&q["fp"]).unwrap()).unwrap();
    assert!(fp["t"].as_u64().unwrap() > 2100);
}

#[tokio::test]
async fn hanime_tv_finds_episodes_by_name_and_shakes_hands() {
    // Which search database the search page names: one that's there, or not.
    let db_there = Arc::new(AtomicBool::new(true));
    let there = db_there.clone();
    let base = server(move |req, base| match path(req) {
        "/search" => {
            let db = if there.load(Ordering::SeqCst) { "db" } else { "gone" };
            page(
                &format!(
                    r#"<script>window.AppConfig = {{"api_url":"x","search_hvs_url":"{}\/api\/{db}"}};</script>"#,
                    base.replace('/', "\\/")
                ),
                base,
            )
        }
        "/api/db" => Resp {
            status: 200,
            headers: vec![],
            body: json!([
                {"id": 1, "name": "Overflow 2", "slug": "overflow-2", "search_titles": ["おーばーふろぉ"]},
                {"id": 2, "name": "Overflow 1", "slug": "overflow-1", "search_titles": "Overflow"},
                {"id": 3, "name": "Overflow Season 2 1", "slug": "overflow-season-2-1"},
                {"id": 4, "name": "Kunoichi 1", "slug": "kunoichi-1", "search_titles": ["Shinobi Girls"]},
                {"id": 5, "name": "Broken", "slug": ""},
            ])
            .to_string()
            .into_bytes(),
        },
        "/videos/hentai/underflow-1" => page(r#"<h1 class="tv-title">Something Else 1</h1>"#, base),
        "/videos/hentai/overflow-1" | "/videos/hentai/overflow-2" => {
            let n = req.target.rsplit('-').next().unwrap().to_owned();
            page(
                &format!(r#"<div class="v-card"><h1 class="tv-title">Overflow {n}</h1></div>"#),
                base,
            )
        }
        "/api/v11/handshake" => {
            assert_eq!(req.method, "POST");
            let h = &req.headers;
            assert_eq!(h["origin"], "https://hanime.tv");
            assert_eq!(h["x-signature-version"], "web2");
            assert_eq!(h["x-csrf-token"], "null");
            let time: u64 = h["x-time"].parse().unwrap();
            assert_eq!(h["x-signature"], super::hanimetv::signature_for_test(time));
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let inner = super::hanimetv::open_for_test(body["token"].as_str().unwrap());
            assert_eq!(inner["directive"], "htv_player_handshake");
            assert_eq!(inner["slug"], "overflow-2");
            assert_eq!(inner["timestamp_unix"], time);
            let manifest = json!({"sources": [
                {"kind": "premium", "src": "/premium/1080.m3u8", "label": "1080p"},
                {"kind": "normal", "src": "/hls/720.m3u8", "label": "720p"},
            ]});
            Resp {
                status: 200,
                headers: vec![("X-Token".into(), super::hanimetv::seal_for_test(&manifest))],
                body: b"{}".to_vec(),
            }
        }
        _ => missing(),
    })
    .await;
    let _net = crate::mock_http::mocked();
    let _bases = Bases::set(&[
        ("https://hanime.tv", &base),
        ("https://auth.hanime.tv", &base),
    ]);
    let c = client();
    let eps = episodes(
        &c,
        StreamSite::HanimeTv,
        &["Overflow".into(), "おーばーふろぉ".into()],
    )
    .await
    .unwrap();
    // From the search database (not "Overflow Season 2").
    assert_eq!(
        eps.iter()
            .map(|e| (e.episode, e.title.as_str(), e.url.clone()))
            .collect::<Vec<_>>(),
        [
            (
                1,
                "Overflow - Episode 1",
                format!("{base}/videos/hentai/overflow-1")
            ),
            (
                2,
                "Overflow - Episode 2",
                format!("{base}/videos/hentai/overflow-2")
            ),
        ]
    );
    // By another of its titles; and a show it hasn't got.
    let by_other = episodes(&c, StreamSite::HanimeTv, &["Shinobi Girls".into()])
        .await
        .unwrap();
    assert_eq!(by_other.len(), 1);
    assert!(by_other[0].url.ends_with("/videos/hentai/kunoichi-1"));
    assert!(
        episodes(&c, StreamSite::HanimeTv, &["Not There".into()])
            .await
            .unwrap()
            .is_empty()
    );
    // Without the database: looked for where its name says.
    db_there.store(false, Ordering::SeqCst);
    let guessed = episodes(&c, StreamSite::HanimeTv, &["Overflow".into()])
        .await
        .unwrap();
    assert_eq!(
        guessed.iter().map(|e| e.episode).collect::<Vec<_>>(),
        [1, 2]
    );
    // A page under the name that's another show's isn't taken.
    assert!(
        episodes(&c, StreamSite::HanimeTv, &["Underflow".into()])
            .await
            .unwrap()
            .is_empty()
    );
    let s = Session::new(&c, "hanime.tv");
    let r = resolve(&s, StreamSite::HanimeTv, &eps[1].url)
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("Overflow 2"));
    assert_eq!(r.streams.len(), 1);
    assert_eq!(
        r.streams[0].media,
        Media::Hls(format!("{base}/hls/720.m3u8").parse().unwrap())
    );
    assert_eq!(r.streams[0].resolution, Some(720));
}

/// An ffmpeg-made DASH presentation of 5 s of H.264 at `size`, with AAC
/// audio in MP4, or (`opus`) Opus in WebM as oppai.stream's 4K has, in a
/// new folder; None without ffmpeg.
fn dash_fixture(size: &str, opus: bool) -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    let audio: &[&str] = if opus {
        &[
            "-c:a",
            "libopus",
            "-b:a",
            "64k",
            "-dash_segment_type",
            "auto",
        ]
    } else {
        &["-c:a", "aac", "-b:a", "64k"]
    };
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate=24:duration=5"))
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=5"])
        .args(["-c:v", "libx264", "-g", "48"])
        .args(audio)
        .args([
            "-f",
            "dash",
            "-seg_duration",
            "2",
            "-use_template",
            "1",
            "-use_timeline",
            "1",
        ])
        .arg(dir.path().join("manifest.mpd"))
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skipping: ffmpeg couldn't make a DASH sample");
        return None;
    }
    Some(dir)
}

#[tokio::test]
async fn oppai_stream_joins_its_dash_with_the_sites_referer() {
    let (Some(uhd), Some(fhd)) = (
        dash_fixture("640x360", true),
        dash_fixture("320x240", false),
    ) else {
        return;
    };
    let (uhd_files, fhd_files) = (uhd.path().to_owned(), fhd.path().to_owned());
    let referers = Arc::new(AtomicUsize::new(0));
    let counted = referers.clone();
    let base = server(move |req, base| {
        let p = path(req);
        if req.headers.get("referer").map(String::as_str) == Some("https://oppai.stream/") {
            counted.fetch_add(1, Ordering::SeqCst);
        }
        let file = |dir: &Path, prefix: &str| match std::fs::read(dir.join(p.trim_start_matches(prefix))) {
            Ok(bytes) => Resp::binary(200, bytes),
            Err(_) => missing(),
        };
        match p {
            "/api/search.php" => {
                let form = req.form();
                assert_eq!(form["text"], "Overflow");
                assert_eq!((form["order"].as_str(), form["page"].as_str()), ("recent", "1"));
                page(
                    r#"<div class='results-num' style='display: none;'>2</div>
                    <a href="{base}/watch/Overflow-1/from/search" class="episode " epid="1"><div class="episode-title-wrap"><p class="s4 white bold episode-title">Overflow</p></div></a>
                    <a href="{base}/watch/Overflow-Season-2-1/from/search" class="episode " epid="1"><p class="episode-title">Overflow Season 2</p></a>"#,
                    base,
                )
            }
            "/watch/Overflow-1" => page(
                &format!(
                    r#"<h1 id="episode-title" class="white bebas">Overflow Episode 1</h1>
                    <script>var folder = "studio\/overflow"; var storageBase = "{}";
                    var layout = 2;
                    var resList = ["4k","1080"];
                    var ep = "1";
                    var res = "1080-1,4k-1";</script>"#,
                    base.replace('/', "\\/")
                ),
                base,
            ),
            p if p.starts_with("/studio/overflow/4K/dash01/") => {
                file(&uhd_files, "/studio/overflow/4K/dash01/")
            }
            p if p.starts_with("/studio/overflow/1080p/dash01/") => {
                file(&fhd_files, "/studio/overflow/1080p/dash01/")
            }
            _ => missing(),
        }
    })
    .await;
    let _net = crate::mock_http::mocked();
    let c = client();
    let _bases = Bases::set(&[("https://oppai.stream", &base)]);
    let eps = episodes(&c, StreamSite::OppaiStream, &["Overflow".into()])
        .await
        .unwrap();
    assert_eq!(eps.len(), 1, "not Season 2's");
    assert_eq!(eps[0].url, format!("{base}/watch/Overflow-1"));
    let url = eps[0].url.clone();
    assert_eq!(
        resolution(&c, StreamSite::OppaiStream, &url).await.unwrap(),
        Some(2160),
        "as the site names it"
    );
    let dir = tempfile::tempdir().unwrap();
    let file = download(
        &c,
        StreamSite::OppaiStream,
        &url,
        &dir.path().join("Overflow 1.5 - 01"),
        &AtomicBool::new(false),
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(file, dir.path().join("Overflow 1.5 - 01.mp4"));
    assert!(referers.load(Ordering::SeqCst) > 4);
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,width:format=duration",
        ])
        .args(["-of", "compact=p=0:nk=1"])
        .arg(&file)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&probe.stdout);
    // The 4K's video (640 wide here), the 1080p's AAC (not the 4K's Opus).
    assert!(out.contains("h264|640") && out.contains("aac"), "{out}");
    assert!(!out.contains("opus"), "{out}");
    let duration: f64 = out.lines().last().unwrap().trim().parse().unwrap();
    assert!((duration - 5.0).abs() < 0.3, "{out}");
}

/// Each site for real, one show: its episodes found by name, and the first
/// one's player asked for its streams (no video is downloaded). Says only
/// what was found: a count, the episode's title and its resolution.
#[tokio::test]
#[ignore = "reaches the real sites"]
async fn live_finds_shows_and_streams() {
    let _net = crate::mock_http::real();
    let c = reqwest::Client::builder()
        .user_agent(concat!("ichioshi/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    // A show each site has in episodes (hanime.tv has Overflow as one
    // video, a whole season).
    let names_for = |site: StreamSite| {
        vec![
            if site == StreamSite::HanimeTv {
                "Sister Breeder"
            } else {
                "Overflow"
            }
            .to_owned(),
        ]
    };
    let mut failed = Vec::new();
    // `LIVE_SITES=oppai.stream,hanime.red`: only those.
    let only = std::env::var("LIVE_SITES").unwrap_or_default();
    for site in [
        StreamSite::HentaiHaven,
        StreamSite::Hentaimama,
        StreamSite::HanimeRed,
        StreamSite::HanimeTv,
        StreamSite::OppaiStream,
    ] {
        if !only.is_empty() && !only.split(',').any(|n| n.trim() == site.name()) {
            continue;
        }
        let names = names_for(site);
        let eps = match episodes(&c, site, &names).await {
            Ok(eps) if !eps.is_empty() => eps,
            Ok(_) => {
                let why = if site == StreamSite::HanimeTv {
                    let s = Session::new(&c, site.name());
                    super::hanimetv::diagnose(&s, &names[0]).await
                } else {
                    String::new()
                };
                println!("{}: FAIL (no episodes found) {why}", site.name());
                failed.push(site.name());
                continue;
            }
            Err(e) => {
                println!("{}: FAIL finding episodes: {e:#}", site.name());
                failed.push(site.name());
                continue;
            }
        };
        let s = Session::new(&c, site.name());
        let outcome = async {
            let r = resolve(&s, site, &eps[0].url).await?;
            let (stream, res) = best(&s, &r).await?;
            match &stream.media {
                Media::Hls(url) => crate::hls::check(&s, url, stream.referer.as_deref()).await?,
                // The server has to say how big it is, to download it.
                Media::File(url) => {
                    let (len, _) =
                        crate::ohentai::probe_with(&c, url.as_str(), stream.referer.as_deref())
                            .await?;
                    anyhow::ensure!(len.is_some(), "the video's server doesn't say its size");
                }
                // Its MPDs: whether one is there is the resolution above.
                Media::Dash(_) => anyhow::ensure!(res.is_some(), "no MPD of it is there"),
            }
            anyhow::Ok((r.title, stream.label, res))
        }
        .await;
        match outcome {
            Ok((title, label, res)) => println!(
                "{}: ok, {} episodes; episode 1 {:?}: {label}, {}",
                site.name(),
                eps.len(),
                title.unwrap_or_default(),
                res.map_or("resolution unknown".into(), |r| format!("{r}p"))
            ),
            Err(e) => {
                println!("{}: FAIL reading episode 1's player: {e:#}", site.name());
                failed.push(site.name());
            }
        }
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}

/// oppai.stream's newer episodes for real: their DASH (4K AV1, its audio
/// taken from the 1080p presentation, Opus in WebM being 4K's) joins. Two
/// segments are downloaded, said only by ffprobe's codecs, then deleted.
#[tokio::test]
#[ignore = "reaches the real site"]
async fn live_oppai_dash_sample() {
    let _net = crate::mock_http::real();
    let c = reqwest::Client::new();
    let eps = episodes(
        &c,
        StreamSite::OppaiStream,
        &["Harem-tou e Youkoso!".into()],
    )
    .await
    .unwrap();
    assert!(!eps.is_empty());
    let s = Session::new(&c, "oppai.stream");
    let r = resolve(&s, StreamSite::OppaiStream, &eps[0].url)
        .await
        .unwrap();
    let Media::Dash(mpds) = &r.streams[0].media else {
        panic!("not DASH: {}", r.streams[0].label)
    };
    let dash = crate::hstream::Dash::at("oppai.stream", "https://oppai.stream/", mpds).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = crate::hstream::download_dash(
        &web::client_like_yt_dlp(None).unwrap(),
        &dash,
        &dir.path().join("sample.mp4"),
        &Default::default(),
        Some(2),
        &AtomicBool::new(false),
        |_, _| {},
    )
    .await
    .unwrap();
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_name,height"])
        .args(["-of", "compact=p=0:nk=1"])
        .arg(&file)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&probe.stdout).replace('\n', " ");
    println!("oppai.stream DASH sample: {out}");
    assert!(out.contains("aac"), "{out}");
}
