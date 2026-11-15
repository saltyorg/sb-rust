//! Saving the staged changes (tui spec, "Editors", Changes and Save;
//! sb-go facts/apply.go, with decision 38's lock timing): per role in
//! sorted order, take Saltbox's role lock, check for drift, prepare a
//! temp file on the pinned directory fd, commit it over the role's file,
//! then release the lock. There is no rollback: a failure stops the save
//! with the earlier roles applied.
//!
//! Why not tempfile's `Builder::tempfile_in(path)`: it resolves the path
//! again, which loses the pinned, symlink-free directory fd. The temp file
//! is created with `openat(O_CREAT|O_EXCL|O_NOFOLLOW)` on that fd
//! (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.openat.html>), owner then
//! mode copied with `fchown` and `fchmod`, written and `fsync`ed, then
//! `renameat` over the file and the directory `fsync`ed
//! (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.renameat.html>,
//! <https://docs.rs/rustix/1.1.6/rustix/fs/fn.fsync.html>), as Saltbox's
//! own `atomic_write` does
//! (<https://github.com/saltyorg/Saltbox/blob/master/library/saltbox_facts.py>).
//! tempfile's `make_in` has the same path-only API
//! (<https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.tempfile_in>).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::fd::OwnedFd;

use rustix::fs::{AtFlags, Gid, Mode, OFlags, Uid};
use sb_cli_core::config::escape_controls;

use super::document::{Document, DocumentError, Edit};
use super::session::{
    Drift, DriftChoice, FileError, RoleLock, RoleState, SaveHooks, Session, read_role, same_file,
};

/// One staged change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A change inside `<role>.ini`.
    Edit { role: String, edit: Edit },
    /// Delete the app: unlink `<role>.ini`.
    DeleteRole { role: String },
}

impl Change {
    /// The role the change is in.
    #[must_use]
    pub fn role(&self) -> &str {
        match self {
            Self::Edit { role, .. } | Self::DeleteRole { role } => role,
        }
    }
}

/// How a role's save ended.
#[derive(Debug)]
pub enum RoleOutcome {
    /// Its changes are on disk (or changed nothing).
    Applied,
    /// Its changes are on disk, but syncing the directory failed: they are
    /// applied (never to be saved again) and may not survive a crash. The
    /// save stopped there (sb-go's "both Applied and Failed").
    Unsynced(SaveError),
    /// It changed on disk since the session read it; nothing was written.
    /// With `reloaded`, the session took the disk version and the role's
    /// staged changes are to be dropped; without, they stay staged.
    Drifted { reloaded: bool },
    /// It failed with this cause; the save stopped there.
    Failed(SaveError),
    /// It wasn't tried: an earlier role failed, the wait was cancelled or
    /// the save was stopped.
    Unattempted,
}

/// Why a role's save failed. No message shows a value.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("unknown app {}", escape_controls(.0))]
    UnknownRole(String),
    #[error("deleting an app conflicts with its other changes")]
    DeleteConflict,
    #[error("timed out waiting for Saltbox's lock")]
    LockTimeout,
    #[error("the file changed on disk during the save; reload required")]
    Changed,
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Document(#[from] DocumentError),
    #[error("{what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: io::Error,
    },
}

impl SaveError {
    fn io(what: &'static str, source: impl Into<io::Error>) -> Self {
        Self::Io {
            what,
            source: source.into(),
        }
    }
}

/// The temp file: created, never there before (a planted symlink
/// included), written only.
const TEMP_FLAGS: OFlags = OFlags::CREATE
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::WRONLY)
    .union(OFlags::CLOEXEC);

/// A role's changes made on its baseline, before any file is touched.
enum Plan {
    /// The bytes stay as they are.
    Unchanged,
    /// Unlink the file.
    Delete,
    /// The new bytes and the role they read as.
    Write(Vec<u8>, super::Role),
}

/// Why [`Session::check`] refused a set of changes: the first role that
/// can't be saved.
#[derive(Debug, thiserror::Error)]
#[error("{}: {cause}", escape_controls(.role))]
pub struct CheckError {
    pub role: String,
    #[source]
    pub cause: SaveError,
}

/// A role's prepared save.
enum Prepared {
    /// The changes leave the bytes as they are.
    Unchanged,
    /// Unlink the file.
    Delete,
    /// Rename the temp file `temp`, holding `state`'s bytes, over the file.
    Replace { temp: String, state: RoleState },
}

