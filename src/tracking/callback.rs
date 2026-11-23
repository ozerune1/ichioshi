//! The one-off web server the browser comes back to after signing in.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
}

pub async fn listen(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| {
            format!(
                "couldn't listen on port {port} for the sign-in (is another sign-in still open?)"
            )
        })
}

/// The next request; gives up after ten minutes, or when cancelled.
pub async fn next(
    listener: &TcpListener,
    cancel: &AtomicBool,
    started: Instant,
) -> Result<(TcpStream, Request)> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        if started.elapsed() > Duration::from_secs(600) {
            bail!("timed out waiting for the sign-in in the browser");
        }
        let Ok(accepted) =
            tokio::time::timeout(Duration::from_millis(250), listener.accept()).await
        else {
            continue;
        };
        let (mut stream, _) = accepted?;
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        let mut header_end = None;
        while header_end.is_none() && data.len() < 32 << 10 {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => data.extend_from_slice(&buf[..n]),
                _ => break,
            }
            header_end = data
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| i + 4);
        }
        let Some(header_end) = header_end else {
            continue;
        };
        let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
        let mut first = head.lines().next().unwrap_or("").split_whitespace();
        let method = first.next().unwrap_or("").to_owned();
        let target = first.next().unwrap_or("/").to_owned();
        let url = url::Url::parse(&format!("http://localhost{target}")).ok();
        let request = Request {
            method,
            path: url.as_ref().map_or("/".into(), |u| u.path().to_owned()),
            query: url
                .map(|u| u.query_pairs().into_owned().collect())
                .unwrap_or_default(),
        };
        return Ok((stream, request));
    }
}

/// A page saying `message` (or, with `script`, a page running it).
pub async fn respond(mut stream: TcpStream, status: &str, message: &str, script: Option<&str>) {
    let script = script
        .map(|s| format!("<script>{s}</script>"))
        .unwrap_or_default();
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Ichioshi</title>\
         <body style=\"font:16px sans-serif;margin:3em\"><p id=m>{message}</p>{script}"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

pub const DONE: &str = "Signed in to Ichioshi. You can close this tab and go back to the app.";
