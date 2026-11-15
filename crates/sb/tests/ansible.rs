//! `sb diag` and `sb install` through the binary with the fake
//! `ansible-playbook` (ansible spec, Testing strategy; module build M30,
//! M31): the argv it got, element by element, its working directory, sb's
//! lines, the run log, and each ending passed on. `SB_ANSIBLE_PLAYBOOK`
//! points at the fake (crates/ansible/tests/fixtures/fake-ansible-playbook)
//! or at the argparse fixture beside it, which parses its arguments with
//! ansible-core's own definitions; no Ansible runs. `systemd-detect-virt`
//! is a checked-in stand-in (tests/fixtures/detect-virt) first on `PATH`.
//! Root runs (`SB_TEST_ROOT=1`): sb logs, and needs root for the command.

mod common;
mod root_gate;
mod tag_cache;

use std::path::PathBuf;
use std::process::{Command, Output};

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ansible/tests/fixtures/fake-ansible-playbook"
);

/// The python3 fixture with ansible-core's `-e`, `-v`, `--tags`,
/// `--skip-tags` and `--become` definitions.
const ARGPARSE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ansible/tests/fixtures/argparse-ansible-playbook"
);

/// `systemd-detect-virt -c` outside a container, and in LXC.
const NOT_LXC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/detect-virt/none"
);
const IN_LXC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/detect-virt/lxc"
);

/// A test's world: a Saltbox root whose inventory puts the appdata root
/// in the world (Sandbox and `saltbox_mod` under it), Saltbox and Sandbox
/// checkouts with a tag cache valid for them (`tag_cache::seed`), a log
/// directory and the fake's record prefix.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        let world = Self { dir };
        std::fs::create_dir_all(world.root().join("inventories/host_vars")).expect("root");
        std::fs::write(
            world.root().join("inventories/host_vars/localhost.yml"),
            format!("server_appdata_path: {}\n", world.appdata().display()),
        )
        .expect("inventory");
        // Both checkouts, with a cache valid for them: no listing runs.
        tag_cache::seed(&world.root(), &world.appdata().join("sandbox"));
        std::fs::create_dir_all(world.appdata().join("saltbox_mod")).expect("mod");
        std::fs::create_dir(world.logs()).expect("logs");
        world
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("saltbox")
    }

    fn appdata(&self) -> PathBuf {
        self.dir.path().join("appdata")
    }

    fn logs(&self) -> PathBuf {
        self.dir.path().join("logs")
    }

    fn record(&self) -> PathBuf {
        self.dir.path().join("argv")
    }

    /// `sb <args>` in this world, with the fake as the playbook program.
    fn sb(&self, args: &[&str]) -> Command {
        let mut cmd = common::sb();
        cmd.args(args)
            .env("SB_SALTBOX_DIR", self.root())
            .env("SB_LOG_DIR", self.logs())
            .env("SB_ANSIBLE_PLAYBOOK", FAKE)
            .env("FAKE_ANSIBLE_LOG", self.record())
            .env_remove("FAKE_ANSIBLE_EXIT")
            .env_remove("FAKE_ANSIBLE_MODE")
            .env("PATH", path_with(NOT_LXC));
        cmd
    }

    /// What the argparse fixture parsed, run by run.
    fn parsed(&self) -> Vec<serde_json::Value> {
        let mut runs = Vec::new();
        for n in 0.. {
            let path = PathBuf::from(format!("{}.{n}.json", self.record().display()));
            let Ok(text) = std::fs::read_to_string(&path) else {
                break;
            };
            runs.push(serde_json::from_str(&text).expect("JSON"));
        }
        runs
    }

    /// Each run's record: its working directory, `$0`, its arguments.
    fn runs(&self) -> Vec<Vec<String>> {
        let mut runs = Vec::new();
        for n in 0.. {
            let path = PathBuf::from(format!("{}.{n}", self.record().display()));
            let Ok(bytes) = std::fs::read(&path) else {
                break;
            };
            let text = String::from_utf8(bytes).expect("UTF-8");
            let mut elements: Vec<String> = text.split('\0').map(str::to_string).collect();
            assert_eq!(elements.pop().as_deref(), Some(""), "NUL-terminated");
            runs.push(elements);
        }
        runs
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.logs().join("sb.log")).unwrap_or_default()
    }
}

/// `dir`, then the test's own `PATH` (sh, sleep and python3 for the
/// fixtures), joined as std joins a `PATH`
/// (<https://doc.rust-lang.org/std/env/fn.join_paths.html>).
fn path_with(dir: &str) -> std::ffi::OsString {
    let own = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(PathBuf::from(dir)).chain(std::env::split_paths(&own)))
        .expect("a PATH")
}

