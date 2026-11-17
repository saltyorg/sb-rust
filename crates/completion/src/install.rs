//! `sb completion install` (completion spec, "`sb completion`"; sb-go's
//! completion/manager.go, detection.go and files.go): each installed
//! shell's registration file for every name sb runs under, in the shell's
//! system loading directory, with sb-go's paths, probes and result lines.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use sb_exec::{Cmd, Ending};
use tokio_util::sync::CancellationToken;

use crate::Shell;

/// sb-go's `probeTimeout` (detection.go).
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// zsh's clean `fpath`, one directory a line (sb-go's `zshProbeCommand`).
const ZSH_PROBE: &str = r#"printf '%s\n' "${fpath[@]}""#;
/// fish's version and system configuration directory (sb-go's
/// `fishProbeCommand`).
const FISH_PROBE: &str = r#"printf '%s\n%s\n' "$version" "$__fish_sysconf_dir""#;

/// A name sb runs under (sb-go's `commandName`, manager.go).
static NAME: LazyLock<Regex> = LazyLock::new(|| {
    // A literal pattern: it compiles (regex's own example:
    // <https://docs.rs/regex/1.13.1/regex/#avoid-re-compiling-regexes-especially-in-a-loop>).
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*$").expect("a valid pattern")
});

/// A fish version (sb-go's `versionPattern`, detection.go).
static FISH_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9]+)\.([0-9]+)(?:\.[0-9]+)?(?:[-(][A-Za-z0-9()._-]+)?$")
        .expect("a valid pattern")
});

/// Where completion files go and shells are looked for: sb-go's paths
/// (manager.go `NewManager`, detection.go), each under a root in place of
/// `/` so tests install under a temporary directory (spec, Project
/// Structure). fish's directory comes from its probe, as it reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// sb-go's paths.
    #[must_use]
    pub fn system() -> Self {
        Self::under(Path::new("/"))
    }

    /// The paths `sb` uses: [`Paths::system`]; in a build with the
    /// `testing` feature (the sb `cargo test` runs), the tree under
    /// [`crate::testing::ROOT_VAR`] when that is set.
    #[must_use]
    pub fn for_sb() -> Self {
        #[cfg(feature = "testing")]
        if let Some(root) = crate::testing::root() {
            return Self::under(&root);
        }
        Self::system()
    }

    /// sb-go's paths under `root`.
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// Whether files may be written in `dir`: anywhere under `/`; under a
    /// test's root, only inside it, with no `..` to climb out, so a
    /// stand-in shell reporting another directory can't send a write onto
    /// the host.
    fn holds(&self, dir: &Path) -> bool {
        use std::path::Component;
        self.root == Path::new("/")
            || (dir.starts_with(&self.root)
                && !dir.components().any(|part| part == Component::ParentDir))
    }

    /// bash-completion's lazy-loading directory for local files.
    #[must_use]
    pub fn bash_dir(&self) -> PathBuf {
        self.at("/usr/local/share/bash-completion/completions")
    }

    /// The `fpath` directory for local functions.
    #[must_use]
    pub fn zsh_dir(&self) -> PathBuf {
        self.at("/usr/local/share/zsh/site-functions")
    }

    /// Where sb-go's scripts before 0.1.2 put bash's files (sb-go's
    /// `BashLegacyDir`), read at startup by bash-completion.
    fn bash_legacy_dir(&self) -> PathBuf {
        self.at("/etc/bash_completion.d")
    }

    /// Where they put zsh's (sb-go's `ZshLegacyDir`).
    fn zsh_legacy_dir(&self) -> PathBuf {
        self.at("/usr/share/zsh/vendor-completions")
    }

    /// `shell`'s directory sb-go's older scripts used, if any.
    fn legacy_dir(&self, shell: Shell) -> Option<PathBuf> {
        match shell {
            Shell::Bash => Some(self.bash_legacy_dir()),
            Shell::Zsh => Some(self.zsh_legacy_dir()),
            Shell::Fish => None,
        }
    }

    /// `shell`'s directory when no probe said where (sb-go's `directory`
    /// with no detection): bash's and zsh's are fixed, fish's comes only
    /// from its probe.
    fn known_dir(&self, shell: Shell) -> Option<PathBuf> {
        match shell {
            Shell::Bash => Some(self.bash_dir()),
            Shell::Zsh => Some(self.zsh_dir()),
            Shell::Fish => None,
        }
    }

    /// bash-completion itself, which bash's files need.
    fn bash_framework(&self) -> PathBuf {
        self.at("/usr/share/bash-completion/bash_completion")
    }

    /// Where shells and sb's names are looked for: not `PATH`, as sb-go.
    fn bin_dirs(&self) -> [PathBuf; 3] {
        [
            self.at("/usr/local/bin"),
            self.at("/usr/bin"),
            self.at("/bin"),
        ]
    }
}

/// A name sb runs under, and the path the shell's file calls for it: the
/// executable itself, or a symlink to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    name: String,
    path: PathBuf,
}

