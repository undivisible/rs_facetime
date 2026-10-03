use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/process_tap.m");
    println!("cargo:rerun-if-changed=native/process_tap.h");
    if env::var_os("CARGO_FEATURE_COREAUDIO_CAPTURE").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
    {
        return;
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("coreaudio-capture supports arm64 and x86_64 macOS targets"),
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let object = out.join("process_tap.o");
    // Apple SDK 14.2+ and the installed command-line tools are required only for
    // this feature. No helper injection, driver install or audio access occurs.
    let status = Command::new("xcrun")
        .args([
            "--sdk",
            "macosx",
            "clang",
            "-c",
            "-std=c11",
            "-fobjc-arc",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-mmacosx-version-min=14.0",
            "-arch",
            arch,
            "native/process_tap.m",
            "-o",
        ])
        .arg(&object)
        .status()
        .expect("coreaudio-capture requires Xcode command-line tools and macOS SDK 14.2+");
    assert!(
        status.success(),
        "failed to compile CoreAudio process-tap adapter"
    );
    let status = Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(out.join("librs_facetime_audio.a"))
        .arg(object)
        .status()
        .expect("xcrun ar");
    assert!(status.success(), "failed to archive CoreAudio adapter");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=rs_facetime_audio");
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
