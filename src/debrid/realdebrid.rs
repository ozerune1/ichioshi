//! Real-Debrid: https://api.real-debrid.com/
//!
//! A torrent waits for us to pick its files before it starts; its `links`
//! (one per picked file, in order) then go through `/unrestrict/link` to
//! become download URLs.

use anyhow::Result;
use reqwest::StatusCode;
use serde_json::Value;

use super::{Account, Client, Gone, Remote, RemoteFile, RemoteState, date, id, num, text};

/// Error codes: 8 bad token, 7 resource not found.
fn check(c: &Client, status: StatusCode, body: &Value) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let code = body["error_code"].as_i64();
    if status == StatusCode::UNAUTHORIZED || code == Some(8) {
        return Err(c.bad_key());
    }
    if status == StatusCode::NOT_FOUND || code == Some(7) {
        return Err(anyhow::Error::new(Gone));
    }
    let message = body["error"].as_str().map(str::to_owned).unwrap_or_else(|| format!("HTTP {status}"));
    Err(c.error(match code {
        Some(9) | Some(20) => format!("{message} (is the account premium?)"),
        _ => message,
    }))
}

pub(super) async fn account(c: &Client) -> Result<Account> {
    let (status, v) = c.send(c.get("/user")).await?;
    check(c, status, &v)?;
    Ok(Account { name: text(&v["username"]), premium: v["type"] == "premium", until: date(&v["expiration"]) })
}

pub(super) async fn add(c: &Client, torrent: &[u8]) -> Result<String> {
    let req = c.put("/torrents/addTorrent").header(reqwest::header::CONTENT_TYPE, "application/x-bittorrent").body(torrent.to_vec());
    let (status, v) = c.send(req).await?;
    check(c, status, &v)?;
    id(&v["id"]).ok_or_else(|| c.error("no torrent ID in the reply"))
}

pub(super) async fn status(c: &Client, torrent: &str) -> Result<Remote> {
    let (status, v) = c.send(c.get(&format!("/torrents/info/{torrent}"))).await?;
    check(c, status, &v)?;
    let mut files: Vec<RemoteFile> = v["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(RemoteFile {
                id: id(&f["id"])?,
                path: text(&f["path"]),
                size: num(&f["bytes"]).unwrap_or(0),
                selected: num(&f["selected"]) == Some(1),
                link: None,
            })
        })
        .collect();
    let word = text(&v["status"]);
    let state = match word.as_str() {
        "waiting_files_selection" => RemoteState::NeedsSelection,
        "downloaded" => {
            // One link per selected file, in file order.
            let links: Vec<String> = v["links"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_owned)).collect();
            let mut selected: Vec<&mut RemoteFile> = files.iter_mut().filter(|f| f.selected).collect();
            selected.sort_by_key(|f| f.id.parse::<u64>().unwrap_or(u64::MAX));
            if links.len() == selected.len() {
                for (file, link) in selected.into_iter().zip(links) {
                    file.link = Some(link);
                }
                RemoteState::Ready
            } else {
                RemoteState::Failed(format!(
                    "Real-Debrid packed the {} files into {} archive link(s); pick fewer files at a time",
                    selected.len(),
                    links.len()
                ))
            }
        }
        "magnet_error" | "error" | "virus" | "dead" => RemoteState::Failed(format!("Real-Debrid reports “{word}”")),
        _ => RemoteState::Working {
            status: word.replace('_', " "),
            progress: num(&v["progress"]).map(|p| p as f32 / 100.0),
            speed: num(&v["speed"]),
            seeders: num(&v["seeders"]),
        },
    };
    Ok(Remote { state, files })
}

pub(super) async fn select(c: &Client, torrent: &str, file_ids: &[String]) -> Result<()> {
    let req = c.post(&format!("/torrents/selectFiles/{torrent}")).form(&[("files", file_ids.join(","))]);
    let (status, v) = c.send(req).await?;
    // 202: already done.
    check(c, status, &v)
}

pub(super) async fn link(c: &Client, file: &RemoteFile) -> Result<String> {
    let link = file.link.as_deref().ok_or_else(|| c.error(format!("no link for {}", file.path)))?;
    let (status, v) = c.send(c.post("/unrestrict/link").form(&[("link", link)])).await?;
    check(c, status, &v)?;
    v["download"].as_str().map(str::to_owned).ok_or_else(|| c.error("no download URL in the reply"))
}

