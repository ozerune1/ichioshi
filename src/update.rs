//! Updates: whether GitHub has a newer Ichioshi (the repository's latest
//! release), its download for this system (checked against the SHA-256
//! GitHub gives), and putting it in place.
//!
//! - Linux: the program in the release's tarball is renamed over this one
//!   (the running copy carries on), and starts once this one has closed.
//!   Installed from a package (.deb, .rpm, Arch), the release's package of
//!   that kind is installed by the system's package manager instead, once
//!   the app has closed (`--install-package`, asking for the password
//!   through pkexec).
//! - Windows: a running program can't be replaced, so the new copy, unpacked
//!   in the state folder, waits for this one to close (`--install-update`),
//!   copies itself over the app's folder and starts it.
//! - Android: the system's installer installs the APK (the phone app asks it).

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Where releases come from.
pub const REPO: &str = "ozerune1/ichioshi";

/// GitHub's API (`ICHIOSHI_UPDATE_API`, when running or building, points it
/// elsewhere: tests).
pub fn api_base() -> String {
    std::env::var("ICHIOSHI_UPDATE_API")
        .ok()
        .or_else(|| option_env!("ICHIOSHI_UPDATE_API").map(str::to_owned))
        .unwrap_or_else(|| "https://api.github.com".to_owned())
}

/// The releases page.
pub fn releases_page() -> String {
    format!("https://github.com/{REPO}/releases")
}

/// A version: major, minor, patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// "v1.2.3" or "1.2.3" ("1.2" is 1.2.0; anything after '-' or '+' is
    /// left out).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let core = s.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let mut next = || parts.next().map(str::parse::<u64>);
        let major = next()?.ok()?;
        let minor = next().transpose().ok()?.unwrap_or(0);
        let patch = next().transpose().ok()?.unwrap_or(0);
        if next().is_some() {
            return None;
        }
        Some(Self(major, minor, patch))
    }

    /// This build's. (`ICHIOSHI_PRETEND_VERSION`, when running or building,
    /// says another: to try updating.)
    pub fn current() -> Self {
        std::env::var("ICHIOSHI_PRETEND_VERSION")
            .ok()
            .or_else(|| option_env!("ICHIOSHI_PRETEND_VERSION").map(str::to_owned))
            .and_then(|v| Self::parse(&v))
            .unwrap_or_else(|| {
                Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate's version is one")
            })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// How this copy of Ichioshi was installed: what an update downloads, and
/// how it's put in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// Unpacked by hand (Linux tarball), the app's folder (Windows), the
    /// phone's app.
    Plain,
    /// By a Linux package, which says so in `share/ichioshi/package` beside
    /// the program's `bin` (`/usr/share/ichioshi/package`).
    Deb,
    Rpm,
    Pacman,
}

impl Install {
    pub fn is_package(self) -> bool {
        self != Self::Plain
    }
}

/// How the program at `exe` was installed.
pub fn installed_by(exe: &Path) -> Install {
    let marker = exe
        .parent()
        .and_then(Path::parent)
        .map(|prefix| prefix.join("share").join("ichioshi").join("package"));
    let said = marker.and_then(|m| std::fs::read_to_string(m).ok());
    match said.as_deref().map(str::trim) {
        Some("deb") => Install::Deb,
        Some("rpm") => Install::Rpm,
        Some("pacman") => Install::Pacman,
        _ => Install::Plain,
    }
}

/// How this copy was installed.
pub fn this_install() -> Install {
    static THIS: std::sync::LazyLock<Install> = std::sync::LazyLock::new(|| {
        if !cfg!(target_os = "linux") {
            return Install::Plain;
        }
        std::env::current_exe().map_or(Install::Plain, |exe| installed_by(&exe))
    });
    *THIS
}

