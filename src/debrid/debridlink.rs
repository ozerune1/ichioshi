//! Debrid-Link: https://debrid-link.com/api_doc/v2/introduction
//!
//! Torrents go in the "seedbox". Each file has a `downloadUrl` that works
//! once its `downloadPercent` reaches 100.

use anyhow::Result;
use reqwest::StatusCode;
use serde_json::Value;

use super::{Account, Client, Gone, Remote, RemoteFile, RemoteState, id, num, text, torrent_part};

fn value(c: &Client, status: StatusCode, body: Value) -> Result<Value> {
    if body["success"] == true {
        return Ok(body["value"].clone());
    }
    match body["error"].as_str().unwrap_or_default() {
        "badToken" => Err(c.bad_key()),
        "badId" => Err(anyhow::Error::new(Gone)),
        "" => Err(c.error(format!("HTTP {status}"))),
        code => Err(c.error(match code {
            "floodDetected" => "too many requests; try again in about an hour".to_owned(),
            "maxTorrent" => "the daily limit of new torrents is reached".to_owned(),
            "maxTransfer" => "too many torrents downloading at once".to_owned(),
            "torrentTooBig" => "the torrent is too big or has too many files".to_owned(),
            "unverifiedEmail" => "verify your email address (or go premium) first".to_owned(),
            other => other.to_owned(),
        })),
    }
}

pub(super) async fn account(c: &Client) -> Result<Account> {
    let (status, v) = c.send(c.get("/account/infos")).await?;
    let v = value(c, status, v)?;
    let kind = num(&v["accountType"]).unwrap_or(0);
    let until = match kind {
        1 => num(&v["premiumLeft"])
            .and_then(|left| chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(left as i64)))
            .map(|d| d.format("%Y-%m-%d").to_string()),
        2 => Some("lifetime".to_owned()),
        _ => None,
    };
    Ok(Account { name: text(&v["username"]), premium: kind > 0, until })
}

pub(super) async fn add(c: &Client, torrent: &[u8], name: &str) -> Result<String> {
    let form = reqwest::multipart::Form::new().part("file", torrent_part(torrent, name));
    let (status, v) = c.send(c.post("/seedbox/add").multipart(form)).await?;
    let t = value(c, status, v)?;
    id(&t["id"]).ok_or_else(|| c.error("no torrent ID in the reply"))
}

pub(super) async fn status(c: &Client, torrent: &str) -> Result<Remote> {
    let (status, v) = c.send(c.get("/seedbox/list").query(&[("ids", torrent)])).await?;
    let t = value(c, status, v)?.as_array().and_then(|l| l.first().cloned()).ok_or_else(|| anyhow::Error::new(Gone))?;
    let files: Vec<RemoteFile> = t["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(RemoteFile {
                id: id(&f["id"])?,
                path: text(&f["name"]),
                size: num(&f["size"]).unwrap_or(0),
                // Only ready files can be downloaded.
                selected: num(&f["downloadPercent"]) == Some(100),
                link: f["downloadUrl"].as_str().filter(|u| !u.is_empty()).map(str::to_owned),
            })
        })
        .collect();
    let percent = num(&t["downloadPercent"]).unwrap_or(0);
    let state = if percent >= 100 && !files.is_empty() && files.iter().all(|f| f.selected) {
        RemoteState::Ready
    } else {
        let word = match num(&t["status"]) {
            Some(0) => "paused",
            Some(1) => "queued",
            Some(2) => "verifying",
            Some(4) => "downloading",
            Some(8 | 100) => "finishing",
            _ => "working",
        };
        RemoteState::Working {
            status: word.to_owned(),
            progress: Some(percent.min(100) as f32 / 100.0),
            speed: num(&t["downloadSpeed"]),
            seeders: num(&t["peersConnected"]),
        }
    };
    Ok(Remote { state, files })
}

pub(super) async fn delete(c: &Client, torrent: &str) -> Result<()> {
    let (status, v) = c.send(c.delete_req(&format!("/seedbox/{torrent}/remove"))).await?;
    match value(c, status, v) {
        Ok(_) => Ok(()),
        Err(e) if e.downcast_ref::<Gone>().is_some() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{BadKey, Client, Provider};
    use super::*;
    use crate::mock_http::{mock, reply};

    fn ok(value: Value) -> crate::mock_http::Resp {
        reply(200, json!({"success": true, "value": value}))
    }

    #[tokio::test]
    async fn adds_to_the_seedbox_and_uses_its_links() {
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let base = mock(move |req| {
            assert_eq!(req.headers["authorization"], "Bearer key");
            match (req.method.as_str(), req.target.split('?').next().unwrap()) {
                ("GET", "/account/infos") => ok(json!({"username": "me", "accountType": 2, "premiumLeft": 0})),
                ("POST", "/seedbox/add") => {
                    assert!(String::from_utf8_lossy(&req.body).contains("name=\"file\"; filename=\"Show.torrent\""));
                    ok(json!({"id": "2115ca3cf4356d24510", "name": "Show", "files": []}))
                }
                ("GET", "/seedbox/list") => {
                    assert_eq!(req.query()["ids"], "2115ca3cf4356d24510");
                    let half = polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                    let pct = if half { 50 } else { 100 };
                    ok(json!([{"id": "2115ca3cf4356d24510", "name": "Show", "status": if half { 4 } else { 100 },
                        "downloadPercent": pct, "downloadSpeed": 700, "peersConnected": 3, "isZip": false,
                        "files": [
                            {"id": "2115ca3cf4356d24510-0", "name": "Show - 01.mkv", "size": 100, "downloadUrl": "https://seed20.debrid.link/dl/a", "downloadPercent": pct},
                            {"id": "2115ca3cf4356d24510-1", "name": "Show - 02.mkv", "size": 200, "downloadUrl": "https://seed20.debrid.link/dl/b", "downloadPercent": pct}
                        ]}]))
                }
                ("DELETE", "/seedbox/2115ca3cf4356d24510/remove") => ok(json!(["2115ca3cf4356d24510"])),
                other => panic!("unexpected {other:?}"),
            }
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::DebridLink, "key", &base);
        assert_eq!(c.account().await.unwrap(), Account { name: "me".into(), premium: true, until: Some("lifetime".into()) });
        let id = c.add(b"t", "Show").await.unwrap();
        match c.status(&id).await.unwrap().state {
            RemoteState::Working { status, progress, speed, seeders } => {
                assert_eq!((status.as_str(), progress, speed, seeders), ("downloading", Some(0.5), Some(700), Some(3)));
            }
            other => panic!("{other:?}"),
        }
        let remote = c.status(&id).await.unwrap();
        assert_eq!(remote.state, RemoteState::Ready);
        assert_eq!(c.link(&id, &remote.files[1]).await.unwrap(), "https://seed20.debrid.link/dl/b");
        c.delete(&id).await.unwrap();
    }

    #[tokio::test]
    async fn maps_errors() {
        let base = mock(|req| match req.target.split('?').next().unwrap() {
            "/account/infos" => reply(401, json!({"success": false, "error": "badToken"})),
            "/seedbox/list" => ok(json!([])),
            _ => reply(400, json!({"success": false, "error": "maxTorrent"})),
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::DebridLink, "k", &base);
        assert!(c.account().await.unwrap_err().downcast_ref::<BadKey>().is_some());
        assert!(c.status("x").await.unwrap_err().downcast_ref::<Gone>().is_some());
        assert_eq!(c.add(b"t", "x").await.unwrap_err().to_string(), "Debrid-Link: the daily limit of new torrents is reached");
    }
}