impl Name {
    /// The command name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The names sb runs under (sb-go's `binaryNamesForExecutable`): its
/// executable's own name, and each symlink to it in the bin directories
/// and its own directory; sorted, each once, the first path found kept.
///
/// # Errors
///
/// When the executable can't be resolved, a directory that exists can't
/// be read, or a name isn't sb-go's pattern (`^[A-Za-z0-9][A-Za-z0-9._-]*$`).
pub fn names(paths: &Paths, executable: &Path) -> Result<Vec<Name>, String> {
    let real = std::fs::canonicalize(executable)
        .map_err(|err| format!("resolve completion executable: {err}"))?;
    let mut found = BTreeMap::new();
    if let Some(name) = real.file_name() {
        found.insert(name.to_string_lossy().into_owned(), real.clone());
    }
    let mut dirs = paths.bin_dirs().to_vec();
    dirs.extend(executable.parent().map(Path::to_path_buf));
    dirs.extend(real.parent().map(Path::to_path_buf));
    let mut seen = Vec::new();
    for dir in dirs {
        let Ok(canonical) = std::fs::canonicalize(&dir) else {
            continue;
        };
        if seen.contains(&canonical) {
            continue;
        }
        seen.push(canonical.clone());
        let entries = std::fs::read_dir(&canonical).map_err(|err| {
            format!(
                "read completion executable directory {}: {err}",
                canonical.display()
            )
        })?;
        for entry in entries.flatten() {
            let is_link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
            if is_link && std::fs::canonicalize(entry.path()).is_ok_and(|target| target == real) {
                let name = entry.file_name().to_string_lossy().into_owned();
                found.entry(name.clone()).or_insert_with(|| dir.join(&name));
            }
        }
    }
    for (name, path) in &found {
        if !NAME.is_match(name) {
            return Err(format!("invalid executable name {name:?}"));
        }
        carried(path)?;
    }
    Ok(found
        .into_iter()
        .map(|(name, path)| Name { name, path })
        .collect())
}

/// Check that clap's stubs carry `path` as it is: UTF-8, with nothing a
/// shell would quote. clap's bash stub puts shlex's quoting inside double
/// quotes, where it doesn't hold (`clap_complete` 4.6.11,
/// `env/shells.rs`), so a `$(...)` in the path would run at Tab time and
/// a space would break completion. sb's paths on Saltbox
/// (`/usr/local/bin/sb`) pass. (clap's templates would also rewrite the
/// words `VAR`, `NAME`, `BIN` and `COMPLETER` in a path; no path of sb's
/// holds them, and checking for them would refuse random test paths.)
fn carried(path: &Path) -> Result<(), String> {
    let plain = path
        .to_str()
        .is_some_and(|text| matches!(shlex::try_quote(text), Ok(std::borrow::Cow::Borrowed(_))));
    if plain {
        Ok(())
    } else {
        Err(format!(
            "invalid executable path {:?}",
            path.display().to_string()
        ))
    }
}

/// One line of an install's report (sb-go's `writeCompletionResults`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// A file written.
    Installed { shell: Shell, path: PathBuf },
    /// A shell left alone, and why.
    Skipped { shell: Shell, reason: String },
    /// A file kept, and why.
    Preserved {
        shell: Shell,
        path: PathBuf,
        reason: String,
    },
    /// A file for a name no longer installed, removed.
    RemovedObsolete { shell: Shell, path: PathBuf },
    /// An old output for an installed name, removed after its new file.
    RemovedOld { path: PathBuf },
    /// A shell or a name that failed, and why.
    Failed { shell: Shell, reason: String },
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Installed { shell, path } => {
                write!(
                    f,
                    "Installed {} completion: {}",
                    shell.name(),
                    path.display()
                )
            }
            Self::Skipped { shell, reason } => {
                write!(f, "Skipped {} completion: {reason}", shell.name())
            }
            Self::Preserved {
                shell,
                path,
                reason,
            } => write!(
                f,
                "Preserved {} completion: {}; {reason}",
                shell.name(),
                path.display()
            ),
            Self::RemovedObsolete { shell, path } => write!(
                f,
                "Removed obsolete {} completion: {}",
                shell.name(),
                path.display()
            ),
            Self::RemovedOld { path } => write!(f, "Removed old completion: {}", path.display()),
            Self::Failed { shell, reason } => {
                write!(f, "Failed {} completion: {reason}", shell.name())
            }
        }
    }
}

/// What an install did: its lines in order, the failures (each sb-go's
/// error text), and the shells skipped as unavailable with why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<Line>,
    pub failures: Vec<String>,
    pub unavailable: Vec<(Shell, String)>,
}

/// What a probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Detection {
    /// The shell isn't installed.
    Missing,
    /// It is, but sb's file wouldn't load: why.
    Unavailable(String),
    /// It is, and its files go in this directory.
    Ready(PathBuf),
}