/// The release's file for this system, by name (none: no download for it).
pub fn asset_name() -> Option<&'static str> {
    if cfg!(all(target_os = "android", target_arch = "aarch64")) {
        Some("Ichioshi-android-arm64.apk")
    } else if cfg!(all(target_os = "android", target_arch = "x86_64")) {
        Some("Ichioshi-android-x86_64.apk")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(match this_install() {
            Install::Plain => "Ichioshi-linux-x86_64.tar.gz",
            Install::Deb => "Ichioshi-linux-x86_64.deb",
            Install::Rpm => "Ichioshi-linux-x86_64.rpm",
            Install::Pacman => "Ichioshi-linux-x86_64.pkg.tar.zst",
        })
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("Ichioshi-windows-x86_64.zip")
    } else {
        None
    }
}

/// A release's file.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Its SHA-256 (hex), as GitHub says.
    pub sha256: Option<String>,
}

/// A release on GitHub.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// Its title.
    pub name: String,
    /// What's new (Markdown).
    pub notes: String,
    /// Its page.
    pub page: String,
    /// The file for this system.
    pub asset: Option<Asset>,
}

impl Release {
    /// Newer than this build?
    pub fn is_newer(&self) -> bool {
        self.version > Version::current()
    }

    /// The first `lines` lines of what's new, as plain text (Markdown's
    /// headings, emphasis, links and tables left out; bullets as •).
    pub fn summary(&self, lines: usize) -> String {
        let link = regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("a valid pattern");
        self.notes
            .lines()
            .map(str::trim)
            .filter(|l| {
                !l.is_empty() && !l.starts_with('|') && !l.chars().all(|c| "-=*_ ".contains(c))
            })
            .map(|l| {
                let l = l.trim_start_matches('#').trim_start();
                let l = match l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                    Some(rest) => format!("• {rest}"),
                    None => l.to_owned(),
                };
                link.replace_all(&l, "$1")
                    .replace("**", "")
                    .replace('`', "")
            })
            .take(lines)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The latest release (not drafts or pre-releases).
pub async fn latest(http: &reqwest::Client, api: &str) -> Result<Release> {
    let resp = http
        .get(format!("{api}/repos/{REPO}/releases/latest"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("couldn't reach GitHub")?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!("there are no releases yet");
    }
    if !status.is_success() {
        bail!("GitHub said HTTP {status}");
    }
    let json: Value = resp
        .json()
        .await
        .context("GitHub's answer wasn't readable")?;
    parse_release(&json, asset_name())
}

fn parse_release(json: &Value, wanted: Option<&str>) -> Result<Release> {
    let tag = json["tag_name"]
        .as_str()
        .context("the release has no tag")?;
    let version = Version::parse(tag).with_context(|| format!("“{tag}” isn't a version"))?;
    let asset = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| wanted.is_some_and(|w| a["name"].as_str() == Some(w)))
        .and_then(|a| {
            Some(Asset {
                name: a["name"].as_str()?.to_owned(),
                url: a["browser_download_url"].as_str()?.to_owned(),
                size: a["size"].as_u64().unwrap_or(0),
                sha256: a["digest"]
                    .as_str()
                    .and_then(|d| d.strip_prefix("sha256:"))
                    .map(str::to_ascii_lowercase),
            })
        });
    let text = |key: &str| json[key].as_str().unwrap_or_default().to_owned();
    let name = text("name");
    Ok(Release {
        version,
        name: if name.trim().is_empty() {
            format!("Ichioshi {version}")
        } else {
            name
        },
        notes: text("body"),
        page: Some(text("html_url"))
            .filter(|p| !p.is_empty())
            .unwrap_or_else(releases_page),
        asset,
    })
}

/// Where a version's download and what's unpacked from it go.
pub fn version_dir(updates: &Path, version: Version) -> PathBuf {
    updates.join(version.to_string())
}

/// Download `asset` into `dir` (resuming a partial download), and check
/// it's what GitHub says it is.
pub async fn download(
    http: &reqwest::Client,
    asset: &Asset,
    dir: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, f64),
) -> Result<PathBuf> {
    if asset.size == 0 {
        bail!("GitHub doesn't say how big {} is", asset.name);
    }
    let dest = dir.join(&asset.name);
    let have = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    if have != asset.size || check(&dest, asset).await.is_err() {
        let _ = std::fs::remove_file(&dest);
        crate::debrid::jobs::fetch(http, &asset.url, &dest, asset.size, cancel, progress).await?;
    }
    if let Err(e) = check(&dest, asset).await {
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }
    Ok(dest)
}

