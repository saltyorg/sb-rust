//! `sb validate-config` through the built binary (config plan Task 11):
//! its help and arguments as a normal run, and the command end to end on
//! fixture sets through `SB_SALTBOX_DIR`, with both credential checks
//! answered by sb-config's fake (root runs: `SB_TEST_ROOT=1`, logging
//! under the test's directory).

mod common;
mod root_gate;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use sb_config::testing::{Answer, Fake, saltbox_root};

const ZONES: &str = r#"{"success":true,"result":[{"id":"z1","name":"example.com"}]}"#;
const SSL_FULL: &str = r#"{"success":true,"result":{"id":"ssl","value":"full"}}"#;

/// The fake answering both checks well.
fn healthy() -> Fake {
    let fake = Fake::start().expect("fake");
    fake.script("/client/v4/zones", Answer::json(200, ZONES));
    fake.script(
        "/client/v4/zones/z1/settings/ssl",
        Answer::json(200, SSL_FULL),
    );
    fake.script("/v2/users/login/", Answer::json(200, "{}"));
    fake
}

/// `sb validate-config` on the Saltbox root `root`, the checks sent to
/// `fake`, plain (piped).
fn validate(root: &std::path::Path, fake: &Fake) -> std::process::Output {
    common::sb()
        .arg("validate-config")
        .env("SB_SALTBOX_DIR", root)
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{}/v2/users/login/", fake.url()),
        )
        .output()
        .expect("run sb")
}

/// sb-go's help text; no `--verbose` (config decision 1).
#[test]
fn help_is_sb_gos_and_verbose_is_gone() {
    // The whole layout, as sb-go's help (cli-core's renderer).
    let help = common::sb()
        .args(["validate-config", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Validate Saltbox configuration files",
        ));
    insta::assert_snapshot!(
        "validate_config_help",
        String::from_utf8_lossy(&help.get_output().stdout)
    );
    for args in [&["validate-config", "-v"][..], &["validate-config", "x"]] {
        common::sb().args(args).assert().code(2);
    }
}

/// A valid set: every step done, both checks sent, exit 0.
#[test]
fn a_valid_set_passes() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let fake = healthy();
    let out = validate(root.path(), &fake);
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
    for line in [
        "sb validate-config",
        "✓ Validated accounts.yml",
        "✓ Cloudflare API credentials validated",
        "✓ Docker Hub credentials validated",
        "✓ Saltbox configuration validated",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    assert!(!text.contains('\x1b'), "{text}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let targets: Vec<String> = fake.seen().into_iter().map(|s| s.target).collect();
    for target in [
        "/client/v4/zones?name=example.com",
        "/client/v4/zones/z1/settings/ssl",
        "/v2/users/login/",
    ] {
        assert!(targets.iter().any(|t| t == target), "{target}: {targets:?}");
    }
    assert_eq!(targets.len(), 3);
}

/// A broken set: the issue in its file's block, exit 1, no secret shown.
#[test]
fn a_broken_set_fails_with_its_issues() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let accounts = root.path().join("accounts.yml");
    let text = std::fs::read_to_string(&accounts).expect("read");
    let broken = text
        .replace("email: seed@example.com", "email: not-an-email")
        .replace("pass: correct-horse-battery", "pass: !!int s3cret-pass");
    // Each edit applied (the fixture could drift).
    assert!(broken.contains("email: not-an-email"), "{broken}");
    assert!(broken.contains("pass: !!int s3cret-pass"), "{broken}");
    std::fs::write(&accounts, broken).expect("write");
    std::fs::remove_file(root.path().join("settings.yml")).expect("remove");
    let out = validate(root.path(), &healthy());
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{text}");
    for line in [
        "✗ Validation of accounts.yml",
        "accounts.yml:6:9: user.pass: cannot decode !!str `[REDACTED]` as a !!int",
        "✗ Validation of settings.yml",
        "required config file not found:",
        "✓ Validated adv_settings.yml",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    assert!(!text.contains("s3cret-pass"), "{text}");
    // The decode error stops accounts.yml's rules: the email isn't checked.
    assert!(!text.contains("user.email"), "{text}");
}

/// Without a fake, both checks go to the harness's refused loopback port
/// (common::apply_sb_env): no test reaches Cloudflare or Docker Hub.
#[test]
fn checks_are_refused_by_default() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let out = common::sb()
        .arg("validate-config")
        .env("SB_SALTBOX_DIR", root.path())
        .output()
        .expect("run sb");
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("✗ Cloudflare API credentials validation"),
        "{text}"
    );
    assert!(
        text.contains("✗ Docker Hub credentials validation"),
        "{text}"
    );
    assert!(text.contains("127.0.0.1"), "{text}");
}

/// A test or debugging URL in place of a service's is in the run log (a
/// leftover export would otherwise send real credentials elsewhere
/// unseen).
#[test]
fn a_service_url_override_is_logged() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let logs = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("log dir");
    let fake = healthy();
    // A password in the URL never reaches the log (the fake ignores it).
    let with_password = fake.url().replace("http://", "http://u:hunter2-url@");
    assert_ne!(with_password, fake.url(), "the fake's URL changed form");
    let out = common::sb()
        .arg("validate-config")
        .env("SB_LOG_DIR", logs.path())
        .env("SB_SALTBOX_DIR", root.path())
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{with_password}/v2/users/login/"),
        )
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let log: String = std::fs::read_dir(logs.path())
        .expect("logs")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect();
    for name in ["CLOUDFLARE_BASE_URL", "SB_DOCKERHUB_URL"] {
        assert!(
            log.contains(&format!("variable=\"{name}\"")),
            "{name}\n{log}"
        );
    }
    // Both URLs, the second without its credentials.
    for url in [
        format!("{}/client/v4/", fake.url()),
        format!("{}/v2/users/login/", fake.url()),
    ] {
        assert!(log.contains(&format!("url=\"{url}\"")), "{url}\n{log}");
    }
    assert!(!log.contains("hunter2-url"), "{log}");
}
