//! Stand-in programs and a Saltbox tree on a temporary root, for
//! sb-python-env's tests and sb's end-to-end tests (python-env spec,
//! Testing Strategy; the `testing` feature, never in a release build).
//! The stand-ins are shell scripts, as sb-host's and sb-repo's; a test
//! that writes one and runs it guards against ETXTBSY as theirs do (a
//! lock held from writing it until its run is over).

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use sb_release::testing::{Answer, Fake};
use sb_release::{Arch, Releases};
use sha2::{Digest as _, Sha256};

use crate::Layout;
use crate::manifest::Manifest;

/// The variable naming the tree's root to the sb `cargo test` builds
/// ([`Layout::for_sb`], [`Env::for_sb`](crate::Env::for_sb)).
pub const ROOT_VAR: &str = "SB_TEST_PYTHON_ENV_ROOT";

/// The commands a generation's manifest exports.
pub const COMMANDS: [&str; 4] = ["ansible", "ansible-playbook", "apprise", "certbot"];

/// The healthy tree's generation and Python release.
pub const GENERATION: &str = "20261113T120000.000000000-0123456789ab";
pub const PYTHON_RELEASE: &str = "python-3-12-7-20261113T120000.000000000";

/// [`ROOT_VAR`]'s root, when set and not empty.
#[must_use]
pub fn root() -> Option<PathBuf> {
    std::env::var_os(ROOT_VAR)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
}

/// The file under the root naming the tree's Ubuntu release (`VERSION_ID`),
/// [`DEFAULT_RELEASE`] when there is none.
const RELEASE_FILE: &str = "os-release-version";

/// The tree's release unless [`Tree::release`] names another: one the
/// deadsnakes cleanup doesn't run on, whatever the host runs.
pub const DEFAULT_RELEASE: &str = "24.04";

/// `env` running the stand-in git, dpkg-query, apt-get and apt-cache of
/// the tree at `root`, apt's update retries cut short, its apt sources and
/// backups under the root (owned by the root's owner in place of root's),
/// and the tree's release in place of the host's.
#[must_use]
pub fn with_stand_ins<'a>(mut env: crate::Env<'a>, root: &Path) -> crate::Env<'a> {
    use std::os::unix::fs::MetadataExt as _;
    let dir = root.join("stand-ins");
    let short = std::time::Duration::from_millis(50);
    let owner = std::fs::metadata(root).map_or(0, |meta| meta.uid());
    env.git = dir.join("git");
    env.apt = sb_host::apt::Apt::new()
        .with_dpkg_query(dir.join("dpkg-query"))
        .with_apt_get(dir.join("apt-get"))
        .with_apt_cache(dir.join("apt-cache"))
        .with_update_timing(std::time::Duration::from_secs(10), short, short, short)
        .with_sources_roots(root.join("etc/apt"), root.join("var/backups"), owner);
    // No file yet is the default release.
    let release = std::fs::read_to_string(root.join(RELEASE_FILE)).unwrap_or_default();
    let release = release.trim();
    env.ubuntu_release = Some(
        if release.is_empty() {
            DEFAULT_RELEASE
        } else {
            release
        }
        .to_owned(),
    );
    env
}

/// A generation on the tree: its directory, venv, Python release and
/// manifest.
#[derive(Debug, Clone)]
pub struct Generation {
    pub dir: PathBuf,
    pub venv: PathBuf,
    pub python_install: PathBuf,
    pub manifest: Manifest,
}

/// The commit the stand-in git reports.
pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The Python the tree pins.
pub const PYTHON: &str = "3.12.7";

/// The uv minimum the tree pins, and the uv its stand-ins report.
pub const UV: &str = "0.13.4";

/// The lock the tree holds.
pub const LOCK: &str = "ansible==12.0.0 --hash=sha256:00\n";

/// The tag of the fact release [`Tree::healthy`] serves.
pub const FACT_TAG: &str = "v1.4.0";

