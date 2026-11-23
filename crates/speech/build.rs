//! Links clang's runtime into macOS builds. whisper.cpp's Metal code checks the macOS version with
//! `@available`, which below the newest macOS it is built for (the app's minimum, 14.0, in the
//! release) becomes a call to `__isPlatformVersionAtLeast`. Rust's standard library defines that
//! function, but the release profile's LTO keeps only what Rust code calls, so the release link
//! failed; `libclang_rt.osx.a` defines it as well, and the linker takes it from there only when
//! nothing else did.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let resources = Command::new("clang")
        .arg("--print-resource-dir")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()));
    let Some(lib) = resources.map(|dir| dir.join("lib/darwin")) else {
        println!("cargo:warning=clang's runtime was not found; a release link may miss __isPlatformVersionAtLeast");
        return;
    };
    if lib.join("libclang_rt.osx.a").is_file() {
        println!("cargo:rustc-link-search=native={}", lib.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    } else {
        println!("cargo:warning={} holds no libclang_rt.osx.a", lib.display());
    }
}
