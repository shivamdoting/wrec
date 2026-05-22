use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
};

fn main() {
    println!("cargo:rerun-if-changed=native/wrec_helper.swift");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let source = manifest_dir.join("native").join("wrec_helper.swift");
    let helper = out_dir.join("wrec-helper");
    let module_cache = out_dir.join("swift-module-cache");
    let target_arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => "arm64",
    };
    let swift_target = format!("{target_arch}-apple-macosx15.0");

    let output = Command::new("xcrun")
        .arg("swiftc")
        .arg("-Osize")
        .arg("-whole-module-optimization")
        .arg("-target")
        .arg(swift_target)
        .arg("-module-cache-path")
        .arg(module_cache)
        .arg("-framework")
        .arg("ScreenCaptureKit")
        .arg("-framework")
        .arg("AVFoundation")
        .arg("-o")
        .arg(&helper)
        .arg(&source)
        .env("MACOSX_DEPLOYMENT_TARGET", "15.0")
        .stdin(Stdio::null())
        .output()
        .expect("failed to invoke xcrun swiftc");

    if !output.status.success() {
        panic!(
            "failed to compile wrec Swift helper:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    println!("cargo:rustc-env=WREC_HELPER_PATH={}", helper.display());
}
