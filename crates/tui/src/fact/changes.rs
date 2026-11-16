//! The staged changes (tui spec, editors, `sb fact`, Changes): what the
//! changes panel lists, keyed by names so they outlive a reload of the
//! catalog after a save, and their form as sb-config's `Change`s, which
//! the session checks before one is staged and saves.

use std::collections::BTreeMap;

use sb_config::facts::apply::Change;
use sb_config::facts::document::Edit;

/// What a staged change is on: an app, one of its copies, or a setting.
/// Its order is the page's: by app, then the app before its copies, a
/// copy before its settings, and by name within each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    App(String),
    Copy(String, String),
    Setting(String, String, String),
}

impl Ord for Target {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order().cmp(&other.order())
    }
}

impl PartialOrd for Target {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Target {
    /// The app, then the copy (none for the app itself), then the key
    /// (none for a copy).
    fn order(&self) -> (&str, Option<&str>, Option<&str>) {
        match self {
            Self::App(role) => (role, None, None),
            Self::Copy(role, copy) => (role, Some(copy), None),
            Self::Setting(role, copy, key) => (role, Some(copy), Some(key)),
        }
    }

    /// The role (app) it is in.
    pub(super) fn role(&self) -> &str {
        match self {
            Self::App(role) | Self::Copy(role, _) | Self::Setting(role, ..) => role,
        }
    }

    /// Whether `self` is strictly inside `outer`: a copy or setting of a
    /// deleted app, a setting of a deleted copy.
    pub(super) fn inside(&self, outer: &Self) -> bool {
        match (self, outer) {
            (Self::Copy(role, _) | Self::Setting(role, ..), Self::App(app)) => role == app,
            (Self::Setting(role, copy, _), Self::Copy(app, app_copy)) => {
                role == app && copy == app_copy
            }
            _ => false,
        }
    }

    /// The app and copy above `self` that a deletion would cover.
    pub(super) fn outer(&self) -> Vec<Self> {
        match self {
            Self::App(_) => Vec::new(),
            Self::Copy(role, _) => vec![Self::App(role.clone())],
            Self::Setting(role, copy, _) => vec![
                Self::App(role.clone()),
                Self::Copy(role.clone(), copy.clone()),
            ],
        }
    }
}

/// A staged change. Its `Debug` leaves the value out (a fact can be a
/// secret).
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Pending {
    /// A setting's new value (or a new setting's).
    Set(String),
    /// Delete the app, the copy or the setting.
    Delete,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Set(_) => f.write_str("Set(..)"),
            Self::Delete => f.write_str("Delete"),
        }
    }
}

/// The staged changes, as sb-config's `Change`s: an app's deletion
/// unlinks its file, a copy's removes its section, a setting's its line;
/// a value is set, or added to its copy.
pub(super) fn to_changes(pending: &BTreeMap<Target, Pending>) -> Vec<Change> {
    pending
        .iter()
        .map(|(target, pending)| match (target, pending) {
            (Target::App(role), _) => Change::DeleteRole { role: role.clone() },
            (Target::Copy(role, copy), _) => Change::Edit {
                role: role.clone(),
                edit: Edit::DeleteInstance {
                    instance: copy.clone(),
                },
            },
            (Target::Setting(role, copy, key), Pending::Delete) => Change::Edit {
                role: role.clone(),
                edit: Edit::DeleteFact {
                    instance: copy.clone(),
                    key: key.clone(),
                },
            },
            (Target::Setting(role, copy, key), Pending::Set(value)) => Change::Edit {
                role: role.clone(),
                edit: Edit::Set {
                    instance: copy.clone(),
                    key: key.clone(),
                    value: value.clone(),
                },
            },
        })
        .collect()
}

