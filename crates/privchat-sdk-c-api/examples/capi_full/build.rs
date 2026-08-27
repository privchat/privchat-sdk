// Builds and links the real cdylib, exactly like a C/C++ consumer would:
// the example declares the extern "C" surface itself (mirroring
// include/privchat_sdk_c_api.h) and links libprivchat_sdk_c_api.dylib.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // examples/capi_full -> examples -> privchat-sdk-c-api -> crates -> repo root
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let sdk_root = manifest.join("../../../..");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let lib_dir = sdk_root.join("target").join(&profile);

    // Ensure the cdylib exists (reuses the SDK workspace target dir, so this
    // is a no-op after the first build).
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["build", "-p", "privchat-sdk-c-api"])
        .current_dir(&sdk_root);
    if profile == "release" {
        cmd.arg("--release");
    }
    let status = cmd.status().expect("failed to invoke cargo for the cdylib");
    assert!(status.success(), "cargo build of privchat-sdk-c-api failed");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=privchat_sdk_c_api");
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }
    println!("cargo:rerun-if-changed=main.rs");
}
