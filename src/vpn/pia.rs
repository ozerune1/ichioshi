//! Private Internet Access, through its app's command-line tool `piactl`.
//!
//! The PIA app makes the VPN connection. With "Request port forwarding" on,
//! it also gets a forwarded port and keeps it alive (PIA's port forwarding
//! API has to be renewed every 15 minutes); `piactl get portforward` reports
//! the port. This app only reads that, and connects or turns forwarding on
//! when you click to.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// What the PIA app reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// Disconnected, Connecting, Connected, Interrupted, Reconnecting, …
    pub connection: String,
    pub port: PortForward,
    /// Whether the next connection will ask for a forwarded port.
    pub request_forwarding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortForward {
    Port(u16),
    /// Not asked for on this connection.
    Inactive,
    Attempting,
    Failed,
    /// The region doesn't forward ports.
    Unavailable,
    Other(String),
}

impl State {
    pub fn connected(&self) -> bool {
        self.connection == "Connected"
    }
}

/// Where the PIA app installs `piactl`, if it's installed.
pub fn find_piactl() -> Option<PathBuf> {
    let installed: &[&str] = if cfg!(target_os = "macos") {
        &["/Applications/Private Internet Access.app/Contents/MacOS/piactl"]
    } else if cfg!(windows) {
        &[r"C:\Program Files\Private Internet Access\piactl.exe"]
    } else {
        &["/opt/piavpn/bin/piactl", "/usr/local/bin/piactl"]
    };
    installed.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Ask the PIA app how the connection is.
pub async fn state(piactl: &Path) -> Result<State> {
    let connection = get(piactl, "connectionstate").await?;
    let port = match get(piactl, "portforward").await?.as_str() {
        "Inactive" => PortForward::Inactive,
        "Attempting" => PortForward::Attempting,
        "Failed" => PortForward::Failed,
        "Unavailable" => PortForward::Unavailable,
        other => other
            .parse()
            .map(PortForward::Port)
            .unwrap_or_else(|_| PortForward::Other(other.to_owned())),
    };
    let request_forwarding = get(piactl, "requestportforward").await? == "true";
    Ok(State {
        connection,
        port,
        request_forwarding,
    })
}

/// Connect (or reconnect, to apply settings). The PIA app must be open, or
/// its background mode on.
pub async fn connect(piactl: &Path) -> Result<()> {
    run(piactl, &["connect"]).await.map(drop)
}

/// Ask for a forwarded port from now on, and reconnect so it takes effect.
pub async fn forward_ports(piactl: &Path) -> Result<()> {
    run(piactl, &["set", "requestportforward", "true"]).await?;
    connect(piactl).await
}

async fn get(piactl: &Path, what: &str) -> Result<String> {
    run(piactl, &["get", what]).await
}

async fn run(piactl: &Path, args: &[&str]) -> Result<String> {
    let mut command = tokio::process::Command::new(piactl);
    command
        .args(["--timeout", "5"])
        .args(args)
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .context("the PIA app didn't answer")?
        .context("could not run piactl")?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let message = stderr
            .lines()
            .chain(stdout.lines())
            .map(str::trim)
            .find(|l| !l.is_empty());
        bail!("PIA: {}", message.unwrap_or("piactl failed"));
    }
    Ok(stdout)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// A stand-in `piactl` that answers `get` from files in its folder (so a
    /// test can change them) and logs other commands to `calls`.
    pub(crate) fn fake_piactl(dir: &Path, connection: &str, port: &str, request: bool) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        set(dir, "connectionstate", connection);
        set(dir, "portforward", port);
        set(
            dir,
            "requestportforward",
            if request { "true" } else { "false" },
        );
        let script = dir.join("piactl");
        let body = format!(
            "#!/bin/sh\nd='{}'\nshift 2\nif [ \"$1\" = get ]; then cat \"$d/$2\"; exit 0; fi\n\
             echo \"$@\" >> \"$d/calls\"\n[ -e \"$d/fail\" ] && {{ cat \"$d/fail\" >&2; exit 1; }}\nexit 0\n",
            dir.display()
        );
        std::fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script
    }

    pub(crate) fn set(dir: &Path, what: &str, value: &str) {
        std::fs::write(dir.join(what), format!("{value}\n")).unwrap();
    }

    #[tokio::test]
    async fn reads_the_state_and_runs_commands() {
        let dir = tempfile::tempdir().unwrap();
        let piactl = fake_piactl(dir.path(), "Connected", "51234", true);
        let s = state(&piactl).await.unwrap();
        assert_eq!(
            s,
            State {
                connection: "Connected".into(),
                port: PortForward::Port(51234),
                request_forwarding: true
            }
        );
        assert!(s.connected());

        set(dir.path(), "portforward", "Attempting");
        assert_eq!(state(&piactl).await.unwrap().port, PortForward::Attempting);

        forward_ports(&piactl).await.unwrap();
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(calls, "set requestportforward true\nconnect\n");

        std::fs::write(
            dir.path().join("fail"),
            "Connection error: daemon inactive\n",
        )
        .unwrap();
        assert_eq!(
            connect(&piactl).await.unwrap_err().to_string(),
            "PIA: Connection error: daemon inactive"
        );
    }

    /// The real PIA app on this computer, read-only (`get` commands only).
    #[tokio::test]
    #[ignore = "needs the PIA app"]
    async fn reads_the_real_pia_app() {
        let Some(piactl) = find_piactl() else { return };
        let s = state(&piactl).await.unwrap();
        println!("{s:?}");
        assert!(!s.connection.is_empty());
    }
}
