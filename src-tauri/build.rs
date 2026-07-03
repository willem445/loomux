fn main() {
    copy_sideloaded_conhost();
    embed_test_manifest();
    tauri_build::build()
}

/// Test executables link the same UI stack as the app (comctl32 v6 imports
/// like TaskDialogIndirect) but don't get tauri-build's manifest, so they
/// fail to load with STATUS_ENTRYPOINT_NOT_FOUND. Embed a minimal
/// common-controls-v6 manifest for test targets only.
fn embed_test_manifest() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("test.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    // `-tests` scoping requires at least one integration-test target to
    // exist (tests/smoke.rs). Applying it unscoped breaks the main binary:
    // tauri-build embeds its own manifest there and the two collide (LNK1123).
    println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}", manifest.display());
}

/// Place `resources/conhost/{conpty.dll, OpenConsole.exe}` next to the built
/// executable (dev and release target dirs). portable-pty prefers an
/// adjacent modern conpty over the inbox Windows conhost, whose full-screen
/// repaint on every resize floods terminal scrollback with duplicate frames.
/// The binaries are not committed by the build — if they're absent this is a
/// no-op and the inbox conhost is used (see resources/conhost/README.md).
fn copy_sideloaded_conhost() {
    use std::path::PathBuf;
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("resources").join("conhost");
    // OUT_DIR is <target>/<profile>/build/<pkg>-<hash>/out; the executable
    // lands three levels up in <target>/<profile>.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(profile_dir) = out.ancestors().nth(3) else {
        return;
    };
    for name in ["conpty.dll", "OpenConsole.exe"] {
        let from = src.join(name);
        println!("cargo:rerun-if-changed={}", from.display());
        if from.is_file() {
            let _ = std::fs::copy(&from, profile_dir.join(name));
        }
    }
}
