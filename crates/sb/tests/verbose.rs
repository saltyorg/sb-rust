//! sb-go's `-v/--verbose` on the commands where sb has no verbose mode
//! (the maintainer, 2026-11-18; crates/sb/src/verbose.rs): accepted, the
//! run as without it but for one notice on stderr, before the steps,
//! naming this run in the run log. Two commands that end at once in the
//! test environment, reaching nothing of the host's (no Saltbox, a
//! refused Docker controller), stand for all;
//! they need root, so they run only with the root tests asked for
//! (`root_gate`): a run that handed over to sudo would lose the test's
//! environment. `sb motd`'s case is in motd.rs, with its isolation.

mod common;
mod root_gate;

/// The notice's text, for the test's log directory.
fn notice_prefix() -> String {
    format!(
        "! -v isn't needed: sb logs full detail to {}",
        common::test_log_dir().display()
    )
}

fn run(args: &[&str]) -> std::process::Output {
    common::sb().args(args).output().expect("run sb")
}

fn notices(output: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter(|line| line.contains("-v isn't needed"))
        .map(ToString::to_string)
        .collect()
}

/// With `-v`, `-vv` or `--verbose` a command ends as without it, stdout
/// the same, and stderr starts with the one notice.
#[test]
fn the_flag_changes_nothing_but_a_notice() {
    if !root_gate::requested() {
        return;
    }
    for command in [&["validate-config"][..], &["docker", "start"]] {
        let without = run(command);
        assert_eq!(notices(&without), Vec::<String>::new(), "{command:?}");
        for flag in ["-v", "-vv", "--verbose"] {
            let mut args = command.to_vec();
            args.push(flag);
            let with = run(&args);
            assert_eq!(
                with.status.code(),
                without.status.code(),
                "{args:?}: {with:?}"
            );
            assert_eq!(with.stdout, without.stdout, "{args:?}");
            let notices = notices(&with);
            assert_eq!(notices.len(), 1, "{args:?}: {notices:?}");
            assert!(notices[0].starts_with(&notice_prefix()), "{}", notices[0]);
            let stderr = String::from_utf8_lossy(&with.stderr);
            assert_eq!(stderr.lines().next(), Some(notices[0].as_str()), "{stderr}");
        }
    }
}

/// A command sb-go gave no `-v` still refuses it, before anything runs
/// (`install`'s own `-v` and `setup`'s silent one: verbose.rs's unit
/// tests).
#[test]
fn others_refuse_it() {
    let diag = run(&["diag", "-v"]);
    assert_eq!(diag.status.code(), Some(2), "{diag:?}");
    assert_eq!(notices(&diag), Vec::<String>::new(), "{diag:?}");
}

/// The flag stays out of every command's help.
#[test]
fn help_never_shows_it() {
    for command in [
        &["update"][..],
        &["motd"],
        &["docker", "start"],
        &["self-update"],
    ] {
        let mut args = command.to_vec();
        args.push("--help");
        let output = run(&args);
        assert!(output.status.success(), "{args:?}");
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(!help.contains("--verbose"), "{args:?}: {help}");
        assert!(!help.contains("verbos"), "{args:?}: {help}");
    }
}