/// Install each of `shells`' files for every one of `names` (sb-go's
/// `Manager.Install`): a shell that isn't installed, or can't load sb's
/// file, is skipped with its reason; a failure is reported and the other
/// shells and names go on. Each shell once; a cancelled `token` stops at
/// the next step. Then, wherever the shell's directory is known, sb-go's
/// cleanup ([`retire`]): `own_dir`, sb's executable's directory, is one
/// of the places a live executable of a retired name is looked for.
pub async fn install(
    paths: &Paths,
    shells: &[Shell],
    names: &[Name],
    own_dir: &Path,
    token: &CancellationToken,
) -> Report {
    let mut report = Report::default();
    let mut done = Vec::new();
    for &shell in shells {
        if token.is_cancelled() {
            report.failures.push("interrupted".to_owned());
            break;
        }
        if done.contains(&shell) {
            continue;
        }
        done.push(shell);
        let retired = |report: &mut Report, dir: Option<PathBuf>| {
            if let Some(dir) = dir {
                retire(paths, shell, names, &dir, own_dir, token, report);
            }
        };
        match detect(paths, shell, token).await {
            Err(reason) => {
                report
                    .failures
                    .push(format!("detect {}: {reason}", shell.name()));
                report.lines.push(Line::Failed { shell, reason });
                retired(&mut report, paths.known_dir(shell));
            }
            Ok(Detection::Missing) => {
                let reason = "shell is not installed".to_owned();
                report.unavailable.push((shell, reason.clone()));
                report.lines.push(Line::Skipped { shell, reason });
                retired(&mut report, paths.known_dir(shell));
            }
            Ok(Detection::Unavailable(reason)) => {
                report.unavailable.push((shell, reason.clone()));
                report.lines.push(Line::Skipped { shell, reason });
                retired(&mut report, paths.known_dir(shell));
            }
            Ok(Detection::Ready(dir)) if !paths.holds(&dir) => {
                let reason = format!(
                    "completion directory {} is outside the install root {}",
                    dir.display(),
                    paths.root.display()
                );
                report
                    .failures
                    .push(format!("install {}: {reason}", shell.name()));
                report.lines.push(Line::Failed { shell, reason });
            }
            Ok(Detection::Ready(dir)) => {
                install_shell(paths, shell, &dir, names, token, &mut report);
                retired(&mut report, Some(dir));
            }
        }
    }
    report
}

/// Write `shell`'s file for each of `names` in `dir`, then remove that
/// name's old outputs (sb-go's `installOne`): the legacy directory's
/// file, and for bash a bare-name file that isn't another name's
/// `.bash` file. Each removal is a line after the file's.
fn install_shell(
    paths: &Paths,
    shell: Shell,
    dir: &Path,
    names: &[Name],
    token: &CancellationToken,
    report: &mut Report,
) {
    for name in names {
        let path = dir.join(file_name(shell, &name.name));
        let mut old = Vec::new();
        if let Some(legacy) = paths.legacy_dir(shell) {
            old.push(legacy.join(legacy_file_name(shell, &name.name)));
        }
        // sb-go's `bashAliasNames` (refreshing a separate live executable
        // named `<name>.bash` with this one's files) is left out: spec,
        // difference 8.
        let bare_is_another_names = names
            .iter()
            .any(|other| name.name.strip_suffix(".bash") == Some(other.name.as_str()));
        if shell == Shell::Bash && !bare_is_another_names {
            old.push(dir.join(&name.name));
        }
        let mut removed = Vec::new();
        let done = if token.is_cancelled() {
            Err("interrupted".to_owned())
        } else {
            ensure_dir(dir)
                .and_then(|()| write_atomic(&path, &content(shell, name, names)))
                .map_err(|err| err.to_string())
                .and_then(|()| {
                    old.into_iter().try_for_each(|old| {
                        if token.is_cancelled() {
                            return Err("interrupted".to_owned());
                        }
                        if remove(&old)? {
                            removed.push(old);
                        }
                        Ok(())
                    })
                })
        };
        match done {
            Ok(()) => report.lines.push(Line::Installed { shell, path }),
            Err(reason) => {
                report.failures.push(format!(
                    "install {} completion for {}: {reason}",
                    shell.name(),
                    name.name
                ));
                report.lines.push(Line::Failed { shell, reason });
            }
        }
        report
            .lines
            .extend(removed.into_iter().map(|path| Line::RemovedOld { path }));
    }
}

/// `name`'s file in a shell's legacy directory (sb-go's `legacyPath`).
fn legacy_file_name(shell: Shell, name: &str) -> String {
    match shell {
        Shell::Bash | Shell::Fish => name.to_owned(),
        Shell::Zsh => format!("_{name}"),
    }
}

/// Remove `path`; whether it was there (sb-go's `removeCompletion`).
fn remove(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!(
            "remove obsolete completion {}: {err}",
            path.display()
        )),
    }
}

