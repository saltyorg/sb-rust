//! What a query matches: the names it is built from, its groups, and the
//! supported releases with what their journalctl does that the reader has
//! to match: the `-u` rule (docs/platform-notes/systemd-journal.md,
//! "Matching") and, for the tools, the `-o json` shape.

use crate::Source;
use std::fmt;

/// One term of a group: the entry carries any of these `FIELD=value`
/// payloads (sd-journal ORs matches on the same field).
pub(crate) type Term = Vec<Vec<u8>>;
/// A conjunction of terms; a query is a disjunction of groups.
pub(crate) type Group = Vec<Term>;

/// The most groups `Query::and` builds: a cross product grows fast, and
/// each group is one walk per file.
pub const QUERY_GROUPS_MAX: usize = 64;

/// A user id. A crate type, so no dependency's type appears in the API;
/// sb-host fills it from its own lookup of the systemd-coredump user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uid(u32);

impl Uid {
    /// The user id `uid`.
    #[must_use]
    pub fn new(uid: u32) -> Self {
        Self(uid)
    }

    /// The number.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A service's unit name, mangled as journalctl mangles Saltbox's names
/// (Difference 7): a name without a dot gets `.service`, and a `.service`
/// name passes unchanged. Every other form is refused, not guessed:
/// other unit types and suffixes, a dot without one, globs, and anything
/// systemd's `unit_name_is_valid` refuses
/// (<https://github.com/systemd/systemd/blob/v255/src/basic/unit-name.c>:
/// the characters of `VALID_CHARS_WITH_AT` before the suffix, no leading
/// `@`, shorter than `UNIT_NAME_MAX`, 256), which journalctl would escape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnitName(String);

/// systemd's `UNIT_NAME_MAX` (src/basic/unit-name.h), the terminating NUL
/// included.
const UNIT_NAME_MAX: usize = 256;

impl UnitName {
    /// The unit `name` names, mangled as above.
    ///
    /// # Errors
    ///
    /// `QueryError::UnitName` for a name that isn't a service's, or that
    /// journalctl would mangle in another way.
    pub fn new(name: &str) -> Result<Self, QueryError> {
        let refused = || QueryError::UnitName(name.to_owned());
        let full = if name.contains('.') {
            name.to_owned()
        } else {
            format!("{name}.service")
        };
        let stem = full.strip_suffix(".service").ok_or_else(refused)?;
        let valid = |c: char| c.is_ascii_alphanumeric() || ":-_.\\@".contains(c);
        if stem.is_empty()
            || stem.starts_with('@')
            || !stem.chars().all(valid)
            || full.len() >= UNIT_NAME_MAX
        {
            return Err(refused());
        }
        Ok(Self(full))
    }

    /// The name, `.service` included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UnitName {
    /// The name as `as_str` gives it: valid characters only, so nothing to
    /// escape.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A field name a query or `Record::field` can name: journald's rule for
/// the names it stores (`journal_field_valid` with protected names allowed,
/// src/libsystemd/sd-journal/journal-file.c v255: 1 to 64 of `A-Z`, `0-9`
/// and `_`, not starting with a digit), and not starting with `__`, which
/// sd-journal refuses as a match (`match_is_valid`, sd-journal.c v249): those
/// are the address fields, never stored. A record's own field names aren't
/// checked against it (`Record::fields`, Difference 14).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldName(Vec<u8>);

impl FieldName {
    /// The field name `name`, checked against the rule above.
    ///
    /// # Errors
    ///
    /// `QueryError::FieldName` for a name outside the rule.
    pub fn new(name: &[u8]) -> Result<Self, QueryError> {
        let valid = (1..=64).contains(&name.len())
            && !name[0].is_ascii_digit()
            && !name.starts_with(b"__")
            && name
                .iter()
                .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if valid {
            Ok(Self(name.to_vec()))
        } else {
            Err(QueryError::FieldName(name.to_vec()))
        }
    }