/// A stand-in saltbox.fact of this build's architecture whose `--version`
/// prints `version`: sb-release's stand-in ELF with its `stand-in sb `
/// prefix blanked, so the line is the version after spaces, which the
/// fact's parse trims. UNVERIFIED: patching the format string in place;
/// checked against the stand-in rustc 1.100.0 builds, which holds the
/// prefix once. Why not a stand-in of its own: it would need a build
/// script running rustc (CONSTRAINTS.md's E5, sb-release's only).
///
/// # Panics
///
/// When the stand-in doesn't hold the prefix exactly once.
#[must_use]
pub fn fact_binary(version: &str) -> Vec<u8> {
    let mut bytes = sb_release::testing::stand_in(version);
    let prefix = b"stand-in sb ";
    let mut found = bytes
        .windows(prefix.len())
        .enumerate()
        .filter(|(_, window)| window == prefix)
        .map(|(at, _)| at);
    let (Some(at), None) = (found.next(), found.next()) else {
        panic!("the stand-in holds its output prefix exactly once");
    };
    bytes[at..at + prefix.len()].fill(b' ');
    bytes
}

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
/// scratch directory, a stand-in git, a uv and uvx reporting [`UV`], and
/// sb-release's fake GitHub with a client for it (nothing scripted on it
/// yet: [`Tree::serve_uv`]).
#[derive(Debug)]
pub struct Tree {
    /// The root; removed when the tree is dropped.
    pub root: tempfile::TempDir,
    pub layout: Layout,
    /// The stand-in git.
    pub git: PathBuf,
    /// The fake proxy, GitHub API and download host.
    pub fake: Fake,
    /// A client of [`Tree::fake`].
    pub releases: Releases,
}