/// sb-go's cleanup (`cleanupObsolete`): in `dir` and the shell's legacy
/// directory, each regular file of a name not in `names` is removed when
/// it is sb's or sb-go's ([`identifies`]), unless an executable of that
/// name is still installed, which keeps it. Others' files are never
/// touched. Its failures are one failure, and one line.
fn retire(
    paths: &Paths,
    shell: Shell,
    names: &[Name],
    dir: &Path,
    own_dir: &Path,
    token: &CancellationToken,
    report: &mut Report,
) {
    if token.is_cancelled() {
        report.failures.push("interrupted".to_owned());
        report.lines.push(Line::Failed {
            shell,
            reason: "interrupted".to_owned(),
        });
        return;
    }
    let mut errors = Vec::new();
    let dirs = std::iter::once((dir.to_path_buf(), false))
        .chain(paths.legacy_dir(shell).map(|legacy| (legacy, true)));
    for (dir, legacy) in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                errors.push(format!(
                    "inspect obsolete {} completions: {err}",
                    shell.name()
                ));
                continue;
            }
        };
        let mut files: Vec<_> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        for file in files {
            let Some(name) = alias_from_file(shell, &file, legacy) else {
                continue;
            };
            if !NAME.is_match(name) || names.iter().any(|known| known.name == name) {
                continue;
            }
            let path = dir.join(&file);
            let data = match std::fs::read(&path) {
                Ok(data) => data,
                Err(err) => {
                    errors.push(format!("read {}: {err}", path.display()));
                    continue;
                }
            };
            if !identifies(&data, shell, name) {
                continue;
            }
            match name_in_use(paths, own_dir, name) {
                Err(err) => errors.push(format!(
                    "check obsolete completion executable {name}: {err}"
                )),
                Ok(true) => report.lines.push(Line::Preserved {
                    shell,
                    path,
                    reason: "preserved completion for a live executable outside this installation"
                        .to_owned(),
                }),
                Ok(false) => match remove(&path) {
                    Ok(true) => report.lines.push(Line::RemovedObsolete { shell, path }),
                    Ok(false) => {}
                    Err(err) => errors.push(err),
                },
            }
        }
    }
    if !errors.is_empty() {
        let reason = errors.join("\n");
        report.failures.push(reason.clone());
        report.lines.push(Line::Failed { shell, reason });
    }
}

/// The name a file in a shell's directory is for (sb-go's
/// `aliasFromFile`): `<name>.bash` (a legacy bash file is the bare name),
/// `_<name>`, `<name>.fish`.
fn alias_from_file(shell: Shell, file: &str, legacy: bool) -> Option<&str> {
    let name = match shell {
        Shell::Bash if legacy => Some(file),
        Shell::Bash => file.strip_suffix(".bash"),
        Shell::Zsh => file.strip_prefix('_'),
        Shell::Fish => file.strip_suffix(".fish"),
    };
    name.filter(|name| !name.is_empty())
}

/// Whether `data` is sb's or sb-go's file for `name` (sb-go's
/// `identifiesCompletion`, aliases.go): sb's marker, sb-go's two markers,
/// or the text of sb-go's scripts before 0.1.2.
fn identifies(data: &[u8], shell: Shell, name: &str) -> bool {
    // Go's `bytes.Contains`; std has no substring search on bytes.
    // https://docs.rs/memchr/2.8.3/memchr/memmem/fn.find.html
    let has = |text: &str| memchr::memmem::find(data, text.as_bytes()).is_some();
    let shell_name = shell.name();
    if has(&marker(shell, name))
        || has(&format!("# sb-go completion {shell_name} {name}\n"))
        || has(&format!(
            "# sb-go managed completion v1 {shell_name} {name} "
        ))
    {
        return true;
    }
    if has("# Custom tag completion for 'install' command with comma-separated support")
        && has(&format!("_{name}_custom_install_tags()"))
    {
        return true;
    }
    match shell {
        Shell::Bash => {
            data.starts_with(format!("# bash completion for {name} ").as_bytes())
                && has(&format!("_{name}_install_completion()"))
        }
        Shell::Zsh => {
            data.starts_with(
                format!("#compdef {name}\n\n# {name} zsh completion script\n").as_bytes(),
            ) && has(&format!("_{name}_install_tags()"))
        }
        Shell::Fish => false,
    }
}

/// Whether `name` is an executable regular file in the bin directories or
/// `own_dir` (sb-go's `executableNameInUse`): a retired alias that is
/// still some program's name keeps its file. Why not the `is_executable`
/// crate: it reads every metadata error as "not executable", where sb-go
/// reports one, so a file isn't removed on a check that couldn't be made.
fn name_in_use(paths: &Paths, own_dir: &Path, name: &str) -> Result<bool, String> {
    let mut dirs = paths.bin_dirs().to_vec();
    if !dirs.iter().any(|dir| dir == own_dir) {
        dirs.push(own_dir.to_path_buf());
    }
    for dir in dirs {
        match std::fs::metadata(dir.join(name)) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.to_string()),
            Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => {
                return Ok(true);
            }
            Ok(_) => {}
        }
    }
    Ok(false)
}

/// `name`'s file in a shell's directory (sb-go's `fileName`).
fn file_name(shell: Shell, name: &str) -> String {
    match shell {
        Shell::Bash => format!("{name}.bash"),
        Shell::Zsh => format!("_{name}"),
        Shell::Fish => format!("{name}.fish"),
    }
}

/// The marker line that names sb's files (spec, difference 3).
fn marker(shell: Shell, name: &str) -> String {
    format!("# sb completion {} {name}\n", shell.name())
}