impl Session {
    /// Checks `changes` against the roles as the session read them, with
    /// no lock and no file touched: every role is known, a deleted app has
    /// no other change, and every edit can be written (the dialect's
    /// rules, [`Document::apply`]). The screen calls it before staging a
    /// change; [`Session::save`] calls it before locking anything.
    ///
    /// # Errors
    ///
    /// [`CheckError`]: the first role, in sorted order, that can't be
    /// saved, and why.
    pub fn check(&self, changes: &[Change]) -> Result<(), CheckError> {
        for (role, changes) in grouped(changes) {
            self.plan(role, &changes).map_err(|cause| CheckError {
                role: role.to_owned(),
                cause,
            })?;
        }
        Ok(())
    }

    /// Saves `changes`, role by role in sorted order, and returns each
    /// role's outcome. Nothing is locked or written unless
    /// [`Session::check`] passes; a role it refuses is
    /// [`RoleOutcome::Failed`] and the others unattempted. A role is
    /// locked only while it is saved; after a failure, an unsynced role, a
    /// cancelled wait or a stop, the remaining roles are
    /// [`RoleOutcome::Unattempted`]. A drift is reported through `hooks`
    /// with the lock released, and the save goes on with the next role.
    #[must_use]
    pub fn save(
        &mut self,
        changes: &[Change],
        hooks: &mut dyn SaveHooks,
    ) -> BTreeMap<String, RoleOutcome> {
        let grouped = grouped(changes);
        let mut outcomes = BTreeMap::new();
        if let Err(refused) = self.check(changes) {
            tracing::warn!(role = refused.role, cause = %refused.cause, "changes refused before the save");
            for role in grouped.keys() {
                outcomes.insert((*role).to_owned(), RoleOutcome::Unattempted);
            }
            outcomes.insert(refused.role, RoleOutcome::Failed(refused.cause));
            return outcomes;
        }
        let mut stopped = false;
        for (role, changes) in grouped {
            stopped = stopped || hooks.stopped();
            let outcome = if stopped {
                RoleOutcome::Unattempted
            } else {
                self.save_role(role, &changes, hooks)
            };
            stopped = stopped
                || matches!(
                    outcome,
                    RoleOutcome::Failed(_) | RoleOutcome::Unsynced(_) | RoleOutcome::Unattempted
                );
            match &outcome {
                RoleOutcome::Failed(cause) | RoleOutcome::Unsynced(cause) => {
                    tracing::warn!(role, outcome = outcome.name(), %cause, "role save outcome");
                }
                outcome => tracing::info!(role, outcome = outcome.name(), "role save outcome"),
            }
            outcomes.insert(role.to_owned(), outcome);
        }
        outcomes
    }

    fn save_role(
        &mut self,
        role: &str,
        changes: &[&Change],
        hooks: &mut dyn SaveHooks,
    ) -> RoleOutcome {
        let Some(before) = self.roles.get(role) else {
            return RoleOutcome::Failed(SaveError::UnknownRole(role.to_owned()));
        };
        let before_role = before.role.clone();
        let lock = match self.lock_role(role, hooks) {
            Ok(Some(lock)) => lock,
            Ok(None) => return RoleOutcome::Unattempted,
            Err(err) => return RoleOutcome::Failed(err),
        };
        let current = match read_role(&self.dir, role) {
            Ok(current) if current.data == self.roles[role].data => current,
            Ok(current) => return self.drifted(role, lock, before_role, Some(current), hooks),
            Err(err) if err.is_not_found() => {
                return self.drifted(role, lock, before_role, None, hooks);
            }
            Err(err) => return RoleOutcome::Failed(err.into()),
        };
        // The same bytes, perhaps in a new file (an identical atomic
        // replacement): its owner, mode and identity are the baseline now.
        self.roles.insert(role.to_owned(), current);
        let result = self
            .prepare(role, changes)
            .and_then(|prepared| self.commit(role, &lock, prepared));
        drop(lock);
        note(&mut self.seam, "released", role);
        match result {
            Ok(None) => RoleOutcome::Applied,
            Ok(Some(err)) => RoleOutcome::Unsynced(err),
            Err(err) => RoleOutcome::Failed(err),
        }
    }

    /// The drift dialog's answer, asked with the lock released.
    fn drifted(
        &mut self,
        role: &str,
        lock: RoleLock,
        before: super::Role,
        current: Option<RoleState>,
        hooks: &mut dyn SaveHooks,
    ) -> RoleOutcome {
        drop(lock);
        tracing::info!(role, deleted = current.is_none(), "role changed on disk");
        let drift = Drift {
            before,
            after: current.as_ref().map(|state| state.role.clone()),
        };
        let reloaded = hooks.drift(role, &drift) == DriftChoice::Reload;
        if reloaded {
            match current {
                Some(state) => self.roles.insert(role.to_owned(), state),
                None => self.roles.remove(role),
            };
        }
        RoleOutcome::Drifted { reloaded }
    }

