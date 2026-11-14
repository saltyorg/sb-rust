//! Stand-in programs and a Saltbox tree on a temporary root, for
//! sb-python-env's tests and sb's end-to-end tests (python-env spec,
//! Testing Strategy; the `testing` feature, never in a release build).
//! The stand-ins are shell scripts, as sb-host's and sb-repo's; a test
//! that writes one and runs it guards against ETXTBSY as theirs do (a
//! lock held from writing it until its run is over).

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use crate::Layout;

/// The commit the stand-in git reports.
pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The Python the tree pins.
pub const PYTHON: &str = "3.12.7";

/// The uv minimum the tree pins, and the uv its stand-ins report.
pub const UV: &str = "0.13.4";

/// The lock the tree holds.
pub const LOCK: &str = "ansible==12.0.0 --hash=sha256:00\n";

/// An executable `name` in `dir` running `script` with `sh`.
///
/// # Panics
///
/// When the file can't be written.
#[must_use = "the stand-in's path is what the test runs"]
pub fn stand_in(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(dir).expect("stand-in dir");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write stand-in");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// A script line appending what the child got to `record`: its argv, its
/// working directory and its environment (`env`, one per line), between
/// markers.
#[must_use]
pub fn record_line(record: &Path) -> String {
    let record = record.display();
    format!("{{ echo \"argv: $0 $*\"; echo \"cwd: $(pwd)\"; env | sort; echo --; }} >> '{record}'")
}

/// A Saltbox tree on a temporary root: the checkout's pins and lock, the
/// scratch directory, a stand-in git, and a uv and uvx reporting [`UV`].
#[derive(Debug)]
pub struct Tree {
    /// The root; removed when the tree is dropped.
    pub root: tempfile::TempDir,
    pub layout: Layout,
    /// The stand-in git.
    pub git: PathBuf,
}

impl Tree {
    /// The tree.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    #[must_use]
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let layout = Layout::under(root.path());
        let write = |path: &Path, text: &str| {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(path, text).expect("write");
        };
        write(&layout.python_pin, &format!("{PYTHON}\n"));
        write(&layout.uv_pin, &format!("{UV}\n"));
        write(&layout.lock, LOCK);
        std::fs::create_dir_all(&layout.scratch).expect("scratch");
        let git = stand_in(
            &root.path().join("stand-ins"),
            "git",
            &format!("[ \"$*\" = 'rev-parse HEAD' ] || exit 2\necho {COMMIT}"),
        );
        let tree = Self { root, layout, git };
        tree.uv(UV, UV);
        tree
    }

    /// The uv and uvx stand-ins, reporting `uv` and `uvx` as their
    /// versions.
    ///
    /// # Panics
    ///
    /// When they can't be written.
    pub fn uv(&self, uv: &str, uvx: &str) {
        let bin = &self.layout.wrappers;
        let calls = self.calls_line();
        let script = format!(
            "{calls}\ncase \"$2\" in\n--version) echo 'uv {uv} (stand-in)' ;;\n\
             pip) [ \"$3\" = check ] || exit 2 ;;\n*) exit 2 ;;\nesac"
        );
        let _ = stand_in(bin, "uv", &script);
        let _ = stand_in(bin, "uvx", &format!("{calls}\necho 'uvx {uvx} (stand-in)'"));
    }

    /// The log every stand-in appends its argv to, one line per call:
    /// `<program> <arguments>`.
    #[must_use]
    pub fn calls(&self) -> PathBuf {
        self.root.path().join("calls.log")
    }

    /// The calls so far, one per line.
    ///
    /// # Panics
    ///
    /// When the log is there but can't be read.
    #[must_use]
    pub fn calls_made(&self) -> Vec<String> {
        match std::fs::read_to_string(self.calls()) {
            Ok(text) => text.lines().map(str::to_string).collect(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => panic!("read calls: {err}"),
        }
    }

    /// Where the venv's entry points record their Ansible config.
    #[must_use]
    pub fn entry_points(&self) -> PathBuf {
        self.root.path().join("entry-points.log")
    }

    /// The script line appending a call to [`Tree::calls`].
    fn calls_line(&self) -> String {
        format!("echo \"$0 $*\" >> '{}'", self.calls().display())
    }

    /// A healthy venv at `venv`: a `bin/python3` reporting [`PYTHON`] and
    /// importing everything, and `ansible`, `certbot` and `apprise`
    /// reporting versions. Each entry point also appends its name and its
    /// Ansible config to [`Tree::entry_points`]: `empty-file` when
    /// `ANSIBLE_CONFIG` names an existing empty file, else its value or
    /// `unset`.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn venv(&self, venv: &Path) {
        let bin = venv.join("bin");
        let calls = self.calls_line();
        self.python(&bin.join("python3"), PYTHON);
        let record = self.entry_points();
        let record = record.display();
        for name in ["ansible", "certbot", "apprise"] {
            let script = format!(
                "{calls}\nc=${{ANSIBLE_CONFIG-unset}}\n\
                 if [ -f \"$c\" ] && [ ! -s \"$c\" ]; then c=empty-file; fi\n\
                 echo \"{name} $c\" >> '{record}'\necho '{name} 1.0'"
            );
            let _ = stand_in(&bin, name, &script);
        }
    }

    /// A Python at `path` reporting `version`, `-I` required, whose `-c`
    /// imports succeed.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn python(&self, path: &Path, version: &str) {
        let calls = self.calls_line();
        let script = format!(
            "{calls}\n[ \"$1\" = -I ] || exit 2\ncase \"$2\" in\n\
             --version) echo 'Python {version}' ;;\n-c) exit 0 ;;\n*) exit 2 ;;\nesac"
        );
        let dir = path.parent().expect("parent");
        let name = path.file_name().expect("name").to_str().expect("utf-8");
        let _ = stand_in(dir, name, &script);
    }

    /// An [`Env`](crate::Env) on this tree with its git.
    #[must_use]
    pub fn env(&self) -> crate::Env<'_> {
        let mut env = crate::Env::new(&self.layout, UV);
        env.git.clone_from(&self.git);
        env
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}
