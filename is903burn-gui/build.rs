use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => {
            println!("cargo:rerun-if-changed=assets/is903burn-gui.rc");
            println!("cargo:rerun-if-changed=assets/is903burn-gui.ico");
            embed_resource::compile_for(
                "assets/is903burn-gui.rc",
                ["is903burn-gui"],
                embed_resource::ParamsMacrosAndIncludeDirs(embed_resource::NONE, ["assets"]),
            )
            .manifest_required()
            .expect("failed to embed Windows application icon");
        }
        Ok("macos") => link_macos_usb(),
        _ => {}
    }
}

fn link_macos_usb() {
    println!("cargo:rerun-if-changed=../tools/is903-macos-probe.c");
    println!("cargo:rerun-if-env-changed=IS903_LIBUSB_PREFIX");
    let prefix = env::var_os("IS903_LIBUSB_PREFIX")
        .map(PathBuf::from)
        .or_else(|| {
            let out = Command::new("brew")
                .args(["--prefix", "libusb"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        })
        .expect("Install libusb 1.0.30 or set IS903_LIBUSB_PREFIX");
    cc::Build::new()
        .file("../tools/is903-macos-probe.c")
        .include(prefix.join("include/libusb-1.0"))
        .define("IS903_BACKEND", None)
        .compile("is903_usb");
    println!(
        "cargo:rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    println!("cargo:rustc-link-lib=dylib=usb-1.0");
}