/// A run's arguments after its working directory and `$0`.
fn args_of(run: &[String]) -> Vec<&str> {
    run[2..].iter().map(String::as_str).collect()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `sb diag`: `<program> saltbox.yml --become --tags diag` in the Saltbox
/// root, attached, with sb-go's lines on stdout and the command in the
/// run log.
#[test]
fn diag_runs_the_diag_tag_in_the_saltbox_repo() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world.sb(&["diag"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let root = world.root().display().to_string();
    assert_eq!(
        world.runs(),
        [vec![
            root.clone(),
            FAKE.to_string(),
            "saltbox.yml".into(),
            "--become".into(),
            "--tags".into(),
            "diag".into(),
        ]]
    );
    assert_eq!(
        stdout(&output),
        format!(
            "Executing Ansible playbook with command: {FAKE} saltbox.yml --become --tags diag\n\
             \nPlaybook {root}/saltbox.yml executed successfully.\n"
        )
    );
    let log = world.log();
    assert!(log.contains("child started"), "{log}");
    assert!(
        log.contains(&format!("{FAKE} saltbox.yml --become --tags diag")),
        "{log}"
    );
    assert!(
        log.contains("way=\"attached\"") || log.contains("way=attached"),
        "{log}"
    );
    assert!(log.contains("playbook program overridden"), "{log}");
}

/// A failed playbook: sb exits with Ansible's own code, exec's block names
/// the command and its code, then sb-go's hint; 99 (Ansible's Ctrl-C) says
/// Ansible was interrupted. No success line.
#[test]
fn a_failed_playbook_passes_its_code_through() {
    if !root_gate::requested() {
        return;
    }
    for (code, hint) in [
        (2, "Scroll up to the failed task to review."),
        (4, "Scroll up to the failed task to review."),
        (250, "Scroll up to the failed task to review."),
        (99, "Ansible was interrupted."),
    ] {
        let world = World::new();
        let output = world
            .sb(&["diag"])
            .env("FAKE_ANSIBLE_EXIT", code.to_string())
            .output()
            .expect("run sb");
        assert_eq!(output.status.code(), Some(code), "{}", stderr(&output));
        let err = console::strip_ansi_codes(&stderr(&output)).into_owned();
        assert!(
            err.contains(&format!(
                "{FAKE} saltbox.yml --become --tags diag exited with code {code}"
            )),
            "{code}: {err}"
        );
        assert!(err.trim_end().ends_with(hint), "{code}: {err}");
        assert!(!stdout(&output).contains("executed successfully"));
        assert_eq!(world.runs().len(), 1);
    }
}

/// A playbook killed by a signal sb never got (the OOM killer's
/// SIGKILL): sb exits 137, with exec's block and sb-go's hint.
#[test]
fn a_playbook_killed_on_its_own_exits_128_plus_the_signal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb(&["diag"])
        .env("FAKE_ANSIBLE_MODE", "self-kill")
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(137), "{}", stderr(&output));
    let err = console::strip_ansi_codes(&stderr(&output)).into_owned();
    assert!(err.contains("was killed by SIGKILL"), "{err}");
    assert!(
        err.trim_end()
            .ends_with("Scroll up to the failed task to review."),
        "{err}"
    );
}

/// A playbook program that can't start is sb's error block, exit 1.
#[test]
fn a_missing_program_is_a_spawn_error() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let missing = world.dir.path().join("no-such-playbook");
    let output = world
        .sb(&["diag"])
        .env("SB_ANSIBLE_PLAYBOOK", &missing)
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains(&format!("could not run {}", missing.display())),
        "{}",
        stderr(&output)
    );
}