pub(super) async fn delete(c: &Client, torrent: &str) -> Result<()> {
    let (status, v) = c.send(c.delete_req(&format!("/torrents/delete/{torrent}"))).await?;
    match check(c, status, &v) {
        Err(e) if e.downcast_ref::<Gone>().is_some() => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::super::{Client, Provider};
    use super::*;
    use crate::mock_http::{Resp, mock, reply};

    #[tokio::test]
    async fn adds_selects_and_unrestricts() {
        let selected = Arc::new(Mutex::new(String::new()));
        let seen = selected.clone();
        let base = mock(move |req| {
            assert_eq!(req.headers["authorization"], "Bearer key");
            match (req.method.as_str(), req.target.as_str()) {
                ("GET", "/user") => reply(200, json!({"username": "me", "type": "premium", "expiration": "2027-03-01T00:00:00.000Z"})),
                ("PUT", "/torrents/addTorrent") => {
                    assert_eq!(req.headers["content-type"], "application/x-bittorrent");
                    assert_eq!(req.body, b"d4:infod...ee");
                    reply(201, json!({"id": "ABC", "uri": "https://api.real-debrid.com/rest/1.0/torrents/info/ABC"}))
                }
                ("GET", "/torrents/info/ABC") => {
                    let picked = seen.lock().unwrap().clone();
                    let files = json!([
                        {"id": 1, "path": "/Show/Show - 01.mkv", "bytes": 100, "selected": i32::from(picked.contains('1'))},
                        {"id": 2, "path": "/Show/Show - 02.mkv", "bytes": 200, "selected": i32::from(picked.contains('2'))},
                        {"id": 3, "path": "/Show/Show - 03.mkv", "bytes": 300, "selected": i32::from(picked.contains('3'))},
                    ]);
                    if picked.is_empty() {
                        reply(200, json!({"id": "ABC", "status": "waiting_files_selection", "progress": 0, "files": files, "links": []}))
                    } else {
                        reply(200, json!({"id": "ABC", "status": "downloaded", "progress": 100, "files": files,
                            "links": ["https://real-debrid.com/d/one", "https://real-debrid.com/d/three"]}))
                    }
                }
                ("POST", "/torrents/selectFiles/ABC") => {
                    *seen.lock().unwrap() = req.form()["files"].clone();
                    Resp { status: 204, headers: vec![], body: Vec::new() }
                }
                ("POST", "/unrestrict/link") => {
                    let link = req.form()["link"].clone();
                    reply(200, json!({"download": format!("https://dl.example/{}", link.rsplit('/').next().unwrap()), "filesize": 1}))
                }
                ("DELETE", "/torrents/delete/ABC") => Resp { status: 204, headers: vec![], body: Vec::new() },
                ("DELETE", "/torrents/delete/GONE") => reply(404, json!({"error": "unknown_ressource", "error_code": 7})),
                other => panic!("unexpected {other:?}"),
            }
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::RealDebrid, "key", &base);
        assert_eq!(c.account().await.unwrap(), Account { name: "me".into(), premium: true, until: Some("2027-03-01".into()) });
        let id = c.add(b"d4:infod...ee", "Show").await.unwrap();
        assert_eq!(id, "ABC");
        assert_eq!(c.status(&id).await.unwrap().state, RemoteState::NeedsSelection);
        c.select(&id, &["1".into(), "3".into()]).await.unwrap();
        assert_eq!(*selected.lock().unwrap(), "1,3");
        let remote = c.status(&id).await.unwrap();
        assert_eq!(remote.state, RemoteState::Ready);
        let third = &remote.files[2];
        assert_eq!((third.path.as_str(), third.link.as_deref()), ("/Show/Show - 03.mkv", Some("https://real-debrid.com/d/three")));
        assert_eq!(remote.files[1].link, None, "not selected");
        assert_eq!(c.link(&id, third).await.unwrap(), "https://dl.example/three");
        c.delete(&id).await.unwrap();
        c.delete("GONE").await.unwrap();
    }

    #[tokio::test]
    async fn reports_bad_keys_and_missing_torrents() {
        let base = mock(|req| match req.target.as_str() {
            "/user" => reply(401, json!({"error": "bad_token", "error_code": 8})),
            _ => reply(404, json!({"error": "unknown_ressource", "error_code": 7})),
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::RealDebrid, "nope", &base);
        assert!(c.account().await.unwrap_err().downcast_ref::<super::super::BadKey>().is_some());
        assert!(c.status("X").await.unwrap_err().downcast_ref::<Gone>().is_some());
    }
}