/// Whether `file` has the SHA-256 GitHub gives for it (fine if it gives none).
async fn check(file: &Path, asset: &Asset) -> Result<()> {
    let Some(want) = asset.sha256.clone() else {
        return Ok(());
    };
    let file = file.to_owned();
    let got = tokio::task::spawn_blocking(move || sha256(&file)).await??;
    if got != want {
        bail!(
            "{} isn't what GitHub says it is (its SHA-256 differs); try again",
            asset.name
        );
    }
    Ok(())
}

/// A file's SHA-256, in hex.
pub fn sha256(file: &Path) -> Result<String> {
    use std::io::Read;
    let mut f =
        std::fs::File::open(file).with_context(|| format!("couldn't read {}", file.display()))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// What's in a release archive: the folder at its top (`Ichioshi/`) left
/// off; nothing that would land outside where it's unpacked.
fn inside(path: &Path) -> Option<PathBuf> {
    let mut parts = path.components();
    match parts.next()? {
        Component::Normal(_) => {}
        _ => return None,
    }
    let mut rest = PathBuf::new();
    for c in parts {
        match c {
            Component::Normal(p) => rest.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!rest.as_os_str().is_empty()).then_some(rest)
}

/// Unpack a release's archive (`.tar.gz` or `.zip`) into `into` (emptied
/// first), without its top folder.
pub fn unpack(archive: &Path, into: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(into);
    std::fs::create_dir_all(into)?;
    let name = archive
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let open = || std::fs::File::open(archive).with_context(|| format!("couldn't open {name}"));
    if name.ends_with(".tar.gz") {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(open()?));
        tar.set_preserve_permissions(true);
        for entry in tar.entries().context("the archive is damaged")? {
            let mut entry = entry.context("the archive is damaged")?;
            let path = entry.path()?.into_owned();
            let Some(rel) = inside(&path) else { continue };
            let out = into.join(rel);
            let kind = entry.header().entry_type();
            if kind.is_dir() {
                std::fs::create_dir_all(&out)?;
            } else if kind.is_file() {
                if let Some(dir) = out.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                entry
                    .unpack(&out)
                    .with_context(|| format!("couldn't unpack {}", out.display()))?;
            }
        }
    } else if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(open()?).context("the archive is damaged")?;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).context("the archive is damaged")?;
            let Some(rel) = file.enclosed_name().as_deref().and_then(inside) else {
                continue;
            };
            let out = into.join(rel);
            if file.is_dir() {
                std::fs::create_dir_all(&out)?;
            } else {
                if let Some(dir) = out.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut f = std::fs::File::create(&out)
                    .with_context(|| format!("couldn't write {}", out.display()))?;
                std::io::copy(&mut file, &mut f)
                    .with_context(|| format!("couldn't unpack {}", out.display()))?;
            }
        }
    } else {
        bail!("{name} isn't an archive Ichioshi can unpack");
    }
    Ok(())
}

/// The program in an unpacked release.
pub fn program_in(dir: &Path) -> PathBuf {
    dir.join(if cfg!(windows) {
        "Ichioshi.exe"
    } else {
        "ichioshi"
    })
}

/// Whether files can be written in `dir` (where the update goes).
fn writable(dir: &Path) -> Result<()> {
    let probe = dir.join(".ichioshi-update-probe");
    std::fs::write(&probe, b"")
        .with_context(|| {
            format!(
                "Ichioshi can't write to {}: update it the way you installed it",
                dir.display()
            )
        })
        .map(|()| {
            let _ = std::fs::remove_file(&probe);
        })
}

/// In the updates folder: why the last update couldn't be put in place
/// (Windows), for the app to say.
pub const FAILED: &str = "failed.txt";

/// What to run once the app has closed: the update's program, put in
/// place.
pub static AFTER_EXIT: Mutex<Option<std::process::Command>> = Mutex::new(None);