/// `diag` takes no arguments (sb-go's `cobra.NoArgs`): clap's usage
/// error, exit 2, nothing run.
#[test]
fn diag_takes_no_arguments() {
    let world = World::new();
    let output = world.sb(&["diag", "extra"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// `sb install`: the tags joined, split and trimmed, routed by prefix,
/// and run Saltbox, then `saltbox_mod`, then Sandbox, each in its own
/// directory with its own playbook, the tags joined with `,` in one
/// element; each run's `Executing...` line shows its tags (decision 17).
#[test]
fn install_runs_each_repo_in_sb_gos_order() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb(&["install", "plex,sandbox-tautulli", "mod-jf", " sonarr ,"])
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let runs = world.runs();
    let dirs: Vec<&str> = runs.iter().map(|run| run[0].as_str()).collect();
    let saltbox = world.root().display().to_string();
    let modded = world.appdata().join("saltbox_mod").display().to_string();
    let sandbox = world.appdata().join("sandbox").display().to_string();
    assert_eq!(dirs, [&saltbox, &modded, &sandbox]);
    assert_eq!(
        args_of(&runs[0]),
        ["saltbox.yml", "--become", "--tags", "plex,sonarr"]
    );
    assert_eq!(
        args_of(&runs[1]),
        ["saltbox_mod.yml", "--become", "--tags", "jf"]
    );
    assert_eq!(
        args_of(&runs[2]),
        ["sandbox.yml", "--become", "--tags", "tautulli"]
    );
    let out = stdout(&output);
    for (playbook, tags) in [
        ("saltbox.yml", "plex,sonarr"),
        ("saltbox_mod.yml", "jf"),
        ("sandbox.yml", "tautulli"),
    ] {
        assert!(
            out.contains(&format!(
                "Executing Ansible playbook with command: {FAKE} {playbook} --become --tags {tags}\n"
            )),
            "{out}"
        );
    }
    for path in [
        format!("{saltbox}/saltbox.yml"),
        format!("{modded}/saltbox_mod.yml"),
        format!("{sandbox}/sandbox.yml"),
    ] {
        assert!(
            out.contains(&format!("\nPlaybook {path} executed successfully.\n")),
            "{out}"
        );
    }
    // A valid cache: no runner, no step.
    assert_eq!(stderr(&output), "");
}

/// `-v` is counted and reaches every playbook as one `-v...` element of
/// the same count, past Ansible's 6 too; none gives no element
/// (capability map, open items; spec success criterion 1).
#[test]
fn verbosity_reaches_every_playbook_as_one_element() {
    if !root_gate::requested() {
        return;
    }
    let cases: [(&[&str], Option<&str>); 9] = [
        (&[], None),
        (&["-v"], Some("-v")),
        (&["-vv"], Some("-vv")),
        (&["-vvv"], Some("-vvv")),
        (&["-vvvv"], Some("-vvvv")),
        (&["-v", "-v"], Some("-vv")),
        (&["--verbose", "--verbose", "--verbose"], Some("-vvv")),
        (&["-vvvvvvv"], Some("-vvvvvvv")),
        (&["-v", "--verbose", "-vv"], Some("-vvvv")),
    ];
    for (flags, expected) in cases {
        let world = World::new();
        let mut args = vec!["install", "plex", "mod-a", "sandbox-b"];
        args.extend_from_slice(flags);
        let output = world.sb(&args).output().expect("run sb");
        assert!(output.status.success(), "{flags:?}: {}", stderr(&output));
        let runs = world.runs();
        assert_eq!(runs.len(), 3, "{flags:?}");
        for run in &runs {
            let verbose: Vec<&str> = args_of(run)
                .into_iter()
                .filter(|arg| arg.starts_with("-v"))
                .collect();
            assert_eq!(
                verbose,
                expected.into_iter().collect::<Vec<_>>(),
                "{flags:?}"
            );
            if let Some(expected) = expected {
                assert_eq!(args_of(run).last(), Some(&expected), "{flags:?}: last");
            }
        }
    }
}

/// The `-e` cases of the spec: each value reaches the fake as one
/// `--extra-vars=<value>` element, byte for byte and in order, never split
/// on commas, joined or passed through a shell; `--skip-tags` joined into
/// one element; every playbook gets the same (capability map, open
/// items; spec success criterion 2).
#[test]
fn every_extra_vars_value_reaches_ansible_whole() {
    if !root_gate::requested() {
        return;
    }
    let values = [
        r#"{"a": 1, "b": [2, 3]}"#,
        r#"{"msg": "say \"hi\", ok", "q": "it's"}"#,
        r#"{"name": "Zoë 名前 🚀"}"#,
        "-x=1",
        "--no-cache",
        "@vars.yml",
        "greeting=hello world",
        "",
        "a=1,b=2",
        "line\nbreak=$HOME `id` ;|&",
    ];
    let world = World::new();
    let mut args = vec!["install", "plex", "sandbox-b"];
    for value in values {
        args.extend(["-e", value]);
    }
    args.extend(["-s", "x,y", "--skip-tags", "z"]);
    let output = world.sb(&args).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let expected: Vec<String> = values
        .iter()
        .map(|value| format!("--extra-vars={value}"))
        .chain(["--skip-tags=x,y,z".to_string()])
        .collect();
    let runs = world.runs();
    assert_eq!(runs.len(), 2);
    for run in &runs {
        assert_eq!(args_of(run)[4..], expected, "{run:?}");
    }
    // The `Executing...` line shows each as given, escaped as sb's
    // screen text is (the newline as `\n`).
    let shown = format!(
        "--tags plex {} --skip-tags=x,y,z\n",
        expected[..values.len()].join(" ").replace('\n', "\\n")
    );
    assert!(stdout(&output).contains(&shown), "{}", stdout(&output));
}

/// Several `-e` keep their order: `-e a=1 -e '{"b": 2}' -e @f.yml` are
/// three elements in that order.
#[test]
fn several_extra_vars_keep_their_order() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb(&[
            "install",
            "plex",
            "-e",
            "a=1",
            "-e",
            r#"{"b": 2}"#,
            "-e",
            "@f.yml",
        ])
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        args_of(&world.runs()[0])[4..],
        [
            "--extra-vars=a=1",
            r#"--extra-vars={"b": 2}"#,
            "--extra-vars=@f.yml"
        ]
    );
}

/// The same values through ansible-core's own argparse definitions: each
/// arrives whole, a leading `-` included, `@file` resolved in the
/// repository the playbook runs in; `-v` counted; the tags one value.
#[test]
fn ansibles_argparse_receives_each_value_whole() {
    if !root_gate::requested() {
        return;
    }
    let values = [
        r#"{"a": 1, "b": [2, 3]}"#,
        r#"{"msg": "say \"hi\", ok", "q": "it's"}"#,
        r#"{"name": "Zoë 名前 🚀"}"#,
        "-x=1",
        "--no-cache",
        "greeting=hello world",
        "",
        "@vars.yml",
    ];
    let world = World::new();
    let mut args = vec!["install", "plex,sonarr", "sandbox-b", "-vvv", "-s", "x,y"];
    for value in values {
        args.extend(["-e", value]);
    }
    let output = world
        .sb(&args)
        .env("SB_ANSIBLE_PLAYBOOK", ARGPARSE)
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let parsed = world.parsed();
    assert_eq!(parsed.len(), 2);
    for (run, (playbook, dir, tags)) in parsed.iter().zip([
        ("saltbox.yml", world.root(), "plex,sonarr"),
        ("sandbox.yml", world.appdata().join("sandbox"), "b"),
    ]) {
        let mut expected: Vec<String> = values[..values.len() - 1]
            .iter()
            .map(ToString::to_string)
            .collect();
        // The fixture resolves it as Ansible does, symlinks followed.
        let dir = dir.canonicalize().expect("the repository");
        expected.push(format!("@{}", dir.join("vars.yml").display()));
        assert_eq!(run["extra_vars"], serde_json::json!(expected), "{run}");
        assert_eq!(run["args"], serde_json::json!([playbook]), "{run}");
        assert_eq!(run["tags"], serde_json::json!([tags]), "{run}");
        assert_eq!(run["skip_tags"], serde_json::json!(["x,y"]), "{run}");
        assert_eq!(run["verbosity"], 3, "{run}");
        assert_eq!(run["become"], true, "{run}");
    }
}