    /// The name's bytes (ASCII).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for FieldName {
    /// The name: ASCII by construction, so nothing to escape.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0))
    }
}

/// A query that can't be built, or a cursor that can't be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum QueryError {
    /// Not a name `UnitName` takes (shown quoted and escaped).
    #[error("{0:?} isn't a service's unit name (only NAME or NAME.service)")]
    UnitName(String),
    /// Not a name `FieldName` takes (shown quoted and escaped, security
    /// audit L5).
    #[error("{:?} isn't a journal field name", String::from_utf8_lossy(.0))]
    FieldName(Vec<u8>),
    /// `Query::and` would build more than `QUERY_GROUPS_MAX` groups.
    #[error("the query would have {groups} groups, over the limit of {QUERY_GROUPS_MAX}")]
    TooManyGroups {
        /// The groups the cross product would have.
        groups: usize,
    },
    /// Not sd-journal's cursor text (`Cursor`'s `FromStr`).
    #[error("not a journal cursor")]
    BadCursor,
}

/// What a page matches, in journalctl's model: an OR of groups, each an
/// AND of terms, each an OR of `FIELD=value` payloads.
#[derive(Clone, PartialEq, Eq)]
pub struct Query {
    groups: Vec<Group>,
}

impl fmt::Debug for Query {
    /// The payloads as lossy text: they are field names and values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text: Vec<Vec<Vec<String>>> = self
            .groups
            .iter()
            .map(|g| {
                g.iter()
                    .map(|t| {
                        t.iter()
                            .map(|v| String::from_utf8_lossy(v).into_owned())
                            .collect()
                    })
                    .collect()
            })
            .collect();
        f.debug_struct("Query").field("groups", &text).finish()
    }
}

impl Query {
    /// journalctl's `-u` for `release`. `coredump_uid` is the
    /// systemd-coredump user's uid if it resolves (sb-host's lookup); the
    /// coredump term follows the journal spec's table (decisions 1 and 4):
    /// on 22.04 and 24.04 always `_UID=0`; on 26.04 that uid or 0 for
    /// `Source::Local` when the uid isn't 0, and `_UID=0` otherwise.
    #[must_use]
    pub fn unit(
        release: &Release,
        unit: &UnitName,
        coredump_uid: Option<Uid>,
        source: &Source,
    ) -> Self {
        Self {
            groups: unit_groups(*release, unit, coredump_uid, source),
        }
    }

    /// `FIELD=value`. Repeating the same field with `or` matches either
    /// value, as sd-journal ORs repeated matches on one field.
    #[must_use]
    pub fn field(name: &FieldName, value: &[u8]) -> Self {
        let mut payload = name.0.clone();
        payload.push(b'=');
        payload.extend_from_slice(value);
        Self {
            groups: vec![vec![vec![payload]]],
        }
    }

    /// Every group of `self` combined by AND with every group of `other`:
    /// a cross product.
    ///
    /// # Errors
    ///
    /// `QueryError::TooManyGroups` past `QUERY_GROUPS_MAX` groups.
    pub fn and(self, other: Query) -> Result<Self, QueryError> {
        let Query { groups: theirs } = other;
        let groups = self.groups.len().saturating_mul(theirs.len());
        if groups > QUERY_GROUPS_MAX {
            return Err(QueryError::TooManyGroups { groups });
        }
        let groups = self
            .groups
            .iter()
            .flat_map(|a| theirs.iter().map(move |b| [&a[..], &b[..]].concat()))
            .collect();
        Ok(Self { groups })
    }

    /// The groups of both. Unlike `and` it can't fail: its groups add up,
    /// one walk each per file, and grow only as the caller builds them.
    #[must_use]
    pub fn or(mut self, other: Query) -> Self {
        self.groups.extend(other.groups);
        self
    }

    /// Every entry: no group, as journalctl with no match (the tools'
    /// whole-journal pages, `testing::tools::every_entry`).
    #[cfg(feature = "testing")]
    pub(crate) fn every() -> Self {
        Self { groups: Vec::new() }
    }

    pub(crate) fn groups(&self) -> &[Group] {
        &self.groups
    }
}

/// One supported Ubuntu release. sb refuses any other release (sb's
/// startup check), so [`RELEASES`] lists them all; a value of this type is
/// always one of its rows, got through [`Release::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    version_id: &'static str,
    systemd: u32,
    unit_rule: UnitRule,
    boot_rule: BootRule,
    /// `-o json` prints `__SEQNUM` and `__SEQNUM_ID` (v255 and later): for
    /// the tools only (journal spec, API).
    #[cfg(feature = "testing")]
    prints_seqnum: bool,
}