/// Put the update unpacked in `unpacked` in place of `exe` (this program),
/// and say what starts it once the app has closed. (Installed by a
/// package, `unpacked` is the package, which is installed once the app has
/// closed: `--install-package`.)
pub fn install(unpacked: &Path, exe: &Path) -> Result<std::process::Command> {
    if installed_by(exe).is_package() {
        let mut c = std::process::Command::new(exe);
        c.arg("--install-package").arg(unpacked);
        return Ok(c);
    }
    let new = program_in(unpacked);
    if !new.is_file() {
        bail!(
            "the download has no {}",
            new.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    let dir = exe.parent().context("the program isn't in a folder")?;
    writable(dir)?;
    if cfg!(windows) {
        // The new copy installs itself once this one has closed.
        let mut c = std::process::Command::new(&new);
        c.arg("--install-update").arg(dir);
        Ok(c)
    } else {
        // Renamed over the program: the running app keeps its own copy.
        let tmp = dir.join(".ichioshi-update");
        std::fs::copy(&new, &tmp).with_context(|| format!("couldn't write {}", tmp.display()))?;
        std::fs::rename(&tmp, exe)
            .with_context(|| format!("couldn't replace {}", exe.display()))?;
        Ok(std::process::Command::new(exe))
    }
}

/// `--install-update <folder>`, run from an unpacked update: wait for the
/// app in `folder` to close, copy this one over it, and (if `start`) start
/// it. Nothing is copied if the app doesn't close within a minute.
pub fn finish_install(folder: &Path, start: bool) -> Result<()> {
    let here = std::env::current_exe()?
        .parent()
        .context("the update isn't in a folder")?
        .to_owned();
    let exe = program_in(folder);
    // A running program can't be opened to write.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match std::fs::OpenOptions::new().write(true).open(&exe) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => return Err(e).context("Ichioshi didn't close"),
        }
    }
    copy_tree(&here, folder)?;
    note_installed_version(folder);
    if start {
        std::process::Command::new(&exe)
            .current_dir(folder)
            .spawn()
            .with_context(|| format!("couldn't start {}", exe.display()))?;
    }
    Ok(())
}

/// Installed by the installer (installer/), the app's entry in Settings ›
/// Apps says which version it is: this one, now it's in place in `folder`.
#[cfg(windows)]
fn note_installed_version(folder: &Path) {
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Ichioshi";
    let Ok(key) = windows_registry::CURRENT_USER
        .options()
        .read()
        .write()
        .open(KEY)
    else {
        return;
    };
    let same = |a: &Path, b: &Path| {
        let norm = |p: &Path| {
            p.to_string_lossy()
                .trim_end_matches(['\\', '/'])
                .to_lowercase()
        };
        norm(a) == norm(b)
    };
    if key
        .get_string("InstallLocation")
        .is_ok_and(|dir| same(Path::new(&dir), folder))
    {
        let _ = key.set_string("DisplayVersion", Version::current().to_string());
    }
}

#[cfg(not(windows))]
fn note_installed_version(_: &Path) {}

/// Copy everything in `from` into `to`, over what's there (each file tried
/// a few times: something may still be letting go of it).
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            copy_tree(&src, &dst)?;
            continue;
        }
        let mut tries = 0;
        loop {
            match std::fs::copy(&src, &dst) {
                Ok(_) => break,
                Err(_) if tries < 20 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("couldn't replace {}", dst.display()));
                }
            }
        }
    }
    Ok(())
}

/// The command that installs the package `file` (a .deb, .rpm or Arch
/// package, by `kind`) with the system's package manager, through pkexec
/// (which asks for the password) unless this is root already.
pub fn package_command(kind: Install, file: &Path, root: bool) -> Result<std::process::Command> {
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
    };
    let (program, args): (&str, &[&str]) = match kind {
        Install::Deb if on_path("apt-get") => ("apt-get", &["install", "-y"]),
        Install::Deb => ("dpkg", &["-i"]),
        Install::Rpm if on_path("dnf") => ("dnf", &["install", "-y"]),
        Install::Rpm if on_path("zypper") => (
            "zypper",
            &["--non-interactive", "install", "--allow-unsigned-rpm"],
        ),
        Install::Rpm => ("rpm", &["-U"]),
        Install::Pacman => ("pacman", &["-U", "--noconfirm"]),
        Install::Plain => bail!("Ichioshi wasn't installed by a package"),
    };
    let mut c = if root {
        std::process::Command::new(program)
    } else {
        if !on_path("pkexec") {
            bail!(
                "installing needs pkexec (polkit), which isn't here: install {} with your package manager",
                file.display()
            );
        }
        let mut c = std::process::Command::new("pkexec");
        c.arg(program);
        c
    };
    c.args(args).arg(file);
    if kind == Install::Deb {
        c.env("DEBIAN_FRONTEND", "noninteractive");
    }
    Ok(c)
}

