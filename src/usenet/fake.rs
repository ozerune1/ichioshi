//! A fake Usenet provider for tests: a real NNTP server on localhost (plain
//! or TLS, with or without a login) serving articles made from real files,
//! yEnc-encoded the way posters do it, with a PAR2 set posted alongside and
//! an NZB listing it all.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::Provider;
use super::nzb::{NzbFile, Segment};

/// An upload: articles for a provider to serve, and the NZB that lists them.
pub struct Upload {
    pub files: Vec<NzbFile>,
    /// Message ID → yEnc body.
    pub articles: HashMap<String, Vec<u8>>,
}

impl Upload {
    pub fn nzb(&self) -> String {
        super::nzb::write(&self.files)
    }

    /// The message IDs of a posted file's articles.
    pub fn ids(&self, name: &str) -> Vec<String> {
        self.files
            .iter()
            .find(|f| f.name == name)
            .unwrap()
            .segments
            .iter()
            .map(|s| s.message_id.clone())
            .collect()
    }

    /// Every recovery volume's articles.
    pub fn volume_ids(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| f.recovery_blocks().is_some())
            .flat_map(|f| f.segments.iter().map(|s| s.message_id.clone()))
            .collect()
    }
}

/// Post `files` in articles of `part` bytes, with a PAR2 set of `block`-byte
/// blocks and `recovery` recovery blocks. `tag` keeps message IDs unique.
pub fn upload(
    tag: &str,
    files: &[(&str, &[u8])],
    part: usize,
    block: u64,
    recovery: u32,
) -> Upload {
    post(tag, files, part, block, recovery, false)
}

/// Like [`upload`], with a PAR2 set for each file (as AnimeTosho does).
pub fn upload_per_file(
    tag: &str,
    files: &[(&str, &[u8])],
    part: usize,
    block: u64,
    recovery: u32,
) -> Upload {
    post(tag, files, part, block, recovery, true)
}

fn post(
    tag: &str,
    files: &[(&str, &[u8])],
    part: usize,
    block: u64,
    recovery: u32,
    per_file: bool,
) -> Upload {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<PathBuf> = files
        .iter()
        .map(|(name, data)| {
            let p = dir.path().join(name);
            std::fs::write(&p, data).unwrap();
            p
        })
        .collect();
    let stem = |name: &str| {
        name.rsplit_once('.')
            .map_or(name.to_owned(), |(s, _)| s.to_owned())
    };
    let par2 = if per_file {
        let made: Vec<PathBuf> = files
            .iter()
            .zip(&paths)
            .flat_map(|((name, _), p)| {
                super::repair::create(
                    dir.path(),
                    &stem(name),
                    std::slice::from_ref(p),
                    block,
                    recovery,
                )
            })
            .collect();
        let mut made = made;
        made.sort();
        made.dedup();
        made
    } else {
        super::repair::create(dir.path(), &stem(files[0].0), &paths, block, recovery)
    };
    let mut all: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, d)| ((*n).to_owned(), d.to_vec()))
        .collect();
    for p in par2 {
        all.push((
            p.file_name().unwrap().to_string_lossy().into_owned(),
            std::fs::read(&p).unwrap(),
        ));
    }

    let mut posted = Vec::new();
    let mut articles = HashMap::new();
    for (f, (name, data)) in all.iter().enumerate() {
        let parts: Vec<&[u8]> = data.chunks(part).collect();
        let total = parts.len() as u32;
        let mut segments = Vec::new();
        for (i, chunk) in parts.iter().enumerate() {
            let number = i as u32 + 1;
            let (body, _crc) = yenc_simd::encode_article(
                chunk,
                name,
                number,
                total,
                (i * part) as u64,
                data.len() as u64,
            );
            let message_id = format!("{tag}.{f}.{number}@fake.example");
            segments.push(Segment {
                number,
                bytes: body.len() as u32,
                message_id: message_id.clone(),
            });
            articles.insert(message_id, body);
        }
        posted.push(NzbFile {
            name: name.clone(),
            segments,
        });
    }
    Upload {
        files: posted,
        articles,
    }
}

#[derive(Clone, Default)]
pub struct Options {
    /// Require this login.
    pub login: Option<(String, String)>,
    pub tls: bool,
    /// Wait this long before each article (to watch a download in progress).
    pub delay: Duration,
}

