//! The harness setup's and gha's shared pieces run against (ops spec,
//! Testing Strategy, "Sequences" and "locale"; the `testing` feature,
//! never in a release build): a temporary root holding the Saltbox
//! checkout, `/srv`, the apt sources and their backups and
//! `/etc/default/locale`, and stand-in `apt-get`, `lsb_release`,
//! `locale`, `locale-gen`, `update-locale`, `dpkg-reconfigure` and
//! `bash` that record their arguments, one call a line, in the order
//! they ran. Nothing reaches the host: every path the pieces take is
//! one of the root's, given to them as an argument. The stand-ins are
//! shell scripts, as sb-host's and sb-python-env's; a test that writes one
//! and runs it guards against ETXTBSY as theirs do (a lock held from
//! writing it until its run is over).

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_host::apt::Apt;

use crate::locale::Locale;
use crate::prereq::Dirs;

/// The codename the stand-in `lsb_release` reports unless
/// [`Host::codename`] names another: a DEB822 release.
pub const CODENAME: &str = "noble";

/// The stand-ins every [`Host`] has.
pub const STAND_INS: [&str; 7] = [
    "apt-get",
    "lsb_release",
    "locale",
    "locale-gen",
    "update-locale",
    "dpkg-reconfigure",
    "bash",
];

/// A temporary host for the shared pieces.
#[derive(Debug)]
pub struct Host {
    /// The root; removed when the host is dropped.
    pub root: tempfile::TempDir,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    /// The host: the Saltbox checkout's directory, an empty
    /// `sources.list.d`, the backups' parent, `/etc/default`, and every
    /// stand-in of [`STAND_INS`], each recording its call and exiting 0
    /// (`lsb_release` printing [`CODENAME`]; `update-locale` writing its
    /// two settings into the locale file, as the real one does).
    ///
    /// # Panics
    ///
    /// When the root can't be written.
    #[must_use]
    pub fn new() -> Self {
        let host = Self {
            root: tempfile::tempdir().expect("tempdir"),
        };
        for dir in [
            host.saltbox(),
            host.sources_d(),
            host.path("var/backups"),
            host.path("etc/default"),
            host.path("srv"),
        ] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        // apt's sources and the backups' parent must not be group- or
        // world-writable (host's refusal), whatever the umask.
        for dir in [
            host.path("etc/apt"),
            host.sources_d(),
            host.path("var/backups"),
        ] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        for name in STAND_INS {
            let _ = host.stand_in(name, "exit 0");
        }
        let codename = host.path("codename");
        let _ = host.stand_in(
            "lsb_release",
            &format!(
                "cat '{}' 2>/dev/null || echo {CODENAME}",
                codename.display()
            ),
        );
        let _ = host.stand_in(
            "update-locale",
            &format!(
                "printf 'LANG=en_US.UTF-8\\nLC_ALL=en_US.UTF-8\\n' > '{}'",
                host.locale_file().display()
            ),
        );
        host
    }

    /// `rel` under the root.
    #[must_use]
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    /// The Saltbox checkout's directory.
    #[must_use]
    pub fn saltbox(&self) -> PathBuf {
        self.path("srv/git/saltbox")
    }

    /// The host's `sources.list.d`.
    #[must_use]
    pub fn sources_d(&self) -> PathBuf {
        self.path("etc/apt/sources.list.d")
    }

    /// The host's `/var/backups/sb`, where moved sources go.
    #[must_use]
    pub fn backups(&self) -> PathBuf {
        self.path("var/backups/sb")
    }

    /// The host's `/etc/default/locale`.
    #[must_use]
    pub fn locale_file(&self) -> PathBuf {
        self.path("etc/default/locale")
    }

    /// The stand-ins' directory.
    #[must_use]
    pub fn stand_ins(&self) -> PathBuf {
        self.path("stand-ins")
    }

