//! The tag cache (ansible spec, "The tag cache"; module build M32):
//! sb-go's `cache.json` at the Saltbox root, one entry per repository
//! keyed by its absolute path, `{"<path>": {"commit": "<sha>", "tags":
//! [...]}}`. An entry is valid for its repository's HEAD commit; a stale,
//! missing or empty one is rebuilt from `ansible-playbook --list-tags`
//! inside a term-ui step, and the whole file written atomically, 0644.

use std::cell::LazyCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use sb_cli_core::exit::Exit;
use sb_exec::Cmd;
use sb_term_ui::{DisplayText, Labels, Runner, Step};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Ansible, Kind, Repo};

/// The cache's file name at the Saltbox root (sb-go layout; Saltbox's
/// `.gitignore` lists it).
pub const FILE: &str = "cache.json";

/// `--list-tags`' line (sb-go ansible/ansible.go, `PrepareAnsibleListTags`):
/// the first `TASK TAGS: [...]`.
static TASK_TAGS: LazyLock<Regex> = LazyLock::new(|| {
    // A literal pattern: it compiles (regex's own example:
    // <https://docs.rs/regex/1.13.1/regex/#avoid-re-compiling-regexes-especially-in-a-loop>).
    Regex::new(r"TASK TAGS:\s*\[(.*?)]").expect("a valid pattern")
});

/// One repository's entry, as sb-go writes it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoTags {
    /// The HEAD commit the tags were listed at.
    pub commit: String,
    /// The tags, in Ansible's order.
    pub tags: Vec<String>,
}

/// A repository's tags and whether they came from the cache (`sb list`'s
/// `tags listed` run-log record).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The tags, in Ansible's order.
    pub tags: Vec<String>,
    /// Whether they came from the file (no listing ran).
    pub cached: bool,
}

/// Why a repository's tags couldn't be had.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Its HEAD commit couldn't be read (repo's `head`).
    #[error(transparent)]
    Head(#[from] sb_repo::RepoError),
    /// The rebuild's step failed; it has shown why (a quiet exit).
    #[error("updating the tags cache failed")]
    Step(#[source] Exit),
}

/// Why a listing or a write failed, shown by the rebuild's step.
#[derive(Debug, thiserror::Error)]
enum RebuildError {
    /// `ansible-playbook --list-tags` printed no `TASK TAGS:` line.
    #[error(
        "error: 'TASK TAGS:' not found in the ansible-playbook output. Please make sure '{0}' is formatted correctly"
    )]
    NoTaskTags(String),
    /// The file couldn't be written.
    #[error("could not write the tags cache {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
}

/// The cache file's entries, every one kept as read, so an entry sb
/// doesn't own (an old appdata root's) is written back as it was. Not
/// `Clone`: two copies each recording a repository would each write the
/// whole file, the last dropping the other's entry.
#[derive(Debug, PartialEq)]
pub struct TagCache {
    path: PathBuf,
    entries: BTreeMap<String, Value>,
}

