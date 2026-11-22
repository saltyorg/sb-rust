//! The facts store read without any lock (tui spec, "`sb fact get` and
//! `sb fact list`"): one value, or the names, for `sb fact get`, `sb fact
//! list` and their Tab completion.
//!
//! [`Store`] keeps the session's rules for the directory and each file
//! (the directory walked from `/` without a symlink and pinned as an fd;
//! each `<app>.ini` opened relative to it with `O_NOFOLLOW|O_NONBLOCK`, a
//! regular file that keeps its identity, at most 1 MiB; `DEFAULT`
//! hidden) and takes neither the editor's `.fact-editor.lock` nor
//! Saltbox's `<role>.ini.lock`: Saltbox replaces a file with
//! `os.replace`, so one read is consistent, and a read must work while
//! `sb fact` is open or Saltbox runs (tui decision 46).
//!
//! No value leaves through an error or a `Debug`: [`LookupError`] carries
//! names only, and [`Found`]'s `Debug` leaves its value out, as
//! [`super::Fact`]'s does.

use std::os::fd::OwnedFd;
use std::path::Path;

use sb_cli_core::exit::escaped;
use serde::Serialize;

use super::Role;
use super::session::{FileError, SessionError, open_directory, read_role, role_names};

/// The facts directory opened for reading: pinned, no lock taken.
#[derive(Debug)]
pub struct Store {
    dir: OwnedFd,
}

impl Store {
    /// Opens the facts directory `path` (absolute), every component
    /// without a symlink. Nothing is read or locked yet.
    ///
    /// # Errors
    ///
    /// [`SessionError::Directory`] for a missing, relative or symlinked
    /// path.
    pub fn open(path: &Path) -> Result<Self, SessionError> {
        let dir = open_directory(path).map_err(SessionError::Directory)?;
        Ok(Self { dir })
    }

    /// The apps: each `<app>.ini` name without its suffix, sorted. One
    /// `readdir`; no file is opened.
    ///
    /// # Errors
    ///
    /// [`SessionError::List`] when the directory can't be read, and
    /// [`SessionError::Role`] for a name that isn't UTF-8 or is `.ini`
    /// alone.
    pub fn apps(&self) -> Result<Vec<String>, SessionError> {
        role_names(&self.dir)
    }

    /// `<app>.ini` read and parsed, as the editor lists it. `None` when no
    /// such file is there; an app name that is empty or holds `/` or NUL
    /// names no file, so it can't reach outside the directory.
    ///
    /// # Errors
    ///
    /// [`FileError`]: a symlink, a FIFO or anything but a regular file, a
    /// file that changed while it was read, one over 1 MiB, one that can't
    /// be opened or doesn't parse.
    pub fn role(&self, app: &str) -> Result<Option<Role>, FileError> {
        if app.is_empty() || app.contains(['/', '\0']) {
            return Ok(None);
        }
        // Saltbox replaces a file with `os.replace`; one that changed while
        // it was read (its identity checked before, at and after the read)
        // is read again once, so a read during an install gets the new
        // version (tui decision 46).
        let mut read = read_role(&self.dir, app);
        if matches!(read, Err(FileError::Changed(_))) {
            read = read_role(&self.dir, app);
        }
        match read {
            Ok(state) => Ok(Some(state.role)),
            Err(err) if err.is_not_found() => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// One fact found by [`get`]: its names and its value, continuation lines
/// joined with `\n`. Its `Debug` leaves the value out.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Found {
    pub app: String,
    pub instance: String,
    pub key: String,
    pub value: String,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Found")
            .field("app", &self.app)
            .field("instance", &self.instance)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Found {
    /// `{"app","instance","key","value"}` on one line, then `\n`
    /// (`sb fact get --json`), every control character escaped (cli-core's
    /// `json::line`, sb's `--json` output).
    #[must_use]
    pub fn json(&self) -> String {
        sb_cli_core::json::line(self)
    }
}

/// One fact's names, as [`list`] gives them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Name {
    pub app: String,
    pub instance: String,
    pub key: String,
}

/// `names` as a JSON array of `{"app","instance","key"}` on one line,
/// then `\n` (`sb fact list --json`).
#[must_use]
pub fn names_json(names: &[Name]) -> String {
    sb_cli_core::json::line(names)
}

/// Why a lookup found nothing. Names only, escaped as cli-core's error
/// block escapes them (escaping twice changes nothing); never a value.
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("no facts for app '{}'; run sb fact list to see them", escaped(.app))]
    UnknownApp { app: String },
    #[error("app '{}' has no instances", escaped(.app))]
    NoInstances { app: String },
    #[error(
        "app '{}' has no instance '{}'; instances: {}",
        escaped(.app),
        escaped(.instance),
        joined(.instances)
    )]
    UnknownInstance {
        app: String,
        instance: String,
        instances: Vec<String>,
    },
    #[error(
        "app '{}' has {} instances; name one: {}",
        escaped(.app),
        .instances.len(),
        joined(.instances)
    )]
    Ambiguous { app: String, instances: Vec<String> },
    #[error(
        "{}/{} has no fact '{}'; facts: {}",
        escaped(.app),
        escaped(.instance),
        escaped(.key),
        joined(.keys)
    )]
    UnknownKey {
        app: String,
        instance: String,
        key: String,
        keys: Vec<String>,
    },
    #[error("{}/{} {} is empty", escaped(.app), escaped(.instance), escaped(.key))]
    Empty {
        app: String,
        instance: String,
        key: String,
    },
    /// The store itself: the facts layer's own text.
    #[error(transparent)]
    Store(#[from] SessionError),
}

