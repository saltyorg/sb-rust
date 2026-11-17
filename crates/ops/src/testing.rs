//! The harness setup's and gha's shared pieces run against (ops spec,
//! Testing Strategy, "Sequences" and "locale"; the `testing` feature,
//! never in a release build): a temporary root holding the Saltbox
//! checkout, `/srv`, the apt sources and their backups and
//! `/etc/default/locale`, and stand-in `apt-get`, `lsb_release`,
//! `locale`, `locale-gen`, `update-locale`, `dpkg-reconfigure` and
//! `bash`, and setup's `systemd-detect-virt` and `dpkg-query`, that
//! record their arguments, one call a line, in the order they ran; and a
//! bare repository standing in for Saltbox's ([`Remote`]). Nothing
//! reaches the host: every path the pieces take is one of the root's,
//! given to them as an argument. The stand-ins are
//! shell scripts, as sb-host's and sb-python-env's; a test that writes one
//! and runs it guards against ETXTBSY as theirs do (a lock held from
//! writing it until its run is over). bench's network fakes are
//! [`network`]'s.

pub mod network;

pub use network::{NETWORK_VARIABLE, NetworkFake, Seen, TestNetwork, from_env, from_file};

/// The fake restore service `sb restore`'s tests run against.
pub mod restore;

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
pub const STAND_INS: [&str; 9] = [
    "apt-get",
    "lsb_release",
    "locale",
    "locale-gen",
    "update-locale",
    "dpkg-reconfigure",
    "bash",
    "systemd-detect-virt",
    "dpkg-query",
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
    /// two settings into the locale file, as the real one does), but
    /// `systemd-detect-virt -c` printing `none` with exit 1 and
    /// `dpkg-query` exiting 1, as on a server that is no container and has
    /// no desktop.
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
        let _ = host.stand_in("systemd-detect-virt", "echo none; exit 1");
        let _ = host.stand_in("dpkg-query", "exit 1");
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

    /// host's setup checks on the stand-in `systemd-detect-virt` and
    /// `dpkg-query`, on an `x86_64` machine.
    #[must_use]
    pub fn checks(&self) -> sb_host::setup::Setup {
        sb_host::setup::Setup::new()
            .with_detect_virt(self.stand_ins().join("systemd-detect-virt"))
            .with_apt(Apt::new().with_dpkg_query(self.stand_ins().join("dpkg-query")))
            .with_machine(|| "x86_64".to_owned())
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

/// `git <args>` in `dir`, which must succeed; its stdout, trimmed. None
/// of the host's git settings, as sb-repo's tests: `GIT_CONFIG_GLOBAL` and
/// `GIT_CONFIG_SYSTEM` at /dev/null skip its config files (git 2.32+,
/// <https://git-scm.com/docs/git#Documentation/git.txt-GITCONFIGGLOBAL>;
/// commit signing, credential helpers and rewrites stay out), and the
/// ignore and attributes files, read without any config, are named
/// (<https://git-scm.com/docs/git-config#Documentation/git-config.txt-coreexcludesFile>).
///
/// # Panics
///
/// When git can't run or fails.
#[must_use = "git's output is what a test checks"]
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(["-c", "core.attributesFile=/dev/null"])
        .args(["-c", "user.name=sb", "-c", "user.email=sb"])
        .args(["-c", "init.defaultBranch=master"])
        .args(["-c", "advice.detachedHead=false"])
        // A checkout setup gave to the Saltbox user; Saltbox gives root's
        // git a safe.directory entry for it (repo spec, assumption 2), a
        // test this one-off option.
        .args(["-c", "safe.directory=*"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare repository standing in for Saltbox's on a temporary root, with
/// the working copy it is pushed from: `master` and `develop`, each
/// holding the given files, `branch.txt` naming the branch, a default for
/// accounts.yml (naming the Saltbox user) and settings.yml, and
/// Saltbox's hooks script; Saltbox's configs, the fact and the
/// inventory's `localhost.yml` ignored, as Saltbox's .gitignore has them. The working
/// copy is left on `develop`.
#[derive(Debug)]
pub struct Remote {
    /// The root; removed when the remote is dropped.
    pub dir: tempfile::TempDir,
    /// The bare repository's `file://` URL.
    pub url: String,
}

impl Remote {
    /// The remote, `files` (paths relative to the checkout, and their
    /// text) in both branches, accounts.yml's default naming `user`.
    ///
    /// # Panics
    ///
    /// When it can't be written or git fails.
    #[must_use]
    pub fn saltbox(files: &[(&str, &str)], user: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let _ = git(dir.path(), &["init", "--quiet", "--bare", "saltbox.git"]);
        let _ = git(dir.path(), &["init", "--quiet", "work"]);
        let remote = Self {
            url: format!("file://{}", dir.path().join("saltbox.git").display()),
            dir,
        };
        let accounts = format!("user:\n  name: {user}\n");
        let fixed = [
            (
                ".gitignore",
                concat!(
                    "accounts.yml\nadv_settings.yml\nbackup_config.yml\nhetzner_vlan.yml\n",
                    "motd.yml\nproviders.yml\nsettings.yml\nansible_facts.d/\n",
                    "inventories/host_vars/localhost.yml\n",
                ),
            ),
            ("defaults/accounts.yml.default", accounts.as_str()),
            ("defaults/settings.yml.default", "settings: seed\n"),
            ("bin/git/init-hooks", "true\n"),
            ("branch.txt", "master\n"),
        ];
        for (rel, text) in files.iter().chain(&fixed) {
            remote.write(rel, text);
        }
        let work = remote.work();
        let _ = git(&work, &["add", "."]);
        let _ = git(&work, &["commit", "--quiet", "-m", "master"]);
        let _ = git(&work, &["remote", "add", "origin", &remote.url]);
        let _ = git(&work, &["push", "--quiet", "origin", "master"]);
        let _ = git(&work, &["checkout", "--quiet", "-b", "develop"]);
        remote.commit("branch.txt", "develop\n");
        remote
    }

    /// The working copy the remote is pushed from.
    #[must_use]
    pub fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    /// `rel` in the working copy holding `text`, its parents made.
    fn write(&self, rel: &str, text: &str) {
        let path = self.work().join(rel);
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).expect("parent");
        std::fs::write(path, text).expect("write");
    }

    /// `rel` holding `text`, committed on the working copy's branch and
    /// pushed.
    ///
    /// # Panics
    ///
    /// When it can't be written or git fails.
    pub fn commit(&self, rel: &str, text: &str) {
        self.write(rel, text);
        let work = self.work();
        let _ = git(&work, &["add", "."]);
        let _ = git(&work, &["commit", "--quiet", "-m", rel]);
        let _ = git(&work, &["push", "--quiet", "origin", "HEAD"]);
    }

    /// A clone of the remote at `branch` into `dest`.
    ///
    /// # Panics
    ///
    /// When git fails or `dest` isn't UTF-8.
    pub fn clone_into(&self, branch: &str, dest: &Path) {
        let dest = dest.to_str().expect("a UTF-8 destination");
        let _ = git(
            self.dir.path(),
            &["clone", "--quiet", "-b", branch, &self.url, dest],
        );
    }
}

/// What the stand-in `ansible-playbook` lists: ansible's fixture of a real
/// `--list-tags`, Saltbox's.
pub const LIST_TAGS: &str = "playbook: saltbox.yml

  play #1 (localhost): Saltbox\tTAGS: []
      TASK TAGS: [always, authelia, docker, plex, sanity-check, sonarr, traefik]
";

/// A stand-in `ansible-playbook` in `dir` listing [`LIST_TAGS`]; its path.
///
/// # Panics
///
/// When it can't be written.
#[must_use = "the stand-in's path is what sb runs"]
pub fn ansible_playbook(dir: &Path) -> PathBuf {
    let path = dir.join("ansible-playbook");
    std::fs::create_dir_all(dir).expect("stand-in dir");
    std::fs::write(&path, format!("#!/bin/sh\ncat <<'LIST'\n{LIST_TAGS}LIST\n"))
        .expect("write the stand-in");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// A Saltbox host for `update` and the branch commands, on a temporary
/// root: the stand-in [`Host`] (apt, the locale programs, bash) with a
/// stand-in `ansible-playbook` listing [`LIST_TAGS`]; python-env's stand-in
/// tree with a healthy venv and its fake GitHub; Saltbox's checkout at the
/// tree's checkout, a clone of its own [`Remote`] holding the tree's pins
/// and lock and Saltbox's pinned schemas, with sb-config's `valid` config
/// set and an inventory pointing the appdata path into the tree's root;
/// Sandbox's checkout there, a clone of its own remote; and sb-config's
/// fake answering both credential checks well. Nothing reaches a network
/// remote or the host's checkouts.
#[derive(Debug)]
pub struct Saltbox {
    pub host: Host,
    pub tree: sb_python_env::testing::Tree,
    pub saltbox_remote: Remote,
    pub sandbox_remote: Remote,
    pub checks: sb_config::testing::Fake,
}

impl Saltbox {
    /// The host, each checkout on the branch named.
    ///
    /// # Panics
    ///
    /// When it can't be written or git fails.
    #[must_use]
    pub fn new(saltbox: &str, sandbox: &str) -> Self {
        let host = Host::new();
        let _ = host.stand_in(
            "ansible-playbook",
            &format!("cat <<'LIST'\n{LIST_TAGS}LIST"),
        );
        let tree = sb_python_env::testing::Tree::new();
        let _ = tree.healthy();
        let layout = &tree.layout;
        let configs = sb_config::testing::saltbox_root("valid").expect("config fixtures");
        let mut files: Vec<(String, String)> = [&layout.python_pin, &layout.uv_pin, &layout.lock]
            .iter()
            .map(|pin| {
                let rel = pin.strip_prefix(&layout.checkout).expect("in the checkout");
                let text = std::fs::read_to_string(pin).expect("pin");
                (rel.display().to_string(), text)
            })
            .collect();
        for entry in std::fs::read_dir(configs.path().join("schema")).expect("schemas") {
            let path = entry.expect("schema").path();
            let name = path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path).expect("schema text");
            files.push((format!("schema/{name}"), text));
        }
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(r, t)| (r.as_str(), t.as_str()))
            .collect();
        let saltbox_remote = Remote::saltbox(&files, "seed");
        let sandbox_remote = Remote::saltbox(&[], "seed");
        std::fs::remove_dir_all(&layout.checkout).expect("remove the tree's checkout");
        let checks = sb_config::testing::Fake::start().expect("checks fake");
        checks.script(
            "/client/v4/zones",
            sb_config::testing::Answer::json(
                200,
                r#"{"success":true,"result":[{"id":"z1","name":"example.com"}]}"#,
            ),
        );
        checks.script(
            "/client/v4/zones/z1/settings/ssl",
            sb_config::testing::Answer::json(
                200,
                r#"{"success":true,"result":{"id":"ssl","value":"full"}}"#,
            ),
        );
        checks.script(
            "/v2/users/login/",
            sb_config::testing::Answer::json(200, "{}"),
        );
        let world = Self {
            host,
            tree,
            saltbox_remote,
            sandbox_remote,
            checks,
        };
        world.saltbox_remote.clone_into(saltbox, world.root());
        for entry in std::fs::read_dir(configs.path()).expect("configs") {
            let path = entry.expect("config").path();
            if path.is_file() {
                std::fs::copy(&path, world.root().join(path.file_name().expect("name")))
                    .expect("copy config");
            }
        }
        let inventory = world.root().join("inventories/host_vars");
        std::fs::create_dir_all(&inventory).expect("inventory dir");
        std::fs::write(
            inventory.join("localhost.yml"),
            format!("server_appdata_path: {}\n", world.appdata().display()),
        )
        .expect("inventory");
        world.sandbox_remote.clone_into(sandbox, &world.sandbox());
        world
    }

    /// The Saltbox root: the tree's checkout.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.tree.layout.checkout
    }

    /// The appdata path.
    #[must_use]
    pub fn appdata(&self) -> PathBuf {
        self.tree.root.path().join("appdata")
    }

    /// The Sandbox checkout.
    #[must_use]
    pub fn sandbox(&self) -> PathBuf {
        self.appdata().join("sandbox")
    }

    /// The credential checks' URLs on the fake: Cloudflare's API base and
    /// Docker Hub's login.
    #[must_use]
    pub fn check_urls(&self) -> (String, String) {
        (
            format!("{}/client/v4/", self.checks.url()),
            format!("{}/v2/users/login/", self.checks.url()),
        )
    }

    /// A checkout's branch and `branch.txt`.
    ///
    /// # Panics
    ///
    /// When git fails or there's no `branch.txt`.
    #[must_use]
    pub fn branch(path: &Path) -> (String, String) {
        let name = git(path, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let text = std::fs::read_to_string(path.join("branch.txt")).expect("branch.txt");
        (name, text.trim().to_owned())
    }
}
