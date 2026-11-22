//! AllDebrid: https://docs.alldebrid.com/
//!
//! Torrents are "magnets". When one is ready, `magnet/files` lists its
//! files as a tree, each with a link that `link/unlock` turns into a
//! download URL.

use std::time::Duration;

use anyhow::Result;
use reqwest::StatusCode;
use serde_json::Value;

use super::{
    Account, Client, Gone, Remote, RemoteFile, RemoteState, date, id, num, text, torrent_part,
};

/// The `data` of a reply, or the error it carries.
fn data(c: &Client, status: StatusCode, body: Value) -> Result<Value> {
    if body["status"] == "success" {
        return Ok(body["data"].clone());
    }
    let code = body["error"]["code"].as_str().unwrap_or_default();
    if code.starts_with("AUTH_") {
        return Err(c.bad_key());
    }
    if code == "MAGNET_INVALID_ID" {
        return Err(anyhow::Error::new(Gone));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(c.error("too many requests; slowing down"));
    }
    match body["error"]["message"].as_str() {
        Some(message) => Err(c.error(message)),
        None => Err(c.error(format!("HTTP {status}"))),
    }
}

pub(super) async fn account(c: &Client) -> Result<Account> {
    let (status, v) = c.send(c.get("/v4/user")).await?;
    let user = data(c, status, v)?["user"].clone();
    Ok(Account {
        name: text(&user["username"]),
        premium: user["isPremium"] == true,
        until: date(&user["premiumUntil"]),
    })
}

pub(super) async fn add(c: &Client, torrent: &[u8], name: &str) -> Result<String> {
    let form = reqwest::multipart::Form::new().part("files[]", torrent_part(torrent, name));
    let (status, v) = c
        .send(c.post("/v4/magnet/upload/file").multipart(form))
        .await?;
    let file = data(c, status, v)?["files"][0].clone();
    if let Some(message) = file["error"]["message"].as_str() {
        return Err(c.error(message));
    }
    id(&file["id"]).ok_or_else(|| c.error("no magnet ID in the reply"))
}

pub(super) async fn status(c: &Client, magnet: &str) -> Result<Remote> {
    let (status, v) = c
        .send(c.post("/v4.1/magnet/status").form(&[("id", magnet)]))
        .await?;
    let magnets = data(c, status, v)?["magnets"].clone();
    // One magnet comes back as an object; be ready for a list too.
    let m = match magnets {
        Value::Array(list) => list
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::Error::new(Gone))?,
        other => other,
    };
    let code = num(&m["statusCode"]).unwrap_or(0);
    let state = match code {
        4 => RemoteState::Ready,
        0..=3 => {
            let size = num(&m["size"]).unwrap_or(0);
            RemoteState::Working {
                status: text(&m["status"]).to_lowercase(),
                progress: (size > 0)
                    .then(|| num(&m["downloaded"]).unwrap_or(0) as f32 / size as f32),
                speed: num(&m["downloadSpeed"]),
                seeders: num(&m["seeders"]),
            }
        }
        _ => RemoteState::Failed(format!("AllDebrid: {}", text(&m["status"]))),
    };
    let files = if state == RemoteState::Ready {
        files(c, magnet).await?
    } else {
        Vec::new()
    };
    Ok(Remote { state, files })
}

/// A ready magnet's files, flattened from AllDebrid's tree.
async fn files(c: &Client, magnet: &str) -> Result<Vec<RemoteFile>> {
    // The docs list this under v4 but show it under v4.1; try both.
    let mut reply = c
        .send(c.post("/v4/magnet/files").form(&[("id[]", magnet)]))
        .await?;
    if reply.0 == StatusCode::NOT_FOUND {
        reply = c
            .send(c.post("/v4.1/magnet/files").form(&[("id[]", magnet)]))
            .await?;
    }
    let entry = data(c, reply.0, reply.1)?["magnets"][0].clone();
    if let Some(message) = entry["error"]["message"].as_str() {
        return Err(c.error(message));
    }
    let mut out = Vec::new();
    flatten(&entry["files"], "", &mut out);
    Ok(out)
}

fn flatten(nodes: &Value, prefix: &str, out: &mut Vec<RemoteFile>) {
    for node in nodes.as_array().into_iter().flatten() {
        let path = if prefix.is_empty() {
            text(&node["n"])
        } else {
            format!("{prefix}/{}", text(&node["n"]))
        };
        if node["e"].is_array() {
            flatten(&node["e"], &path, out);
        } else {
            out.push(RemoteFile {
                id: path.clone(),
                path,
                size: num(&node["s"]).unwrap_or(0),
                selected: true,
                link: node["l"].as_str().map(str::to_owned),
            });
        }
    }
}

pub(super) async fn link(c: &Client, file: &RemoteFile) -> Result<String> {
    let link = file
        .link
        .as_deref()
        .ok_or_else(|| c.error(format!("no link for {}", file.path)))?;
    let (status, v) = c
        .send(c.post("/v4/link/unlock").form(&[("link", link)]))
        .await?;
    let unlocked = data(c, status, v)?;
    if let Some(url) = unlocked["link"].as_str().filter(|l| !l.is_empty()) {
        return Ok(url.to_owned());
    }
    // Some links take a moment to generate: poll every 5 seconds.
    let delayed =
        id(&unlocked["delayed"]).ok_or_else(|| c.error("no download link in the reply"))?;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (status, v) = c
            .send(c.post("/v4/link/delayed").form(&[("id", delayed.as_str())]))
            .await?;
        let d = data(c, status, v)?;
        match num(&d["status"]) {
            Some(2) => {
                return d["link"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| c.error("no download link in the reply"));
            }
            Some(3) => return Err(c.error("couldn't generate the download link")),
            _ => {}
        }
    }
    Err(c.error("timed out waiting for the download link"))
}

