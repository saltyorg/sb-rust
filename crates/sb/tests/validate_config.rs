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

/// An old-format motd.yml is migrated, its original copied into
/// `SB_BACKUPS_DIR`, and the run passes (motd decision 4).
#[test]
fn an_old_motd_yml_is_migrated() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let old = "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: plex-token-zz\n";
    std::fs::write(root.path().join("motd.yml"), old).expect("motd.yml");
    let backups = root.path().join("backups");
    let fake = healthy();
    let out = common::sb()
        .arg("validate-config")
        .env("SB_SALTBOX_DIR", root.path())
        .env("SB_BACKUPS_DIR", &backups)
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{}/v2/users/login/", fake.url()),
        )
        .output()
        .expect("run sb");
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
    let notice = format!(
        "Migrated motd.yml to the current format (backup: {}/motd.yml.",
        backups.display()
    );
    assert!(text.contains(&notice), "{text}");
    let copies: Vec<_> = std::fs::read_dir(&backups).expect("backups").collect();
    assert_eq!(copies.len(), 1);
    let copy = copies[0].as_ref().expect("entry").path();
    assert_eq!(std::fs::read_to_string(copy).expect("copy"), old);
    assert!(
        std::fs::read_to_string(root.path().join("motd.yml"))
            .expect("motd.yml")
            .contains("  instances:\n")
    );
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
    // Each issue once, in its file's block, with the log's pointer; not
    // repeated by cli-core.
    assert_eq!(
        text.matches("user.pass: cannot decode").count(),
        1,
        "{text}"
    );
    assert!(text.contains("full output: "), "{text}");
}

/// Without a fake, both checks go to the harness's refused loopback port
/// (`common::apply_sb_env`): no test reaches Cloudflare or Docker Hub.
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

/// The checks run concurrently (both requests open at once) and both
/// finish when one fails (config decision 3): once both are released,
/// Docker Hub's refusal shows its block and Cloudflare goes on to pass.
#[test]
fn both_checks_finish_when_one_fails() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let fake = Fake::start().expect("fake");
    fake.script("/client/v4/zones", Answer::json(200, ZONES).held());
    fake.script(
        "/client/v4/zones/z1/settings/ssl",
        Answer::json(200, SSL_FULL),
    );
    fake.script(
        "/v2/users/login/",
        Answer::json(401, r#"{"message":"Incorrect authentication credentials"}"#).held(),
    );
    let child = common::sb()
        .arg("validate-config")
        .env("SB_SALTBOX_DIR", root.path())
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{}/v2/users/login/", fake.url()),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run sb");
    // Both requests open at once, both held: concurrent, whichever check
    // starts first.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let both = || {
        let seen: Vec<String> = fake.seen().into_iter().map(|s| s.target).collect();
        seen.iter().any(|t| t.starts_with("/client/v4/zones?"))
            && seen.iter().any(|t| t == "/v2/users/login/")
    };
    while !both() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let concurrent = both();
    fake.release();
    let out = child.wait_with_output().expect("wait for sb");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(concurrent, "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}");
    for line in [
        "✗ Docker Hub credentials validation",
        "docker hub authentication failed (HTTP 401): Incorrect authentication credentials",
        "✓ Cloudflare API credentials validated",
        "✗ Validating accounts.yml   sub-step failed",
        "full output: ",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    // The failure is shown once: in its block, not again by cli-core.
    assert_eq!(
        text.matches("Incorrect authentication credentials").count(),
        1,
        "{text}"
    );
}

/// Warnings and skipped checks are notices under their file's step:
/// sb-go's short-password text and the "no credentials" skip.
#[test]
fn warnings_and_skipped_checks_are_notices() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let accounts = root.path().join("accounts.yml");
    let text = std::fs::read_to_string(&accounts).expect("read");
    let edited = text
        .replace("pass: correct-horse-battery", "pass: short1")
        .split("dockerhub:")
        .next()
        .expect("before dockerhub")
        .to_string();
    assert!(edited.contains("pass: short1"), "{edited}");
    std::fs::write(&accounts, edited).expect("write");
    let fake = healthy();
    let out = validate(root.path(), &fake);
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
    for line in [
        "! WARNING: Password is shorter than 12 characters (6).",
        "› no Docker Hub credentials set; skipped",
        "› inventories/host_vars/localhost.yml not found; skipped",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    assert_eq!(fake.seen().len(), 2, "only Cloudflare's requests");
}