/// Whether this runs as root.
#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

/// `--install-package <file>`, run once the app has closed: install the
/// update's package with the package manager.
pub fn install_package(file: &Path) -> Result<()> {
    let file = std::path::absolute(file)?;
    let status = package_command(this_install(), &file, is_root())?
        .status()
        .context("couldn't start the package manager")?;
    match status.code() {
        Some(0) => Ok(()),
        // pkexec: the password dialog was dismissed, or not allowed.
        Some(126 | 127) if !is_root() => bail!("it wasn't allowed (the password was cancelled)"),
        _ => bail!("the package manager failed ({status})"),
    }
}

/// Remove what's left of updates to this version or older.
pub fn tidy(updates: &Path) {
    let Ok(entries) = std::fs::read_dir(updates) else {
        return;
    };
    for entry in entries.flatten() {
        let done = Version::parse(&entry.file_name().to_string_lossy())
            .is_some_and(|v| v <= Version::current());
        if done {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `--update`: update now, without the window (what it does is printed).
/// Nothing starts afterwards.
pub fn update_now(state_dir: &Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("ichioshi/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let release = rt.block_on(latest(&http, &api_base()))?;
    if !release.is_newer() {
        println!("Ichioshi {} is the latest.", Version::current());
        return Ok(());
    }
    let asset = release
        .asset
        .clone()
        .with_context(|| format!("{} has no download for this system", release.name))?;
    println!("Downloading {} ({})…", release.name, asset.name);
    let dir = version_dir(&state_dir.join("update"), release.version);
    let file = rt.block_on(download(
        &http,
        &asset,
        &dir,
        &AtomicBool::new(false),
        |_, _| {},
    ))?;
    if this_install().is_package() {
        install_package(&file)?;
        println!("Installed {}.", release.name);
        return Ok(());
    }
    let unpacked = dir.join("unpacked");
    unpack(&file, &unpacked)?;
    let exe = std::env::current_exe()?;
    let mut then = install(&unpacked, &exe)?;
    if cfg!(windows) {
        // It finishes once this has closed; nothing starts after it.
        then.arg("--no-start").spawn()?;
        println!("Installing {} once this closes.", release.name);
    } else {
        println!("Installed {}.", release.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ichioshi-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(Version::parse("v0.2.0"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("1.10"), Some(Version(1, 10, 0)));
        assert_eq!(Version::parse("2.0.1-beta.1"), Some(Version(2, 0, 1)));
        assert_eq!(Version::parse("latest"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 0));
        assert_eq!(Version(0, 2, 0).to_string(), "0.2.0");
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn notes_read_as_plain_text() {
        let r = Release {
            version: Version(1, 0, 0),
            name: String::new(),
            notes: "## What's new\n\n| a | b |\n|---|---|\n- **Faster** [downloads](https://x)\n* `--update`\n---\nMore".into(),
            page: String::new(),
            asset: None,
        };
        assert_eq!(
            r.summary(10),
            "What's new\n• Faster downloads\n• --update\nMore"
        );
        assert_eq!(r.summary(2), "What's new\n• Faster downloads");
    }

    #[test]
    fn a_release_gives_this_systems_file_and_its_digest() {
        let json = serde_json::json!({
            "tag_name": "v9.1.0",
            "name": "Ichioshi 9.1.0",
            "body": "New things",
            "html_url": "https://github.com/ozerune1/ichioshi/releases/tag/v9.1.0",
            "assets": [
                {"name": "Ichioshi-windows-x86_64.zip", "size": 10,
                 "browser_download_url": "https://x/w.zip", "digest": "sha256:AB"},
                {"name": "Ichioshi-linux-x86_64.tar.gz", "size": 20,
                 "browser_download_url": "https://x/l.tar.gz", "digest": "sha256:cd"},
            ],
        });
        let r = parse_release(&json, Some("Ichioshi-windows-x86_64.zip")).unwrap();
        assert_eq!(r.version, Version(9, 1, 0));
        assert!(r.is_newer());
        assert_eq!(r.notes, "New things");
        let a = r.asset.unwrap();
        assert_eq!(
            (a.url.as_str(), a.size, a.sha256.as_deref()),
            ("https://x/w.zip", 10, Some("ab"))
        );
        // No file for this system: the release still says what's new.
        let r = parse_release(&json, Some("Ichioshi-android-arm64.apk")).unwrap();
        assert_eq!(r.asset, None);
        // A release without a title is named after its version.
        let r = parse_release(&serde_json::json!({"tag_name": "0.3"}), None).unwrap();
        assert_eq!(r.name, "Ichioshi 0.3.0");
        assert_eq!(r.page, releases_page());
    }

    /// A package says it installed the program; its updates are packages
    /// too, installed by the package manager once the app has closed.
    #[test]
    fn packages_are_updated_by_the_package_manager() {
        let dir = temp("package");
        let exe = dir.join("usr").join("bin").join("ichioshi");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(installed_by(&exe), Install::Plain);
        let marker = dir.join("usr").join("share").join("ichioshi");
        std::fs::create_dir_all(&marker).unwrap();
        for (said, kind) in [
            ("deb\n", Install::Deb),
            ("rpm", Install::Rpm),
            ("pacman", Install::Pacman),
            ("snap", Install::Plain),
        ] {
            std::fs::write(marker.join("package"), said).unwrap();
            assert_eq!(installed_by(&exe), kind, "{said}");
        }
        std::fs::write(marker.join("package"), "pacman").unwrap();
        let file = dir.join("Ichioshi-linux-x86_64.pkg.tar.zst");
        let after = install(&file, &exe).unwrap();
        assert_eq!(after.get_program(), exe.as_os_str());
        let args: Vec<_> = after.get_args().collect();
        assert_eq!(
            args,
            [std::ffi::OsStr::new("--install-package"), file.as_os_str()]
        );

        let c = package_command(Install::Pacman, &file, true).unwrap();
        assert_eq!(c.get_program(), "pacman");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args[..2], ["-U", "--noconfirm"]);
        assert_eq!(args[2], file.as_os_str());
        assert!(package_command(Install::Plain, &file, true).is_err());
        // Not root: the password is asked for (pkexec), if it's there.
        if let Ok(c) = package_command(Install::Pacman, &file, false) {
            assert_eq!(c.get_program(), "pkexec");
            assert_eq!(c.get_args().next().unwrap(), "pacman");
        }
    }

    fn tarball(path: &Path, files: &[(&str, &[u8], u32)]) {
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        for (name, data, mode) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            tar.append_data(&mut h, name, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    /// The tarball's program comes out executable, the top folder left off.
    #[test]
    fn unpacks_a_tarball() {
        let dir = temp("tar");
        let archive = dir.join("Ichioshi-linux-x86_64.tar.gz");
        tarball(
            &archive,
            &[
                ("Ichioshi/ichioshi", b"new program", 0o755),
                ("Ichioshi/icons/ichioshi.png", b"png", 0o644),
            ],
        );
        let out = dir.join("unpacked");
        unpack(&archive, &out).unwrap();
        assert_eq!(std::fs::read(out.join("ichioshi")).unwrap(), b"new program");
        assert_eq!(
            std::fs::read(out.join("icons/ichioshi.png")).unwrap(),
            b"png"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(out.join("ichioshi"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Nothing in an archive lands outside where it's unpacked.
    #[test]
    fn paths_stay_inside() {
        assert_eq!(
            inside(Path::new("Ichioshi/a/b.dll")),
            Some(PathBuf::from("a/b.dll"))
        );
        assert_eq!(inside(Path::new("Ichioshi/../../etc/passwd")), None);
        assert_eq!(inside(Path::new("/etc/passwd")), None);
        assert_eq!(inside(Path::new("Ichioshi/")), None);
    }

    /// The Windows zip: its folder unpacked whole.
    #[test]
    fn unpacks_a_zip() {
        let dir = temp("zip");
        let archive = dir.join("Ichioshi-windows-x86_64.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in [
            ("Ichioshi/Ichioshi.exe", &b"exe"[..]),
            ("Ichioshi/lib/gstreamer-1.0/libgstplayback.dll", b"dll"),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        let out = dir.join("unpacked");
        unpack(&archive, &out).unwrap();
        assert_eq!(std::fs::read(out.join("Ichioshi.exe")).unwrap(), b"exe");
        assert_eq!(
            std::fs::read(out.join("lib/gstreamer-1.0/libgstplayback.dll")).unwrap(),
            b"dll"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Linux: the new program is renamed over the old, and it's what starts.
    #[cfg(unix)]
    #[test]
    fn installs_over_the_program() {
        let dir = temp("install");
        let unpacked = dir.join("unpacked");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::fs::write(unpacked.join("ichioshi"), b"new").unwrap();
        let app = dir.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let exe = app.join("ichioshi");
        std::fs::write(&exe, b"old").unwrap();
        let start = install(&unpacked, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(start.get_program(), exe.as_os_str());
        assert!(!app.join(".ichioshi-update").exists());
        // A folder it can't write to: says so, and changes nothing.
        let missing = dir.join("nowhere/ichioshi");
        let e = install(&unpacked, &missing).unwrap_err();
        assert!(
            e.to_string().contains("update it the way you installed it"),
            "{e}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The new copy copies itself over the app's folder, keeping what it
    /// doesn't replace.
    #[test]
    fn copies_a_folder_over_another() {
        let dir = temp("tree");
        let (from, to) = (dir.join("new"), dir.join("app"));
        std::fs::create_dir_all(from.join("lib")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("Ichioshi.exe"), b"new").unwrap();
        std::fs::write(from.join("lib/a.dll"), b"a2").unwrap();
        std::fs::write(to.join("Ichioshi.exe"), b"old").unwrap();
        std::fs::write(to.join("settings.ini"), b"mine").unwrap();
        copy_tree(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("Ichioshi.exe")).unwrap(), b"new");
        assert_eq!(std::fs::read(to.join("lib/a.dll")).unwrap(), b"a2");
        assert_eq!(std::fs::read(to.join("settings.ini")).unwrap(), b"mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Downloads are checked against GitHub's SHA-256: a file that isn't
    /// what it says is thrown away.
    #[test]
    fn downloads_are_checked() {
        use crate::mock_http::{Resp, mock};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let body = b"the new release".to_vec();
        let served = body.clone();
        let base = rt.block_on(mock(move |_| Resp::binary(200, served.clone())));
        let dir = temp("download");
        let probe = dir.join("probe");
        std::fs::write(&probe, &body).unwrap();
        let right = sha256(&probe).unwrap();
        let http = reqwest::Client::new();
        let asset = |sha256: Option<String>| Asset {
            name: "Ichioshi-linux-x86_64.tar.gz".into(),
            url: format!("{base}/download"),
            size: body.len() as u64,
            sha256,
        };
        let no = AtomicBool::new(false);
        let got = rt
            .block_on(download(&http, &asset(Some(right)), &dir, &no, |_, _| {}))
            .unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), body);
        let e = rt
            .block_on(download(
                &http,
                &asset(Some("00".repeat(32))),
                &dir,
                &no,
                |_, _| {},
            ))
            .unwrap_err();
        assert!(e.to_string().contains("SHA-256"), "{e}");
        assert!(!got.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Old updates are tidied away; newer ones (still to install) stay.
    #[test]
    fn tidies_old_updates() {
        let dir = temp("tidy");
        let Version(a, b, c) = Version::current();
        for v in [
            "0.0.1".to_owned(),
            format!("{a}.{b}.{c}"),
            format!("{}.0.0", a + 1),
        ] {
            std::fs::create_dir_all(dir.join(v)).unwrap();
        }
        tidy(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec![format!("{}.0.0", a + 1)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