/// The supported releases. A new LTS is one row here, checked by the
/// matrix (compare.sh and compat.sh run every row); it needs code only if
/// its systemd brings a new `-u` rule, `-o json` shape or incompatible file
/// flag (file.rs `INCOMPAT_KNOWN`), or a new order between boots.
pub const RELEASES: &[Release] = &[
    Release {
        version_id: "22.04",
        systemd: 249,
        unit_rule: UnitRule::Pid1,
        boot_rule: BootRule::Realtime,
        #[cfg(feature = "testing")]
        prints_seqnum: false,
    },
    Release {
        version_id: "24.04",
        systemd: 255,
        unit_rule: UnitRule::Pid1,
        boot_rule: BootRule::NewestTail,
        #[cfg(feature = "testing")]
        prints_seqnum: true,
    },
    Release {
        version_id: "26.04",
        systemd: 259,
        unit_rule: UnitRule::InitScope,
        boot_rule: BootRule::NewestTail,
        #[cfg(feature = "testing")]
        prints_seqnum: true,
    },
];

/// journalctl's `-u <unit>` rule, by the systemd versions that have it
/// (src/shared/logs-show.c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnitRule {
    /// 249 to 259.0, `add_matches_for_unit`: messages from `_PID=1` about
    /// the unit; coredumps only from root.
    Pid1,
    /// 259.1 to main (2026-11), `add_matches_for_unit_full`: messages from
    /// `_SYSTEMD_CGROUP=/init.scope` about the unit, to include PID 1's
    /// helpers; coredumps from root or the `systemd-coredump` user
    /// (src/journal/journalctl-filter.c), with sb's stricter rule for the
    /// uid term (journal spec, decisions 1 and 4).
    InitScope,
}

/// How journalctl orders entries of two different boots that no sequence
/// number orders (src/libsystemd/sd-journal/sd-journal.c), by the systemd
/// versions that have it (journal spec, decision 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BootRule {
    /// 249, `journal_file_compare_locations` (journal-file.c): by
    /// realtime, then the XOR hash.
    Realtime,
    /// 255 to 259, `compare_boot_ids` in `compare_locations` and
    /// `compare_with_location`: first by each boot's newest file's tail
    /// realtime, for boots of one machine that are in that table; then as
    /// `Realtime`.
    NewestTail,
}

/// A release that isn't one of [`RELEASES`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReleaseError {
    /// The `VERSION_ID` names no supported release. The value is shown
    /// quoted and escaped (`Debug`), as it comes from a host file.
    #[error("Ubuntu release {0:?} isn't supported")]
    Unsupported(String),
}

impl Release {
    /// The row of `RELEASES` whose `VERSION_ID` (/etc/os-release) is
    /// `version_id`, exactly.
    ///
    /// # Errors
    ///
    /// `ReleaseError::Unsupported` for any other value.
    pub fn parse(version_id: &str) -> Result<Self, ReleaseError> {
        RELEASES
            .iter()
            .find(|r| r.version_id == version_id)
            .copied()
            .ok_or_else(|| ReleaseError::Unsupported(version_id.to_owned()))
    }

    /// `VERSION_ID` in /etc/os-release, and the `ubuntu:` image tag the
    /// matrix runs.
    #[must_use]
    pub fn version_id(&self) -> &'static str {
        self.version_id
    }

    /// The systemd major version the release ships: documentation, and the
    /// matrix's check that the image still ships it. A rule can change
    /// within a major (259.1); the matrix's fixtures catch that, not this.
    #[must_use]
    pub fn systemd(&self) -> u32 {
        self.systemd
    }

    /// journalctl's `-u` rule on this release.
    #[must_use]
    pub fn unit_rule(&self) -> UnitRule {
        self.unit_rule
    }

    /// journalctl's order between boots on this release.
    #[must_use]
    pub fn boot_rule(&self) -> BootRule {
        self.boot_rule
    }

    /// Whether `-o json` prints `__SEQNUM` and `__SEQNUM_ID` (the tools'
    /// JSON form only).
    #[cfg(feature = "testing")]
    #[must_use]
    pub fn prints_seqnum(&self) -> bool {
        self.prints_seqnum
    }
}