impl TagCache {
    /// The cache at the Saltbox `root`, read only: no child runs
    /// (completion's reader, within sb's startup budget). A missing file
    /// is empty; an unreadable or corrupt one is empty too, logged, and
    /// written over at the next rebuild (spec, difference 10). The read is
    /// cli-core's `read_small`: a regular file of at most 1 MiB in UTF-8,
    /// opened without blocking.
    #[must_use]
    pub fn load(root: &Path) -> Self {
        let path = root.join(FILE);
        let entries = match sb_cli_core::config::read_small(&path) {
            Ok(None) => BTreeMap::new(),
            Ok(Some(text)) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "tags cache is corrupt; treated as empty, rebuilt and written over"
                );
                BTreeMap::new()
            }),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "tags cache is unreadable; treated as empty, rebuilt and written over"
                );
                BTreeMap::new()
            }
        };
        Self { path, entries }
    }

    /// The file it reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `repo`'s entry, if there is one of sb-go's shape.
    #[must_use]
    pub fn entry(&self, repo: &Repo) -> Option<RepoTags> {
        let value = self.entries.get(&key(repo))?;
        serde_json::from_value(value.clone()).ok()
    }

    /// The strings in `repo`'s entry's `tags`, whatever its commit, any
    /// other value skipped (sb-go `cmd/tag_completion.go`,
    /// `cachedTagStrings`): completion's read, which offers the last tags
    /// written and runs no listing (completion spec, Tab completion).
    #[must_use]
    pub fn cached_tags(&self, repo: &Repo) -> Vec<String> {
        self.entries
            .get(&key(repo))
            .and_then(|entry| entry.get("tags"))
            .and_then(Value::as_array)
            .map_or_default(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
    }

    /// `repo`'s cached tags if they are valid for `head`: the entry exists,
    /// its tags aren't empty and its commit is `head` (spec, "Valid";
    /// invalidated by commit only, sb-go e8e7341).
    #[must_use]
    pub fn fresh(&self, repo: &Repo, head: &str) -> Option<Vec<String>> {
        self.entry(repo)
            .filter(|entry| entry.commit == head && !entry.tags.is_empty())
            .map(|entry| entry.tags)
    }

    /// `repo`'s tags: from the cache when valid for its checkout's HEAD,
    /// else rebuilt in a sub-step of `parent` labelled `Updating <Repo>
    /// tags cache` and written (spec, `tags(repo, step)`). `checkout` is
    /// sb-repo's handle on the same directory; `repo` is Saltbox or
    /// Sandbox (`saltbox_mod` is never cached: list it with
    /// [`Ansible::list_tags`]).
    ///
    /// # Errors
    ///
    /// [`CacheError::Head`] when the HEAD commit can't be read;
    /// [`CacheError::Step`] when the rebuild's step failed (it shows why).
    pub async fn tags(
        &mut self,
        ansible: &Ansible,
        repo: &Repo,
        checkout: &sb_repo::Repo,
        parent: &mut Step,
    ) -> Result<Listed, CacheError> {
        let head = match self.lookup(repo, checkout, &parent.token()).await? {
            Lookup::Fresh(tags) => return Ok(Listed { tags, cached: true }),
            Lookup::Stale(head) => head,
        };
        let tags = self.rebuild(ansible, repo, head, parent).await?;
        Ok(Listed {
            tags,
            cached: false,
        })
    }

    /// [`TagCache::tags`] for a command with no parent step (`install`,
    /// `list`: sb-go has none there): a rebuild is a top-level step of
    /// `runner`, which starts at the first one, so a valid cache prints
    /// nothing (sb-go's texts; spec, difference 9). The step is
    /// transient when `runner` is ([`LazyRunner::transient`]).
    ///
    /// # Errors
    ///
    /// As [`TagCache::tags`].
    pub async fn tags_on<F: FnOnce() -> Runner>(
        &mut self,
        ansible: &Ansible,
        repo: &Repo,
        checkout: &sb_repo::Repo,
        runner: &mut LazyRunner<F>,
    ) -> Result<Listed, CacheError> {
        let head = match self.lookup(repo, checkout, &runner.token()).await? {
            Lookup::Fresh(tags) => return Ok(Listed { tags, cached: true }),
            Lookup::Stale(head) => head,
        };
        let tags = self.rebuild_on(ansible, repo, head, runner).await?;
        Ok(Listed {
            tags,
            cached: false,
        })
    }

    /// [`TagCache::refresh`] for a command with no parent step (`sb list
    /// --refresh`; spec, decision 21): `repo`'s tags rebuilt and written
    /// whatever the cache holds, in a top-level step of `runner`, as
    /// [`TagCache::tags_on`]'s rebuild.
    ///
    /// # Errors
    ///
    /// As [`TagCache::tags`].
    pub async fn refresh_on<F: FnOnce() -> Runner>(
        &mut self,
        ansible: &Ansible,
        repo: &Repo,
        checkout: &sb_repo::Repo,
        runner: &mut LazyRunner<F>,
    ) -> Result<Vec<String>, CacheError> {
        same_repo(repo, checkout);
        let head = checkout.head(&runner.token()).await?;
        self.rebuild_on(ansible, repo, head, runner).await
    }

    /// The listing in a top-level step of `runner`, transient when its
    /// steps are.
    async fn rebuild_on<F: FnOnce() -> Runner>(
        &mut self,
        ansible: &Ansible,
        repo: &Repo,
        head: String,
        runner: &mut LazyRunner<F>,
    ) -> Result<Vec<String>, CacheError> {
        let label = label(repo.kind());
        let labels = runner.labels(label.clone(), label);
        runner
            .runner()
            .step(labels, async |step| {
                self.list_and_record(ansible, repo, head, &step.token())
                    .await
            })
            .await
            .map_err(CacheError::Step)
    }

    /// `repo`'s cached tags if valid for its checkout's HEAD, else that
    /// HEAD, for the rebuild.
    async fn lookup(
        &self,
        repo: &Repo,
        checkout: &sb_repo::Repo,
        token: &CancellationToken,
    ) -> Result<Lookup, CacheError> {
        same_repo(repo, checkout);
        let head = checkout.head(token).await?;
        Ok(match self.fresh(repo, &head) {
            Some(tags) => Lookup::Fresh(tags),
            None => Lookup::Stale(head),
        })
    }

    /// `repo`'s tags rebuilt and written, whatever the cache holds
    /// (`update`, `branch` and `branch-sandbox` after a pull; spec,
    /// `refresh(repo, step)`), in a sub-step of `parent`.
    ///
    /// # Errors
    ///
    /// As [`TagCache::tags`].
    pub async fn refresh(
        &mut self,
        ansible: &Ansible,
        repo: &Repo,
        checkout: &sb_repo::Repo,
        parent: &mut Step,
    ) -> Result<Vec<String>, CacheError> {
        same_repo(repo, checkout);
        let head = checkout.head(&parent.token()).await?;
        self.rebuild(ansible, repo, head, parent).await
    }

    /// The listing in its sub-step of `parent`.
    async fn rebuild(
        &mut self,
        ansible: &Ansible,
        repo: &Repo,
        head: String,
        parent: &mut Step,
    ) -> Result<Vec<String>, CacheError> {
        let label = label(repo.kind());
        parent
            .step(Labels::new(label.clone(), label), async |step| {
                self.list_and_record(ansible, repo, head, &step.token())
                    .await
            })
            .await
            .map_err(CacheError::Step)
    }

    /// A rebuild step's body: the listing, then the entry recorded and the
    /// file written when it changed.
    async fn list_and_record(
        &mut self,
        ansible: &Ansible,
        repo: &Repo,
        head: String,
        token: &CancellationToken,
    ) -> anyhow::Result<Vec<String>> {
        let tags = ansible.list_tags(repo, token).await?;
        let entry = RepoTags { commit: head, tags };
        self.record(repo, &entry)?;
        Ok(entry.tags)
    }

    /// Store `entry` for `repo` and write the file, unless the entry is
    /// already there as it is (no write: the file and its mtime are
    /// kept). `saltbox_mod` is never stored (spec, "File").
    fn record(&mut self, repo: &Repo, entry: &RepoTags) -> Result<(), RebuildError> {
        if repo.kind() == Kind::Mod {
            return Ok(());
        }
        let value = serde_json::to_value(entry).expect("strings serialize");
        if self.entries.get(&key(repo)) == Some(&value) {
            return Ok(());
        }
        self.entries.insert(key(repo), value);
        self.write()
    }

    /// The whole file, atomically: a temporary file in the same directory,
    /// mode 0644 set explicitly (completion reads it without sudo, under
    /// any umask), synced, then renamed over the old one
    /// (<https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist>).
    /// Why not tempfile's `Builder::permissions`: the umask narrows it
    /// (its docs, and crates/host's `apt` sources write). The directory
    /// isn't synced after the rename: after a power loss the old file or
    /// the new one is there, and either is a valid cache (a corrupt one
    /// is rebuilt).
    fn write(&self) -> Result<(), RebuildError> {
        let failed = |cause| RebuildError::Write {
            path: self.path.clone(),
            cause,
        };
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(dir).map_err(failed)?;
        let bytes = serde_json::to_vec(&self.entries).expect("JSON values serialize");
        file.write_all(&bytes).map_err(failed)?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .map_err(failed)?;
        file.as_file().sync_all().map_err(failed)?;
        file.persist(&self.path).map_err(|err| failed(err.error))?;
        Ok(())
    }
}

