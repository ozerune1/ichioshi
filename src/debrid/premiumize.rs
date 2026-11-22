//! Premiumize: https://www.premiumize.me/api
//!
//! A torrent becomes a "transfer". When it finishes, its files are in the
//! cloud: one file (`file_id`) or a folder (`folder_id`), whose listing has
//! a download link for each file. Errors come back with HTTP 200 and
//! `"status": "error"`.

use anyhow::Result;
use serde_json::Value;

use super::{Account, Client, Gone, Remote, RemoteFile, RemoteState, date, float, id, num, text, torrent_part};

fn check(c: &Client, body: Value) -> Result<Value> {
    if body["status"] == "success" {
        return Ok(body);
    }
    match body["code"].as_str().unwrap_or_default() {
        "authentication_failed" => Err(c.bad_key()),
        "not_found" => Err(anyhow::Error::new(Gone)),
        _ => Err(c.error(body["message"].as_str().unwrap_or("unexpected reply"))),
    }
}

pub(super) async fn account(c: &Client) -> Result<Account> {
    let v = check(c, c.send(c.get("/account/info")).await?.1)?;
    let until = date(&v["premium_until"]);
    Ok(Account { name: format!("customer {}", text(&v["customer_id"])), premium: until.is_some(), until })
}

pub(super) async fn add(c: &Client, torrent: &[u8], name: &str) -> Result<String> {
    let form = reqwest::multipart::Form::new().part("src", torrent_part(torrent, name));
    let v = check(c, c.send(c.post("/transfer/create").multipart(form)).await?.1)?;
    id(&v["id"]).ok_or_else(|| c.error("no transfer ID in the reply"))
}