    /// `changes` made on `role`'s baseline, with every refusal
    /// [`Session::check`] names.
    fn plan(&self, role: &str, changes: &[&Change]) -> Result<Plan, SaveError> {
        let state = self
            .roles
            .get(role)
            .ok_or_else(|| SaveError::UnknownRole(role.to_owned()))?;
        let mut edits = Vec::new();
        for change in changes {
            match change {
                Change::DeleteRole { .. } if changes.len() == 1 => return Ok(Plan::Delete),
                Change::DeleteRole { .. } => return Err(SaveError::DeleteConflict),
                Change::Edit { edit, .. } => edits.push(edit.clone()),
            }
        }
        let data = Document::parse(&state.data)?.apply(&edits)?;
        if data == state.data {
            return Ok(Plan::Unchanged);
        }
        let new_role = Document::parse(&data)?.role(role);
        Ok(Plan::Write(data, new_role))
    }

    /// The new bytes in a temp file on the pinned directory, with the
    /// file's owner and mode, written and synced.
    fn prepare(&mut self, role: &str, changes: &[&Change]) -> Result<Prepared, SaveError> {
        for change in changes {
            if let Change::Edit { edit, .. } = change {
                tracing::info!(role, change = ?EditKind(edit), "staged change");
            }
        }
        let (data, new_role) = match self.plan(role, changes)? {
            Plan::Unchanged => return Ok(Prepared::Unchanged),
            Plan::Delete => return Ok(Prepared::Delete),
            Plan::Write(data, new_role) => (data, new_role),
        };
        let state = &self.roles[role];
        // O_EXCL refuses any existing entry, a planted symlink included, so
        // a predictable name (fastrand isn't a CSPRNG) can't redirect the
        // write; a clash fails the save instead.
        // fastrand's documented recipe for a random string
        // (<https://docs.rs/fastrand/2.5.0/fastrand/#examples>).
        let random: String = std::iter::repeat_with(fastrand::alphanumeric)
            .take(16)
            .collect();
        let temp = format!(".{role}-{random}.tmp");
        let fd = rustix::fs::openat(
            &self.dir,
            temp.as_str(),
            TEMP_FLAGS,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|err| SaveError::io("create temp file", err))?;
        let written = if fails(&self.seam, "write", role) {
            Err(SaveError::io("write temp file", rustix::io::Errno::IO))
        } else {
            write_temp(fd, state, &data)
        };
        note(&mut self.seam, "prepared", role);
        match written {
            Ok(()) => Ok(Prepared::Replace {
                temp,
                state: RoleState {
                    data,
                    role: new_role,
                    ..state.clone()
                },
            }),
            Err(err) => {
                self.unlink_temp(&temp);
                Err(err)
            }
        }
    }

    /// Checks the baseline again, then puts the prepared file in place and
    /// syncs the directory. `Ok(Some(..))`: in place, but the sync failed.
    fn commit(
        &mut self,
        role: &str,
        lock: &RoleLock,
        prepared: Prepared,
    ) -> Result<Option<SaveError>, SaveError> {
        note(&mut self.seam, "commit", role);
        if let Err(err) = self.check_baseline(role, lock) {
            if let Prepared::Replace { temp, .. } = &prepared {
                self.unlink_temp(temp);
            }
            return Err(err);
        }
        let name = format!("{role}.ini");
        match prepared {
            Prepared::Unchanged => return Ok(None),
            Prepared::Delete => {
                rustix::fs::unlinkat(&self.dir, name.as_str(), AtFlags::empty())
                    .map_err(|err| SaveError::io("delete facts file", err))?;
                self.roles.remove(role);
            }
            Prepared::Replace { temp, state } => {
                let renamed = if fails(&self.seam, "rename", role) {
                    Err(rustix::io::Errno::IO)
                } else {
                    rustix::fs::renameat(&self.dir, temp.as_str(), &self.dir, name.as_str())
                };
                if let Err(err) = renamed {
                    self.unlink_temp(&temp);
                    return Err(SaveError::io("replace facts file", err));
                }
                self.roles.insert(role.to_owned(), state);
            }
        }
        note(&mut self.seam, "renamed", role);
        let synced = if fails(&self.seam, "sync", role) {
            Err(rustix::io::Errno::IO)
        } else {
            rustix::fs::fsync(&self.dir)
        };
        if let Err(err) = synced {
            return Ok(Some(SaveError::io("sync facts directory", err)));
        }
        note(&mut self.seam, "synced", role);
        Ok(None)
    }

