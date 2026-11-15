//! The 22.04 deadsnakes cleanup (python-env spec, "Deadsnakes cleanup";
//! sb-go's `python/cleanup.go`; decisions 10 and 22): the Python 3.12
//! packages Saltbox once installed from the deadsnakes PPA removed, and
//! the PPA's source files moved aside, on sb-host's apt pieces
//! (amendments H2 and H3). The parsing of dpkg-query's rows and
//! apt-cache's policy is here, as pure functions over their text, sb-go's
//! rules one for one. Why not rust-apt
//! (<https://docs.rs/rust-apt/0.11.3/rust_apt/>): it binds libapt-pkg's
//! C++ (a build dependency on libapt-pkg-dev, and each release's own
//! libapt-pkg under one sb binary), against a pure-Rust sb and sb-go's
//! text rules (the status-file skip, the suffix fallback).

use std::path::PathBuf;

use sb_host::apt::{Apt, PackageListError, PackagePolicyError, PackageRow, SourcesError};
use sb_host::journal::Release;
use sb_term_ui::Step;
use tokio_util::sync::CancellationToken;

use crate::Env;

/// The one release the cleanup runs on (sb-go's `ShouldCleanupDeadsnakes`).
const JAMMY: &str = "22.04";

/// The Python minor deadsnakes gave Saltbox (sb-go's
/// `legacyDeadsnakesPythonMinor`).
const MINOR: &str = "3.12";

/// What a source file mentions to be moved aside, ASCII case ignored, and
/// the backup folder it goes to. The path both of the PPA's hostnames
/// ([`ORIGINS`]) share, so the file rule is the package rule's: a
/// `deadsnakes-snapshot` source sb-go keeps (its package then has another
/// origin, and is kept) stays too, where `deadsnakes` alone would move it
/// and leave that package to look like deadsnakes' on the next run (python-env
/// spec, "Deadsnakes cleanup", amended 2026-11-15).
const NEEDLE: &str = "/deadsnakes/ppa";
const BACKUP: &str = "deadsnakes";

/// The PPA's two hostnames in an apt origin (sb-go's markers).
const ORIGINS: [&str; 2] = [
    "ppa.launchpadcontent.net/deadsnakes/ppa",
    "ppa.launchpad.net/deadsnakes/ppa",
];