/// `sb install x -e '{"a": 1, "b": [2, 3]}'` shows the JSON whole, as
/// sb-go does, in the `Executing...` line and in the run log's start
/// record, which quotes its fields as tracing's `Debug` does (spec success
/// criterion 5; the maintainer, 2026-11-14).
#[test]
fn an_extra_vars_json_is_shown_and_logged_whole() {
    if !root_gate::requested() {
        return;
    }
    let json = r#"{"a": 1, "b": [2, 3]}"#;
    let world = World::new();
    let output = world
        // `x` isn't in the seeded cache: `--no-cache` skips the validation.
        .sb(&["install", "x", "--no-cache", "-e", json])
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let command = format!("{FAKE} saltbox.yml --become --tags x --extra-vars={json}");
    assert!(
        stdout(&output).contains(&format!(
            "Executing Ansible playbook with command: {command}\n"
        )),
        "{}",
        stdout(&output)
    );
    let log = world.log();
    let logged = format!("command={command:?} masked=0 ");
    assert!(
        log.lines()
            .any(|line| line.contains("child started") && line.contains(&logged)),
        "{log}"
    );
}

/// Every `-e` value is shown and logged as given, in the `Executing...`
/// line, the run log and exec's failure block: a value to keep out of
/// them goes in an `@file` (spec success criterion 5; the maintainer,
/// 2026-11-14).
#[test]
fn an_extra_vars_value_is_in_the_line_the_log_and_the_failure() {
    if !root_gate::requested() {
        return;
    }
    let token = "tok-6b1c9e0d-not-real";
    let world = World::new();
    let value = format!(r#"{{"api_token": "{token}"}}"#);
    let output = world
        .sb(&["install", "plex", "-e", &value, "-e", &format!("t={token}")])
        .env("FAKE_ANSIBLE_EXIT", "2")
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    // It reached Ansible, whole.
    assert!(args_of(&world.runs()[0]).contains(&format!("--extra-vars={value}").as_str()));
    let (out, err, log) = (stdout(&output), stderr(&output), world.log());
    let shown = format!("--tags plex --extra-vars={value} --extra-vars=t={token}");
    assert!(out.contains(&shown), "{out}");
    let err = console::strip_ansi_codes(&err).into_owned();
    assert!(
        err.contains(&format!("{shown} exited with code 2")),
        "{err}"
    );
    assert!(log.contains("child started"), "{log}");
    assert!(log.contains(&format!("--extra-vars=t={token}")), "{log}");
}

/// A failed playbook stops the rest: sb exits with its code, and the later
/// playbooks don't run.
#[test]
fn a_failed_playbook_stops_the_later_ones() {
    if !root_gate::requested() {
        return;
    }
    for code in [2, 4, 250] {
        let world = World::new();
        let output = world
            .sb(&["install", "plex", "mod-a", "sandbox-b"])
            .env("FAKE_ANSIBLE_EXIT", code.to_string())
            .output()
            .expect("run sb");
        assert_eq!(output.status.code(), Some(code), "{}", stderr(&output));
        let runs = world.runs();
        assert_eq!(runs.len(), 1, "{code}: only Saltbox ran");
        assert_eq!(args_of(&runs[0])[0], "saltbox.yml");
        assert!(
            stderr(&output)
                .trim_end()
                .ends_with("Scroll up to the failed task to review."),
            "{}",
            stderr(&output)
        );
    }
}

/// In an LXC container nothing runs: host's text, exit 1. The check comes
/// before the tags are read, as sb-go's does.
#[test]
fn lxc_refuses_before_anything_runs() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    for tags in ["plex", ","] {
        let output = world
            .sb(&["install", tags])
            .env("PATH", path_with(IN_LXC))
            .output()
            .expect("run sb");
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        assert!(
            stderr(&output).contains(
                "UNSUPPORTED VIRTUALIZATION - Install cancelled: Running in an LXC container is not supported"
            ),
            "{}",
            stderr(&output)
        );
    }
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// Tags that leave nothing are sb-go's `no tags provided`, exit 1; no
/// argument at all is clap's usage error, exit 2.
#[test]
fn no_tags_is_an_error() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world.sb(&["install", " , ", ""]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("no tags provided"),
        "{}",
        stderr(&output)
    );
    let output = world.sb(&["install"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// `--force-disk-full` fails at once with host's text for the appdata
/// root, exit 1; nothing runs.
#[test]
fn force_disk_full_fails_with_hosts_text() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb(&["install", "plex", "--force-disk-full"])
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(
        err.contains(&format!(
            "INSUFFICIENT DISK SPACE - Install cancelled: {} is 100.0% full",
            world.appdata().display()
        )),
        "{err}"
    );
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// `--no-cache` runs the playbook with no tag listing and no validation:
/// one run, by tag, a tag no repository has included (a role in
/// development), with no cache at all.
#[test]
fn no_cache_runs_without_a_listing() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    std::fs::remove_file(world.root().join("cache.json")).expect("no cache");
    let output = world
        .sb(&["install", "my-role", "--no-cache"])
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let runs = world.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        args_of(&runs[0]),
        ["saltbox.yml", "--become", "--tags", "my-role"]
    );
}

/// A tag with a suggestion stops the run before anything runs: sb-go's
/// layout on stderr, Saltbox's suggestions then Sandbox's, each sorted by
/// the typed tag; exit 1. Mod tags aren't checked. The run log has the
/// count, not the tags (decision 17).
#[test]
fn a_tag_with_a_suggestion_runs_nothing() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb(&[
            "install",
            "plx,tautulli",
            "sandbox-sonarr",
            "kubernetes",
            "mod-anything",
            "sonarr",
        ])
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
    let (err, log_file) = (stderr(&output), world.logs().join("sb.log"));
    let (shown, pointer) = err.split_once("\n  details: ").expect("the log pointer");
    assert!(
        pointer.starts_with(&format!("{} (run ", log_file.display())),
        "{pointer}"
    );
    assert_eq!(
        format!("{shown}\n"),
        "✗ Tag validation found some issues:\n\
         \n  Tag: kubernetes not present in Saltbox or Sandbox\
         \n  Add: --no-cache if developing your own role\n\
         \n  Tag: plx not present in Saltbox\
         \n  Did you mean: plex\n\
         \n  Tag: tautulli not present in Saltbox\
         \n  Try: sandbox-tautulli (from Sandbox)\n\
         \n  Tag: sandbox-sonarr not present in Sandbox\
         \n  Try: sonarr (from Saltbox)\n\n"
    );
    assert_eq!(stdout(&output), "");
    let log = world.log();
    assert!(log.contains("tag validation found some issues"), "{log}");
    assert!(log.contains("count=4"), "{log}");
    for tag in ["kubernetes", "plx", "tautulli"] {
        assert!(!log.contains(tag), "{tag}: {log}");
    }
}

