//! `completion install` under a temporary root (completion spec, Testing
//! strategy, "Install, under a root prefix"): stand-in shells, sh scripts
//! in the root's `usr/bin`, answer the probes, fail them, hang, or report
//! an old fish. Nothing is written outside the root.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use sb_completion::Shell;
use sb_completion::install::{self, Line, Paths, Report};
use tokio_util::sync::CancellationToken;

/// Tests that write a stand-in shell and run it hold this from writing it
/// until its run is over, so no other test's fork holds the script's
/// write descriptor when it runs (exec fails with ETXTBSY otherwise; as
/// sb-host's `SPAWNS`).
static SPAWNS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A root with an executable `sb` of its own (the names' target).
struct Root {
    dir: tempfile::TempDir,
}

impl Root {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = Self { dir };
        root.write("opt/sb/sb", "#!/bin/sh\n", 0o755);
        root
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn paths(&self) -> Paths {
        Paths::under(self.path())
    }

    fn at(&self, path: &str) -> PathBuf {
        self.path().join(path)
    }

    fn write(&self, path: &str, text: &str, mode: u32) {
        let path = self.at(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, text).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// A stand-in shell in `usr/bin` running `script` for any probe.
    fn shell(&self, name: &str, script: &str) {
        self.write(
            &format!("usr/bin/{name}"),
            &format!("#!/bin/sh\n{script}\n"),
            0o755,
        );
    }

    /// bash with bash-completion, zsh whose clean fpath has the root's
    /// site-functions, fish 3.7 with its sysconf under the root.
    fn all_shells(&self) {
        self.shell("bash", "exit 0");
        self.write("usr/share/bash-completion/bash_completion", "", 0o644);
        let fpath = self.paths().zsh_dir();
        self.shell(
            "zsh",
            &format!(
                "printf '%s\\n' /usr/share/zsh/functions '{}'",
                fpath.display()
            ),
        );
        let sysconf = self.at("etc/fish");
        self.shell(
            "fish",
            &format!("printf '3.7.0\\n%s\\n' '{}'", sysconf.display()),
        );
    }

    fn executable(&self) -> PathBuf {
        self.at("opt/sb/sb")
    }

    fn names(&self) -> Vec<install::Name> {
        install::names(&self.paths(), &self.executable()).expect("names")
    }

    fn install(&self, shells: &[Shell]) -> Report {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let names = self.names();
        let report = runtime.block_on(install::install(
            &self.paths(),
            shells,
            &names,
            &CancellationToken::new(),
        ));
        // Every file written is under the root: a stand-in that reported
        // a directory outside it would write the host's.
        for line in &report.lines {
            if let Line::Installed { path, .. } = line {
                assert!(
                    path.starts_with(self.path()),
                    "outside the root: {}",
                    path.display()
                );
            }
        }
        report
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.at(path)).expect("read")
    }

    fn mode(&self, path: &str) -> u32 {
        std::fs::metadata(self.at(path))
            .expect("meta")
            .permissions()
            .mode()
            & 0o7777
    }
}

fn lines(report: &Report) -> Vec<String> {
    report.lines.iter().map(ToString::to_string).collect()
}

const BASH: &str = "usr/local/share/bash-completion/completions/sb.bash";
const ZSH: &str = "usr/local/share/zsh/site-functions/_sb";
const FISH: &str = "etc/fish/completions/sb.fish";

/// Every shell installed: each file where its probe says, 0644, its
/// directories 0755, the marker, clap's stub calling sb's own path.
#[test]
fn each_shell_gets_its_file() {
    let _spawns = SPAWNS.lock();
    let root = Root::new();
    root.all_shells();
    let report = root.install(&Shell::ALL);
    let at = |path: &str| root.at(path).display().to_string();
    assert_eq!(
        lines(&report),
        [
            format!("Installed bash completion: {}", at(BASH)),
            format!("Installed zsh completion: {}", at(ZSH)),
            format!("Installed fish completion: {}", at(FISH)),
        ]
    );
    assert_eq!(report.failures, Vec::<String>::new());
    assert_eq!(report.unavailable, Vec::new());
    let sb = root.executable().display().to_string();
    for (path, first) in [
        (BASH, "# sb completion bash sb\n"),
        (ZSH, "#compdef sb\n# sb completion zsh sb\n"),
        (FISH, "# sb completion fish sb\n"),
    ] {
        let text = root.read(path);
        assert!(text.starts_with(first), "{path}: {text}");
        assert!(text.contains(&sb), "{path}: {text}");
        assert_eq!(root.mode(path), 0o644, "{path}");
    }
    for dir in [
        "usr/local/share/bash-completion",
        "usr/local/share/zsh/site-functions",
        "etc/fish/completions",
    ] {
        assert_eq!(root.mode(dir), 0o755, "{dir}");
    }
    // Again over the files: replaced, the same.
    let before = root.read(ZSH);
    assert_eq!(lines(&root.install(&[Shell::Zsh])).len(), 1);
    assert_eq!(root.read(ZSH), before);
}

/// A shell missing, bash without bash-completion, zsh whose clean fpath
/// lacks the directory, fish older than 3.3: each skipped with sb-go's
/// reason, nothing written, and listed as unavailable.
#[test]
fn unavailable_shells_are_skipped_with_sb_gos_reasons() {
    let _spawns = SPAWNS.lock();
    let root = Root::new();
    root.shell("bash", "exit 0");
    root.shell("zsh", "printf '%s\\n' /usr/share/zsh/functions");
    root.shell("fish", "printf '3.2.2\\n/etc/fish\\n'");
    let report = root.install(&Shell::ALL);
    let zsh_reason = format!(
        "Zsh's clean function path does not contain {}; add it to the administrator fpath before installing",
        root.paths().zsh_dir().display()
    );
    let fish_reason = "Fish 3.3 or newer is required for configuration-free detection";
    assert_eq!(
        lines(&report),
        [
            "Skipped bash completion: bash-completion is not installed".to_owned(),
            format!("Skipped zsh completion: {zsh_reason}"),
            format!("Skipped fish completion: {fish_reason}"),
        ]
    );
    assert_eq!(report.failures, Vec::<String>::new());
    assert_eq!(
        report.unavailable,
        [
            (Shell::Bash, "bash-completion is not installed".to_owned()),
            (Shell::Zsh, zsh_reason),
            (Shell::Fish, fish_reason.to_owned()),
        ]
    );
    assert!(!root.at("usr/local/share").exists());
    let empty = Root::new();
    assert_eq!(
        lines(&empty.install(&[Shell::Fish])),
        ["Skipped fish completion: shell is not installed"]
    );
}

/// A probe that fails, or that finds no executable shell file, is a
/// failure for that shell; the others still install. Each shell once.
#[test]
fn a_failed_probe_fails_only_its_shell() {
    let _spawns = SPAWNS.lock();
    let root = Root::new();
    root.all_shells();
    root.shell("zsh", "echo 'zsh: broken' >&2; exit 3");
    root.write("usr/bin/fish", "", 0o644);
    let report = root.install(&[Shell::Zsh, Shell::Fish, Shell::Bash, Shell::Zsh]);
    let fish = root.at("usr/bin/fish").display().to_string();
    assert_eq!(
        lines(&report),
        [
            "Failed zsh completion: shell probe failed: exit status 3 (zsh: broken)".to_owned(),
            format!("Failed fish completion: fish is not an executable regular file: {fish}"),
            format!("Installed bash completion: {}", root.at(BASH).display()),
        ]
    );
    assert_eq!(
        report.failures,
        [
            "detect zsh: shell probe failed: exit status 3 (zsh: broken)".to_owned(),
            format!("detect fish: fish is not an executable regular file: {fish}"),
        ]
    );
}

/// Under a test root, a directory a shell reports outside that root is
/// refused: a stand-in can't send a write elsewhere (a `..` never gets
/// this far: fish's directory must be clean). The outside directory is
/// another temporary one.
#[test]
fn a_directory_outside_the_root_is_refused() {
    let _spawns = SPAWNS.lock();
    let root = Root::new();
    let outside = tempfile::tempdir().expect("temp dir");
    {
        let sysconf = outside.path().to_path_buf();
        root.shell(
            "fish",
            &format!("printf '3.7.0\\n%s\\n' '{}'", sysconf.display()),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let report = runtime.block_on(install::install(
            &root.paths(),
            &[Shell::Fish],
            &root.names(),
            &CancellationToken::new(),
        ));
        let dir = sysconf.join("completions");
        let reason = format!(
            "completion directory {} is outside the install root {}",
            dir.display(),
            root.path().display()
        );
        assert_eq!(
            report.lines,
            [Line::Failed {
                shell: Shell::Fish,
                reason: reason.clone()
            }],
            "{}",
            sysconf.display()
        );
        assert_eq!(report.failures, [format!("install fish: {reason}")]);
    }
    assert_eq!(std::fs::read_dir(outside.path()).expect("list").count(), 0);
}

/// A probe that hangs is ended after 5 s (sb-go's `probeTimeout`).
#[test]
fn a_hanging_probe_times_out() {
    let _spawns = SPAWNS.lock();
    let root = Root::new();
    root.shell("fish", "exec sleep 30");
    let start = std::time::Instant::now();
    let report = root.install(&[Shell::Fish]);
    assert!(start.elapsed() < std::time::Duration::from_secs(15));
    assert_eq!(
        lines(&report),
        ["Failed fish completion: shell probe interrupted: timed out"]
    );
}

/// A probe sees no environment of sb's: a fixed PATH, HOME=/ and the C
/// locale, and no user configuration flags missing.
#[test]
fn probes_run_clean() {
    let _spawns = SPAWNS.lock();
    let root = Root::new();
    let seen = root.at("seen");
    root.shell(
        "fish",
        &format!(
            "{{ printf '%s|' \"$@\"; echo; env | sort; }} > '{}'; printf '3.3\\n%s\\n' '{}'",
            seen.display(),
            root.at("etc/fish").display()
        ),
    );
    let report = root.install(&[Shell::Fish]);
    assert_eq!(report.failures, Vec::<String>::new());
    let text = std::fs::read_to_string(&seen).expect("seen");
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some(r#"--no-config|-c|printf '%s\n%s\n' "$version" "$__fish_sysconf_dir"|"#)
    );
    let env: Vec<&str> = lines.collect();
    for var in [
        "PATH=/usr/local/bin:/usr/bin:/bin",
        "HOME=/",
        "LC_ALL=C",
        "LANG=C",
    ] {
        assert!(env.contains(&var), "{var}: {env:?}");
    }
    assert!(!env.iter().any(|line| line.starts_with("USER=")), "{env:?}");
}

/// Names: the executable's own, and each symlink to it in the bin
/// directories (a dangling one and one to another file left out); each
/// file calls its own name's path; bash's file holds every name.
#[test]
fn every_name_gets_a_file_calling_its_own_path() {
    let _spawns = SPAWNS.lock();
    let root = Root::new();
    root.all_shells();
    std::fs::create_dir_all(root.at("usr/local/bin")).expect("dir");
    std::os::unix::fs::symlink(root.executable(), root.at("usr/local/bin/saltbox")).expect("link");
    std::os::unix::fs::symlink(root.executable(), root.at("usr/bin/sbx")).expect("link");
    std::os::unix::fs::symlink(root.at("missing"), root.at("usr/local/bin/broken")).expect("link");
    std::os::unix::fs::symlink(root.at("usr/bin/zsh"), root.at("usr/local/bin/other"))
        .expect("link");
    let names: Vec<String> = root
        .names()
        .iter()
        .map(|name| name.name().to_owned())
        .collect();
    assert_eq!(names, ["saltbox", "sb", "sbx"]);
    let report = root.install(&[Shell::Zsh, Shell::Bash]);
    assert_eq!(report.lines.len(), 6, "{:?}", lines(&report));
    let zsh = root.read("usr/local/share/zsh/site-functions/_saltbox");
    assert!(
        zsh.starts_with("#compdef saltbox\n# sb completion zsh saltbox\n"),
        "{zsh}"
    );
    let link = root.at("usr/local/bin/saltbox").display().to_string();
    assert!(zsh.contains(&link), "{zsh}");
    let bash = root.read("usr/local/share/bash-completion/completions/sbx.bash");
    assert!(bash.starts_with("# sb completion bash sbx\n"), "{bash}");
    for name in ["saltbox", "sb", "sbx"] {
        assert!(bash.contains(&format!(" {name}\n")), "{name}: {bash}");
    }
    assert!(
        bash.contains(&root.at("usr/bin/sbx").display().to_string()),
        "{bash}"
    );
}

/// A name outside sb-go's pattern fails the whole install, as sb-go's.
#[test]
fn an_invalid_name_is_an_error() {
    let root = Root::new();
    std::fs::create_dir_all(root.at("usr/local/bin")).expect("dir");
    std::os::unix::fs::symlink(root.executable(), root.at("usr/local/bin/sb copy")).expect("link");
    assert_eq!(
        install::names(&root.paths(), &root.executable()),
        Err("invalid executable name \"sb copy\"".to_owned())
    );
    assert!(install::names(&root.paths(), &root.at("missing")).is_err());
    // A path the stubs can't carry as it is (a space).
    root.write("opt/my sb/sb", "#!/bin/sh\n", 0o755);
    let spaced = root.at("opt/my sb/sb");
    assert_eq!(
        install::names(&root.paths(), &spaced),
        Err(format!(
            "invalid executable path {:?}",
            spaced.display().to_string()
        ))
    );
}

/// A cancelled install starts no shell.
#[test]
fn a_cancelled_install_stops() {
    let root = Root::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let token = CancellationToken::new();
    token.cancel();
    let report = runtime.block_on(install::install(
        &root.paths(),
        &Shell::ALL,
        &root.names(),
        &token,
    ));
    assert_eq!(report.lines, Vec::<Line>::new());
    assert_eq!(report.failures, ["interrupted"]);
}
