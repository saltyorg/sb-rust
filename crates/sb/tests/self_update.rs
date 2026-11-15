//! `sb self-update` end to end (release spec, "Testing strategy"): a copy
//! of the built sb in a temporary directory updates itself from
//! sb-release's fake GitHub (`SB_RELEASE_TEST_BASES`, a test build only),
//! so no test reaches GitHub or replaces an installed sb. Root runs
//! (`SB_TEST_ROOT=1`): the command needs root. The binary is a raw build,
//! so self-update is off unless `--force-update`.

mod common;
mod root_gate;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use sb_release::testing::{Answer, Fake, stand_in};
use sha2::{Digest, Sha256};

/// The running test build's version (a raw build: the package's).
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// What the new release's asset holds: the stand-in sb of its version.
fn new_sb() -> Vec<u8> {
    stand_in("99.0.0")
}

/// A copy of the built sb in its own directory, the bases file of a fake
/// offering `tag` with `body` as this architecture's asset.
struct Setup {
    fake: Fake,
    dir: tempfile::TempDir,
    sb: PathBuf,
    bases: PathBuf,
}

fn setup(tag: &str, body: &[u8]) -> Setup {
    setup_serving(tag, body, Answer::new(200, body.to_vec()))
}

/// [`setup`] with the asset served as `answer`.
fn setup_serving(tag: &str, body: &[u8], answer: Answer) -> Setup {
    let fake = Fake::start().expect("fake");
    let asset = sb_release::asset_name().expect("an architecture with an asset");
    let path = format!("/saltyorg/sb-go/releases/download/{tag}/{asset}");
    fake.script(
        "/version",
        Answer::json(format!(
            r#"[{{"tag_name":"{tag}","draft":false,"prerelease":false,"assets":[{{"name":"{asset}","size":{},"digest":"sha256:{}","browser_download_url":"{}{path}"}}]}}]"#,
            body.len(),
            hex::encode(Sha256::digest(body)),
            fake.url()
        )),
    );
    fake.script(&path, answer);
    let dir = tempfile::tempdir().expect("dir");
    let sb = dir.path().join("sb");
    std::fs::copy(assert_cmd::cargo::cargo_bin("sb"), &sb).expect("copy sb");
    let bases = dir.path().join("bases.json");
    fake.write_bases(&bases).expect("bases");
    Setup {
        fake,
        dir,
        sb,
        bases,
    }
}

impl Setup {
    /// The copy, with the test environment and the fake's bases.
    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        common::apply_sb_env(&mut cmd);
        cmd.env(sb_release::testing::BASES_VARIABLE, &self.bases);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = self.command(self.sb.to_str().expect("path"));
        // No controlling terminal: a question can't wait on one.
        cmd.args(["self-update", "--color=never"]).args(args);
        let mut setsid = self.command("setsid");
        setsid.arg("-w").arg(cmd.get_program()).args(cmd.get_args());
        setsid.output().expect("run sb")
    }

    /// The copy's bytes.
    fn sb_bytes(&self) -> Vec<u8> {
        std::fs::read(&self.sb).expect("sb")
    }

    /// The directory's files but the copy and the bases file.
    fn leftovers(&self) -> Vec<String> {
        leftovers(self.dir.path())
    }

    fn downloaded(&self) -> bool {
        self.fake
            .seen()
            .iter()
            .any(|s| s.target.contains("/releases/download/"))
    }
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != "sb" && name != "bases.json")
        .collect()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A raw build has self-update off: sb-go's two lines (its CI greps
/// them), exit 0, nothing asked of GitHub.
#[test]
fn a_raw_build_is_self_update_disabled() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", &new_sb());
    let before = setup.sb_bytes();
    let out = setup.run(&[]);
    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(
        shown.contains("Self-update is disabled in this build"),
        "{shown}"
    );
    assert!(
        shown.contains("Use --force-update to override this restriction"),
        "{shown}"
    );
    assert_eq!(setup.fake.seen(), Vec::new());
    assert_eq!(setup.sb_bytes(), before);
}

/// `--force-update --yes`: looked up, downloaded, verified, put in place
/// with mode 0755, with sb-go's lines; the User-Agent on every request.
#[test]
fn a_forced_update_replaces_sb() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", &new_sb());
    let out = setup.run(&["--force-update", "--yes"]);
    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    for line in [
        "Forcing self-update despite build configuration",
        &format!("New sb CLI version available: 99.0.0 (current: {CURRENT})"),
        "Checked the new binary",
        "Successfully updated sb CLI to version: 99.0.0",
    ] {
        assert!(shown.contains(line), "{line}: {shown}");
    }
    assert_eq!(setup.sb_bytes(), new_sb());
    let mode = std::fs::metadata(&setup.sb)
        .expect("sb")
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o755);
    assert_eq!(setup.leftovers(), Vec::<String>::new());
    // The new file runs.
    let ran = Command::new(&setup.sb).output().expect("run the new sb");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "stand-in sb 99.0.0\n");
    for request in setup.fake.seen() {
        assert_eq!(
            request.header("user-agent"),
            Some(format!("sb/{CURRENT}").as_str()),
            "{request:?}"
        );
    }
}