pub(super) async fn delete(c: &Client, magnet: &str) -> Result<()> {
    let (status, v) = c
        .send(c.post("/v4/magnet/delete").form(&[("id", magnet)]))
        .await?;
    match data(c, status, v) {
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

    fn ok(data: Value) -> crate::mock_http::Resp {
        reply(200, json!({"status": "success", "data": data}))
    }

    #[tokio::test]
    async fn uploads_waits_and_unlocks() {
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let base = mock(move |req| {
            assert_eq!(req.headers["authorization"], "Bearer key");
            match (req.method.as_str(), req.target.as_str()) {
                ("GET", "/v4/user") => ok(json!({"user": {"username": "me", "isPremium": true, "premiumUntil": 1_800_000_000}})),
                ("POST", "/v4/magnet/upload/file") => {
                    let body = String::from_utf8_lossy(&req.body);
                    assert!(body.contains("name=\"files[]\"; filename=\"Show.torrent\""), "{body}");
                    assert!(body.contains("application/x-bittorrent"));
                    ok(json!({"files": [{"file": "Show.torrent", "name": "Show", "hash": "abc", "id": 77, "size": 300, "ready": false}]}))
                }
                ("POST", "/v4.1/magnet/status") => {
                    assert_eq!(req.form()["id"], "77");
                    if polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        ok(json!({"magnets": {"id": 77, "filename": "Show", "size": 300, "status": "Downloading", "statusCode": 1,
                            "downloaded": 150, "downloadSpeed": 1000, "seeders": 4}}))
                    } else {
                        ok(json!({"magnets": {"id": 77, "filename": "Show", "size": 300, "status": "Ready", "statusCode": 4}}))
                    }
                }
                // The documented v4 path is missing here: the client tries v4.1.
                ("POST", "/v4/magnet/files") => reply(404, json!({})),
                ("POST", "/v4.1/magnet/files") => {
                    assert_eq!(req.form()["id[]"], "77");
                    ok(json!({"magnets": [{"id": "77", "files": [
                        {"n": "Show", "e": [
                            {"n": "Show - 01.mkv", "s": 100, "l": "https://alldebrid.com/f/one"},
                            {"n": "Extras", "e": [{"n": "NCOP.mkv", "s": 200, "l": "https://alldebrid.com/f/two"}]}
                        ]}
                    ]}]}))
                }
                ("POST", "/v4/link/unlock") if req.form()["link"].ends_with("one") => ok(json!({"link": "https://dl.example/one", "filesize": 100})),
                ("POST", "/v4/link/unlock") => ok(json!({"link": "", "delayed": 9})),
                ("POST", "/v4/link/delayed") => ok(json!({"status": 2, "time_left": 0, "link": "https://dl.example/two"})),
                ("POST", "/v4/magnet/delete") => ok(json!({"message": "Magnet was successfully deleted"})),
                other => panic!("unexpected {other:?}"),
            }
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::AllDebrid, "key", &base);
        assert_eq!(
            c.account().await.unwrap(),
            Account {
                name: "me".into(),
                premium: true,
                until: Some("2027-01-15".into())
            }
        );
        let id = c.add(b"torrent", "Show").await.unwrap();
        assert_eq!(id, "77");
        match c.status(&id).await.unwrap().state {
            RemoteState::Working {
                progress,
                speed,
                seeders,
                ..
            } => {
                assert_eq!((progress, speed, seeders), (Some(0.5), Some(1000), Some(4)));
            }
            other => panic!("{other:?}"),
        }
        let remote = c.status(&id).await.unwrap();
        assert_eq!(remote.state, RemoteState::Ready);
        let paths: Vec<&str> = remote.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Show/Show - 01.mkv", "Show/Extras/NCOP.mkv"]);
        assert_eq!(
            c.link(&id, &remote.files[0]).await.unwrap(),
            "https://dl.example/one"
        );
        tokio::time::pause(); // the delayed link polls every 5 s
        assert_eq!(
            c.link(&id, &remote.files[1]).await.unwrap(),
            "https://dl.example/two"
        );
        c.delete(&id).await.unwrap();
    }

    #[tokio::test]
    async fn maps_errors() {
        let base = mock(|req| match req.target.as_str() {
            "/v4/user" => reply(401, json!({"status": "error", "error": {"code": "AUTH_BAD_APIKEY", "message": "The auth apikey is invalid"}})),
            "/v4.1/magnet/status" => reply(200, json!({"status": "error", "error": {"code": "MAGNET_INVALID_ID", "message": "Invalid id"}})),
            _ => reply(200, json!({"status": "error", "error": {"code": "MAGNET_TOO_MANY_ACTIVE", "message": "Already have maximum allowed active magnets (30)"}})),
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::AllDebrid, "k", &base);
        assert!(
            c.account()
                .await
                .unwrap_err()
                .downcast_ref::<BadKey>()
                .is_some()
        );
        assert!(
            c.status("1")
                .await
                .unwrap_err()
                .downcast_ref::<Gone>()
                .is_some()
        );
        let err = c.add(b"t", "x").await.unwrap_err().to_string();
        assert_eq!(
            err,
            "AllDebrid: Already have maximum allowed active magnets (30)"
        );
    }
}