/// systemd-coredump's message (catalog `SD_MESSAGE_COREDUMP`).
const COREDUMP_MESSAGE_ID: &[u8] = b"MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1";

fn pair(field: &str, value: &str) -> Vec<u8> {
    format!("{field}={value}").into_bytes()
}

/// journalctl's `-u <unit>` match set on `release`, as groups (an OR of
/// ANDs). The coredump group's uid term follows the journal spec's table
/// (decisions 1 and 4): on 249 and 255 it is `_UID=0`, as journalctl's;
/// on 259 it is the systemd-coredump user's uid or 0 for the local
/// journal when that uid resolves to a uid other than 0, and `_UID=0`
/// otherwise, where journalctl drops the term (Difference 1).
pub(crate) fn unit_groups(
    release: Release,
    unit: &UnitName,
    coredump_uid: Option<Uid>,
    source: &Source,
) -> Vec<Group> {
    let unit = unit.as_str();
    let root = || b"_UID=0".to_vec();
    let own = vec![vec![pair("_SYSTEMD_UNIT", unit)]];
    let daemons = vec![vec![root()], vec![pair("OBJECT_SYSTEMD_UNIT", unit)]];
    match release.unit_rule {
        UnitRule::Pid1 => vec![
            own,
            vec![
                vec![COREDUMP_MESSAGE_ID.to_vec()],
                vec![root()],
                vec![pair("COREDUMP_UNIT", unit)],
            ],
            vec![vec![b"_PID=1".to_vec()], vec![pair("UNIT", unit)]],
            daemons,
        ],
        UnitRule::InitScope => {
            // A local uid may not apply to foreign files (Directory, Files).
            let coredump_user = match (source, coredump_uid) {
                (Source::Local, Some(uid)) if uid.get() != 0 => {
                    vec![format!("_UID={}", uid.get()).into_bytes(), root()]
                }
                _ => vec![root()],
            };
            vec![
                own,
                // The v259 tag itself still has _PID=1 (platform note).
                vec![
                    vec![b"_SYSTEMD_CGROUP=/init.scope".to_vec()],
                    vec![pair("UNIT", unit)],
                ],
                daemons,
                // In journalctl's order of adding (v259 logs-show.c,
                // `add_matches_for_unit_full`): the forward page's seek
                // walks terms in that order, reversed (forward.rs).
                vec![
                    vec![COREDUMP_MESSAGE_ID.to_vec()],
                    coredump_user,
                    vec![pair("COREDUMP_UNIT", unit)],
                ],
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn release(version_id: &str) -> Release {
        Release::parse(version_id).expect("a supported release")
    }

    fn unit() -> UnitName {
        UnitName::new("a").expect("a unit name")
    }

    /// The coredump group: the one carrying the coredump `MESSAGE_ID`.
    fn coredump(groups: &[Group]) -> &Group {
        groups
            .iter()
            .find(|g| g.iter().any(|t| t == &[COREDUMP_MESSAGE_ID.to_vec()]))
            .expect("a coredump group")
    }

    /// The coredump group's uid term.
    fn uid_term(groups: &[Group]) -> Vec<String> {
        let group = coredump(groups);
        let terms: Vec<&Vec<Vec<u8>>> = group
            .iter()
            .filter(|t| t.iter().all(|v| v.starts_with(b"_UID=")))
            .collect();
        assert_eq!(terms.len(), 1, "{group:?}");
        terms[0]
            .iter()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect()
    }

    /// Ported from the prototype's
    /// `init_scope_rule_on_external_files_drops_the_coredump_uid`, re-targeted
    /// to the spec's decision 4: where journalctl drops the uid term for
    /// `--file`/`-D`, sb requires `_UID=0` (Difference 1). The group has
    /// three terms, not two.
    #[test]
    fn init_scope_rule_on_external_files_requires_uid_0() {
        for source in [
            Source::Directory(PathBuf::from("/d")),
            Source::Files(vec![PathBuf::from("/f.journal")]),
        ] {
            let g = unit_groups(release("26.04"), &unit(), Some(Uid::new(998)), &source);
            assert_eq!(g[3].len(), 3, "{source:?}");
            assert_eq!(uid_term(&g), ["_UID=0"], "{source:?}");
        }
    }

    /// Each row is found by its own `VERSION_ID`, once; anything else is
    /// refused.
    #[test]
    fn releases_parse_by_version_id() {
        for r in RELEASES {
            assert_eq!(Release::parse(r.version_id()), Ok(*r));
            assert_eq!(
                RELEASES
                    .iter()
                    .filter(|o| o.version_id() == r.version_id())
                    .count(),
                1
            );
        }
        for other in ["20.04", "26.10", "", "24.04.1"] {
            assert_eq!(
                Release::parse(other),
                Err(ReleaseError::Unsupported(other.to_owned())),
                "{other}"
            );
        }
    }

    /// The rows keep the rules the matrix checked: `_PID=1` before 259.1,
    /// init.scope after.
    #[test]
    fn unit_rule_per_release() {
        let has = |v: &str, term: &[u8]| {
            unit_groups(release(v), &unit(), None, &Source::Files(Vec::new()))
                .iter()
                .flatten()
                .flatten()
                .any(|p| p.as_slice() == term)
        };
        for v in ["22.04", "24.04"] {
            assert!(has(v, b"_PID=1") && !has(v, b"_SYSTEMD_CGROUP=/init.scope"));
            assert_eq!(release(v).unit_rule(), UnitRule::Pid1);
        }
        assert!(has("26.04", b"_SYSTEMD_CGROUP=/init.scope") && !has("26.04", b"_PID=1"));
        assert_eq!(release("26.04").unit_rule(), UnitRule::InitScope);
    }

    /// The boot step per row: systemd 249 has none; 255 and 259 order
    /// boots by their newest file's tail (decision 11).
    #[test]
    fn boot_rule_per_release() {
        assert_eq!(release("22.04").boot_rule(), BootRule::Realtime);
        for v in ["24.04", "26.04"] {
            assert_eq!(release(v).boot_rule(), BootRule::NewestTail);
        }
    }

    /// The rows' other columns, as the matrix and the tools read them.
    #[test]
    fn rows_name_their_systemd_and_json_shape() {
        let rows: Vec<(&str, u32, bool)> = RELEASES
            .iter()
            .map(|r| (r.version_id(), r.systemd(), r.prints_seqnum()))
            .collect();
        assert_eq!(
            rows,
            [
                ("22.04", 249, false),
                ("24.04", 255, true),
                ("26.04", 259, true)
            ]
        );
    }

    /// The journal spec's coredump table (decisions 1 and 4), for every
    /// release, source and uid, `Some(0)` included: on 249 and 255 the term
    /// is always `_UID=0`; on 259 only the local journal with a uid other
    /// than 0 accepts that uid too.
    #[test]
    fn the_coredump_term_follows_the_table() {
        let sources = [
            Source::Local,
            Source::Directory(PathBuf::from("/d")),
            Source::Files(vec![PathBuf::from("/f.journal")]),
        ];
        let uids = [None, Some(Uid::new(0)), Some(Uid::new(998))];
        for r in RELEASES {
            for source in &sources {
                for uid in uids {
                    let groups = unit_groups(*r, &unit(), uid, source);
                    let local_user = r.unit_rule() == UnitRule::InitScope
                        && matches!(source, Source::Local)
                        && uid == Some(Uid::new(998));
                    let want: &[&str] = if local_user {
                        &["_UID=998", "_UID=0"]
                    } else {
                        &["_UID=0"]
                    };
                    assert_eq!(
                        uid_term(&groups),
                        want,
                        "{} {source:?} {uid:?}",
                        r.version_id()
                    );
                    let unit_term = vec![b"COREDUMP_UNIT=a.service".to_vec()];
                    assert!(coredump(&groups).contains(&unit_term));
                    assert_eq!(coredump(&groups).len(), 3);
                }
            }
        }
    }

    /// The whole match set of each rule, term by term, in the order
    /// journalctl adds them (v249 and v259 logs-show.c; platform note,
    /// "Matching"), which the forward page's seek depends on.
    #[test]
    fn the_unit_groups_are_journalctls() {
        let text = |groups: Vec<Group>| -> Vec<Vec<Vec<String>>> {
            groups
                .into_iter()
                .map(|g| {
                    g.into_iter()
                        .map(|t| {
                            t.into_iter()
                                .map(|v| String::from_utf8(v).expect("utf-8"))
                                .collect()
                        })
                        .collect()
                })
                .collect()
        };
        let id = "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1";
        let pid1 = text(unit_groups(release("22.04"), &unit(), None, &Source::Local));
        assert_eq!(
            pid1,
            [
                vec![vec!["_SYSTEMD_UNIT=a.service"]],
                vec![vec![id], vec!["_UID=0"], vec!["COREDUMP_UNIT=a.service"]],
                vec![vec!["_PID=1"], vec!["UNIT=a.service"]],
                vec![vec!["_UID=0"], vec!["OBJECT_SYSTEMD_UNIT=a.service"]],
            ]
        );
        let init = text(unit_groups(
            release("26.04"),
            &unit(),
            Some(Uid::new(998)),
            &Source::Local,
        ));
        assert_eq!(
            init,
            [
                vec![vec!["_SYSTEMD_UNIT=a.service"]],
                vec![vec!["_SYSTEMD_CGROUP=/init.scope"], vec!["UNIT=a.service"]],
                vec![vec!["_UID=0"], vec!["OBJECT_SYSTEMD_UNIT=a.service"]],
                vec![
                    vec![id],
                    vec!["_UID=998", "_UID=0"],
                    vec!["COREDUMP_UNIT=a.service"]
                ],
            ]
        );
    }

    /// The release error quotes and escapes what it was given: it comes
    /// from a host file.
    #[test]
    fn an_unsupported_release_is_shown_escaped() {
        let e = Release::parse("2\u{1b}[31m").expect_err("unsupported");
        assert_eq!(
            e.to_string(),
            "Ubuntu release \"2\\u{1b}[31m\" isn't supported"
        );
    }

    /// Difference 7: a name without a dot gets `.service`; a `.service`
    /// name passes unchanged; the names systemd allows in a service's name
    /// pass, templates and instances included.
    #[test]
    fn unit_names_are_mangled_only_with_service() {
        for (given, want) in [
            ("plex", "plex.service"),
            ("plex.service", "plex.service"),
            ("a.b.service", "a.b.service"),
            // Split so the instance name doesn't read as an address.
            ("getty@tty1", concat!("getty@tty1", ".service")),
            ("getty@.service", "getty@.service"),
            ("x\\x2dy:z_w-1", "x\\x2dy:z_w-1.service"),
        ] {
            assert_eq!(
                UnitName::new(given).map(|u| u.as_str().to_owned()),
                Ok(want.to_owned()),
                "{given}"
            );
        }
    }

    /// Difference 7: every other form journalctl would mangle (globs,
    /// other suffixes, a dot without `.service`, characters it escapes) is
    /// refused, not guessed.
    #[test]
    fn other_unit_names_are_refused() {
        let long = "a".repeat(UNIT_NAME_MAX - ".service".len());
        for name in [
            "",
            ".service",
            "plex*",
            "pl?x.service",
            "[ab].service",
            "a.slice",
            "a.socket",
            "a.b",
            "a b",
            "a/b.service",
            "@x.service",
            "caf\u{e9}",
            long.as_str(),
        ] {
            assert_eq!(
                UnitName::new(name),
                Err(QueryError::UnitName(name.to_owned())),
                "{name:?}"
            );
        }
        let longest = "a".repeat(UNIT_NAME_MAX - 1 - ".service".len());
        assert_eq!(
            UnitName::new(&longest).expect("fits").as_str().len(),
            UNIT_NAME_MAX - 1
        );
    }

    /// journald's field-name rule, without the address fields.
    #[test]
    fn field_names_follow_journald() {
        for good in [&b"MESSAGE"[..], b"_SYSTEMD_UNIT", b"A1_", b"_", &[b'A'; 64]] {
            assert_eq!(
                FieldName::new(good).map(|f| f.as_bytes().to_vec()),
                Ok(good.to_vec())
            );
        }
        for bad in [
            &b""[..],
            b"message",
            b"1A",
            b"__CURSOR",
            b"A-B",
            b"A=B",
            b"\xc3\x89",
            &[b'A'; 65],
        ] {
            assert_eq!(
                FieldName::new(bad),
                Err(QueryError::FieldName(bad.to_vec())),
                "{bad:?}"
            );
        }
    }

    /// A refused name is shown quoted and escaped (security audit L5).
    #[test]
    fn refused_names_are_shown_escaped() {
        let field = FieldName::new(b"A\x1b]0;t\x07").expect_err("refused");
        assert_eq!(
            field.to_string(),
            "\"A\\u{1b}]0;t\\u{7}\" isn't a journal field name"
        );
        let unit = UnitName::new("a\u{1b}[2J").expect_err("refused");
        assert_eq!(
            unit.to_string(),
            "\"a\\u{1b}[2J\" isn't a service's unit name (only NAME or NAME.service)"
        );
        assert!(
            !field
                .to_string()
                .chars()
                .chain(unit.to_string().chars())
                .any(char::is_control)
        );
    }

    fn field(name: &str, value: &str) -> Query {
        Query::field(
            &FieldName::new(name.as_bytes()).expect("name"),
            value.as_bytes(),
        )
    }

    fn payloads(q: &Query) -> Vec<Vec<Vec<String>>> {
        q.groups()
            .iter()
            .map(|g| {
                g.iter()
                    .map(|t| {
                        t.iter()
                            .map(|v| String::from_utf8_lossy(v).into_owned())
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    /// `field` is one payload; `or` adds groups; `and` is the cross
    /// product of the groups, terms in order.
    #[test]
    fn queries_combine_as_journalctl_matches() {
        let q = field("A", "1").or(field("A", "2"));
        assert_eq!(payloads(&q), [vec![vec!["A=1"]], vec![vec!["A=2"]]]);
        let q = q.and(field("B", "3")).expect("2 groups");
        assert_eq!(
            payloads(&q),
            [
                vec![vec!["A=1"], vec!["B=3"]],
                vec![vec!["A=2"], vec!["B=3"]]
            ]
        );
        let raw = Query::field(&FieldName::new(b"M").expect("name"), b"\xff=\x00");
        assert_eq!(raw.groups(), [vec![vec![b"M=\xff=\x00".to_vec()]]]);
        assert_eq!(
            format!("{q:?}"),
            r#"Query { groups: [[["A=1"], ["B=3"]], [["A=2"], ["B=3"]]] }"#
        );
    }

    /// `and` builds at most 64 groups.
    #[test]
    fn and_refuses_past_64_groups() {
        let wide = |n: usize| (1..n).fold(field("A", "0"), |q, i| q.or(field("A", &i.to_string())));
        assert_eq!(wide(8).and(wide(8)).expect("64").groups().len(), 64);
        assert_eq!(wide(64).and(wide(1)).expect("64").groups().len(), 64);
        assert_eq!(
            wide(13).and(wide(5)),
            Err(QueryError::TooManyGroups { groups: 65 })
        );
        assert_eq!(
            wide(65).and(wide(1)),
            Err(QueryError::TooManyGroups { groups: 65 })
        );
        assert_eq!(
            QueryError::TooManyGroups { groups: 65 }.to_string(),
            "the query would have 65 groups, over the limit of 64"
        );
    }

    /// `Query::unit` is the release's unit rule.
    #[test]
    fn a_unit_query_is_the_rule() {
        let r = release("24.04");
        let q = Query::unit(&r, &unit(), None, &Source::Local);
        assert_eq!(q.groups(), unit_groups(r, &unit(), None, &Source::Local));
    }

    #[test]
    fn uids_hold_their_value() {
        assert_eq!(Uid::new(998).get(), 998);
        assert_eq!(Uid::new(998).to_string(), "998");
    }

    #[test]
    fn names_display_as_they_are() {
        assert_eq!(unit().to_string(), "a.service");
        assert_eq!(FieldName::new(b"_PID").expect("name").to_string(), "_PID");
    }
}