#[test]
fn the_latest_version_is_left_alone() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v0.0.1", &new_sb());
    let before = setup.sb_bytes();
    let out = setup.run(&["--force-update", "--yes"]);
    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(
        shown.contains(&format!("Current binary is the latest version: {CURRENT}")),
        "{shown}"
    );
    assert_eq!(setup.sb_bytes(), before);
    assert!(!setup.downloaded());
}

/// No terminal and no `--yes`: the error names the flag, exit 1, nothing
/// downloaded.
#[test]
fn without_a_terminal_yes_is_needed() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", &new_sb());
    let before = setup.sb_bytes();
    let out = setup.run(&["--force-update"]);
    let shown = text(&out);
    assert_eq!(out.status.code(), Some(1), "{shown}");
    assert!(shown.contains("answer it with --yes"), "{shown}");
    assert!(!setup.downloaded());
    assert_eq!(setup.sb_bytes(), before);
}

/// `--force-update` exists only in builds with self-update off; this raw
/// build has it.
#[test]
fn a_raw_build_has_force_update() {
    let out = common::sb()
        .args(["self-update", "--help", "--color=never"])
        .output()
        .expect("help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--force-update"), "{help}");
    assert!(help.contains("-y --yes"), "{help}");
    assert!(!help.contains("--verbose"), "{help}");
}

/// Answered on a terminal (util-linux `script`, as term-ui's PTY tests):
/// `y` or `yes` in any case, then Enter, updates; anything else (`nay`,
/// `no thank you`, Enter alone) declines with sb-go's warning.
#[test]
fn the_question_is_answered_on_the_terminal() {
    if !root_gate::requested() {
        return;
    }
    for (keys, updated) in [
        ("y\r", true),
        ("Y\r", true),
        (" yes \r", true),
        ("nay\r", false),
        ("no thank you\r", false),
        ("\r", false),
    ] {
        let setup = setup("v99.0.0", &new_sb());
        let before = setup.sb_bytes();
        let out = on_a_terminal(&setup, "", keys);
        assert!(
            out.contains("Do you want to update [y/N]"),
            "{keys:?}: {out}"
        );
        let answer = if updated { "yes" } else { "no" };
        assert!(
            out.contains(&format!("Do you want to update {answer}")),
            "{keys:?}: {out}"
        );
        if updated {
            assert!(
                out.contains("Successfully updated sb CLI to version: 99.0.0"),
                "{out}"
            );
            assert_eq!(setup.sb_bytes(), new_sb());
        } else {
            assert!(out.contains("Update of sb CLI cancelled"), "{out}");
            assert!(!setup.downloaded());
            assert_eq!(setup.sb_bytes(), before);
        }
        assert!(out.contains("EXIT=0"), "{out}");
    }
}

/// What was typed before the question shows is dropped, never taken as
/// the answer: `y` and Enter typed at once, then Enter at the question,
/// declines.
#[test]
fn type_ahead_doesnt_answer_the_question() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", &new_sb());
    let before = setup.sb_bytes();
    let out = on_a_terminal_typing(&setup, "", "y\r", "\r");
    assert!(out.contains("Update of sb CLI cancelled"), "{out}");
    assert!(!setup.downloaded());
    assert_eq!(setup.sb_bytes(), before);
}

/// Ctrl-C at the question ends sb by the signal (130 to the shell), with
/// "interrupted" and the cursor shown (never left hidden by whatever drew
/// last), and nothing downloaded.
#[test]
fn ctrl_c_at_the_question_shows_the_cursor() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", &new_sb());
    let before = setup.sb_bytes();
    let out = on_a_terminal(&setup, "", "no\x03");
    let shown = out.rfind("\x1b[?25h").expect("the cursor is shown");
    if let Some(hidden) = out.rfind("\x1b[?25l") {
        assert!(shown > hidden, "{out:?}");
    }
    assert!(!out.contains("Do you want to update no"), "{out}");
    assert!(out.contains("EXIT=130"), "{out}");
    assert!(out.contains("interrupted"), "{out}");
    assert!(!setup.downloaded());
    assert_eq!(setup.sb_bytes(), before);
}

/// From a background process group the question isn't asked (a read
/// would stop sb): the error names the flag.
#[test]
fn in_the_background_yes_is_needed() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", &new_sb());
    let before = setup.sb_bytes();
    let out = on_a_terminal(&setup, "set -m; ", "");
    assert!(
        out.contains("sb runs in the background; answer it with --yes"),
        "{out}"
    );
    assert!(out.contains("EXIT=1"), "{out}");
    assert_eq!(setup.sb_bytes(), before);
}