/// What the cache holds for a repository at its HEAD.
enum Lookup {
    /// Valid tags.
    Fresh(Vec<String>),
    /// Nothing valid: the HEAD commit a rebuild records.
    Stale(String),
}

/// A term-ui runner started by its first step, for a command whose
/// rebuilds have no parent step (`install`, `list`; the M32 follow-up,
/// settled in M33): sb-go prints nothing when the cache is valid, and a
/// started runner shows its title from its first step on (term-ui holds
/// it back only for transient steps). `start` is the command's own
/// [`Runner::start`] (its title, shutdown and run log), as sb-release's
/// `self_update` takes one. Why not a runner started up front: its title
/// would print on every run. The start is std's `LazyCell`
/// (<https://doc.rust-lang.org/std/cell/struct.LazyCell.html#method.force_mut>).
/// Dropping it ends the runner's drawing, if it started ([`Runner`]'s
/// `Drop` does what its `finish` does): before an attached run (exec
/// refuses one while a runner is live) or a command's output.
pub struct LazyRunner<F> {
    runner: LazyCell<Runner, F>,
    token: CancellationToken,
    transient: bool,
}

impl<F: FnOnce() -> Runner> LazyRunner<F> {
    /// Not started yet; `token` is the run's (the shutdown's), for what
    /// runs before the first step (the HEAD read).
    pub fn new(token: CancellationToken, start: F) -> Self {
        Self {
            runner: LazyCell::new(start),
            token,
            transient: false,
        }
    }

