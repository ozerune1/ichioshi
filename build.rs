//! Windows builds: the executable's icon and description, from
//! assets/windows/ichioshi.rc, compiled with Zig's resource compiler (Windows
//! builds are cross-compiled with Zig: see scripts/windows/build.sh).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/windows/ichioshi.rc");
    println!("cargo:rerun-if-changed=assets/icon/ichioshi.ico");
    println!("cargo:rerun-if-env-changed=ZIG");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ichioshi.res");
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let made = Command::new(&zig)
        .args(["rc", "/fo"])
        .arg(&out)
        .arg("assets/windows/ichioshi.rc")
        .status()
        .is_ok_and(|s| s.success());
    if made {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=no icon in the .exe: `{zig} rc` didn't run");
    }
}