/// `names` escaped and joined by `, `.
fn joined(names: &[String]) -> String {
    names
        .iter()
        .map(|name| escaped(name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `app`'s role, or the miss: [`LookupError::UnknownApp`] when there is
/// no such file, and the file's error, naming it, as the editor's open
/// names it (`catalog <app>: <cause>`).
fn role_or_miss(store: &Store, app: &str) -> Result<Role, LookupError> {
    match store.role(app) {
        Ok(Some(role)) => Ok(role),
        Ok(None) => Err(LookupError::UnknownApp {
            app: app.to_owned(),
        }),
        Err(cause) => Err(SessionError::Role {
            role: app.to_owned(),
            cause,
        }
        .into()),
    }
}

/// The instance of `role` that `instance` names, or with `None` its only
/// one (the two-word form).
fn instance<'a>(
    role: &'a Role,
    instance: Option<&str>,
) -> Result<&'a super::Instance, LookupError> {
    let names = || role.instances.iter().map(|i| i.name.clone()).collect();
    if role.instances.is_empty() {
        return Err(LookupError::NoInstances {
            app: role.name.clone(),
        });
    }
    match instance {
        Some(name) => role
            .instances
            .iter()
            .find(|i| i.name == name)
            .ok_or_else(|| LookupError::UnknownInstance {
                app: role.name.clone(),
                instance: name.to_owned(),
                instances: names(),
            }),
        None => match role.instances.as_slice() {
            [only] => Ok(only),
            _ => Err(LookupError::Ambiguous {
                app: role.name.clone(),
                instances: names(),
            }),
        },
    }
}

/// `sb fact get`: the value of `app`'s `key` in `instance`, or with `None`
/// in the app's only instance.
///
/// # Errors
///
/// [`LookupError`]: an unknown app, an app with no instance, an unknown
/// instance, `None` for an app with several, an unknown key, an empty
/// value, or the file's own error.
pub fn get(
    store: &Store,
    app: &str,
    instance_name: Option<&str>,
    key: &str,
) -> Result<Found, LookupError> {
    let role = role_or_miss(store, app)?;
    let instance = instance(&role, instance_name)?;
    let Some(fact) = instance.facts.iter().find(|fact| fact.key == key) else {
        return Err(LookupError::UnknownKey {
            app: role.name.clone(),
            instance: instance.name.clone(),
            key: key.to_owned(),
            keys: instance.facts.iter().map(|f| f.key.clone()).collect(),
        });
    };
    if fact.value.is_empty() {
        return Err(LookupError::Empty {
            app: role.name.clone(),
            instance: instance.name.clone(),
            key: key.to_owned(),
        });
    }
    Ok(Found {
        app: role.name.clone(),
        instance: instance.name.clone(),
        key: fact.key.clone(),
        value: fact.value.clone(),
    })
}

/// `sb fact list`: every fact's names, sorted by app, instance and key;
/// narrowed to `app`, and within it to `instance`.
///
/// # Errors
///
/// [`LookupError`]: an unknown app or instance, and the store's or a
/// file's own error (any listed file that can't be read fails the list,
/// naming it, as the editor's open does).
pub fn list(
    store: &Store,
    app: Option<&str>,
    instance_name: Option<&str>,
) -> Result<Vec<Name>, LookupError> {
    let roles = if let Some(app) = app {
        vec![role_or_miss(store, app)?]
    } else {
        let mut roles = Vec::new();
        for app in store.apps()? {
            match role_or_miss(store, &app) {
                Ok(role) => roles.push(role),
                // Deleted since the directory was read.
                Err(LookupError::UnknownApp { .. }) => {}
                Err(err) => return Err(err),
            }
        }
        roles
    };
    let mut names = Vec::new();
    for role in &roles {
        let instances = match instance_name {
            Some(name) => std::slice::from_ref(instance(role, Some(name))?),
            None => role.instances.as_slice(),
        };
        for instance in instances {
            names.extend(instance.facts.iter().map(|fact| Name {
                app: role.name.clone(),
                instance: instance.name.clone(),
                key: fact.key.clone(),
            }));
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests;
