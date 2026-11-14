//! One attached run by tag with the fake `ansible-playbook`
//! (tests/fixtures/fake-ansible-playbook): the argv it got, element by
//! element, its working directory, sb's lines, and how each ending is
//! passed on. No Ansible runs.

use std::path::{Path, PathBuf};

use sb_ansible::{Ansible, Repo, RunArgs, RunError, Tags, pass_through};
use sb_cli_core::cancel::Shutdown;
use sb_exec::Ending;

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-ansible-playbook"
);

/// A Saltbox root holding the fake's settings directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        std::fs::create_dir_all(dir.path().join("saltbox/.fake-ansible")).expect("root");
        Self { dir }
    }

    fn repo(&self) -> Repo {
        Repo::saltbox(&self.dir.path().join("saltbox"))
    }

    /// The fake's settings directory: it reads its settings there when
    /// the environment doesn't set them, and writes its records there.
    fn settings(&self) -> PathBuf {
        self.dir.path().join("saltbox/.fake-ansible")
    }

    /// The fake, with this test's settings. They go in files the fake
    /// reads in the directory it runs in, not in the environment, which
    /// cargo's test threads share (the attached child inherits it), and
    /// not in a script written for the test: a program written and run
    /// while another thread forks can fail with ETXTBSY.
    fn fake(&self, settings: &[(&str, &str)]) -> Ansible {
        for (name, value) in settings {
            std::fs::write(self.settings().join(name), value).expect("a setting");
        }
        Ansible::with_program(FAKE)
    }

    /// Each run's record: its working directory, then `$0` and its
    /// arguments.
    fn runs(&self) -> Vec<Vec<String>> {
        let mut runs = Vec::new();
        for n in 0.. {
            let path = self.settings().join(format!("argv.{n}"));
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
}

async fn run(
    ansible: &Ansible,
    repo: &Repo,
    tags: Tags<'_>,
    args: &RunArgs,
) -> (sb_exec::Ran, String) {
    let mut out = Vec::new();
    let ran = ansible
        .run(repo, tags, args, &Shutdown::new(), &mut out)
        .await
        .expect("it ran");
    (ran, String::from_utf8(out).expect("UTF-8"))
}

/// `diag`'s run: the argv element by element, in the repository's
/// directory, with sb-go's lines around it.
#[tokio::test]
async fn diag_runs_in_the_repo_with_its_lines() {
    let world = World::new();
    let ansible = world.fake(&[]);
    let repo = world.repo();
    let (ran, out) = run(&ansible, &repo, Tags::Literal("diag"), &RunArgs::default()).await;
    assert_eq!(ran.outcome.ending, Ending::Exited(0));
    let program = ansible.program().display().to_string();
    assert_eq!(
        world.runs(),
        [vec![
            repo.dir().display().to_string(),
            FAKE.to_string(),
            "saltbox.yml".into(),
            "--become".into(),
            "--tags".into(),
            "diag".into(),
        ]]
    );
    assert_eq!(
        out,
        format!(
            "Executing Ansible playbook with command: {program} saltbox.yml --become --tags diag\n\
             \nPlaybook {} executed successfully.\n",
            repo.playbook_path().display()
        )
    );
    pass_through(ran, &mut Vec::new()).expect("exit 0 passes");
}

/// Given tags and every pass-through argument reach the child whole, in
/// order; the `Executing...` line shows them masked.
#[tokio::test]
async fn every_value_reaches_the_child_whole() {
    let world = World::new();
    let ansible = world.fake(&[]);
    let tags = ["plex".to_string(), "sonarr".to_string()];
    let args = RunArgs {
        extra_vars: vec![
            r#"{"a": 1, "b": [2, 3]}"#.into(),
            "-x=1".into(),
            "greeting=hello world".into(),
            "@vars.yml".into(),
            String::new(),
        ],
        skip_tags: vec!["x".into(), "y".into()],
        verbose: 2,
    };
    let (ran, out) = run(&ansible, &world.repo(), Tags::Given(&tags), &args).await;
    assert_eq!(ran.outcome.ending, Ending::Exited(0));
    assert_eq!(
        world.runs()[0][2..],
        [
            "saltbox.yml",
            "--become",
            "--tags",
            "plex,sonarr",
            r#"--extra-vars={"a": 1, "b": [2, 3]}"#,
            "--extra-vars=-x=1",
            "--extra-vars=greeting=hello world",
            "--extra-vars=@vars.yml",
            "--extra-vars=",
            "--skip-tags=x,y",
            "-vv",
        ]
    );
    let first = out.lines().next().expect("a line");
    assert!(
        first.ends_with(
            " saltbox.yml --become --tags [ARG] --extra-vars=[ARG] --extra-vars=[ARG] \
             --extra-vars=[ARG] --extra-vars=[ARG] --extra-vars=[ARG] --skip-tags=[ARG] -vv"
        ),
        "{first}"
    );
    assert!(!out.contains("plex") && !out.contains("hello"), "{out}");
}

/// A failed playbook: no success line, exec's block naming the command
/// and its code, sb-go's hint, and Ansible's code to end sb with.
#[tokio::test]
async fn a_failure_passes_its_code_with_the_hint() {
    for (code, hint) in [
        ("2", "Scroll up to the failed task to review."),
        ("4", "Scroll up to the failed task to review."),
        ("250", "Scroll up to the failed task to review."),
        ("99", "Ansible was interrupted."),
    ] {
        let world = World::new();
        let ansible = world.fake(&[("FAKE_ANSIBLE_EXIT", code)]);
        let (ran, out) = run(
            &ansible,
            &world.repo(),
            Tags::Literal("diag"),
            &RunArgs::default(),
        )
        .await;
        assert!(!out.contains("executed successfully"), "{out}");
        let mut err = Vec::new();
        let exit = pass_through(ran, &mut err).expect_err("it failed");
        assert_eq!(exit.0.to_string(), code);
        let err = String::from_utf8(err).expect("UTF-8");
        assert!(
            err.contains(&format!(
                "saltbox.yml --become --tags diag exited with code {code}"
            )),
            "{err}"
        );
        assert!(err.ends_with(&format!("{hint}\n")), "{code}: {err}");
    }
}

/// A playbook killed by a signal sb never got (the OOM killer's
/// SIGKILL): sb ends with 128 plus the signal, after exec's block naming
/// it and sb-go's hint.
#[tokio::test]
async fn a_playbook_killed_on_its_own_ends_with_128_plus_the_signal() {
    let world = World::new();
    let ansible = world.fake(&[("FAKE_ANSIBLE_MODE", "self-kill")]);
    let (ran, out) = run(
        &ansible,
        &world.repo(),
        Tags::Literal("diag"),
        &RunArgs::default(),
    )
    .await;
    assert_eq!(ran.outcome.ending, Ending::Signalled(9));
    assert!(!out.contains("executed successfully"), "{out}");
    let mut err = Vec::new();
    let exit = pass_through(ran, &mut err).expect_err("it was killed");
    assert_eq!(exit.0, 137);
    let err = String::from_utf8(err).expect("UTF-8");
    assert!(err.contains("was killed by SIGKILL"), "{err}");
    assert!(
        err.ends_with("Scroll up to the failed task to review.\n"),
        "{err}"
    );
}

/// A missing program, or a missing repository directory, is exec's spawn
/// error, after the `Executing...` line.
#[tokio::test]
async fn a_program_that_cannot_start_is_a_spawn_error() {
    let world = World::new();
    let missing = Ansible::with_program(world.dir.path().join("no-such-playbook"));
    let mut out = Vec::new();
    let err = missing
        .run(
            &world.repo(),
            Tags::Literal("diag"),
            &RunArgs::default(),
            &Shutdown::new(),
            &mut out,
        )
        .await
        .expect_err("not started");
    assert!(matches!(err, RunError::Spawn(_)), "{err:?}");
    assert!(err.to_string().starts_with("could not run "), "{err}");
    assert!(String::from_utf8_lossy(&out).starts_with("Executing Ansible playbook"));

    let ansible = world.fake(&[]);
    let gone = Repo::saltbox(Path::new(&world.dir.path().join("no-saltbox")));
    let err = ansible
        .run(
            &gone,
            Tags::Literal("diag"),
            &RunArgs::default(),
            &Shutdown::new(),
            &mut Vec::new(),
        )
        .await
        .expect_err("no directory");
    assert!(matches!(err, RunError::Spawn(_)), "{err:?}");
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// sb's lines that can't be written are the output error cli-core makes
/// quiet for a broken pipe; nothing runs.
#[tokio::test]
async fn an_unwritable_output_is_an_output_error() {
    struct Closed;
    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let world = World::new();
    let ansible = world.fake(&[]);
    let err = ansible
        .run(
            &world.repo(),
            Tags::Literal("diag"),
            &RunArgs::default(),
            &Shutdown::new(),
            &mut Closed,
        )
        .await
        .expect_err("unwritable");
    assert!(matches!(err, RunError::Output(_)), "{err:?}");
    assert_eq!(sb_cli_core::exit::quiet_exit(&err), Some(141));
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// The argparse fixture parses as ansible-core does: sb-go's two-element
/// `--extra-vars -x=1` is refused (argparse's usage error, exit 2), the
/// one-element form sb sends is taken whole (spec, assumption 6).
#[test]
fn the_argparse_fixture_refuses_what_ansible_refuses() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/argparse-ansible-playbook"
    );
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
    let log = dir.path().join("argv");
    let run = |args: &[&str]| {
        std::process::Command::new(fixture)
            .args(args)
            .env("FAKE_ANSIBLE_LOG", &log)
            .output()
            .expect("python3")
    };
    let refused = run(&["saltbox.yml", "--extra-vars", "-x=1"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("expected one argument"),
        "{refused:?}"
    );
    let taken = run(&["saltbox.yml", "--extra-vars=-x=1"]);
    assert!(taken.status.success(), "{taken:?}");
    let parsed: String = std::fs::read_to_string(dir.path().join("argv.0.json")).expect("record");
    assert!(parsed.contains(r#""extra_vars": ["-x=1"]"#), "{parsed}");
}