    /// The stand-in `name`, which records its call, fails when
    /// [`Host::fail`] names it, then runs `script` (`sh`); its path.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    #[must_use = "the stand-in's path is what a piece runs"]
    pub fn stand_in(&self, name: &str, script: &str) -> PathBuf {
        let dir = self.stand_ins();
        std::fs::create_dir_all(&dir).expect("stand-in dir");
        let path = dir.join(name);
        let calls = self.path("calls.log").display().to_string();
        let failing = self.path("fail").join(name).display().to_string();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{calls}'\n\
                 [ -e '{failing}' ] && {{ echo '{name} failed (stand-in)' >&2; exit 1; }}\n\
                 {script}\n"
            ),
        )
        .expect("write stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// Makes the stand-in `name` fail from now on (stderr `<name> failed
    /// (stand-in)`, exit 1).
    ///
    /// # Panics
    ///
    /// When the marker can't be written.
    pub fn fail(&self, name: &str) {
        let dir = self.path("fail");
        std::fs::create_dir_all(&dir).expect("fail dir");
        std::fs::write(dir.join(name), "").expect("fail marker");
    }

    /// The codename the stand-in `lsb_release` reports from now on.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn codename(&self, codename: &str) {
        std::fs::write(self.path("codename"), format!("{codename}\n")).expect("codename");
    }

    /// The calls so far, in order, each `<program> <arguments>` (none
    /// before the first).
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        // No log yet is no call yet.
        let text = std::fs::read_to_string(self.path("calls.log")).unwrap_or_default();
        text.lines().map(str::to_string).collect()
    }

    /// host's apt on the stand-ins, its update retries cut short, its
    /// sources and backups under the root, owned by the root's owner in
    /// place of root.
    ///
    /// # Panics
    ///
    /// When the root can't be inspected.
    #[must_use]
    pub fn apt(&self) -> Apt {
        let short = Duration::from_millis(50);
        let owner = std::fs::metadata(self.root.path()).expect("root").uid();
        Apt::new()
            .with_apt_get(self.stand_ins().join("apt-get"))
            .with_lsb_release(self.stand_ins().join("lsb_release"))
            .with_update_timing(Duration::from_secs(10), short, short, short)
            .with_sources_roots(self.path("etc/apt"), self.path("var/backups"), owner)
    }

    /// `/srv/git` and `/srv/ansible` under the root.
    #[must_use]
    pub fn dirs(&self) -> Dirs {
        Dirs {
            git: self.path("srv/git"),
            ansible: self.path("srv/ansible"),
        }
    }

    /// The locale steps' stand-ins and the root's locale file.
    #[must_use]
    pub fn locale(&self) -> Locale {
        let program = |name: &str| self.stand_ins().join(name);
        Locale {
            locale: program("locale"),
            locale_gen: program("locale-gen"),
            update_locale: program("update-locale"),
            dpkg_reconfigure: program("dpkg-reconfigure"),
            file: self.locale_file(),
        }
    }

    /// The stand-in `bash`.
    #[must_use]
    pub fn bash(&self) -> PathBuf {
        self.stand_ins().join("bash")
    }

    /// Writes `text` into the Saltbox checkout at `rel`, its parents made.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    #[must_use = "the file's path is what a test checks"]
    pub fn saltbox_file(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.saltbox().join(rel);
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).expect("parent");
        std::fs::write(&path, text).expect("write");
        path
    }
}

/// The host paths the namespace covers with an empty tmpfs (ops spec,
/// Testing Strategy, "Sequences"): apt's sources and their backups, the
/// locale file's directory, and `/srv`.
pub const COVERED: [&str; 4] = ["/etc/apt", "/var/backups", "/etc/default", "/srv"];

impl Host {
    /// `program` with `args`, as root, in a private mount namespace where
    /// each of [`COVERED`] is an empty tmpfs (0755, root's) and this
    /// host's stand-ins come first on `PATH`; `setup` (shell lines, run
    /// first, under `set -e`) prepares the covered paths. The program
    /// runs only when every mount and the setup succeeded, so nothing it
    /// does reaches the host's `/etc/apt`, `/var/backups`, `/etc/default`
    /// or `/srv`; the mounts end with the namespace. unshare(1) and
    /// mount(8): <https://man7.org/linux/man-pages/man1/unshare.1.html>,
    /// <https://man7.org/linux/man-pages/man8/mount.8.html> (as
    /// scripts/check.sh's mutation sandbox). Tests start it directly
    /// (the `testing` feature's processes are exempt from sb-exec's rule).
    ///
    /// # Panics
    ///
    /// When a stand-in path holds a colon, which `PATH` can't carry.
    #[must_use]
    pub fn namespaced(&self, program: &Path, args: &[&str], setup: &str) -> std::process::Command {
        let mut script = String::from("set -e\n");
        for path in COVERED {
            script.push_str("mount -t tmpfs -o mode=0755,nosuid,nodev sb-ops-test ");
            script.push_str(path);
            script.push_str("\nmountpoint -q ");
            script.push_str(path);
            script.push('\n');
        }
        script.push_str(setup);
        script.push_str("\nexec \"$@\"\n");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![self.stand_ins()];
        paths.extend(std::env::split_paths(&path));
        let mut cmd = std::process::Command::new("unshare");
        cmd.args(["--mount", "--propagation", "private", "--", "sh", "-c"])
            .arg(script)
            .arg("sh")
            .arg(program)
            .args(args)
            .env(
                "PATH",
                std::env::join_paths(paths).expect("PATH without a colon in a stand-in path"),
            );
        cmd
    }
}
