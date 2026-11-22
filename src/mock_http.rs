//! A tiny HTTP/1.1 server standing in for web APIs in tests.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The web API base URLs (`ANILIST_BASE`, `SEADEX_BASE`, `KITSU_BASE`,
/// `DOWNLOAD_BASE`) are global: a test pointing them at a mock holds this
/// exclusively ([`mocked`]), and one using the real sites shares it ([`real`]).
static NET: std::sync::RwLock<()> = std::sync::RwLock::new(());

pub fn mocked() -> std::sync::RwLockWriteGuard<'static, ()> {
    NET.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn real() -> std::sync::RwLockReadGuard<'static, ()> {
    NET.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A request as the mock server saw it.
#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    pub target: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn form(&self) -> HashMap<String, String> {
        url::form_urlencoded::parse(&self.body)
            .into_owned()
            .collect()
    }
    pub fn query(&self) -> HashMap<String, String> {
        url::Url::parse(&format!("http://x{}", self.target))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    /// Raw bytes (a file download).
    pub fn binary(status: u16, body: Vec<u8>) -> Self {
        Resp {
            status,
            headers: vec![("Content-Type".into(), "application/octet-stream".into())],
            body,
        }
    }
}

pub fn reply(status: u16, body: serde_json::Value) -> Resp {
    Resp {
        status,
        headers: vec![],
        body: body.to_string().into_bytes(),
    }
}

/// A tiny HTTP/1.1 server standing in for Google: one request per
/// connection, answered by `handler`.
pub async fn mock(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 65536];
                let head_end = loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
                let mut lines = head.lines();
                let mut first = lines.next().unwrap().split_whitespace();
                let (method, target) = (
                    first.next().unwrap().to_owned(),
                    first.next().unwrap().to_owned(),
                );
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| l.split_once(": "))
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.to_owned()))
                    .collect();
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while data.len() < head_end + len {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                }
                let req = Req {
                    method,
                    target,
                    headers,
                    body: data[head_end..].to_vec(),
                };
                let resp = handler(&req);
                let mut out = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    resp.status,
                    resp.body.len()
                );
                if resp.status < 300
                    && !resp.body.is_empty()
                    && !resp
                        .headers
                        .iter()
                        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    out.push_str("Content-Type: application/json\r\n");
                }
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let mut bytes = out.into_bytes();
                bytes.extend_from_slice(&resp.body);
                let _ = stream.write_all(&bytes).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    base
}