    /// Every step started on it is transient (term-ui's
    /// `Labels::transient`): [`TagCache::tags_on`]'s and
    /// [`TagCache::refresh_on`]'s rebuilds and `sb
    /// list -m`'s listing show while they run; when every step
    /// succeeded they are gone, title included, and when one failed the
    /// whole tree stays. `sb list`'s runner
    /// is, its output being the tags (spec, decision 18); `install`'s
    /// isn't, so its rebuild steps stay.
    #[must_use]
    pub fn transient(self) -> Self {
        Self {
            transient: true,
            ..self
        }
    }

    /// A step's labels, transient when this runner's are.
    pub(crate) fn labels(&self, running: impl Into<String>, done: impl Into<String>) -> Labels {
        let labels = Labels::new(running, done);
        if self.transient {
            labels.transient()
        } else {
            labels
        }
    }

    /// The run's token, for what runs before the first step (the HEAD
    /// read in [`TagCache::tags_on`]).
    #[must_use]
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Whether a step has started it.
    #[must_use]
    pub fn started(&self) -> bool {
        LazyCell::get(&self.runner).is_some()
    }

    /// The runner, started now if it wasn't.
    pub(crate) fn runner(&mut self) -> &mut Runner {
        LazyCell::force_mut(&mut self.runner)
    }
}

impl<F: FnOnce() -> Runner> fmt::Debug for LazyRunner<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyRunner")
            .field("started", &self.started())
            .finish_non_exhaustive()
    }
}

/// The step's label for `kind`'s rebuild, sb-go's in every command
/// (cmd/update.go:300, cmd/branch.go:95): running, done and failed alike,
/// as sb-go's `normalizeTaskSpec` has a task with only its running text.
#[must_use]
pub fn label(kind: Kind) -> String {
    let name = match kind {
        Kind::Saltbox => "Saltbox",
        Kind::Sandbox => "Sandbox",
        Kind::Mod => "Saltbox_mod",
    };
    format!("Updating {name} tags cache")
}

/// A caller's two handles name one directory and a cacheable repository:
/// a mismatch would store one repository's commit under the other's key;
/// `saltbox_mod` would be listed on every call and never stored (a bug in
/// the caller, M-PANIC-ON-BUG, checked in debug builds).
fn same_repo(repo: &Repo, checkout: &sb_repo::Repo) {
    debug_assert_eq!(checkout.path(), repo.dir(), "one repository's handles");
    debug_assert_ne!(repo.kind(), Kind::Mod, "saltbox_mod is never cached");
}