/// The transfer's entry in the transfer list.
async fn transfer(c: &Client, transfer: &str) -> Result<Value> {
    let v = check(c, c.send(c.get("/transfer/list")).await?.1)?;
    v["transfers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| id(&t["id"]).as_deref() == Some(transfer))
        .cloned()
        .ok_or_else(|| anyhow::Error::new(Gone))
}

pub(super) async fn status(c: &Client, id_: &str) -> Result<Remote> {
    let t = transfer(c, id_).await?;
    let word = text(&t["status"]);
    match word.as_str() {
        // "seeding" means it's done and in the cloud already.
        "finished" | "seeding" => {
            let files = if let Some(file) = id(&t["file_id"]) {
                vec![item(c, &file).await?]
            } else if let Some(folder) = id(&t["folder_id"]) {
                let mut out = Vec::new();
                list_folder(c, &folder, "", &mut out, 0).await?;
                out
            } else {
                return Ok(Remote {
                    state: RemoteState::Failed("Premiumize put the files outside your cloud (external cloud set up?)".into()),
                    files: Vec::new(),
                });
            };
            Ok(Remote { state: RemoteState::Ready, files })
        }
        "error" => {
            let message = text(&t["message"]);
            Ok(Remote {
                state: RemoteState::Failed(if message.is_empty() { "Premiumize: transfer failed".into() } else { format!("Premiumize: {message}") }),
                files: Vec::new(),
            })
        }
        _ => {
            let message = text(&t["message"]);
            Ok(Remote {
                state: RemoteState::Working {
                    status: if message.is_empty() { word } else { message },
                    progress: float(&t["progress"]).map(|p| p as f32),
                    speed: None,
                    seeders: None,
                },
                files: Vec::new(),
            })
        }
    }
}

async fn item(c: &Client, file: &str) -> Result<RemoteFile> {
    let v = check(c, c.send(c.get("/item/details").query(&[("id", file)])).await?.1)?;
    Ok(RemoteFile {
        id: file.to_owned(),
        path: text(&v["name"]),
        size: num(&v["size"]).unwrap_or(0),
        selected: true,
        link: v["link"].as_str().map(str::to_owned),
    })
}

/// Every file under a folder (listings aren't recursive).
async fn list_folder(c: &Client, folder: &str, prefix: &str, out: &mut Vec<RemoteFile>, depth: usize) -> Result<()> {
    if depth > 20 {
        return Ok(());
    }
    let v = check(c, c.send(c.get("/folder/list").query(&[("id", folder)])).await?.1)?;
    for entry in v["content"].as_array().into_iter().flatten() {
        let name = text(&entry["name"]);
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let Some(entry_id) = id(&entry["id"]) else { continue };
        if entry["type"] == "folder" {
            Box::pin(list_folder(c, &entry_id, &path, out, depth + 1)).await?;
        } else {
            out.push(RemoteFile {
                id: entry_id,
                path,
                size: num(&entry["size"]).unwrap_or(0),
                selected: true,
                link: entry["link"].as_str().map(str::to_owned),
            });
        }
    }
    Ok(())
}

/// Delete the transfer and the files it put in the cloud.
pub(super) async fn delete(c: &Client, id_: &str) -> Result<()> {
    let t = match transfer(c, id_).await {
        Ok(t) => t,
        Err(e) if e.downcast_ref::<Gone>().is_some() => return Ok(()),
        Err(e) => return Err(e),
    };
    // A single file's folder_id is the folder it landed in, not its own.
    if let Some(file) = id(&t["file_id"]) {
        check(c, c.send(c.post("/item/delete").form(&[("id", file)])).await?.1)?;
    } else if let Some(folder) = id(&t["folder_id"]) {
        check(c, c.send(c.post("/folder/delete").form(&[("id", folder)])).await?.1)?;
    }
    check(c, c.send(c.post("/transfer/delete").form(&[("id", id_)])).await?.1)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::super::{BadKey, Client, Provider};
    use super::*;
    use crate::mock_http::{mock, reply};

    #[tokio::test]
    async fn transfers_and_lists_the_cloud_folder() {
        let deleted = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = deleted.clone();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = finished.clone();
        let base = mock(move |req| {
            assert_eq!(req.headers["authorization"], "Bearer key");
            let path = req.target.split('?').next().unwrap();
            match (req.method.as_str(), path) {
                ("GET", "/account/info") => reply(200, json!({"status": "success", "customer_id": "123", "premium_until": 1_800_000_000, "limit_used": 0.1})),
                ("POST", "/transfer/create") => {
                    assert!(String::from_utf8_lossy(&req.body).contains("name=\"src\"; filename=\"Show.torrent\""));
                    reply(200, json!({"status": "success", "id": "t1", "name": "Show"}))
                }
                ("GET", "/transfer/list") => {
                    let t = if done.load(std::sync::atomic::Ordering::SeqCst) {
                        json!({"id": "t1", "name": "Show", "status": "finished", "progress": 1.0, "message": "", "folder_id": "f1", "file_id": null})
                    } else {
                        done.store(true, std::sync::atomic::Ordering::SeqCst);
                        json!({"id": "t1", "name": "Show", "status": "running", "progress": 0.25, "message": "Downloading 1.2 MB/s", "folder_id": null, "file_id": null})
                    };
                    reply(200, json!({"status": "success", "transfers": [{"id": "other", "status": "finished"}, t]}))
                }
                ("GET", "/folder/list") => match req.query()["id"].as_str() {
                    "f1" => reply(200, json!({"status": "success", "content": [
                        {"id": "a", "name": "Show - 01.mkv", "type": "file", "size": 100, "link": "https://dl.example/a"},
                        {"id": "f2", "name": "Extras", "type": "folder"}
                    ]})),
                    "f2" => reply(200, json!({"status": "success", "content": [
                        {"id": "b", "name": "NCOP.mkv", "type": "file", "size": "200", "link": "https://dl.example/b"}
                    ]})),
                    other => panic!("folder {other}"),
                },
                ("POST", "/folder/delete") | ("POST", "/transfer/delete") => {
                    log.lock().unwrap().push(format!("{path} {}", req.form()["id"]));
                    reply(200, json!({"status": "success"}))
                }
                other => panic!("unexpected {other:?}"),
            }
        })
        .await;
        let c = Client::with_base(reqwest::Client::new(), Provider::Premiumize, "key", &base);
        assert_eq!(c.account().await.unwrap(), Account { name: "customer 123".into(), premium: true, until: Some("2027-01-15".into()) });
        let id = c.add(b"t", "Show").await.unwrap();
        match c.status(&id).await.unwrap().state {
            RemoteState::Working { status, progress, .. } => assert_eq!((status.as_str(), progress), ("Downloading 1.2 MB/s", Some(0.25))),
            other => panic!("{other:?}"),
        }
        let remote = c.status(&id).await.unwrap();
        assert_eq!(remote.state, RemoteState::Ready);
        let files: Vec<(&str, u64, Option<&str>)> = remote.files.iter().map(|f| (f.path.as_str(), f.size, f.link.as_deref())).collect();
        assert_eq!(files, [("Show - 01.mkv", 100, Some("https://dl.example/a")), ("Extras/NCOP.mkv", 200, Some("https://dl.example/b"))]);
        assert_eq!(c.link(&id, &remote.files[1]).await.unwrap(), "https://dl.example/b");
        c.delete(&id).await.unwrap();
        assert_eq!(*deleted.lock().unwrap(), ["/folder/delete f1", "/transfer/delete t1"]);
        assert!(c.status("missing").await.unwrap_err().downcast_ref::<Gone>().is_some());
    }

    #[tokio::test]
    async fn errors_come_with_http_200() {
        let base = mock(|_| reply(200, json!({"status": "error", "message": "API key invalid", "code": "authentication_failed"}))).await;
        let c = Client::with_base(reqwest::Client::new(), Provider::Premiumize, "k", &base);
        assert!(c.account().await.unwrap_err().downcast_ref::<BadKey>().is_some());
    }
}