/// No known secret reaches any output or the run log, even when both
/// services echo back the credential each was sent (success criterion 6,
/// through the binary). A service only receives its own credential, so
/// that is all it can echo; a password never leaves the host and is
/// covered where it can show, an issue on it
/// (`a_broken_set_fails_with_its_issues`; sb-config's run log test).
#[test]
fn no_known_secret_reaches_any_output_or_the_log() {
    const TOKEN: &str = "token-not-a-real-secret-2";
    const HUB_TOKEN: &str = "hub-not-a-real-secret-3";
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let accounts = format!(
        "user:\n  name: seed\n  email: seed@example.com\n  domain: example.com\n  \
         pass: correct-horse-battery\ncloudflare:\n  scoped_token: {TOKEN}\n\
         dockerhub:\n  user: seed\n  token: {HUB_TOKEN}\n"
    );
    std::fs::write(root.path().join("accounts.yml"), accounts).expect("write");
    let fake = Fake::start().expect("fake");
    fake.script(
        "/client/v4/zones",
        Answer::json(
            403,
            format!(
                r#"{{"success":false,"errors":[{{"code":9109,"message":"bad token {TOKEN}"}}]}}"#
            ),
        ),
    );
    fake.script(
        "/v2/users/login/",
        Answer::json(
            401,
            format!(r#"{{"message":"wrong password {HUB_TOKEN}"}}"#),
        ),
    );
    let logs = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("logs");
    let out = common::sb()
        .arg("validate-config")
        .env("SB_LOG_DIR", logs.path())
        .env("SB_SALTBOX_DIR", root.path())
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{}/v2/users/login/", fake.url()),
        )
        .output()
        .expect("run sb");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    // The refusals are shown, the echoed credentials taken out.
    assert!(stderr.contains("bad token [REDACTED]"), "{stderr}");
    assert!(stderr.contains("wrong password [REDACTED]"), "{stderr}");
    let log: String = std::fs::read_dir(logs.path())
        .expect("logs")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect();
    assert!(log.contains("[REDACTED]"), "{log}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for secret in [TOKEN, HUB_TOKEN] {
        for (name, text) in [("stderr", &*stderr), ("stdout", &*stdout), ("log", &*log)] {
            assert!(!text.contains(secret), "{secret} in {name}:\n{text}");
        }
    }
}

/// sb on a terminal (util-linux `script`, as colour.rs), its checks
/// answered by `fake`.
fn validate_on_a_terminal(
    root: &std::path::Path,
    fake: &Fake,
    args: &str,
    env: &[(&str, &str)],
) -> String {
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("timeout");
    common::apply_sb_env(&mut cmd);
    cmd.args([
        "30",
        "script",
        "-qec",
        &format!("stty cols 100 rows 30; exec '{program}' validate-config {args}"),
        "/dev/null",
    ])
    .env("TERM", "xterm-256color")
    .env_remove("NO_COLOR")
    .env_remove("CLICOLOR")
    .env_remove("CLICOLOR_FORCE")
    .env("SB_SALTBOX_DIR", root)
    .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
    .env(
        "SB_DOCKERHUB_URL",
        format!("{}/v2/users/login/", fake.url()),
    );
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run script");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // script -e passes sb's exit code through.
    assert_eq!(out.status.code(), Some(0), "{args} {env:?}: {text:?}");
    text
}

/// Whether `text` has a colour code (SGR), as opposed to the live area's
/// cursor and erase codes.
fn has_colour(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find("\x1b[") {
        let after = &rest[at + 2..];
        let end = after
            .find(|c: char| !(c.is_ascii_digit() || c == ';'))
            .unwrap_or(after.len());
        if after[end..].starts_with('m') && end > 0 && &after[..end] != "0" {
            return true;
        }
        rest = &after[end..];
    }
    false
}

/// On a terminal the run is live and coloured; `--color=never` and
/// `NO_COLOR` take the colour away, as for every sb output (success
/// criterion 9). Piped output is plain (`a_valid_set_passes`).
#[test]
fn colour_follows_the_flag_and_no_color_on_a_terminal() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox_root("valid").expect("root");
    let coloured = validate_on_a_terminal(root.path(), &healthy(), "", &[]);
    assert!(has_colour(&coloured), "{coloured:?}");
    assert!(
        coloured.contains("Saltbox configuration validated"),
        "{coloured:?}"
    );
    for (args, env) in [("--color=never", &[][..]), ("", &[("NO_COLOR", "1")][..])] {
        let plain = validate_on_a_terminal(root.path(), &healthy(), args, env);
        assert!(!has_colour(&plain), "{args} {env:?}: {plain:?}");
        assert!(
            plain.contains("Saltbox configuration validated"),
            "{plain:?}"
        );
    }
}