    /// Whether the role's file and lock are as the save found them: the
    /// same file with the same bytes, and the lock's name still the locked
    /// file.
    fn check_baseline(&self, role: &str, lock: &RoleLock) -> Result<(), SaveError> {
        let state = &self.roles[role];
        let changed = |err: FileError| {
            if matches!(err, FileError::Changed(_)) || err.is_not_found() {
                SaveError::Changed
            } else {
                err.into()
            }
        };
        same_file(&self.dir, &format!("{role}.ini"), state.id).map_err(changed)?;
        let current = read_role(&self.dir, role).map_err(changed)?;
        if current.data != state.data {
            return Err(SaveError::Changed);
        }
        same_file(&self.dir, &format!("{role}.ini.lock"), lock.id).map_err(changed)?;
        Ok(())
    }

    fn unlink_temp(&self, temp: &str) {
        if let Err(err) = rustix::fs::unlinkat(&self.dir, temp, AtFlags::empty()) {
            tracing::warn!(error = %err, "temp file not removed");
        }
    }
}

/// `changes` by role, in sorted order.
fn grouped(changes: &[Change]) -> BTreeMap<&str, Vec<&Change>> {
    let mut grouped: BTreeMap<&str, Vec<&Change>> = BTreeMap::new();
    for change in changes {
        grouped.entry(change.role()).or_default().push(change);
    }
    grouped
}

/// Owner first, then mode (chown can clear the setuid and setgid bits),
/// then the bytes, synced.
fn write_temp(fd: OwnedFd, state: &RoleState, data: &[u8]) -> Result<(), SaveError> {
    rustix::fs::fchown(
        &fd,
        Some(Uid::from_raw(state.uid)),
        Some(Gid::from_raw(state.gid)),
    )
    .map_err(|err| SaveError::io("preserve facts ownership", err))?;
    rustix::fs::fchmod(&fd, Mode::from_raw_mode(state.mode))
        .map_err(|err| SaveError::io("preserve facts mode", err))?;
    let mut file = std::fs::File::from(fd);
    file.write_all(data)
        .map_err(|err| SaveError::io("write temp file", err))?;
    file.sync_all()
        .map_err(|err| SaveError::io("sync temp file", err))?;
    Ok(())
}

/// An edit for the log: its kind, instance and key, never its value.
struct EditKind<'a>(&'a Edit);

impl std::fmt::Debug for EditKind<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Edit::Set { instance, key, .. } => write!(
                f,
                "set {}/{}",
                escape_controls(instance),
                escape_controls(key)
            ),
            Edit::DeleteFact { instance, key } => {
                write!(
                    f,
                    "delete {}/{}",
                    escape_controls(instance),
                    escape_controls(key)
                )
            }
            Edit::DeleteInstance { instance } => write!(f, "delete {}", escape_controls(instance)),
        }
    }
}

impl RoleOutcome {
    /// The outcome's name for the log.
    fn name(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Unsynced(_) => "applied, not synced",
            Self::Drifted { reloaded: true } => "drifted, reloaded",
            Self::Drifted { reloaded: false } => "drifted, kept",
            Self::Failed(_) => "failed",
            Self::Unattempted => "unattempted",
        }
    }
}

/// The test seam (tui spec, Testing Strategy: the directory fsync is
/// checked by the call order, not by crashing a disk): each step's name,
/// a callback before the commit's checks, and one step (`write`, `rename`
/// or `sync`) of one role made to fail. Empty outside tests.
#[cfg(test)]
type BeforeCommit = Box<dyn FnMut(&str)>;

#[cfg(test)]
#[derive(Default)]
pub(super) struct Seam {
    pub(super) events: Vec<String>,
    pub(super) before_commit: Option<BeforeCommit>,
    pub(super) fail: Option<(&'static str, String)>,
}

#[cfg(not(test))]
#[derive(Debug, Default)]
pub(super) struct Seam {}

#[cfg(test)]
impl std::fmt::Debug for Seam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seam")
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

/// Records the step `event` of `role`'s save; before the commit, runs the
/// test's callback.
#[cfg(test)]
fn note(seam: &mut Seam, event: &str, role: &str) {
    seam.events.push(format!("{event} {role}"));
    if event == "commit"
        && let Some(callback) = seam.before_commit.as_mut()
    {
        callback(role);
    }
}

#[cfg(not(test))]
fn note(_seam: &mut Seam, _event: &str, _role: &str) {}

/// Whether the test made the step `step` of `role`'s save fail.
#[cfg(test)]
fn fails(seam: &Seam, step: &str, role: &str) -> bool {
    seam.fail
        .as_ref()
        .is_some_and(|(failing, name)| *failing == step && name == role)
}

#[cfg(not(test))]
fn fails(_seam: &Seam, _step: &str, _role: &str) -> bool {
    false
}

#[cfg(test)]
mod tests;
