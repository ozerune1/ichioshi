//! Fetching articles from your providers (over `nzb-nntp`'s connections and
//! pools). Main providers are asked in order; backup providers only when no
//! main one has the article.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use nzb_nntp::{ConnectionPool, NntpConnection, NntpError, ServerConfig};

use super::{Config, Provider};

/// How long one article may take before the connection is given up on.
const BODY_TIMEOUT: Duration = Duration::from_secs(60);

/// Why an article couldn't be fetched.
#[derive(Clone, Debug, PartialEq)]
pub enum FetchError {
    /// Every provider says it doesn't have it.
    Missing,
    /// Some provider couldn't be asked (network, login…); worth trying again later.
    Unreachable(String),
}

struct Slot {
    name: String,
    pool: Arc<ConnectionPool>,
    connections: usize,
    backup: bool,
    /// Set when the provider rejects the login: it isn't asked again.
    login_failed: Mutex<Option<String>>,
}

/// Your enabled providers, main ones first.
pub struct Providers {
    slots: Vec<Slot>,
}

fn server_config(p: &Provider) -> ServerConfig {
    let mut c = ServerConfig::new(p.id.to_string(), p.host.trim());
    c.name = p.label().to_owned();
    c.port = p.port;
    c.ssl = p.tls;
    c.username = Some(p.username.clone()).filter(|u| !u.is_empty());
    c.password = (!p.username.is_empty()).then(|| p.password.clone());
    c.connections = p.connections.max(1);
    c.trusted_fingerprint = p.fingerprint.clone();
    c.connect_timeout_secs = 20;
    c.pipelining = 1;
    c
}

fn is_login_error(e: &NntpError) -> bool {
    matches!(
        e,
        NntpError::Auth(_) | NntpError::AuthRequired(_) | NntpError::PermissionDenied(_)
    )
}

impl Providers {
    pub fn new(config: &Config) -> Self {
        let mut usable: Vec<&Provider> = config.providers.iter().filter(|p| p.usable()).collect();
        // Stable: main providers keep their order, then the backups in theirs.
        usable.sort_by_key(|p| p.backup);
        let slots = usable
            .into_iter()
            .map(|p| Slot {
                name: p.label().to_owned(),
                pool: Arc::new(ConnectionPool::new(Arc::new(server_config(p)))),
                connections: usize::from(p.connections.max(1)),
                backup: p.backup,
                login_failed: Mutex::default(),
            })
            .collect();
        Self { slots }
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// How many articles to fetch at once: the main providers' connections.
    pub fn workers(&self) -> usize {
        let main: usize = self
            .slots
            .iter()
            .filter(|s| !s.backup)
            .map(|s| s.connections)
            .sum();
        let n = if main == 0 {
            self.slots.iter().map(|s| s.connections).sum()
        } else {
            main
        };
        n.clamp(1, 100)
    }

    pub fn name(&self, slot: usize) -> &str {
        self.slots.get(slot).map_or("?", |s| s.name.as_str())
    }

    /// Every provider rejected the login: the message for the first.
    pub fn all_logins_failed(&self) -> Option<String> {
        let failed: Vec<String> = self
            .slots
            .iter()
            .filter_map(|s| s.login_failed.lock().unwrap().clone())
            .collect();
        (!self.slots.is_empty() && failed.len() == self.slots.len()).then(|| failed[0].clone())
    }

    /// Fetch an article's body (dot-unstuffed, still yEnc), from the first
    /// provider that has it, skipping the providers in `skip`. Returns the
    /// provider it came from.
    pub async fn body(
        &self,
        message_id: &str,
        skip: &[usize],
    ) -> Result<(Vec<u8>, usize), FetchError> {
        let mut unreachable: Option<String> = None;
        for (i, slot) in self.slots.iter().enumerate() {
            if skip.contains(&i) {
                continue;
            }
            if let Some(why) = slot.login_failed.lock().unwrap().clone() {
                unreachable.get_or_insert(why);
                continue;
            }
            // One retry on a fresh connection after a connection problem.
            for _ in 0..2 {
                let mut pooled = match slot.pool.acquire().await {
                    Ok(c) => c,
                    Err(e) => {
                        let why = format!("{}: {e}", slot.name);
                        if is_login_error(&e) {
                            *slot.login_failed.lock().unwrap() =
                                Some(format!("{} rejected the login", slot.name));
                        }
                        unreachable = Some(why);
                        break;
                    }
                };
                match tokio::time::timeout(BODY_TIMEOUT, pooled.conn.fetch_body(message_id)).await {
                    Ok(Ok(resp)) => {
                        slot.pool.release(pooled);
                        return Ok((resp.data.unwrap_or_default(), i));
                    }
                    Ok(Err(NntpError::ArticleNotFound(_) | NntpError::NoArticleSelected(_))) => {
                        slot.pool.release(pooled);
                        break;
                    }
                    Ok(Err(e)) if is_login_error(&e) => {
                        slot.pool.discard(pooled);
                        *slot.login_failed.lock().unwrap() =
                            Some(format!("{} rejected the login", slot.name));
                        unreachable = Some(format!("{}: {e}", slot.name));
                        break;
                    }
                    Ok(Err(e)) => {
                        slot.pool.discard(pooled);
                        unreachable = Some(format!("{}: {e}", slot.name));
                    }
                    Err(_) => {
                        slot.pool.discard(pooled);
                        unreachable = Some(format!(
                            "{}: no answer for {}s",
                            slot.name,
                            BODY_TIMEOUT.as_secs()
                        ));
                    }
                }
            }
        }
        Err(unreachable.map_or(FetchError::Missing, FetchError::Unreachable))
    }
}

/// Connect and log in, to check a provider's settings.
pub async fn check(provider: &Provider) -> Result<()> {
    let config = server_config(provider);
    let mut conn = NntpConnection::new(config.id.clone());
    let connect = conn.connect(&config);
    match tokio::time::timeout(Duration::from_secs(30), connect).await {
        Ok(Ok(())) => {
            let _ = conn.quit().await;
            Ok(())
        }
        Ok(Err(e)) if is_login_error(&e) => Err(anyhow!("the server rejected the login ({e})")),
        Ok(Err(e)) => Err(anyhow!("{e}")),
        Err(_) => Err(anyhow!(
            "no answer from {}:{}",
            provider.host,
            provider.port
        )),
    }
}