/// `name`'s file: clap's stub for it, calling its own path, with the
/// marker first, or second after zsh's `#compdef` line, which compinit
/// reads first (sb-go's `completionContent`). bash's holds every name's
/// stub: bash-completion versions differ on whether they look up
/// `<name>` or `<name>.bash` (sb-go's bash bundle, manager.go).
fn content(shell: Shell, name: &Name, names: &[Name]) -> Vec<u8> {
    let stub = |name: &Name| shell.registration(&name.name, &name.path.to_string_lossy());
    let body = match shell {
        Shell::Bash => names.iter().flat_map(stub).collect(),
        Shell::Zsh | Shell::Fish => stub(name),
    };
    let marker = marker(shell, &name.name);
    let mut out = Vec::with_capacity(marker.len() + body.len());
    let first_line = body
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|end| body.split_at(end + 1));
    match first_line {
        Some((first, rest)) if shell == Shell::Zsh && first.starts_with(b"#compdef ") => {
            out.extend_from_slice(first);
            out.extend_from_slice(marker.as_bytes());
            out.extend_from_slice(rest);
        }
        _ => {
            out.extend_from_slice(marker.as_bytes());
            out.extend_from_slice(&body);
        }
    }
    out
}

/// Create `dir` and any missing parent, each 0755 whatever the umask, as
/// sb-go's `ensureDirectory`: made with mode 0755, so never wider, then
/// set to it. Why not std's `DirBuilder::recursive` with `mode` alone:
/// mkdir(2) takes the umask off the mode
/// (<https://man7.org/linux/man-pages/man2/mkdir.2.html>), so a 0077
/// umask would leave 0700.
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    match std::fs::DirBuilder::new().mode(0o755).create(dir) {
        Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if dir.is_dir() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "completion directory path is not a directory: {}",
                    dir.display()
                )))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            match dir.parent() {
                Some(parent) if parent != dir => ensure_dir(parent)?,
                _ => return Err(err),
            }
            ensure_dir(dir)
        }
        Err(err) => Err(std::io::Error::other(format!(
            "create completion directory {}: {err}",
            dir.display()
        ))),
    }
}

/// Write `path` whole or not at all: a temporary file in its directory,
/// 0644, synced, then renamed over it (sb-go's `atomicWrite`; tempfile's
/// `persist`, <https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist>).
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let mut file = tempfile::Builder::new()
        .prefix(".sb-completion-")
        .tempfile_in(dir)
        .map_err(|err| std::io::Error::other(format!("create temporary completion: {err}")))?;
    // Set on the open file, not through tempfile's `Builder::permissions`:
    // that mode is masked by the umask (tempfile 3.27.0's docs), and the
    // file must be 0644 whatever the umask.
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    file.write_all(data)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|err| {
        std::io::Error::other(format!(
            "replace completion {}: {}",
            path.display(),
            err.error
        ))
    })?;
    Ok(())
}

/// Whether `shell` is installed and where its files go (sb-go's `detect`).
async fn detect(
    paths: &Paths,
    shell: Shell,
    token: &CancellationToken,
) -> Result<Detection, String> {
    let Some(executable) = shell_executable(paths, shell.name())? else {
        return Ok(Detection::Missing);
    };
    match shell {
        Shell::Bash => {
            let framework = paths.bash_framework();
            match std::fs::metadata(&framework) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(
                    Detection::Unavailable("bash-completion is not installed".to_owned()),
                ),
                Err(err) => Err(format!("inspect bash-completion: {err}")),
                Ok(meta) if !meta.is_file() => {
                    Err("bash-completion framework is not a regular file".to_owned())
                }
                Ok(_) => Ok(Detection::Ready(paths.bash_dir())),
            }
        }
        Shell::Zsh => {
            let output = probe(
                Cmd::new(executable)
                    .arg("-d")
                    .arg("-f")
                    .arg("-c")
                    .arg(ZSH_PROBE),
                token,
            )
            .await?;
            let dir = paths.zsh_dir();
            if output.lines().any(|line| Path::new(line) == dir) {
                Ok(Detection::Ready(dir))
            } else {
                Ok(Detection::Unavailable(format!(
                    "Zsh's clean function path does not contain {}; add it to the administrator fpath before installing",
                    dir.display()
                )))
            }
        }
        Shell::Fish => {
            let output = probe(
                Cmd::new(executable)
                    .arg("--no-config")
                    .arg("-c")
                    .arg(FISH_PROBE),
                token,
            )
            .await?;
            fish_metadata(&output)
        }
    }
}

/// fish's probe output read (sb-go's `fishMetadata`): its version, which
/// must be 3.3 or newer, and a clean absolute directory other than `/`.
fn fish_metadata(output: &str) -> Result<Detection, String> {
    let lines: Vec<&str> = output
        .strip_suffix('\n')
        .unwrap_or(output)
        .split('\n')
        .collect();
    let [version, sysconf] = lines[..] else {
        return Err("fish returned malformed installation metadata".to_owned());
    };
    let Some(parts) = FISH_VERSION.captures(version) else {
        return Err(format!(
            "fish returned malformed version metadata {version:?}"
        ));
    };
    let number = |index: usize| parts[index].parse::<u64>().ok();
    if !matches!((number(1), number(2)), (Some(major), Some(minor)) if (major, minor) >= (3, 3)) {
        return Ok(Detection::Unavailable(
            "Fish 3.3 or newer is required for configuration-free detection".to_owned(),
        ));
    }
    let path = Path::new(sysconf);
    if !path.is_absolute() || !is_clean(sysconf) || sysconf == "/" || sysconf.contains('\0') {
        return Err(format!(
            "fish returned an invalid system configuration directory {sysconf:?}"
        ));
    }
    Ok(Detection::Ready(path.join("completions")))
}

