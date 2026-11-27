//! aws-lc is built without its CPU-jitter entropy source (ADR-027):
//! `.cargo/config.toml` sets `AWS_LC_SYS_NO_JITTER_ENTROPY`, which
//! aws-lc-sys's build script reads. If upstream renamed the variable, the
//! build would still succeed and every TLS command would pay the jitter
//! collection again, so these tests hold the setting to its effect, once
//! at run time and once in the linked code.

use object::read::elf::ElfFile64;
use object::read::{Object, ObjectSymbol};

/// `try_fips_cpu_jitter_entropy` errs when aws-lc uses the opt-out entropy
/// source, and only reads which source is configured; it starts no
/// collection:
/// <https://docs.rs/aws-lc-rs/1.18.2/aws_lc_rs/fn.try_fips_cpu_jitter_entropy.html>
#[test]
fn aws_lc_is_built_without_cpu_jitter_entropy() {
    assert!(
        aws_lc_rs::try_fips_cpu_jitter_entropy().is_err(),
        "aws-lc uses CPU-jitter entropy: AWS_LC_SYS_NO_JITTER_ENTROPY in .cargo/config.toml no longer takes effect"
    );
}

/// The same from the build's side, independent of that function's meaning:
/// with jitter, the entropy-source table links jitterentropy's
/// `jent_entropy_init` (prefixed by aws-lc-sys, e.g.
/// `aws_lc_0_46_0_jent_entropy_init`) into this test binary; without it,
/// the library isn't compiled at all. The symbol the first test calls
/// shows the symbol table is there to read (test profiles aren't
/// stripped; only `production` is). `current_exe()` is this test binary,
/// not sb: it links the one aws-lc-sys build in the graph, as sb does, so
/// its symbols say what sb's build compiled; `SB_TEST_BINARY`, another
/// build of sb, doesn't reach it.
#[test]
fn aws_lc_links_no_jitterentropy_code() {
    let exe = std::env::current_exe().expect("the test binary's path");
    let data = std::fs::read(&exe).expect("the test binary");
    let elf = ElfFile64::<object::Endianness>::parse(&*data).expect("an ELF test binary");
    let names: Vec<&str> = elf
        .symbols()
        .filter_map(|symbol| symbol.name().ok())
        .collect();
    assert!(
        names
            .iter()
            .any(|name| name.ends_with("FIPS_is_entropy_cpu_jitter")),
        "no aws-lc symbols in {}: stripped, or aws-lc-rs no longer linked",
        exe.display()
    );
    let jitter: Vec<&&str> = names
        .iter()
        .filter(|name| name.contains("jent_entropy_init"))
        .collect();
    assert!(
        jitter.is_empty(),
        "jitterentropy is linked ({jitter:?}): AWS_LC_SYS_NO_JITTER_ENTROPY in .cargo/config.toml no longer takes effect"
    );
}
