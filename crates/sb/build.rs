//! Identical code folding for sb's optimised `x86_64-unknown-linux-gnu` builds
//! (ADR-026): lld's identical code folding, `--icf=all`
//! (<https://github.com/llvm/llvm-project/blob/llvmorg-23.1.5/lld/docs/ld.lld.1>,
//! the LLD rustc 1.100.0 links with), merges functions whose code and
//! references are the same. It takes 0.23 MB off the production binary
//! on top of ADR-026's routing (0.26 MB on its own) and leaves the
//! commands' instruction counts as they were.
//!
//! - **For the sb binary only:** `cargo::rustc-link-arg-bin`
//!   (<https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-link-arg-bin>).
//! - **In optimised builds only** (`OPT_LEVEL` not `0`: the release
//!   profile and those that inherit it, production and release-tests),
//!   as cargo recommends over `PROFILE`
//!   (<https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts>).
//! - **For `x86_64-unknown-linux-gnu` only:** rustc links it with LLD
//!   by default since 1.90
//!   (<https://blog.rust-lang.org/2025/09/18/Rust-1.90.0/>). The aarch64
//!   build links with GNU ld (`build/Dockerfile`), which has no `--icf`.
//!
//! Why not `.cargo/config.toml`'s `target.x86_64-unknown-linux-gnu.rustflags`:
//! without `--target`, as every dev and test build here runs, cargo
//! passes them to every compiler invocation on the host
//! (<https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags>),
//! and a profile's own `rustflags` is unstable
//! (<https://doc.rust-lang.org/cargo/reference/unstable.html#profile-rustflags-option>).
//! Why not `--icf=safe`: rustc emits no address-significance table, so it
//! folds nothing (the binary is byte for byte the same; ADR-026).

use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let optimised = env::var("OPT_LEVEL").is_ok_and(|level| level != "0");
    let lld = env::var("TARGET").is_ok_and(|target| target == "x86_64-unknown-linux-gnu");
    if optimised && lld {
        println!("cargo::rustc-link-arg-bin=sb=-Wl,--icf=all");
    }
}