/// No cache: each repository's tags are listed in a step of a runner
/// titled `sb install`, Saltbox first, written, then the playbooks run.
/// A valid cache shows nothing (the run-order test's empty stderr).
#[test]
fn a_missing_cache_is_rebuilt_as_steps_before_the_runs() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let sandbox = world.appdata().join("sandbox");
    std::fs::remove_file(world.root().join("cache.json")).expect("no cache");
    tag_cache::listing(&world.root(), &tag_cache::SALTBOX_TAGS);
    tag_cache::listing(&sandbox, &tag_cache::SANDBOX_TAGS);
    let output = world
        .sb(&["install", "plex", "sandbox-b"])
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let runs = world.runs();
    let listing = ["--become", "--list-tags", "--skip-tags=always"];
    assert_eq!(args_of(&runs[0])[1..], listing);
    assert_eq!(args_of(&runs[0])[0], "saltbox.yml");
    assert_eq!(args_of(&runs[1])[1..], listing);
    assert_eq!(args_of(&runs[1])[0], "sandbox.yml");
    assert_eq!(
        args_of(&runs[2]),
        ["saltbox.yml", "--become", "--tags", "plex"]
    );
    assert_eq!(
        args_of(&runs[3]),
        ["sandbox.yml", "--become", "--tags", "b"]
    );
    let err = stderr(&output);
    assert!(err.starts_with("sb install\n"), "{err}");
    let saltbox = err.find("Updating Saltbox tags cache");
    let sandbox_step = err.find("Updating Sandbox tags cache");
    assert!(saltbox.is_some() && saltbox < sandbox_step, "{err}");
    // install's rebuild steps stay once done (only sb list's go,
    // decision 18).
    assert!(err.contains("\n✓ Updating Saltbox tags cache"), "{err}");
    assert!(err.contains("\n✓ Updating Sandbox tags cache"), "{err}");
    let cache = std::fs::read_to_string(world.root().join("cache.json")).expect("written");
    assert!(cache.contains("tautulli"), "{cache}");
}

/// Saltbox's tags that can't be had are fatal, with sb-go's text, and
/// nothing runs: a HEAD that can't be read (repo's error below it), or a
/// listing that fails (its step shows why).
#[test]
fn a_broken_saltbox_is_fatal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    // An unborn branch: no HEAD. (Removing `.git` alone would leave git
    // finding the repository the test's directory is in.)
    std::fs::remove_dir_all(world.root().join(".git")).expect("no repository");
    tag_cache::git(&world.root(), &["init", "--quiet"]);
    let output = world.sb(&["install", "plex"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(
        err.contains("✗ saltbox install appears broken: tags cache missing or empty"),
        "{err}"
    );
    assert!(err.contains("caused by:"), "{err}");
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());

    let world = World::new();
    std::fs::remove_file(world.root().join("cache.json")).expect("no cache");
    tag_cache::listing(&world.root(), &tag_cache::SALTBOX_TAGS);
    let output = world
        .sb(&["install", "plex"])
        .env("FAKE_ANSIBLE_EXIT", "4")
        .env(
            "FAKE_ANSIBLE_STDERR",
            "ERROR! the playbook could not be parsed",
        )
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("✗ Updating Saltbox tags cache"), "{err}");
    assert!(
        err.contains("ERROR! the playbook could not be parsed"),
        "{err}"
    );
    assert!(
        err.contains("saltbox install appears broken: tags cache missing or empty"),
        "{err}"
    );
    assert_eq!(world.runs().len(), 1, "only the listing ran");
}