pub struct FakeProvider {
    pub port: u16,
    articles: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// BODY requests, by message ID.
    pub requests: Arc<Mutex<Vec<String>>>,
    open: Arc<AtomicUsize>,
    /// The most connections open at once.
    pub peak: Arc<AtomicUsize>,
    options: Options,
    fingerprint: Option<String>,
}

impl FakeProvider {
    pub async fn start(articles: HashMap<String, Vec<u8>>, options: Options) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (acceptor, fingerprint) = if options.tls {
            let cert = rcgen::generate_simple_self_signed(vec![
                "127.0.0.1".to_owned(),
                "localhost".to_owned(),
            ])
            .unwrap();
            let der = cert.cert.der().clone();
            let fingerprint = ring::digest::digest(&ring::digest::SHA256, &der)
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let key = tokio_rustls::rustls::pki_types::PrivateKeyDer::Pkcs8(
                cert.signing_key.serialize_der().into(),
            );
            let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(Arc::new(
                tokio_rustls::rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der], key)
            .unwrap();
            (
                Some(tokio_rustls::TlsAcceptor::from(Arc::new(config))),
                Some(fingerprint),
            )
        } else {
            (None, None)
        };
        let me = Self {
            port,
            articles: Arc::new(Mutex::new(articles)),
            requests: Arc::default(),
            open: Arc::default(),
            peak: Arc::default(),
            options,
            fingerprint,
        };
        let (articles, requests, open, peak, options) = (
            me.articles.clone(),
            me.requests.clone(),
            me.open.clone(),
            me.peak.clone(),
            me.options.clone(),
        );
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let (articles, requests, open, peak, options, acceptor) = (
                    articles.clone(),
                    requests.clone(),
                    open.clone(),
                    peak.clone(),
                    options.clone(),
                    acceptor.clone(),
                );
                tokio::spawn(async move {
                    let now = open.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    match acceptor {
                        Some(a) => {
                            if let Ok(tls) = a.accept(tcp).await {
                                serve(tls, &articles, &requests, &options).await;
                            }
                        }
                        None => serve(tcp, &articles, &requests, &options).await,
                    }
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        me
    }

    /// Settings to use this provider.
    pub fn provider(&self, name: &str) -> Provider {
        let (username, password) = self.options.login.clone().unwrap_or_default();
        Provider {
            name: name.to_owned(),
            host: "127.0.0.1".to_owned(),
            port: self.port,
            tls: self.options.tls,
            username,
            password,
            connections: 4,
            fingerprint: self.fingerprint.clone(),
            ..Default::default()
        }
    }

    /// Lose articles (expired, or taken down).
    pub fn lose(&self, ids: &[String]) {
        let mut a = self.articles.lock().unwrap();
        for id in ids {
            a.remove(id);
        }
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    articles: &Mutex<HashMap<String, Vec<u8>>>,
    requests: &Mutex<Vec<String>>,
    options: &Options,
) {
    let mut stream = BufReader::new(stream);
    let mut authed = options.login.is_none();
    let mut user = String::new();
    if stream
        .get_mut()
        .write_all(b"200 Fake News ready\r\n")
        .await
        .is_err()
    {
        return;
    }
    let mut line = String::new();
    loop {
        line.clear();
        if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let command = line.trim_end();
        let (verb, arg) = command.split_once(' ').unwrap_or((command, ""));
        let reply: Vec<u8> = match verb.to_ascii_uppercase().as_str() {
            "CAPABILITIES" => {
                b"101 Capability list:\r\nVERSION 2\r\nREADER\r\nAUTHINFO USER\r\n.\r\n".to_vec()
            }
            "MODE" => b"200 Reader mode\r\n".to_vec(),
            "DATE" => b"111 20261122120000\r\n".to_vec(),
            "QUIT" => {
                let _ = stream.get_mut().write_all(b"205 Bye\r\n").await;
                return;
            }
            "AUTHINFO" => {
                let (kind, value) = arg.split_once(' ').unwrap_or((arg, ""));
                match (kind.to_ascii_uppercase().as_str(), &options.login) {
                    ("USER", _) => {
                        user = value.to_owned();
                        b"381 Password required\r\n".to_vec()
                    }
                    ("PASS", Some((u, p))) if *u == user && p == value => {
                        authed = true;
                        b"281 Authentication accepted\r\n".to_vec()
                    }
                    ("PASS", None) => {
                        authed = true;
                        b"281 Authentication accepted\r\n".to_vec()
                    }
                    _ => b"481 Authentication failed\r\n".to_vec(),
                }
            }
            "BODY" | "ARTICLE" if !authed => b"480 Authentication required\r\n".to_vec(),
            "BODY" => {
                let id = arg.trim().trim_matches(['<', '>']).to_owned();
                requests.lock().unwrap().push(id.clone());
                if !options.delay.is_zero() {
                    tokio::time::sleep(options.delay).await;
                }
                let body = articles.lock().unwrap().get(&id).cloned();
                match body {
                    Some(body) => {
                        let mut out = format!("222 0 <{id}>\r\n").into_bytes();
                        for l in body.split_inclusive(|b| *b == b'\n') {
                            if l.first() == Some(&b'.') {
                                out.push(b'.');
                            }
                            out.extend_from_slice(l);
                        }
                        if !out.ends_with(b"\r\n") {
                            out.extend_from_slice(b"\r\n");
                        }
                        out.extend_from_slice(b".\r\n");
                        out
                    }
                    None => b"430 No such article\r\n".to_vec(),
                }
            }
            _ => b"500 Unknown command\r\n".to_vec(),
        };
        if stream.get_mut().write_all(&reply).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usenet::Config;
    use crate::usenet::nntp::{FetchError, Providers, check};

    #[tokio::test]
    async fn serves_articles_over_tls_with_a_login() {
        let data: Vec<u8> = (0..50_000u32).map(|i| (i % 256) as u8).collect();
        let up = upload("t", &[("a.mkv", &data)], 20_000, 4096, 2);
        let login = Some(("me".to_owned(), "secret".to_owned()));
        let fake = FakeProvider::start(
            up.articles.clone(),
            Options {
                login,
                tls: true,
                ..Default::default()
            },
        )
        .await;
        let provider = fake.provider("Fake");
        check(&provider).await.unwrap();
        let wrong = Provider {
            password: "nope".into(),
            ..provider.clone()
        };
        assert!(
            check(&wrong)
                .await
                .unwrap_err()
                .to_string()
                .contains("rejected the login")
        );

        let providers = Providers::new(&Config {
            providers: vec![provider],
        });
        let ids = up.ids("a.mkv");
        let mut got = Vec::new();
        for id in &ids {
            let (raw, slot) = providers.body(id, &[]).await.unwrap();
            assert_eq!(slot, 0);
            let part = yenc_simd::decode_yenc(&raw).unwrap();
            assert_eq!(part.part_begin, Some(got.len() as u64));
            got.extend_from_slice(&part.data);
        }
        assert!(got == data);
        fake.lose(&ids[..1]);
        assert_eq!(
            providers.body(&ids[0], &[]).await.unwrap_err(),
            FetchError::Missing
        );
        assert!(up.files.iter().any(|f| f.is_par2()) && !up.volume_ids().is_empty());
    }

    #[tokio::test]
    async fn backups_are_only_asked_for_what_the_main_provider_lacks() {
        let data = vec![7u8; 30_000];
        let up = upload("b", &[("a.mkv", &data)], 10_000, 4096, 1);
        let main = FakeProvider::start(up.articles.clone(), Options::default()).await;
        let backup = FakeProvider::start(up.articles.clone(), Options::default()).await;
        let ids = up.ids("a.mkv");
        main.lose(&ids[1..2]);
        // The backup comes first in the list, but is a backup.
        let config = Config {
            providers: vec![
                Provider {
                    backup: true,
                    id: 2,
                    ..backup.provider("Block")
                },
                Provider {
                    id: 1,
                    ..main.provider("Main")
                },
            ],
        };
        let providers = Providers::new(&config);
        assert_eq!(
            providers.workers(),
            4,
            "only the main provider's connections"
        );
        for id in &ids {
            providers.body(id, &[]).await.unwrap();
        }
        assert_eq!(
            backup.requests.lock().unwrap().clone(),
            vec![ids[1].clone()]
        );

        // Nobody at the address: worth retrying, not "missing".
        let down = Providers::new(&Config {
            providers: vec![Provider {
                port: 9,
                ..main.provider("Down")
            }],
        });
        assert!(matches!(
            down.body(&ids[0], &[]).await,
            Err(FetchError::Unreachable(_))
        ));
    }
}