/// Whether `path` is as Go's `filepath.Clean` leaves it: no `.` or `..`,
/// no doubled or trailing `/`. Compared as text: `Path`'s own equality
/// compares components, which would call `/etc//fish` clean.
fn is_clean(path: &str) -> bool {
    use std::path::Component;
    let rebuilt: PathBuf = Path::new(path).components().collect();
    let dots = Path::new(path)
        .components()
        .any(|part| matches!(part, Component::CurDir | Component::ParentDir));
    !dots && rebuilt.as_os_str() == path
}

/// The shell's executable in the bin directories, the first found (sb-go's
/// `shellExecutable`). Why not the `which` crate: it skips a file that
/// isn't executable and looks on, where sb-go stops with an error.
fn shell_executable(paths: &Paths, name: &str) -> Result<Option<PathBuf>, String> {
    for dir in paths.bin_dirs() {
        let candidate = dir.join(name);
        match std::fs::metadata(&candidate) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("inspect {name} executable: {err}")),
            Ok(meta) if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 => {
                return Err(format!(
                    "{name} is not an executable regular file: {}",
                    candidate.display()
                ));
            }
            Ok(_) => return Ok(Some(candidate)),
        }
    }
    Ok(None)
}

/// Run a probe (sb-go's `runProbe`): no environment but a fixed `PATH`,
/// `HOME=/` and the C locale, so no user configuration is read; 5 s at
/// most. Its stdout, or why it failed with its output.
async fn probe(cmd: Cmd, token: &CancellationToken) -> Result<String, String> {
    let cmd = cmd
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .timeout(PROBE_TIMEOUT);
    let captured = cmd
        .capture(token)
        .await
        .map_err(|err| format!("shell probe failed: {err}"))?;
    match captured.outcome.ending {
        Ending::Exited(0) => Ok(captured.stdout),
        Ending::Interrupted | Ending::TimedOut => Err(format!(
            "shell probe interrupted: {}",
            captured.outcome.ending.reason()
        )),
        // Go's `exec.ExitError` text, as sb-go's message carries it.
        Ending::Exited(code) => Err(format!(
            "shell probe failed: exit status {code} ({})",
            format!("{}{}", captured.stdout, captured.stderr).trim()
        )),
        ending @ Ending::Signalled(_) => Err(format!(
            "shell probe failed: {} ({})",
            ending.reason(),
            format!("{}{}", captured.stdout, captured.stderr).trim()
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt as _;

    use super::*;

    #[test]
    fn fish_metadata_is_read_as_sb_go_reads_it() {
        let ready = |dir: &str| Ok(Detection::Ready(PathBuf::from(dir)));
        assert_eq!(
            fish_metadata("3.3.1\n/etc/fish\n"),
            ready("/etc/fish/completions")
        );
        assert_eq!(
            fish_metadata("4.2.1\n/etc/fish"),
            ready("/etc/fish/completions")
        );
        assert_eq!(
            fish_metadata("3.10\n/etc/fish\n"),
            ready("/etc/fish/completions")
        );
        assert_eq!(
            fish_metadata("3.7.0-1\n/etc/fish\n"),
            ready("/etc/fish/completions")
        );
        let old = Ok(Detection::Unavailable(
            "Fish 3.3 or newer is required for configuration-free detection".to_owned(),
        ));
        assert_eq!(fish_metadata("3.2.2\n/etc/fish\n"), old);
        assert_eq!(fish_metadata("2.7\n/etc/fish\n"), old);
        for bad in ["", "3.3\n", "3.3\n/etc/fish\nmore\n"] {
            assert_eq!(
                fish_metadata(bad),
                Err("fish returned malformed installation metadata".to_owned()),
                "{bad:?}"
            );
        }
        assert_eq!(
            fish_metadata("three\n/etc/fish\n"),
            Err("fish returned malformed version metadata \"three\"".to_owned())
        );
        for dir in [
            "etc/fish",
            "/",
            "/etc/../fish",
            "/etc//fish",
            "/etc/fish/",
            "/etc/./fish",
        ] {
            assert_eq!(
                fish_metadata(&format!("3.3\n{dir}\n")),
                Err(format!(
                    "fish returned an invalid system configuration directory {dir:?}"
                )),
                "{dir}"
            );
        }
    }

    /// Under `/` every directory holds; under a test root only those
    /// inside it, with no `..`.
    #[test]
    fn a_test_root_holds_only_its_own_directories() {
        assert!(Paths::system().holds(Path::new("/etc/fish/completions")));
        let paths = Paths::under(Path::new("/tmp/root"));
        assert!(paths.holds(Path::new("/tmp/root/etc/fish/completions")));
        assert!(paths.holds(&paths.zsh_dir()));
        assert!(!paths.holds(Path::new("/etc/fish/completions")));
        assert!(!paths.holds(Path::new("/tmp/rootx/completions")));
        assert!(!paths.holds(Path::new("/tmp/root/../etc/completions")));
    }

    /// sb-go's `identifiesCompletion` cases: each marker for its shell and
    /// name only; the old scripts' text needs both its parts.
    #[test]
    fn sb_and_sb_go_files_are_recognised() {
        let yes =
            |data: &str, shell, name| assert!(identifies(data.as_bytes(), shell, name), "{data:?}");
        let no = |data: &str, shell, name| {
            assert!(!identifies(data.as_bytes(), shell, name), "{data:?}");
        };
        yes("x\n# sb completion zsh old\n", Shell::Zsh, "old");
        no("# sb completion zsh old\n", Shell::Bash, "old");
        no("# sb completion zsh older\n", Shell::Zsh, "old");
        no("# sb completion zsh old", Shell::Zsh, "old");
        yes("# sb-go completion fish sbx\n", Shell::Fish, "sbx");
        no("# sb-go completion fish sbx2\n", Shell::Fish, "sbx");
        yes(
            "# sb-go managed completion v1 bash sbx 0123\n",
            Shell::Bash,
            "sbx",
        );
        no(
            "# sb-go managed completion v1 bash sbx\n",
            Shell::Bash,
            "sbx",
        );
        let custom = "# Custom tag completion for 'install' command with comma-separated support\n";
        yes(
            &format!("{custom}_sbx_custom_install_tags() {{"),
            Shell::Fish,
            "sbx",
        );
        no(custom, Shell::Zsh, "sbx");
        no("_sbx_custom_install_tags() {", Shell::Zsh, "sbx");
        yes(
            "# bash completion for sbx -*- shell-script -*-\n_sbx_install_completion() {",
            Shell::Bash,
            "sbx",
        );
        no("# bash completion for sbx -*-\n", Shell::Bash, "sbx");
        no(
            "x# bash completion for sbx \n_sbx_install_completion()",
            Shell::Bash,
            "sbx",
        );
        no(
            "# bash completion for sbx \n_sbx_install_completion()",
            Shell::Zsh,
            "sbx",
        );
        yes(
            "#compdef sbx\n\n# sbx zsh completion script\n_sbx_install_tags() {",
            Shell::Zsh,
            "sbx",
        );
        no(
            "#compdef sbx\n# sbx zsh completion script\n_sbx_install_tags()",
            Shell::Zsh,
            "sbx",
        );
        no(
            "#compdef sbx\n\n# sbx zsh completion script\n",
            Shell::Zsh,
            "sbx",
        );
        no("", Shell::Fish, "sbx");
    }

    #[test]
    fn a_files_name_is_sb_gos_alias() {
        assert_eq!(alias_from_file(Shell::Bash, "sbx.bash", false), Some("sbx"));
        assert_eq!(alias_from_file(Shell::Bash, "sbx", false), None);
        assert_eq!(alias_from_file(Shell::Bash, ".bash", false), None);
        assert_eq!(alias_from_file(Shell::Bash, "sbx", true), Some("sbx"));
        assert_eq!(alias_from_file(Shell::Zsh, "_sbx", false), Some("sbx"));
        assert_eq!(alias_from_file(Shell::Zsh, "_", true), None);
        assert_eq!(alias_from_file(Shell::Zsh, "sbx", false), None);
        assert_eq!(alias_from_file(Shell::Fish, "sbx.fish", false), Some("sbx"));
        assert_eq!(alias_from_file(Shell::Fish, "sbx", false), None);
        assert_eq!(legacy_file_name(Shell::Bash, "sbx"), "sbx");
        assert_eq!(legacy_file_name(Shell::Zsh, "sbx"), "_sbx");
    }

    /// A live executable is an executable regular file of that name in the
    /// bin directories or sb's own; a plain file or a directory isn't.
    #[test]
    fn a_live_executable_is_an_executable_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::under(root.path());
        let own = root.path().join("own");
        std::fs::create_dir_all(root.path().join("usr/bin/dir")).expect("dirs");
        std::fs::create_dir_all(&own).expect("own");
        let write = |path: &Path, mode| {
            std::fs::write(path, "").expect("write");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        write(&root.path().join("usr/bin/plain"), 0o644);
        write(&root.path().join("usr/bin/live"), 0o755);
        write(&own.join("mine"), 0o700);
        assert_eq!(name_in_use(&paths, &own, "live"), Ok(true));
        assert_eq!(name_in_use(&paths, &own, "mine"), Ok(true));
        assert_eq!(name_in_use(&paths, &own, "plain"), Ok(false));
        assert_eq!(name_in_use(&paths, &own, "dir"), Ok(false));
        assert_eq!(name_in_use(&paths, &own, "none"), Ok(false));
        assert_eq!(
            name_in_use(&paths, &root.path().join("usr/bin"), "mine"),
            Ok(false)
        );
        // A directory that is a file: an error, not "not installed".
        assert_eq!(
            name_in_use(&paths, &root.path().join("usr/bin/plain"), "mine"),
            Err("Not a directory (os error 20)".to_owned())
        );
    }

    /// A path the stubs can't carry as it is fails the install.
    #[test]
    fn only_paths_the_stubs_carry() {
        assert_eq!(carried(Path::new("/usr/local/bin/sb")), Ok(()));
        assert_eq!(carried(Path::new("/opt/sb-2.0/s_b+x")), Ok(()));
        for bad in [
            "/opt/$(touch x)/sb",
            "/opt/my dir/sb",
            "/opt/a'b/sb",
            "/opt/`x`/sb",
        ] {
            assert_eq!(
                carried(Path::new(bad)),
                Err(format!("invalid executable path {bad:?}")),
                "{bad}"
            );
        }
        let not_utf8 = std::ffi::OsStr::from_bytes(b"/opt/\xff/sb");
        assert!(carried(Path::new(not_utf8)).is_err());
    }

    #[test]
    fn file_names_and_markers_are_sb_gos_shape() {
        assert_eq!(file_name(Shell::Bash, "sb"), "sb.bash");
        assert_eq!(file_name(Shell::Zsh, "sb"), "_sb");
        assert_eq!(file_name(Shell::Fish, "sb"), "sb.fish");
        assert_eq!(
            marker(Shell::Zsh, "saltbox"),
            "# sb completion zsh saltbox\n"
        );
    }

    fn name(name: &str, path: &str) -> Name {
        Name {
            name: name.to_owned(),
            path: PathBuf::from(path),
        }
    }

    /// The marker first, or after zsh's `#compdef`; each name's stub calls
    /// its own path; bash's file holds every name's stub.
    #[test]
    fn content_is_the_marker_and_claps_stubs() {
        let names = [
            name("saltbox", "/usr/bin/saltbox"),
            name("sb", "/usr/local/bin/sb"),
        ];
        let text = |shell, index: usize| {
            String::from_utf8(content(shell, &names[index], &names)).expect("UTF-8")
        };
        let zsh = text(Shell::Zsh, 0);
        assert!(
            zsh.starts_with("#compdef saltbox\n# sb completion zsh saltbox\n"),
            "{zsh}"
        );
        assert!(zsh.contains("/usr/bin/saltbox"), "{zsh}");
        assert!(!zsh.contains("/usr/local/bin/sb"), "{zsh}");
        let fish = text(Shell::Fish, 1);
        assert!(
            fish.starts_with("# sb completion fish sb\ncomplete "),
            "{fish}"
        );
        assert!(fish.contains("/usr/local/bin/sb"), "{fish}");
        let bash = text(Shell::Bash, 1);
        assert!(bash.starts_with("# sb completion bash sb\n"), "{bash}");
        assert_eq!(bash.matches("# sb completion").count(), 1, "{bash}");
        for (registered, path) in [("saltbox", "/usr/bin/saltbox"), ("sb", "/usr/local/bin/sb")] {
            assert!(
                bash.contains(&format!("_clap_complete_{registered} {registered}\n")),
                "{bash}"
            );
            assert!(bash.contains(path), "{bash}");
        }
    }

    #[test]
    fn lines_are_sb_gos_texts() {
        let path = PathBuf::from("/x/sb.bash");
        let cases = [
            (
                Line::Installed {
                    shell: Shell::Bash,
                    path: path.clone(),
                },
                "Installed bash completion: /x/sb.bash",
            ),
            (
                Line::Skipped {
                    shell: Shell::Zsh,
                    reason: "why".to_owned(),
                },
                "Skipped zsh completion: why",
            ),
            (
                Line::Preserved {
                    shell: Shell::Fish,
                    path: path.clone(),
                    reason: "why".to_owned(),
                },
                "Preserved fish completion: /x/sb.bash; why",
            ),
            (
                Line::RemovedObsolete {
                    shell: Shell::Bash,
                    path: path.clone(),
                },
                "Removed obsolete bash completion: /x/sb.bash",
            ),
            (
                Line::RemovedOld { path },
                "Removed old completion: /x/sb.bash",
            ),
            (
                Line::Failed {
                    shell: Shell::Fish,
                    reason: "why".to_owned(),
                },
                "Failed fish completion: why",
            ),
        ];
        for (line, text) in cases {
            assert_eq!(line.to_string(), text);
        }
    }

    /// Missing directories are made 0755 (the umask, the process's, is
    /// the test's: sb's binary test runs under 0077); an existing one is
    /// kept; a file in the way fails.
    #[test]
    fn directories_are_made_0755() {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = root.path().join("a/b/c");
        ensure_dir(&dir).expect("made");
        for path in [root.path().join("a"), root.path().join("a/b"), dir.clone()] {
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "{}", path.display());
        }
        ensure_dir(&dir).expect("kept");
        std::fs::write(root.path().join("file"), "").expect("file");
        let err = ensure_dir(&root.path().join("file")).expect_err("not a dir");
        assert!(err.to_string().contains("is not a directory"), "{err}");
    }

    /// The file is written whole, 0644, and replaces the old one; no
    /// temporary file is left.
    #[test]
    fn writes_are_atomic_and_0644() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("sb.bash");
        std::fs::write(&path, "old").expect("old");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        write_atomic(&path, b"new").expect("written");
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        assert_eq!(std::fs::read_dir(root.path()).expect("list").count(), 1);
        let missing = root.path().join("no/such/sb.bash");
        assert!(write_atomic(&missing, b"x").is_err());
    }
}