/// No Sandbox checkout counts as no Sandbox tags: Saltbox's tags still
/// run, and a Sandbox tag is then found nowhere.
#[test]
fn no_sandbox_counts_as_no_sandbox_tags() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    std::fs::remove_dir_all(world.appdata().join("sandbox")).expect("no Sandbox");
    let output = world.sb(&["install", "plex"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(world.runs().len(), 1);
    assert!(
        world
            .log()
            .contains("Sandbox tags unavailable; counted as none")
    );
    let output = world
        .sb(&["install", "sandbox-b"])
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Tag: sandbox-b not present in Saltbox or Sandbox"),
        "{}",
        stderr(&output)
    );
    assert_eq!(world.runs().len(), 1, "nothing more ran");
}

/// Mod tags alone read no cache: they run even when Saltbox's tags can't
/// be had.
#[test]
fn mod_tags_alone_read_no_cache() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    std::fs::remove_dir_all(world.root().join(".git")).expect("no repository");
    tag_cache::git(&world.root(), &["init", "--quiet"]);
    let output = world.sb(&["install", "mod-a"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        args_of(&world.runs()[0]),
        ["saltbox_mod.yml", "--become", "--tags", "a"]
    );
}

/// `sb install --help` lists sb-go's flags with their short help and
/// hides `--force-disk-full`.
#[test]
fn install_help_lists_sb_gos_flags() {
    let output = common::sb()
        .args(["install", "--help"])
        .output()
        .expect("run sb");
    assert!(output.status.success());
    let text = console::strip_ansi_codes(&stdout(&output)).into_owned();
    for line in [
        "Extra variables to pass to Ansible",
        "Tags to skip during Ansible playbook execution",
        "Increase verbosity level (can be used multiple times, e.g. -vvv)",
        "Skip cache validation and always perform tag checks",
    ] {
        assert!(text.contains(line), "{text}");
    }
    assert!(!text.contains("force-disk-full"), "{text}");
}

/// `-e`'s long help (ansible spec, `sb install`) shows on `--help`, under
/// the help column, and not on `-h`, which shows the short help.
#[test]
fn install_long_help_is_on_help_only() {
    let help = |flag| {
        let output = common::sb()
            .args(["install", flag, "--color=never"])
            .output()
            .expect("run sb");
        assert!(output.status.success());
        stdout(&output)
    };
    let long = help("--help");
    // Wrapped at 120 columns, under the help column (cli-core spec, Help).
    assert!(
        long.contains("\n                     Each value reaches Ansible whole: key=value, YAML or JSON, or @file (a path relative to the\n                     repository the playbook runs in).\n"),
        "{long}"
    );
    assert!(
        long.contains("Extra variables to pass to Ansible\n                     Each value"),
        "{long}"
    );
    let short = help("-h");
    assert!(!short.contains("Each value reaches"), "{short}");
    assert!(
        short.contains("Extra variables to pass to Ansible\n"),
        "{short}"
    );
}

/// `sb list` from a valid cache: each repository's title with `(cached)`,
/// its tags in columns at 80 (stdout isn't a terminal), nothing on
/// stderr, no listing run.
#[test]
fn list_shows_the_cached_tags_in_columns() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world.sb(&["list"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "Saltbox tags: (cached)\n\nalways  plex  sonarr\n\
         \nSandbox tags (prepend sandbox-): (cached)\n\nalways  b  tautulli\n"
    );
    assert_eq!(stderr(&output), "");
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// No cache: each repository is listed in a transient step of a runner
/// titled `sb list`, written, and shown without `(cached)`; piped, the
/// steps that succeeded print nothing, nor the title (decision 18).
#[test]
fn list_rebuilds_a_missing_cache_as_steps() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    std::fs::remove_file(world.root().join("cache.json")).expect("no cache");
    tag_cache::listing(&world.root(), &["always", "plex", "sonarr"]);
    tag_cache::listing(&world.appdata().join("sandbox"), &["always", "b"]);
    let output = world.sb(&["list"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "Saltbox tags:\n\nalways  plex  sonarr\n\
         \nSandbox tags (prepend sandbox-):\n\nalways  b\n"
    );
    assert_eq!(stderr(&output), "");
    assert_eq!(world.runs().len(), 2);
    assert!(world.root().join("cache.json").exists());
    // The steps are logged as every step is.
    let log = world.log();
    assert!(log.contains("Updating Saltbox tags cache"), "{log}");
    assert!(log.contains("Updating Sandbox tags cache"), "{log}");
}

/// A rebuild that fails keeps the whole tree under the `sb list` title,
/// the rebuild that succeeded before it included, piped as on a terminal
/// (decision 18, the maintainer 2026-11-14).
#[test]
fn list_keeps_the_whole_tree_when_a_rebuild_fails() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let sandbox = world.appdata().join("sandbox");
    std::fs::remove_file(world.root().join("cache.json")).expect("no cache");
    tag_cache::listing(&world.root(), &["always", "plex"]);
    tag_cache::listing(&sandbox, &["always", "b"]);
    std::fs::write(sandbox.join(".fake-ansible/FAKE_ANSIBLE_EXIT"), "4").expect("exit");
    std::fs::write(
        sandbox.join(".fake-ansible/FAKE_ANSIBLE_STDERR"),
        "ERROR! the playbook could not be found",
    )
    .expect("stderr");
    let output = world.sb(&["list"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Saltbox tags:\n\nalways  plex\n");
    let err = stderr(&output);
    assert!(
        err.starts_with("sb list\n- Updating Saltbox tags cache\n✓ Updating Saltbox tags cache   "),
        "{err}"
    );
    assert!(
        err.contains("\n- Updating Sandbox tags cache\n✗ Updating Sandbox tags cache   exit 4 · "),
        "{err}"
    );
    assert!(
        err.contains("ERROR! the playbook could not be found"),
        "{err}"
    );
}

/// `-m`: `saltbox_mod` listed fresh after the others, never cached; with
/// no directory, sb-go's notice first and the rest as usual.
#[test]
fn list_include_mod_lists_saltbox_mod_fresh() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let modded = world.appdata().join("saltbox_mod");
    tag_cache::listing(&modded, &["always", "jf"]);
    let output = world.sb(&["list", "-m"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.ends_with("\nSaltbox_mod tags (prepend mod-):\n\nalways  jf\n"),
        "{out}"
    );
    // Its listing's step is transient: piped, a success prints nothing,
    // nor the runner's title (the maintainer, 2026-11-14).
    assert_eq!(stderr(&output), "");
    let runs = world.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0][0], modded.display().to_string());
    assert_eq!(
        args_of(&runs[0]),
        [
            "saltbox_mod.yml",
            "--become",
            "--list-tags",
            "--skip-tags=always"
        ]
    );
    let cache = std::fs::read_to_string(world.root().join("cache.json")).expect("cache");
    assert!(!cache.contains("saltbox_mod"), "{cache}");

    std::fs::remove_dir_all(&modded).expect("no saltbox_mod");
    let output = world
        .sb(&["list", "--include-mod"])
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with(
            "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.\n\
             Saltbox tags: (cached)\n"
        ),
        "{}",
        stdout(&output)
    );
}