/// `repo`'s key: its absolute path, as sb-go's layout writes it.
fn key(repo: &Repo) -> String {
    repo.dir().to_string_lossy().into_owned()
}

/// The tags of `--list-tags` output (sb-go `PrepareAnsibleListTags`): the
/// first `TASK TAGS: [...]` after escape sequences are dropped (term-ui's
/// `DisplayText`; defensive: ansible-core 2.21.3 prints it plain even
/// under Saltbox's `force_color`, spec "Verification notes"), split on
/// `,` and each trimmed; `[]` is none.
fn parse_list_tags(stdout: &str, playbook: &str) -> Result<Vec<String>, RebuildError> {
    let plain = DisplayText::from_bytes(stdout.as_bytes(), false);
    let inner = TASK_TAGS
        .captures(plain.as_str())
        .and_then(|caps| caps.get(1))
        .ok_or_else(|| RebuildError::NoTaskTags(playbook.to_string()))?
        .as_str()
        .trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    Ok(inner.split(',').map(|tag| tag.trim().to_string()).collect())
}

impl Ansible {
    /// `ansible-playbook <playbook> --become --list-tags --skip-tags=always`
    /// in `repo`'s directory, captured (spec, "Rebuild"; one listing for
    /// every writer, decision 10), its tags.
    ///
    /// # Errors
    ///
    /// The child couldn't start, didn't exit 0 (exec's `Failed`, with
    /// stderr's last lines), or printed no `TASK TAGS:` line.
    pub async fn list_tags(
        &self,
        repo: &Repo,
        token: &CancellationToken,
    ) -> anyhow::Result<Vec<String>> {
        let captured = Cmd::new(self.program())
            .cwd(repo.dir())
            .arg(repo.kind().playbook())
            .arg("--become")
            .arg("--list-tags")
            .arg("--skip-tags=always")
            .capture(token)
            .await?
            .check()?;
        Ok(parse_list_tags(&captured.stdout, repo.kind().playbook())?)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use super::*;

    const LISTING: &str = include_str!("../tests/fixtures/list-tags.txt");
    const COLOUR: &str = include_str!("../tests/fixtures/list-tags-colour.txt");
    const REAL: &str = include_str!("../tests/fixtures/list-tags-real.txt");
    const SB_GO: &str = include_str!("../tests/fixtures/cache-sb-go.json");

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    /// A Saltbox root holding `text` as its cache file (none for `None`).
    fn root_with(text: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        if let Some(text) = text {
            std::fs::write(dir.path().join(FILE), text).expect("cache");
        }
        dir
    }

    /// The first `TASK TAGS:` line, in Ansible's order; with colour codes
    /// dropped first (defensive, see `parse_list_tags`); `[]` is none; no line is
    /// an error naming the playbook (sb-go's text).
    #[test]
    fn list_tags_output_parses_as_sb_go_parses_it() {
        assert_eq!(
            parse_list_tags(LISTING, "saltbox.yml").expect("tags"),
            strings(&[
                "always",
                "authelia",
                "docker",
                "plex",
                "sanity-check",
                "sonarr",
                "traefik"
            ])
        );
        assert_eq!(
            parse_list_tags(COLOUR, "sandbox.yml").expect("tags"),
            strings(&["always", "autobrr", "bazarr", "overseerr", "tautulli"])
        );
        assert_eq!(
            parse_list_tags("      TASK TAGS: []\n", "x.yml").expect("tags"),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_list_tags("TASK TAGS:[ a ,b,  c ]", "x.yml").expect("tags"),
            strings(&["a", "b", "c"])
        );
        let missing = parse_list_tags("playbook: saltbox.yml\n", "saltbox.yml").expect_err("none");
        assert_eq!(
            missing.to_string(),
            "error: 'TASK TAGS:' not found in the ansible-playbook output. Please make sure 'saltbox.yml' is formatted correctly"
        );
    }

    /// The listing of a real ansible-core (container.sh, M35; ansible
    /// spec, "Verification notes"): ansible-core 2.21.3's, plain, with
    /// its blank first line and the play line's tab.
    #[test]
    fn a_real_ansible_listing_parses() {
        assert_eq!(
            parse_list_tags(REAL, "saltbox.yml").expect("tags"),
            strings(&[
                "baseline",
                "check",
                "plex",
                "slow",
                "sonarr",
                "sonarr-config"
            ])
        );
    }

    /// sb-go's file loads unchanged: each repository's entry by its path.
    #[test]
    fn an_sb_go_cache_loads_unchanged() {
        let dir = root_with(Some(SB_GO));
        let cache = TagCache::load(dir.path());
        assert_eq!(cache.path(), dir.path().join(FILE));
        let saltbox = cache
            .entry(&Repo::saltbox(Path::new("/srv/git/saltbox")))
            .expect("Saltbox's entry");
        assert_eq!(saltbox.commit, "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432");
        assert_eq!(saltbox.tags.len(), 7);
        let sandbox = Repo::sandbox(Path::new("/opt"));
        assert_eq!(
            cache.fresh(&sandbox, "5d3c1f0e9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d"),
            Some(strings(&[
                "always",
                "autobrr",
                "bazarr",
                "overseerr",
                "tautulli"
            ]))
        );
    }

    /// Valid only for the same commit and with tags: another commit, an
    /// empty list, a missing entry or one of another shape is stale.
    #[test]
    fn an_entry_is_fresh_only_for_its_commit_with_tags() {
        let text = r#"{"/s":{"commit":"c1","tags":["a"]},"/e":{"commit":"c1","tags":[]},"/x":{"commit":"c1"}}"#;
        let dir = root_with(Some(text));
        let cache = TagCache::load(dir.path());
        let repo = |path: &str| Repo::saltbox(Path::new(path));
        assert_eq!(cache.fresh(&repo("/s"), "c1"), Some(strings(&["a"])));
        assert_eq!(cache.fresh(&repo("/s"), "c2"), None);
        assert_eq!(cache.fresh(&repo("/e"), "c1"), None);
        assert_eq!(cache.fresh(&repo("/x"), "c1"), None);
        assert_eq!(cache.entry(&repo("/x")), None);
        assert_eq!(cache.fresh(&repo("/none"), "c1"), None);
    }

    /// A missing, corrupt or unreadable file is empty (and the last two are
    /// written over at the next rebuild).
    #[test]
    fn a_missing_or_corrupt_file_is_empty() {
        let repo = Repo::saltbox(Path::new("/srv/git/saltbox"));
        for text in [None, Some("{not json"), Some("[1, 2]"), Some("\u{0}")] {
            let dir = root_with(text);
            assert_eq!(TagCache::load(dir.path()).entry(&repo), None, "{text:?}");
        }
        let dir = root_with(None);
        std::fs::create_dir(dir.path().join(FILE)).expect("a directory in its place");
        assert_eq!(TagCache::load(dir.path()).entry(&repo), None);
        let dir = root_with(Some("{broken"));
        let mut cache = TagCache::load(dir.path());
        let entry = RepoTags {
            commit: "c".into(),
            tags: strings(&["a"]),
        };
        cache.record(&repo, &entry).expect("written over");
        assert_eq!(TagCache::load(dir.path()).entry(&repo), Some(entry));
    }

    /// A write keeps every other entry as it was read (an old appdata
    /// root's, with a field sb doesn't know), in sb-go's shape: one JSON
    /// object keyed by path, each `{"commit", "tags"}`.
    #[test]
    fn a_write_keeps_the_other_entries_in_sb_gos_shape() {
        let dir = root_with(Some(SB_GO));
        let mut cache = TagCache::load(dir.path());
        let saltbox = Repo::saltbox(Path::new("/srv/git/saltbox"));
        cache
            .record(
                &saltbox,
                &RepoTags {
                    commit: "new".into(),
                    tags: strings(&["plex"]),
                },
            )
            .expect("write");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(FILE)).expect("read"))
                .expect("JSON");
        let before: serde_json::Value = serde_json::from_str(SB_GO).expect("JSON");
        assert_eq!(written["/opt/sandbox"], before["/opt/sandbox"]);
        assert_eq!(
            written["/old/appdata/sandbox"],
            before["/old/appdata/sandbox"]
        );
        assert_eq!(
            written["/srv/git/saltbox"],
            serde_json::json!({"commit": "new", "tags": ["plex"]})
        );
        assert_eq!(written.as_object().map(serde_json::Map::len), Some(3));
    }

    /// An entry already there as it is isn't written: the file and its
    /// mtime are kept. `saltbox_mod` is never stored.
    #[test]
    fn nothing_is_written_when_nothing_changed() {
        let dir = root_with(Some(SB_GO));
        let path = dir.path().join(FILE);
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(old))
            .expect("an old mtime");
        let mut cache = TagCache::load(dir.path());
        let saltbox = Repo::saltbox(Path::new("/srv/git/saltbox"));
        let same = cache.entry(&saltbox).expect("entry");
        cache.record(&saltbox, &same).expect("no write");
        cache
            .record(
                &Repo::saltbox_mod(Path::new("/opt")),
                &RepoTags {
                    commit: "m".into(),
                    tags: strings(&["x"]),
                },
            )
            .expect("not stored");
        let meta = std::fs::metadata(&path).expect("meta");
        assert_eq!(meta.modified().expect("mtime"), old);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), SB_GO);
        assert_eq!(cache.entry(&Repo::saltbox_mod(Path::new("/opt"))), None);
    }

    /// The test's child: a write under the umask its parent set.
    const UMASK_CHILD: &str = "SB_ANSIBLE_TEST_UMASK_CHILD";

    /// The file is 0644 after a write under a `0077` umask, the old file
    /// replaced whole. The umask is set in a child (this test, run again
    /// by itself under `sh -c 'umask 077; exec ...'`): a umask is the
    /// process's, and the other tests' threads share it.
    #[test]
    fn the_file_is_0644_under_any_umask() {
        if let Some(root) = std::env::var_os(UMASK_CHILD) {
            let root = PathBuf::from(root);
            let mut cache = TagCache::load(&root);
            cache
                .record(
                    &Repo::saltbox(Path::new("/srv/git/saltbox")),
                    &RepoTags {
                        commit: "c".into(),
                        tags: strings(&["a"]),
                    },
                )
                .expect("write");
            return;
        }
        let dir = root_with(Some(&"x".repeat(100_000)));
        std::fs::set_permissions(
            dir.path().join(FILE),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod");
        let status = std::process::Command::new("sh")
            .args(["-c", r#"umask 077; exec "$0" "$@""#])
            .arg(std::env::current_exe().expect("this test"))
            .args([
                "--exact",
                "cache::tests::the_file_is_0644_under_any_umask",
                "--quiet",
            ])
            .env(UMASK_CHILD, dir.path())
            .stdout(std::process::Stdio::null())
            .status()
            .expect("the child");
        assert!(status.success(), "{status:?}");
        let meta = std::fs::metadata(dir.path().join(FILE)).expect("meta");
        assert_eq!(meta.mode() & 0o777, 0o644);
        let text = std::fs::read_to_string(dir.path().join(FILE)).expect("read");
        assert_eq!(text, r#"{"/srv/git/saltbox":{"commit":"c","tags":["a"]}}"#);
        // Only the file: no temporary file left beside it.
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 1);
    }

    /// A write that can't happen names the file.
    #[test]
    fn a_failed_write_names_the_file() {
        let dir = root_with(None);
        let gone = dir.path().join("no-such-root");
        let mut cache = TagCache::load(&gone);
        let err = cache
            .record(
                &Repo::saltbox(Path::new("/srv/git/saltbox")),
                &RepoTags {
                    commit: "c".into(),
                    tags: strings(&["a"]),
                },
            )
            .expect_err("no directory");
        assert_eq!(
            err.to_string(),
            format!(
                "could not write the tags cache {}",
                gone.join(FILE).display()
            )
        );
    }

    std::thread_local! {
        /// This thread's captured log text while [`capture`] runs, else
        /// `None`.
        static CAPTURE: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
    }

    /// The global subscriber's writer: appends to the calling thread's
    /// [`CAPTURE`] buffer when one is on, and drops the text otherwise.
    /// The fmt layer writes on the thread that emits the event.
    struct ThreadCapture;

    impl std::io::Write for ThreadCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            CAPTURE.with_borrow_mut(|buffer| {
                if let Some(buffer) = buffer {
                    buffer.extend_from_slice(bytes);
                }
            });
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs `body` with this thread's log records captured, and returns
    /// its result with their text (sb-host's `capture`, M10's fix).
    ///
    /// One global subscriber for the whole test binary, installed once
    /// (<https://docs.rs/tracing/0.1.44/tracing/subscriber/fn.set_global_default.html>).
    /// A scoped one (`with_default`) is not enough: callsite interest is
    /// process-wide, and while the scoped one is the only dispatcher, a
    /// callsite first hit on a thread without a subscriber (another test
    /// loading a corrupt file) is cached as "never" for as long as it
    /// lives (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>;
    /// tracing-core 0.1.36's `Rebuilder::JustOne` in src/callsite.rs), so
    /// a record went missing under load. With a global dispatcher always
    /// present, no callsite is cached as "never". No other test in this
    /// binary installs a subscriber.
    fn capture<T>(body: impl FnOnce() -> T) -> (T, String) {
        static GLOBAL: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(|| ThreadCapture)
                .with_ansi(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("the only global subscriber");
        });
        CAPTURE.set(Some(Vec::new()));
        let result = body();
        let bytes = CAPTURE.take().unwrap_or_default();
        (result, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A corrupt or unreadable file is logged as such (the spec's notice:
    /// treated as empty, rebuilt and written over), naming the file;
    /// captured through [`capture`].
    #[test]
    fn a_corrupt_or_unreadable_file_is_logged() {
        let logged = |dir: &Path| capture(|| TagCache::load(dir)).1;
        let corrupt = root_with(Some("{not json"));
        let text = logged(corrupt.path());
        assert!(
            text.contains("tags cache is corrupt; treated as empty, rebuilt and written over"),
            "{text}"
        );
        assert!(
            text.contains(&corrupt.path().join(FILE).display().to_string()),
            "{text}"
        );
        let unreadable = root_with(None);
        std::fs::create_dir(unreadable.path().join(FILE)).expect("a directory in its place");
        let text = logged(unreadable.path());
        assert!(text.contains("tags cache is unreadable"), "{text}");
        assert_eq!(logged(root_with(None).path()), "");
    }

    /// Completion's read (sb-go `cmd/tag_completion.go`, `cachedTagStrings`):
    /// the strings in an entry's `tags`, whatever its commit, others
    /// skipped; nothing for a missing entry, or one whose `tags` isn't a
    /// list.
    #[test]
    fn cached_tags_are_an_entrys_strings_whatever_its_commit() {
        let dir = root_with(Some(
            r#"{"/srv/git/saltbox": {"tags": ["plex", 42, "sonarr", null]},
                "/opt/sandbox": {"commit": "abc", "tags": "plex"},
                "/opt/saltbox_mod": {"commit": "abc", "tags": ["custom"]}}"#,
        ));
        let cache = TagCache::load(dir.path());
        assert_eq!(
            cache.cached_tags(&Repo::saltbox(Path::new("/srv/git/saltbox"))),
            strings(&["plex", "sonarr"])
        );
        assert_eq!(
            cache.cached_tags(&Repo::sandbox(Path::new("/opt"))),
            Vec::<String>::new()
        );
        assert_eq!(
            cache.cached_tags(&Repo::saltbox_mod(Path::new("/opt"))),
            strings(&["custom"])
        );
        assert_eq!(
            cache.cached_tags(&Repo::sandbox(Path::new("/x"))),
            Vec::<String>::new()
        );
    }

    #[test]
    fn each_repo_has_sb_gos_label() {
        assert_eq!(label(Kind::Saltbox), "Updating Saltbox tags cache");
        assert_eq!(label(Kind::Sandbox), "Updating Sandbox tags cache");
        assert_eq!(label(Kind::Mod), "Updating Saltbox_mod tags cache");
    }
}
