//! The installer's parts: the app's version (from the app's Cargo.toml), what
//! setup.exe carries (the app's folder with the uninstaller in it,
//! `ICHIOSHI_SETUP_PAYLOAD`: empty when not given, as in tests), and on
//! Windows each program's icon, description and manifest, compiled with
//! Zig's resource compiler (as the app's own build.rs does).

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let app_toml = here.join("../Cargo.toml");
    println!("cargo:rerun-if-changed={}", app_toml.display());
    let version = std::fs::read_to_string(&app_toml)
        .unwrap()
        .lines()
        .find_map(|l| {
            l.strip_prefix("version = \"")?
                .strip_suffix('"')
                .map(str::to_owned)
        })
        .expect("the app's version");
    println!("cargo:rustc-env=ICHIOSHI_VERSION={version}");

    println!("cargo:rerun-if-env-changed=ICHIOSHI_SETUP_PAYLOAD");
    let payload = match std::env::var("ICHIOSHI_SETUP_PAYLOAD") {
        Ok(p) => {
            println!("cargo:rerun-if-changed={p}");
            PathBuf::from(p)
        }
        Err(_) => {
            let empty = out.join("payload.empty");
            std::fs::write(&empty, b"").unwrap();
            empty
        }
    };
    println!("cargo:rustc-env=SETUP_PAYLOAD={}", payload.display());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        resources(&here, &out, &version);
    }
}

/// Each program's icon, version information and manifest.
fn resources(here: &Path, out: &Path, version: &str) {
    println!("cargo:rerun-if-env-changed=ZIG");
    println!("cargo:rerun-if-changed=windows/setup.manifest");
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let icon = here.join("../assets/icon/ichioshi.ico");
    let manifest = here.join("windows/setup.manifest");
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (a, b, c) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    for (bin, what) in [
        ("setup", "Ichioshi Setup"),
        ("uninstall", "Uninstall Ichioshi"),
    ] {
        let rc = out.join(format!("{bin}.rc"));
        std::fs::write(
            &rc,
            format!(
                r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "{what}"
      VALUE "ProductName", "Ichioshi"
      VALUE "OriginalFilename", "{bin}.exe"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
                icon = icon.display(),
                manifest = manifest.display(),
            ),
        )
        .unwrap();
        let res = out.join(format!("{bin}.res"));
        let made = Command::new(&zig)
            .args(["rc", "/fo"])
            .arg(&res)
            .arg(&rc)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            println!("cargo:rustc-link-arg-bin={bin}={}", res.display());
        } else {
            println!("cargo:warning=no icon or manifest in {bin}.exe: `{zig} rc` didn't run");
        }
    }
}