/// `sb self-update --force-update` under `script`, `keys` typed once the
/// question shows; `prefix` before the command (`set -m; ` puts it in a
/// background job). Returns what the terminal showed.
fn on_a_terminal(setup: &Setup, prefix: &str, keys: &str) -> String {
    on_a_terminal_typing(setup, prefix, "", keys)
}

/// [`on_a_terminal`], `early` typed at once, before the question.
fn on_a_terminal_typing(setup: &Setup, prefix: &str, early: &str, keys: &str) -> String {
    use std::io::Write;
    let shell =
        format!("{prefix}\"$SB\" self-update --force-update --color=never & wait $!; echo EXIT=$?");
    let mut script = setup.command("timeout");
    script
        .args(["30", "script", "-qec", &shell, "/dev/null"])
        .env("SB", &setup.sb)
        .env("TERM", "xterm-256color")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = script.spawn().expect("run script");
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(early.as_bytes()).expect("type early");
    // What the terminal shows, read as it comes, so the keys are typed
    // once the question is up (no fixed sleep, which a busy machine could
    // outrun; as term-ui's signals tests).
    let shown = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let reader = {
        use std::io::Read;
        let mut stdout = child.stdout.take().expect("stdout");
        let shown = std::sync::Arc::clone(&shown);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                shown
                    .lock()
                    .expect("shown")
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        })
    };
    if !keys.is_empty() {
        let start = std::time::Instant::now();
        while !shown
            .lock()
            .expect("shown")
            .contains("Do you want to update [y/N]")
        {
            assert!(start.elapsed() < Duration::from_secs(20), "no question");
            std::thread::sleep(Duration::from_millis(20));
        }
        stdin.write_all(keys.as_bytes()).expect("type");
    }
    // Kept open until the run ends: `script` ends the session at its end.
    child.wait().expect("script");
    drop(stdin);
    reader.join().expect("reader");
    shown.lock().expect("shown").clone()
}

/// SIGINT mid-download: interrupted, sb unchanged, nothing staged.
#[test]
fn an_interrupted_download_changes_nothing() {
    if !root_gate::requested() {
        return;
    }
    let body = vec![b'x'; 256 * 1024];
    let slow = Answer::new(200, body.clone()).trickled(4096, Duration::from_millis(50));
    let setup = setup_serving("v99.0.0", &body, slow);
    let before = setup.sb_bytes();
    let mut cmd = setup.command(setup.sb.to_str().expect("path"));
    cmd.args(["self-update", "--force-update", "--yes", "--color=never"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().expect("run sb");
    // Signalled once the download has started (the fake has seen it).
    let start = std::time::Instant::now();
    while !setup.downloaded() {
        assert!(start.elapsed() < Duration::from_secs(20), "no download");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    let pid = rustix::process::Pid::from_child(&child);
    rustix::process::kill_process(pid, rustix::process::Signal::INT).expect("SIGINT");
    let out = child.wait_with_output().expect("sb");
    assert!(!out.status.success(), "{}", text(&out));
    assert_eq!(setup.sb_bytes(), before);
    assert_eq!(setup.leftovers(), Vec::<String>::new());
}

/// The new binary must report the release's version (decided.md D-Q5):
/// one reporting another is refused, exit 1, sb unchanged, nothing staged.
#[test]
fn a_binary_reporting_another_version_is_refused() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", &stand_in("98.0.0"));
    let before = setup.sb_bytes();
    let out = setup.run(&["--force-update", "--yes"]);
    let shown = text(&out);
    assert_eq!(out.status.code(), Some(1), "{shown}");
    for line in [
        "binary update failed validation",
        "the new binary reports version 98.0.0, not 99.0.0",
    ] {
        assert!(shown.contains(line), "{line}: {shown}");
    }
    assert_eq!(setup.sb_bytes(), before);
    assert_eq!(setup.leftovers(), Vec::<String>::new());
}

/// An asset that isn't an ELF binary is refused before it runs: exit 1,
/// sb unchanged, nothing staged.
#[test]
fn an_asset_that_is_not_an_elf_is_refused() {
    if !root_gate::requested() {
        return;
    }
    let setup = setup("v99.0.0", b"#!/bin/sh\necho '{\"version\":\"99.0.0\"}'\n");
    let before = setup.sb_bytes();
    let out = setup.run(&["--force-update", "--yes"]);
    let shown = text(&out);
    assert_eq!(out.status.code(), Some(1), "{shown}");
    for line in [
        "binary update failed validation",
        "open ELF executable: Unknown file magic",
    ] {
        assert!(shown.contains(line), "{line}: {shown}");
    }
    assert_eq!(setup.sb_bytes(), before);
    assert_eq!(setup.leftovers(), Vec::<String>::new());
}