/// Why the cleanup stopped, in sb-go's words (cleanup.go). A child's
/// failure stays the source, so the step shows its last lines.
#[derive(Debug, thiserror::Error)]
pub enum DeadsnakesError {
    /// dpkg-query couldn't list the packages.
    #[error(transparent)]
    List(#[from] PackageListError),
    /// A row dpkg-query gave isn't one sb-go accepts.
    #[error("parse installed packages: line {line}: {reason}")]
    Parse { line: usize, reason: &'static str },
    /// apt-cache couldn't give a package's policy.
    #[error(transparent)]
    Policy(#[from] PackagePolicyError),
    /// The policy doesn't list the installed version.
    #[error("apt policy for {name} does not describe installed version {version}")]
    Undescribed { name: String, version: String },
    #[error("remove deadsnakes Python packages")]
    Remove(#[source] anyhow::Error),
    #[error("autoremove deadsnakes Python dependencies")]
    Autoremove(#[source] anyhow::Error),
    /// The query after the removal failed.
    #[error("verify deadsnakes package removal")]
    Verify(#[source] Box<DeadsnakesError>),
    #[error("deadsnakes Python packages remain after cleanup: {}", .0.join(", "))]
    PackagesRemain(Vec<String>),
    /// The source files couldn't be moved aside or checked again.
    #[error("remove deadsnakes repositories")]
    Repositories(#[source] RepositoriesError),
}

/// Why the source files weren't cleaned up, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum RepositoriesError {
    #[error(transparent)]
    Move(SourcesError),
    #[error("update apt package lists after removing deadsnakes repositories")]
    Update(#[source] anyhow::Error),
    #[error("verify deadsnakes repository removal: {0}")]
    Verify(SourcesError),
    #[error("deadsnakes repository files remain after cleanup: {}", shown(.0))]
    Remain(Vec<PathBuf>),
}

/// Paths joined with ", ", as sb-go joins them, each escaped as sb-host
/// logs a moved name, so a control character in one can't forge a line.
fn shown(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string().escape_debug().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The cleanup on 22.04 (spec, `cleanup_deadsnakes`; sb-go's
/// `CleanupDeadsnakesIfNeeded`), in `step`, whose output apt streams
/// into: the deadsnakes packages removed, then autoremove, then the query
/// again; then each source file mentioning deadsnakes moved aside into
/// `/var/backups/sb/deadsnakes/<run id>/`, `apt-get update`, and the
/// directory checked again. Returns whether it removed or moved anything.
/// On any other release, or one that can't be read (`check_support`
/// passed first, so it is a supported one), it does nothing.
///
/// # Errors
///
/// [`DeadsnakesError`]: a query, the removal, a package or source file
/// left, the move, or the update.
pub async fn cleanup_deadsnakes(step: &mut Step, env: &Env<'_>) -> Result<bool, DeadsnakesError> {
    if !on_jammy(env) {
        return Ok(false);
    }
    let token = step.token();
    let packages = packages(&env.apt, &token).await?;
    if !packages.is_empty() {
        let names: Vec<&str> = packages.iter().map(String::as_str).collect();
        env.apt
            .remove(step, &names)
            .await
            .map_err(DeadsnakesError::Remove)?;
        env.apt
            .autoremove(step)
            .await
            .map_err(DeadsnakesError::Autoremove)?;
        let left = packages_left(&env.apt, &token).await?;
        if !left.is_empty() {
            return Err(DeadsnakesError::PackagesRemain(left));
        }
    }
    let moved = repositories(step, env)
        .await
        .map_err(DeadsnakesError::Repositories)?;
    Ok(!packages.is_empty() || moved)
}

/// The query after the removal, its failure wrapped as sb-go's.
async fn packages_left(
    apt: &Apt,
    token: &CancellationToken,
) -> Result<Vec<String>, DeadsnakesError> {
    packages(apt, token)
        .await
        .map_err(|err| DeadsnakesError::Verify(Box::new(err)))
}

/// Whether the host is 22.04: the `Env`'s release when it names one
/// (tests), else the host's.
fn on_jammy(env: &Env<'_>) -> bool {
    let release = match &env.ubuntu_release {
        Some(id) => Release::parse(id).map_err(sb_host::UbuntuReleaseError::from),
        None => sb_host::ubuntu_release(),
    };
    match release {
        Ok(release) => release.version_id() == JAMMY,
        Err(err) => {
            tracing::info!(%err, "no deadsnakes cleanup: the release isn't known");
            false
        }
    }
}

/// The installed deadsnakes packages, in name order (sb-go's
/// `installedDeadsnakesPackages`): the candidates from dpkg-query, each
/// kept when its installed version's origin is the PPA's.
async fn packages(apt: &Apt, token: &CancellationToken) -> Result<Vec<String>, DeadsnakesError> {
    let rows = apt.package_list(token).await?;
    let mut found = Vec::new();
    for (name, version) in candidates(&rows)? {
        let policy = apt.package_policy(&name, token).await?;
        if from_deadsnakes(&policy, &name, &version)? {
            found.push(name);
        }
    }
    Ok(found)
}

/// The source files moved aside, then `apt-get update` and the check
/// (sb-go's `removeDeadsnakesRepositories`); whether any moved.
async fn repositories(step: &mut Step, env: &Env<'_>) -> Result<bool, RepositoriesError> {
    let moved = env
        .apt
        .remove_sources_mentioning(NEEDLE, BACKUP, &env.run_id)
        .map_err(RepositoriesError::Move)?;
    let Some(backup) = moved.first().and_then(|path| path.parent()) else {
        return Ok(false);
    };
    step.info(format!(
        "Moved {} deadsnakes repository files to {}",
        moved.len(),
        backup.display()
    ));
    env.apt
        .update_lists(step)
        .await
        .map_err(RepositoriesError::Update)?;
    let left = env
        .apt
        .sources_mentioning(NEEDLE)
        .map_err(RepositoriesError::Verify)?;
    if left.is_empty() {
        Ok(true)
    } else {
        Err(RepositoriesError::Remain(left))
    }
}

/// The installed rows that may be deadsnakes', as (name, version), in name
/// order (sb-go's `parseInstalledLegacyPythonPackages`): fields trimmed,
/// an empty name or an installed row without a version refused.
fn candidates(rows: &[PackageRow]) -> Result<Vec<(String, String)>, DeadsnakesError> {
    let mut found = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let failed = |reason| DeadsnakesError::Parse {
            line: index + 1,
            reason,
        };
        let (binary, version) = (row.binary.trim(), row.version.trim());
        if binary.is_empty() {
            return Err(failed("package name is empty"));
        }
        let installed = row.status.trim() == "ii";
        if installed && version.is_empty() {
            return Err(failed("installed package version is empty"));
        }
        if installed && legacy(binary, row.source.trim()) {
            found.push((binary.to_owned(), version.to_owned()));
        }
    }
    found.sort();
    Ok(found)
}

/// sb-go's `isLegacyDeadsnakesPackage`: source `python3.12`, or a binary
/// (without its architecture) `python3.12`, `python3.12-*`,
/// `libpython3.12*` or `idle-python3.12[-*]`.
fn legacy(binary: &str, source: &str) -> bool {
    let python = format!("python{MINOR}");
    if source == python {
        return true;
    }
    let name = binary.split_once(':').map_or(binary, |(name, _)| name);
    let idle = format!("idle-{python}");
    name == python
        || name.starts_with(&format!("{python}-"))
        || name.starts_with(&format!("lib{python}"))
        || name == idle
        || name.starts_with(&format!("{idle}-"))
}

/// Whether `name`'s installed `version` is deadsnakes' (sb-go's
/// `installedVersionHasDeadsnakesOrigin`): only the PPA as its origin, or,
/// with no source left, deadsnakes' `+focal<N>`/`+jammy<N>` suffix.
fn from_deadsnakes(policy: &str, name: &str, version: &str) -> Result<bool, DeadsnakesError> {
    let Some(origins) = origins(policy, version) else {
        return Err(DeadsnakesError::Undescribed {
            name: name.to_owned(),
            version: version.to_owned(),
        });
    };
    Ok(!origins.other && (origins.deadsnakes || legacy_suffix(version)))
}

/// The origins apt-cache lists under the `***` (installed) version.
#[derive(Debug, Default, PartialEq, Eq)]
struct Origins {
    deadsnakes: bool,
    other: bool,
}

/// sb-go's `policyVersionOrigins`: the origin lines below the `***`
/// line of `version`, up to the next version's line (five spaces of
/// indent, not eight), dpkg's status file left out; `None` when no `***`
/// line names `version`.
fn origins(policy: &str, version: &str) -> Option<Origins> {
    let mut lines = policy.lines();
    lines.find(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some("***") && fields.next() == Some(version)
    })?;
    let mut found = Origins::default();
    for line in lines {
        if line.starts_with("     ") && !line.starts_with("        ") {
            break;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [priority, rest @ ..] = fields.as_slice() else {
            continue;
        };
        if rest.is_empty() || priority.parse::<i64>().is_err() {
            continue;
        }
        let origin = rest.join(" ").to_lowercase();
        if origin.contains("/var/lib/dpkg/status") {
            continue;
        }
        if ORIGINS.iter().any(|marker| origin.contains(marker)) {
            found.deadsnakes = true;
        } else {
            found.other = true;
        }
    }
    Some(found)
}

/// sb-go's `hasLegacyDeadsnakesVersionSuffix`: the last `+focal` (else
/// `+jammy`) followed by digits only, at least one.
fn legacy_suffix(version: &str) -> bool {
    ["+focal", "+jammy"]
        .iter()
        .find_map(|marker| version.rsplit_once(marker))
        .is_some_and(|(_, revision)| {
            !revision.is_empty() && revision.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture from tests/fixtures/deadsnakes (dpkg-query rows and
    /// apt-cache policies in 22.04's formats, from sb-go's tests).
    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/deadsnakes")
            .join(name);
        std::fs::read_to_string(path).expect("fixture")
    }

    fn row(binary: &str, source: &str, status: &str, version: &str) -> PackageRow {
        PackageRow {
            binary: binary.to_owned(),
            source: source.to_owned(),
            status: status.to_owned(),
            version: version.to_owned(),
        }
    }

    /// The fixture's rows as sb-host's `package_list` reads them.
    fn rows(text: &str) -> Vec<PackageRow> {
        text.lines()
            .map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                row(fields[0], fields[1], fields[2].trim_end(), fields[3])
            })
            .collect()
    }

    /// sb-go's `TestParseInstalledLegacyPythonPackages`, plus a removed
    /// (`un`) row without a version and an unrelated package.
    #[test]
    fn the_candidates_are_sb_gos() {
        let found = candidates(&rows(&fixture("dpkg-query.txt"))).expect("parse");
        let want = [
            "2to3.12",
            "idle-python3.12",
            "libpython3.12-stdlib:amd64",
            "libpython3.12t64:amd64",
            "python3.12",
            "python3.12-distutils",
        ];
        assert_eq!(
            found,
            want.map(|name| (name.to_owned(), "3.12.12-1+jammy1".to_owned()))
        );
    }

    /// sb-go's two refusals, with its line numbers.
    #[test]
    fn a_row_without_a_name_or_an_installed_version_is_refused() {
        let good = row("python3.13", "python3.13", "ii", "3.13.7-1");
        let nameless = row(" ", "python3.12", "ii", "3.12.12-1+jammy1");
        let error = candidates(&[good.clone(), nameless]).expect_err("refused");
        assert_eq!(
            error.to_string(),
            "parse installed packages: line 2: package name is empty"
        );
        let versionless = row("python3.12", "python3.12", "ii", " ");
        let error = candidates(&[versionless]).expect_err("refused");
        assert_eq!(
            error.to_string(),
            "parse installed packages: line 1: installed package version is empty"
        );
        let removed = row("python3.12", "python3.12", "rc", "");
        assert_eq!(candidates(&[good, removed]).expect("parse"), []);
    }

    /// sb-go's `TestLegacyDeadsnakesPackageMatchingIsIndependentOfManagedPython`,
    /// and the edges of each prefix.
    #[test]
    fn the_package_rule_is_sb_gos() {
        let cases = [
            ("python3.12", "python3.12", true),
            ("python3.12-tk", "python3-stdlib-extensions", true),
            ("libpython3.12t64:amd64", "python3.12", true),
            ("idle-python3.12", "python3.12", true),
            ("2to3.12", "python3.12", true),
            ("python3.13", "python3.13", false),
            ("libpython3.13-stdlib:amd64", "python3.13", false),
            ("unrelated", "unrelated", false),
            ("python3.12:amd64", "other", true),
            ("python3.12x", "other", false),
            ("libpython3.12-dev:arm64", "other", true),
            ("idle-python3.12", "other", true),
            ("idle-python3.12-x", "other", true),
            ("idle-python3.12x", "other", false),
            ("xpython3.12", "other", false),
        ];
        for (binary, source, want) in cases {
            assert_eq!(legacy(binary, source), want, "{binary} {source}");
        }
    }

    /// The origins under the installed version, for each 22.04 fixture
    /// (sb-go's `TestPolicyVersion*`).
    #[test]
    fn the_origins_are_read_under_the_installed_version_only() {
        let version = "3.12.12-1+jammy1";
        let cases = [
            ("policy-ppa.txt", true, false),
            ("policy-old-host.txt", true, false),
            ("policy-ambiguous.txt", true, true),
            ("policy-other-version.txt", false, true),
            ("policy-snapshot.txt", false, true),
            ("policy-vanished.txt", false, false),
        ];
        for (name, deadsnakes, other) in cases {
            assert_eq!(
                origins(&fixture(name), version),
                Some(Origins { deadsnakes, other }),
                "{name}"
            );
        }
        assert_eq!(
            origins(&fixture("policy-ppa.txt"), "3.12.10-1+jammy1"),
            None
        );
        assert_eq!(origins("", version), None);
    }

    /// sb-go lowercases the origin; a line that isn't a priority and an
    /// origin is passed over.
    #[test]
    fn an_origin_is_matched_whatever_its_case() {
        let policy = " *** 1.0 500\n        500 HTTPS://PPA.LaunchpadContent.NET/DeadSnakes/PPA/ubuntu jammy/main\n  \n        \
                      not-a-number https://example.invalid\n        500\n";
        assert_eq!(
            origins(policy, "1.0"),
            Some(Origins {
                deadsnakes: true,
                other: false
            })
        );
    }

    /// sb-go's `TestLegacyDeadsnakesVersionSuffix`.
    #[test]
    fn the_version_suffix_is_sb_gos() {
        let cases = [
            ("3.12.12-1+jammy1", true),
            ("3.12.10-1+focal2", true),
            ("3.12.12-1ubuntu0.1", false),
            ("3.12.12-1+jammy", false),
            ("3.12.12-1+jammy1custom", false),
            ("99.0-1", false),
            ("1+jammy1+jammy22", true),
        ];
        for (version, want) in cases {
            assert_eq!(legacy_suffix(version), want, "{version}");
        }
    }

    /// The PPA alone, or no source left with the suffix; any other origin
    /// keeps the package; an undescribed version is sb-go's error.
    #[test]
    fn a_package_is_deadsnakes_only_from_the_ppa_alone() {
        let version = "3.12.12-1+jammy1";
        let cases = [
            ("policy-ppa.txt", true),
            ("policy-old-host.txt", true),
            ("policy-vanished.txt", true),
            ("policy-ambiguous.txt", false),
            ("policy-other-version.txt", false),
            ("policy-snapshot.txt", false),
        ];
        for (name, want) in cases {
            let got = from_deadsnakes(&fixture(name), "python3.12", version).expect("policy");
            assert_eq!(got, want, "{name}");
        }
        let vanished = " *** 99.0-1 100\n        100 /var/lib/dpkg/status\n";
        assert!(!from_deadsnakes(vanished, "python3.12-tk", "99.0-1").expect("policy"));
        let error = from_deadsnakes(&fixture("policy-ppa.txt"), "python3.12", "3.12.0-1")
            .expect_err("undescribed");
        assert_eq!(
            error.to_string(),
            "apt policy for python3.12 does not describe installed version 3.12.0-1"
        );
    }

    /// Paths joined with ", ", each escaped: a name with a control
    /// character can't forge a line (only root can make one there).
    #[test]
    fn leftover_files_are_named_escaped() {
        let left = RepositoriesError::Remain(vec![
            PathBuf::from("/etc/apt/sources.list.d/a.list"),
            PathBuf::from("/etc/apt/sources.list.d/evil\nname.list"),
        ]);
        assert_eq!(
            left.to_string(),
            "deadsnakes repository files remain after cleanup: \
             /etc/apt/sources.list.d/a.list, /etc/apt/sources.list.d/evil\\nname.list"
        );
    }
}
