//! Embeds assets/icon.ico in the Windows exe, so Explorer and the taskbar show it.

use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let windres = if cfg!(windows) { "windres" } else { "x86_64-w64-mingw32-windres" };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("icon.o");
    let status = Command::new(windres)
        .args(["--include-dir", "assets", "-O", "coff", "-i", "assets/icon.rc", "-o"])
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("could not run {windres}: {e}"));
    assert!(status.success(), "{windres} failed");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
