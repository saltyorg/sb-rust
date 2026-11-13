//! With the `testing` feature only, builds the stand-in sb that
//! self-update's tests serve as a release asset (`stand_in.rs`;
//! `testing::stand_in`): an ELF of the target's architecture that runs.
//! Without the feature (every release and production build), nothing.
//! rustc is cargo's own (`RUSTC`), for the build's target (`TARGET`), with
//! the target's linker when one is configured (`RUSTC_LINKER`)
//! (<https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts>).
//! Without the build's `RUSTFLAGS`: a coverage build's instrumentation
//! would make the stand-in write profile files where it runs.
//!
//! Why not a `[[bin]]` target (the spec's "built as a test binary"): its
//! path, `CARGO_BIN_EXE_<name>`, is set only for the same package's
//! integration tests
//! (<https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-crates>),
//! so neither sb-release's unit tests nor sb's end-to-end tests could
//! find it, and artifact dependencies (`-Z bindeps`) are unstable.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=stand_in.rs");
    if env::var_os("CARGO_FEATURE_TESTING").is_none() {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let rustc = env::var_os("RUSTC").expect("cargo sets RUSTC");
    let target = env::var("TARGET").expect("cargo sets TARGET");
    #[expect(
        clippy::disallowed_methods,
        reason = "rustc for the test stand-in sb: at build time only, only with the `testing` feature, never inside sb; a build script can't use sb-exec (CONSTRAINTS.md E5)"
    )]
    let mut rustc = Command::new(rustc);
    rustc
        .args([
            "--edition=2024",
            "--crate-name=stand_in",
            "--target",
            &target,
        ])
        .args([
            "-C",
            "opt-level=1",
            "-C",
            "debuginfo=0",
            "-C",
            "strip=symbols",
        ]);
    if let Some(linker) = env::var_os("RUSTC_LINKER") {
        let mut flag = std::ffi::OsString::from("linker=");
        flag.push(linker);
        rustc.arg("-C").arg(flag);
    }
    let status = rustc
        .arg("-o")
        .arg(out.join("stand-in"))
        .arg("stand_in.rs")
        .status()
        .expect("run rustc for the stand-in sb");
    assert!(status.success(), "rustc failed to build the stand-in sb");
}