/// A failing repository between two good ones: the others print, then
/// one error naming it, exit 1.
#[test]
fn list_reports_a_failing_repo_after_the_others() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let sandbox = world.appdata().join("sandbox");
    std::fs::remove_dir_all(&sandbox).expect("no Sandbox");
    tag_cache::listing(&world.appdata().join("saltbox_mod"), &["jf"]);
    let output = world.sb(&["list", "-m"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "Saltbox tags: (cached)\n\nalways  plex  sonarr\n\
         \nSaltbox_mod tags (prepend mod-):\n\njf\n"
    );
    let err = stderr(&output);
    assert!(
        err.contains("✗ errors occurred while listing tags"),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "error running and caching ansible tags for {}",
            sandbox.display()
        )),
        "{err}"
    );
}

/// A search: sb-go's heading line and one table, later repositories as
/// spanning rows; no match says so, exit 0.
#[test]
fn list_with_a_query_searches_every_repo() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world.sb(&["list", "TAUTULI"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "Found 1 matching tag(s) for 'TAUTULI':\n\n\
         ╭────────────────────────────────────────╮\n\
         │                Sandbox                 │\n\
         ├──────────┬─────────────────────────────┤\n\
         │ tautulli │ sb install sandbox-tautulli │\n\
         ╰──────────┴─────────────────────────────╯\n\n"
    );
    let output = world.sb(&["list", "zzzzzz"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "No tags found matching 'zzzzzz'\n");
}

/// `sb list` has no `-v` (spec, difference 8): a usage error, exit 2.
#[test]
fn list_has_no_verbose_flag() {
    let output = common::sb().args(["list", "-v"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}

/// `saltbox_mod`'s failures are its own: a listing that fails, or a
/// directory that can't be inspected (a symlink loop, not a missing
/// directory: no notice), each a line of the error, the others printed.
#[test]
fn list_reports_saltbox_mods_failures() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let modded = world.appdata().join("saltbox_mod");
    tag_cache::listing(&modded, &["jf"]);
    std::fs::write(modded.join(".fake-ansible/FAKE_ANSIBLE_EXIT"), "4").expect("exit");
    std::fs::write(
        modded.join(".fake-ansible/FAKE_ANSIBLE_STDERR"),
        "ERROR! the playbook could not be found",
    )
    .expect("stderr");
    let output = world.sb(&["list", "-m"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    // The failed listing's step stays, with its cause, as any step's.
    let err = stderr(&output);
    assert!(
        err.starts_with(
            "sb list\n- Listing Saltbox_mod tags\n✗ Listing Saltbox_mod tags   exit 4 · "
        ),
        "{err}"
    );
    assert!(
        err.contains("ERROR! the playbook could not be found"),
        "{err}"
    );
    assert!(
        stdout(&output).contains("Sandbox tags"),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains(&format!(
            "error running ansible list tags for {}",
            modded.display()
        )),
        "{}",
        stderr(&output)
    );

    std::fs::remove_dir_all(&modded).expect("no saltbox_mod");
    std::os::unix::fs::symlink(&modded, &modded).expect("a loop");
    let output = world.sb(&["list", "-m"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        !stdout(&output).contains("not found"),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("inspect Saltbox-mod directory:"),
        "{}",
        stderr(&output)
    );
}

/// On a terminal, the columns fit its width (console's size, as term-ui's
/// tables read it), not the 80 of piped output.
#[test]
fn list_fits_the_terminals_width() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let tags: Vec<String> = (0..30).map(|n| format!("tag-{n:02}")).collect();
    // Saltbox's entry with these tags, Sandbox's as seeded: no listing.
    let path = world.root().join("cache.json");
    let mut cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("cache")).expect("JSON");
    cache[world.root().display().to_string()]["tags"] = serde_json::json!(tags);
    std::fs::write(&path, cache.to_string()).expect("cache");
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = Command::new("script");
    common::apply_sb_env(&mut cmd);
    let output = cmd
        .args([
            "-qec",
            &format!("stty cols 40 rows 24; exec '{program}' list"),
            "/dev/null",
        ])
        .env("SB_SALTBOX_DIR", world.root())
        .env("SB_LOG_DIR", world.logs())
        .env("SB_ANSIBLE_PLAYBOOK", FAKE)
        .env("TERM", "xterm-256color")
        .output()
        .expect("run script");
    let out = console::strip_ansi_codes(&stdout(&output)).replace('\r', "");
    // 30 tags of 6 characters: 5 to a 40-column line (80 would fit 10).
    assert!(
        out.contains("tag-00  tag-06  tag-12  tag-18  tag-24\n"),
        "{out}"
    );
    assert!(
        out.lines()
            .filter(|line| line.starts_with("tag-"))
            .all(|line| line.chars().count() <= 40),
        "{out}"
    );
}

/// On a terminal, `saltbox_mod`'s listing shows as a step while it runs
/// (`Listing Saltbox_mod tags`, the maintainer's text, 2026-11-14), and
/// nothing of it, nor the runner's title, is left on the screen once the
/// tags print. The screen is the PTY's bytes read through vt100.
#[test]
fn list_shows_the_mod_listing_only_while_it_runs() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    tag_cache::listing(&world.appdata().join("saltbox_mod"), &["always", "jf"]);
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = Command::new("script");
    common::apply_sb_env(&mut cmd);
    let output = cmd
        .args([
            "-qec",
            &format!("stty cols 80 rows 24; exec '{program}' list -m"),
            "/dev/null",
        ])
        .env("SB_SALTBOX_DIR", world.root())
        .env("SB_LOG_DIR", world.logs())
        .env("SB_ANSIBLE_PLAYBOOK", FAKE)
        .env("TERM", "xterm-256color")
        .output()
        .expect("run script");
    assert!(output.status.success(), "{output:?}");
    let drawn = console::strip_ansi_codes(&stdout(&output)).into_owned();
    assert!(drawn.contains("Listing Saltbox_mod tags"), "{drawn:?}");
    let mut screen = vt100::Parser::new(24, 80, 0);
    screen.process(&output.stdout);
    let shown = screen.screen().contents();
    assert!(shown.starts_with("Saltbox tags: (cached)\n"), "{shown}");
    assert!(
        shown.contains("Saltbox_mod tags (prepend mod-):\n\nalways  jf"),
        "{shown}"
    );
    assert!(!shown.contains("Listing"), "{shown}");
    assert!(!shown.contains("sb list"), "{shown}");
    // The step is logged as every step is.
    let log = world.log();
    assert!(log.contains("Listing Saltbox_mod tags"), "{log}");
}

/// On a terminal, a stale cache's rebuild steps show while they run and
/// are gone, the `sb list` title with them, once the tags print
/// (decision 18). The screen is the PTY's bytes read through vt100.
#[test]
fn list_shows_the_rebuild_only_while_it_runs() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    std::fs::remove_file(world.root().join("cache.json")).expect("no cache");
    tag_cache::listing(&world.root(), &["always", "plex"]);
    tag_cache::listing(&world.appdata().join("sandbox"), &["always", "b"]);
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = Command::new("script");
    common::apply_sb_env(&mut cmd);
    let output = cmd
        .args([
            "-qec",
            &format!("stty cols 80 rows 24; exec '{program}' list"),
            "/dev/null",
        ])
        .env("SB_SALTBOX_DIR", world.root())
        .env("SB_LOG_DIR", world.logs())
        .env("SB_ANSIBLE_PLAYBOOK", FAKE)
        .env("TERM", "xterm-256color")
        .output()
        .expect("run script");
    assert!(output.status.success(), "{output:?}");
    let drawn = console::strip_ansi_codes(&stdout(&output)).into_owned();
    assert!(drawn.contains("Updating Saltbox tags cache"), "{drawn:?}");
    assert!(drawn.contains("Updating Sandbox tags cache"), "{drawn:?}");
    let mut screen = vt100::Parser::new(24, 80, 0);
    screen.process(&output.stdout);
    let shown = screen.screen().contents();
    assert_eq!(
        shown.trim_end(),
        "Saltbox tags:\n\nalways  plex\n\nSandbox tags (prepend sandbox-):\n\nalways  b",
        "{shown}"
    );
}

/// A search with `-m` and no `saltbox_mod` prints sb-go's notice first, as
/// the listing does; an empty query is no query (sb-go: `query != ""`).
#[test]
fn list_search_shows_the_mod_notice_and_an_empty_query_lists() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    std::fs::remove_dir_all(world.appdata().join("saltbox_mod")).expect("no saltbox_mod");
    let output = world.sb(&["list", "plex", "-m"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with(
            "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.\n\
             Found 1 matching tag(s) for 'plex':\n"
        ),
        "{}",
        stdout(&output)
    );
    let output = world.sb(&["list", ""]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("Saltbox tags: (cached)\n"),
        "{}",
        stdout(&output)
    );
}
