//! Network trouble: telling a dropped connection from a refusal, and trying
//! again when it's the connection.

/// A server couldn't be reached, or the connection to it dropped (said
/// readably: "couldn't reach Sonarr (…)").
#[derive(Debug)]
pub struct Unreachable(pub String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// Whether what went wrong was the connection (it dropped, timed out or
/// couldn't be made), not the site refusing.
pub fn dropped(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.is::<Unreachable>()
            || c.downcast_ref::<reqwest::Error>()
                .is_some_and(connection_trouble)
    })
}

/// A request that failed for the connection's sake.
pub fn connection_trouble(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_body() || e.is_decode() || e.is_request()
}

/// What went wrong, briefly: a server that couldn't be reached says so
/// in a few words (not the whole chain of what failed underneath).
pub fn brief(e: &anyhow::Error) -> String {
    if let Some(u) = e.chain().find_map(|c| c.downcast_ref::<Unreachable>()) {
        return u.0.clone();
    }
    if dropped(e) {
        return format!("couldn't reach it ({})", e.root_cause());
    }
    format!("{e:#}")
}

/// `attempt`, again (twice more, a moment and then a few seconds later)
/// while the connection is what fails: a phone's drops (in the background,
/// say), and long lists' pages are big.
pub async fn retrying<T, F, Fut>(mut attempt: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut wait = std::time::Duration::from_secs(1);
    let mut tries = 1;
    loop {
        match attempt().await {
            Err(e) if tries < 3 && dropped(&e) => {
                tracing::warn!("trying again: {e:#}");
                tokio::time::sleep(wait).await;
                wait *= 3;
                tries += 1;
            }
            done => return done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn says_it_briefly() {
        let e = anyhow::Error::new(Unreachable("couldn't reach Sonarr".into()))
            .context("Sonarr (Show)");
        assert_eq!(brief(&e), "couldn't reach Sonarr");
        let e = anyhow::anyhow!("AniList answered 500").context("AniList");
        assert_eq!(brief(&e), "AniList: AniList answered 500");
    }

    /// Dropped connections are tried again; refusals aren't.
    #[tokio::test(start_paused = true)]
    async fn tries_again_only_when_the_connection_drops() {
        let tries = AtomicU32::new(0);
        let got = retrying(|| async {
            if tries.fetch_add(1, Ordering::Relaxed) < 2 {
                Err(Unreachable("couldn't reach Sonarr".into()).into())
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(got.unwrap(), 7);
        assert_eq!(tries.load(Ordering::Relaxed), 3);

        let tries = AtomicU32::new(0);
        let got: anyhow::Result<()> = retrying(|| async {
            tries.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("Sonarr refused the API key (HTTP 401)")
        })
        .await;
        assert!(got.is_err());
        assert_eq!(tries.load(Ordering::Relaxed), 1);

        // Three tries at most.
        let tries = AtomicU32::new(0);
        let got: anyhow::Result<()> = retrying(|| async {
            tries.fetch_add(1, Ordering::Relaxed);
            Err(anyhow::Error::new(Unreachable("down".into())).context("Sonarr (Show)"))
        })
        .await;
        assert!(dropped(&got.unwrap_err()));
        assert_eq!(tries.load(Ordering::Relaxed), 3);
    }
}