/// A change's kind for the run log: never its value.
pub(super) fn kind(target: &Target, pending: &Pending, added: bool) -> &'static str {
    match (target, pending) {
        (Target::App(_), _) => "delete app",
        (Target::Copy(..), _) => "delete copy",
        (Target::Setting(..), Pending::Delete) => "delete setting",
        (Target::Setting(..), Pending::Set(_)) if added => "add setting",
        (Target::Setting(..), Pending::Set(_)) => "set setting",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(role: &str, copy: &str, key: &str) -> Target {
        Target::Setting(role.into(), copy.into(), key.into())
    }

    /// The panel lists changes in the page's order.
    #[test]
    fn targets_sort_as_the_page_does() {
        let mut targets = vec![
            setting("b", "b", "k"),
            Target::App("c".into()),
            setting("a", "a2", "k"),
            Target::Copy("a".into(), "a2".into()),
            Target::App("a".into()),
            setting("a", "a", "z"),
        ];
        targets.sort();
        assert_eq!(
            targets,
            [
                Target::App("a".into()),
                setting("a", "a", "z"),
                Target::Copy("a".into(), "a2".into()),
                setting("a", "a2", "k"),
                setting("b", "b", "k"),
                Target::App("c".into()),
            ]
        );
    }

    #[test]
    fn inside_is_strict_and_by_name() {
        let app = Target::App("plex".into());
        let copy = Target::Copy("plex".into(), "plex2".into());
        assert!(copy.inside(&app));
        assert!(setting("plex", "plex2", "k").inside(&app));
        assert!(setting("plex", "plex2", "k").inside(&copy));
        assert!(!setting("plex", "plex", "k").inside(&copy));
        assert!(!setting("plexx", "plex2", "k").inside(&app));
        assert!(!app.inside(&app));
        assert!(!copy.inside(&copy));
        assert!(!app.inside(&copy));
        assert!(!Target::Copy("sonarr".into(), "plex2".into()).inside(&app));
    }

    #[test]
    fn outer_names_the_app_and_copy_above() {
        assert_eq!(Target::App("a".into()).outer(), []);
        assert_eq!(
            Target::Copy("a".into(), "b".into()).outer(),
            [Target::App("a".into())]
        );
        assert_eq!(
            setting("a", "b", "k").outer(),
            [
                Target::App("a".into()),
                Target::Copy("a".into(), "b".into())
            ]
        );
        assert_eq!(setting("a", "b", "k").role(), "a");
    }

    #[test]
    fn each_change_is_sb_config_s() {
        let pending = BTreeMap::from([
            (Target::App("a".into()), Pending::Delete),
            (Target::Copy("b".into(), "b2".into()), Pending::Delete),
            (setting("c", "c", "k"), Pending::Delete),
            (setting("d", "d", "k"), Pending::Set("v".into())),
        ]);
        assert_eq!(
            to_changes(&pending),
            [
                Change::DeleteRole { role: "a".into() },
                Change::Edit {
                    role: "b".into(),
                    edit: Edit::DeleteInstance {
                        instance: "b2".into()
                    }
                },
                Change::Edit {
                    role: "c".into(),
                    edit: Edit::DeleteFact {
                        instance: "c".into(),
                        key: "k".into()
                    }
                },
                Change::Edit {
                    role: "d".into(),
                    edit: Edit::Set {
                        instance: "d".into(),
                        key: "k".into(),
                        value: "v".into()
                    }
                },
            ]
        );
    }

    #[test]
    fn kinds_and_debug_never_show_a_value() {
        let set = Pending::Set("hunter2".into());
        assert_eq!(format!("{set:?}"), "Set(..)");
        assert_eq!(format!("{:?}", Pending::Delete), "Delete");
        assert_eq!(
            kind(&Target::App("a".into()), &Pending::Delete, false),
            "delete app"
        );
        assert_eq!(
            kind(
                &Target::Copy("a".into(), "b".into()),
                &Pending::Delete,
                false
            ),
            "delete copy"
        );
        assert_eq!(
            kind(&setting("a", "a", "k"), &Pending::Delete, false),
            "delete setting"
        );
        assert_eq!(kind(&setting("a", "a", "k"), &set, true), "add setting");
        assert_eq!(kind(&setting("a", "a", "k"), &set, false), "set setting");
    }
}