/// The file under the root holding the fake's bases, for an sb run
/// ([`sb_release::testing::BASES_VARIABLE`]).
const BASES: &str = "release-bases.json";

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
        let fake = Fake::start().expect("fake GitHub");
        fake.write_bases(&root.path().join(BASES)).expect("bases");
        let releases = fake.releases("sb/test").expect("release client");
        let tree = Self {
            root,
            layout,
            git,
            fake,
            releases,
        };
        let templates = tree.root.path().join("templates");
        tree.python(&templates.join("python3"), PYTHON);
        tree.entry_points(&templates.join("bin"));
        tree.uv(UV, UV);
        tree.dpkg_query();
        let _ = stand_in(&tree.stand_ins(), "apt-get", &tree.apt_get_script());
        let policies = tree.root.path().join("policy");
        let _ = stand_in(
            &tree.stand_ins(),
            "apt-cache",
            &format!("{}\ncat '{}/'\"$3\"", tree.calls_line(), policies.display()),
        );
        tree
    }

    /// The stand-in apt-get's script: records its call; its operation
    /// (the first argument that is neither an option nor `-o`'s value)
    /// fails when [`Tree::fail`] names `apt-get-<op>`; `remove` makes the
    /// rows of [`Tree::rows_after_remove`] dpkg's; `update` runs the
    /// script of [`Tree::on_update`].
    fn apt_get_script(&self) -> String {
        let root = self.root.path().display().to_string();
        let script = r#"prev=; op=
for a; do
  if [ -z "$op" ] && [ "$prev" != -o ] && [ "${a#-}" = "$a" ]; then op=$a; fi
  prev=$a
done
[ -e 'ROOT/fail/apt-get-'"$op" ] && { echo "E: apt-get $op failed (stand-in)" >&2; exit 100; }
if [ "$op" = remove ] && [ -e 'ROOT/rows-after-remove' ]; then
  cp 'ROOT/rows-after-remove' 'ROOT/dpkg-rows'
fi
if [ "$op" = update ] && [ -e 'ROOT/on-update' ]; then sh 'ROOT/on-update'; fi
exit 0"#;
        format!("{}\n{}", self.calls_line(), script.replace("ROOT", &root))
    }

    /// The tree's Ubuntu release: `version_id` in place of
    /// [`DEFAULT_RELEASE`].
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn release(&self, version_id: &str) {
        std::fs::write(self.root.path().join(RELEASE_FILE), version_id).expect("release");
    }

    /// The tree's `etc/apt/sources.list.d`, made 0755 with `var/backups`
    /// (the checks sb-host makes of the real ones) when missing.
    ///
    /// # Panics
    ///
    /// When they can't be made.
    #[must_use]
    pub fn sources_d(&self) -> PathBuf {
        let sources_d = self.root.path().join("etc/apt/sources.list.d");
        for dir in [&sources_d, &self.backups()] {
            std::fs::create_dir_all(dir).expect("dir");
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        sources_d
    }

    /// The tree's `var/backups`.
    #[must_use]
    pub fn backups(&self) -> PathBuf {
        self.root.path().join("var/backups")
    }

    /// dpkg-query's package rows (`package_list`'s format, one per line).
    ///
    /// # Panics
    ///
    /// When they can't be written.
    pub fn rows(&self, rows: &str) {
        std::fs::write(self.root.path().join("dpkg-rows"), rows).expect("rows");
    }

    /// The rows dpkg-query gives once apt-get has run `remove`.
    ///
    /// # Panics
    ///
    /// When they can't be written.
    pub fn rows_after_remove(&self, rows: &str) {
        std::fs::write(self.root.path().join("rows-after-remove"), rows).expect("rows");
    }

    /// apt-cache's `policy` answer for `name`.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn policy(&self, name: &str, text: &str) {
        let dir = self.root.path().join("policy");
        std::fs::create_dir_all(&dir).expect("policy dir");
        std::fs::write(dir.join(name), text).expect("policy");
    }

    /// A shell script apt-get runs on `update`.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn on_update(&self, script: &str) {
        std::fs::write(self.root.path().join("on-update"), script).expect("on-update");
    }

    /// The uv and uvx stand-ins, reporting `uv` and `uvx` as their
    /// versions. uv also runs `pip check` (passing), and the build's
    /// `python install` (the template Python into the install dir's
    /// `cpython/bin/python3.12`), `python find` (that path, or
    /// [`Tree::find_answer`]'s), `venv` (the template Python as
    /// `bin/python3`) and `pip sync` (the template entry points into the
    /// venv's `bin`); [`Tree::fail`], [`Tree::fail_once`] and
    /// [`Tree::hang`] change one of them.
    ///
    /// # Panics
    ///
    /// When they can't be written.
    pub fn uv(&self, uv: &str, uvx: &str) {
        let bin = &self.layout.wrappers;
        let _ = stand_in(bin, "uv", &self.uv_script(uv));
        let _ = stand_in(bin, "uvx", &self.uvx_script(uvx));
    }

    /// The stand-in uvx's script, reporting `uvx` as its version.
    fn uvx_script(&self, uvx: &str) -> String {
        format!("{}\necho 'uvx {uvx} (stand-in)'", self.calls_line())
    }

    /// The stand-in uv's script ([`Tree::uv`]), reporting `uv` as its
    /// version.
    fn uv_script(&self, uv: &str) -> String {
        let calls = self.calls_line();
        let root = self.root.path().display();
        // `$1` is `--no-config`; `$2 $3` name the operation. A file named
        // after it under `fail/` makes it fail, under `hang/` hang (after
        // touching `hang/started`).
        format!(
            "{calls}\n\
             op=\"$2-$3\"\n\
             [ -e '{root}/fail/'\"$op\" ] && {{ echo \"uv: $op failed (stand-in)\" >&2; exit 1; }}\n\
             [ -e '{root}/fail-once/'\"$op\" ] && {{ rm -f '{root}/fail-once/'\"$op\"; \
             echo \"uv: $op failed once (stand-in)\" >&2; exit 1; }}\n\
             [ -e '{root}/hang/'\"$op\" ] && {{ : > '{root}/hang/started'; exec sleep 600; }}\n\
             prev=; last=; dir=; py=\n\
             for a; do\n\
               [ \"$prev\" = --install-dir ] && dir=$a\n\
               [ \"$prev\" = --python ] && py=$a\n\
               prev=$a; last=$a\n\
             done\n\
             case \"$op\" in\n\
             --version-) echo 'uv {uv} (stand-in)' ;;\n\
             pip-check) ;;\n\
             cache-clean) ;;\n\
             python-install) mkdir -p \"$dir/cpython/bin\" && cp '{root}/templates/python3' \"$dir/cpython/bin/python3.12\" ;;\n\
             python-find) if [ -e '{root}/find-answer' ]; then cat '{root}/find-answer'; \
             else echo \"$UV_PYTHON_INSTALL_DIR/cpython/bin/python3.12\"; fi ;;\n\
             venv-*) mkdir -p \"$last/bin\" && cp '{root}/templates/python3' \"$last/bin/python3\" ;;\n\
             pip-sync) cp '{root}/templates/bin/'* \"$(dirname \"$py\")/\" ;;\n\
             *) exit 2 ;;\n\
             esac"
        )
    }

    /// uv's release archive for this build's architecture, as astral-sh
    /// lays it out: `uv-<arch>-unknown-linux-gnu/uv` and `.../uvx`, the
    /// stand-ins of [`Tree::uv`] reporting `uv` and `uvx`.
    ///
    /// # Panics
    ///
    /// When it can't be built.
    #[must_use]
    pub fn uv_archive(&self, uv: &str, uvx: &str) -> Vec<u8> {
        let arch = Arch::current().expect("arch");
        let dir = format!("uv-{}-unknown-linux-gnu", arch.rust_name());
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gzip);
        for (name, script) in [("uv", self.uv_script(uv)), ("uvx", self.uvx_script(uvx))] {
            let body = format!("#!/bin/sh\n{script}\n");
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            tar.append_data(&mut header, format!("{dir}/{name}"), body.as_bytes())
                .expect("entry");
        }
        tar.into_inner().expect("tar").finish().expect("gzip")
    }

    /// The fake serving uv's release [`UV`] for `arch` with `archive` as
    /// its asset, from GitHub's API (the proxy is down: unscripted, 404).
    pub fn serve_uv(&self, arch: Arch, archive: &[u8]) {
        let name = crate::uv::asset_name(arch);
        self.serve(
            &format!("/api/repos/astral-sh/uv/releases/tags/{UV}"),
            &format!("/astral-sh/uv/releases/download/{UV}/{name}"),
            UV,
            &name,
            archive,
        );
    }

    /// The fake answering `api` with release `tag`, its one asset `name`
    /// holding `body`, served at `download`.
    fn serve(&self, api: &str, download: &str, tag: &str, name: &str, body: &[u8]) {
        let release = serde_json::json!({
            "tag_name": tag,
            "assets": [{
                "name": name,
                "size": body.len(),
                "digest": format!("sha256:{}", hex::encode(Sha256::digest(body))),
                "browser_download_url": format!("{}{download}", self.fake.url()),
            }],
        });
        self.fake.script(api, Answer::json(release.to_string()));
        self.fake.script(download, Answer::new(200, body.to_vec()));
    }

    /// The fake's bases file, for an sb run's
    /// [`sb_release::testing::BASES_VARIABLE`].
    #[must_use]
    pub fn bases(&self) -> PathBuf {
        self.root.path().join(BASES)
    }

    /// The log every stand-in appends its argv to, one line per call:
    /// `<program> <arguments>`.
    #[must_use]
    pub fn calls(&self) -> PathBuf {
        self.root.path().join("calls.log")
    }

    /// The calls so far, one per line (none when the log can't be read).
    #[must_use]
    pub fn calls_made(&self) -> Vec<String> {
        // No log yet is no call yet.
        let text = std::fs::read_to_string(self.calls()).unwrap_or_default();
        text.lines().map(str::to_string).collect()
    }

    /// The directory of the stand-in git, dpkg-query and apt-get.
    #[must_use]
    pub fn stand_ins(&self) -> PathBuf {
        self.root.path().join("stand-ins")
    }

    /// A dpkg-query whose package list is [`Tree::rows`]' (none by
    /// default).
    ///
    /// # Panics
    ///
    /// When it can't be written.
    fn dpkg_query(&self) {
        let rows = self.root.path().join("dpkg-rows");
        let script = format!(
            "{}\ncat '{}' 2>/dev/null; exit 0",
            self.calls_line(),
            rows.display()
        );
        let _ = stand_in(&self.stand_ins(), "dpkg-query", &script);
    }

    /// A generation `id` with a healthy venv on the Python release
    /// `python_id`, its manifest matching the tree's pins; not active.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    #[must_use]
    pub fn generation(&self, id: &str, python_id: &str) -> Generation {
        let python_install = self.layout.python_releases.join(python_id);
        let python = python_install.join("bin/python3.12");
        self.python(&python, PYTHON);
        let dir = self.layout.ansible_releases.join(id);
        let venv = dir.join("venv");
        self.venv(&venv);
        let manifest = Manifest {
            schema_version: crate::manifest::SCHEMA,
            generation_id: id.to_string(),
            python_version: PYTHON.to_string(),
            python_path: python.display().to_string(),
            python_install: python_install.display().to_string(),
            uv_version: UV.to_string(),
            lock_sha256: crate::toolchain::file_sha256(&self.layout.lock).expect("lock"),
            saltbox_commit: COMMIT.to_string(),
            created_at: "2026-11-13T12:00:00Z".to_string(),
            exported_commands: COMMANDS.iter().map(ToString::to_string).collect(),
        };
        self.manifest(&dir, &manifest);
        Generation {
            dir,
            venv,
            python_install,
            manifest,
        }
    }

    /// `manifest` written into `dir` as sb-go writes it.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn manifest(&self, dir: &Path, manifest: &Manifest) {
        let mut text = serde_json::to_string_pretty(manifest).expect("json");
        text.push('\n');
        std::fs::write(dir.join(crate::manifest::FILE), text).expect("manifest");
    }

    /// The active link pointed at `venv`.
    ///
    /// # Panics
    ///
    /// When it can't be made.
    pub fn activate(&self, venv: &Path) {
        let link = &self.layout.active_venv;
        std::fs::create_dir_all(link.parent().expect("parent")).expect("ansible dir");
        let _ = std::fs::remove_file(link);
        std::os::unix::fs::symlink(venv, link).expect("link");
    }

    /// A healthy tree: one generation, active, and the fact of the
    /// release [`FACT_TAG`] the fake serves for this build's architecture.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    #[must_use]
    pub fn healthy(&self) -> Generation {
        let generation = self.generation(GENERATION, PYTHON_RELEASE);
        self.activate(&generation.venv);
        let body = fact_binary(FACT_TAG.trim_start_matches('v'));
        self.serve_fact(Arch::current().expect("arch"), FACT_TAG, &body);
        self.fact(&body);
        generation
    }

    /// The fake serving saltyorg/ansible-facts' latest release `tag`, its
    /// asset for `arch` holding `body`, from GitHub's API (the proxy is
    /// down: unscripted, 404).
    pub fn serve_fact(&self, arch: Arch, tag: &str, body: &[u8]) {
        let name = crate::fact::asset_name(arch);
        self.serve(
            "/api/repos/saltyorg/ansible-facts/releases/latest",
            &format!("/saltyorg/ansible-facts/releases/download/{tag}/{name}"),
            tag,
            &name,
            body,
        );
    }

    /// The layout's fact holding `body`, mode 0755.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn fact(&self, body: &[u8]) {
        let path = &self.layout.fact;
        std::fs::create_dir_all(path.parent().expect("parent")).expect("facts dir");
        std::fs::write(path, body).expect("fact");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// The layout's fact as a script running `script` (after recording its
    /// call in [`Tree::calls`]).
    pub fn fact_script(&self, script: &str) {
        let body = format!("#!/bin/sh\n{}\n{script}\n", self.calls_line());
        self.fact(body.as_bytes());
    }

    /// Where the venv's entry points record their Ansible config.
    #[must_use]
    pub fn entry_points_log(&self) -> PathBuf {
        self.root.path().join("entry-points.log")
    }

    /// The script line appending a call to [`Tree::calls`].
    fn calls_line(&self) -> String {
        format!("echo \"$0 $*\" >> '{}'", self.calls().display())
    }

    /// A healthy venv at `venv`: a `bin/python3` reporting [`PYTHON`] and
    /// importing everything, and `ansible`, `certbot` and `apprise`
    /// reporting versions. Each entry point also appends its name and its
    /// Ansible config to [`Tree::entry_points_log`]: `empty-file` when
    /// `ANSIBLE_CONFIG` names an existing empty file, else its value or
    /// `unset`.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn venv(&self, venv: &Path) {
        let bin = venv.join("bin");
        self.python(&bin.join("python3"), PYTHON);
        self.entry_points(&bin);
    }

    /// The stand-in `ansible`, `certbot` and `apprise` in `bin`, each
    /// recording its Ansible config (see [`Tree::venv`]).
    fn entry_points(&self, bin: &Path) {
        let calls = self.calls_line();
        let record = self.entry_points_log();
        let record = record.display();
        for name in ["ansible", "certbot", "apprise"] {
            let script = format!(
                "{calls}\nc=${{ANSIBLE_CONFIG-unset}}\n\
                 if [ -f \"$c\" ] && [ ! -s \"$c\" ]; then c=empty-file; fi\n\
                 echo \"{name} $c\" >> '{record}'\necho '{name} 1.0'"
            );
            let _ = stand_in(bin, name, &script);
        }
    }

    /// Make uv's `op` (`python-install`, `venv---python`, `pip-sync`,
    /// `pip-check`, ...: its first two arguments after `--no-config`,
    /// joined by `-`) fail.
    ///
    /// # Panics
    ///
    /// When the marker can't be written.
    pub fn fail(&self, op: &str) {
        self.mark("fail", op);
    }

    /// Make uv's `op` fail on its next call only.
    ///
    /// # Panics
    ///
    /// When the marker can't be written.
    pub fn fail_once(&self, op: &str) {
        self.mark("fail-once", op);
    }

    /// Make uv's `op` hang, after it touches `hang/started` under the root.
    ///
    /// # Panics
    ///
    /// When the marker can't be written.
    pub fn hang(&self, op: &str) {
        self.mark("hang", op);
    }

    /// Make `uv python find` print `answer`.
    ///
    /// # Panics
    ///
    /// When it can't be written.
    pub fn find_answer(&self, answer: &str) {
        std::fs::write(self.root.path().join("find-answer"), answer).expect("answer");
    }

    fn mark(&self, kind: &str, op: &str) {
        let dir = self.root.path().join(kind);
        std::fs::create_dir_all(&dir).expect("marker dir");
        std::fs::write(dir.join(op), "").expect("marker");
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

    /// An [`Env`](crate::Env) on this tree with its git and the fake's
    /// client, for this build's architecture.
    ///
    /// # Panics
    ///
    /// On an architecture sb has no releases for.
    #[must_use]
    pub fn env(&self) -> crate::Env<'_> {
        let arch = Arch::current().expect("arch");
        let env = crate::Env::new(&self.layout, &self.releases, UV, arch);
        with_stand_ins(env, self.root.path())
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}
