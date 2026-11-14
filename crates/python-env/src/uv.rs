//! The host's uv and uvx (python-env spec, step 2; sb-go's `python/uv.go`,
//! `binaryVersionWithRuntime` and the start of
//! `ensureVersionWithProxyAndRuntime`). So far the probe: both report the
//! uv this sb pins. The download that replaces a wrong or missing one is
//! slice 4's (module build M18); until then a wrong uv fails the step.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::Env;
use crate::isolate::{IsolateError, Kind};

/// Each `--version` probe's limit (spec, difference 8).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why uv or uvx isn't the pinned one.
#[derive(Debug, thiserror::Error)]
pub enum UvError {
    /// The probe's child couldn't be prepared.
    #[error(transparent)]
    Isolate(#[from] IsolateError),
    /// The probe couldn't run or failed (exec's text).
    #[error("{0}")]
    Run(String),
    /// Its output isn't `<name> <version> ...` (sb-go's text).
    #[error("unexpected {name} version output {output:?}")]
    Output { name: &'static str, output: String },
    /// It runs, but isn't the pinned release (sb-go installs the pinned
    /// one here; module build M18 does).
    #[error("{} reports version {found}, expected {expected}", path.display())]
    Version {
        path: PathBuf,
        found: String,
        expected: String,
    },
}

/// The version in `output`, which must start with `name` (sb-go splits
/// the trimmed output at white space: `uv 0.13.4 (abc 2026-01-01)`).
fn parse_version(name: &'static str, output: &str) -> Result<String, UvError> {
    let output = output.trim();
    let mut fields = output.split_whitespace();
    match (fields.next(), fields.next()) {
        (Some(first), Some(version)) if first == name => Ok(version.to_string()),
        _ => Err(UvError::Output {
            name,
            output: output.to_string(),
        }),
    }
}

/// The version `program` reports as `name`: `<program> --no-config
/// --version`, isolated as every uv child, captured within 10 s.
///
/// # Errors
///
/// [`UvError`]: the child can't be prepared or run, fails, or prints
/// something else.
pub async fn version_of(
    env: &Env<'_>,
    program: &Path,
    name: &'static str,
    token: &CancellationToken,
) -> Result<String, UvError> {
    let probe = env
        .isolated(Kind::Uv { cache: false }, program)?
        .map(|cmd| cmd.arg("--version").timeout(PROBE_TIMEOUT));
    let output = crate::captured(&probe.cmd, token)
        .await
        .map_err(UvError::Run)?;
    parse_version(name, &output)
}

/// Both of the layout's uv and uvx report `env.uv_version` (spec, step 2,
/// its first bullet).
///
/// # Errors
///
/// [`UvError`]: the first that doesn't, uv's first.
pub async fn check_installed(env: &Env<'_>, token: &CancellationToken) -> Result<(), UvError> {
    for (path, name) in [(&env.layout.uv, "uv"), (&env.layout.uvx, "uvx")] {
        let found = version_of(env, path, name, token).await?;
        if found != env.uv_version {
            return Err(UvError::Version {
                path: path.clone(),
                found,
                expected: env.uv_version.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_the_second_field() {
        let got = parse_version("uv", "uv 0.13.4 (0cf2d7a 2026-11-01)\n").expect("parsed");
        assert_eq!(got, "0.13.4");
        let got = parse_version("uvx", "  uvx 0.13.4\n").expect("parsed");
        assert_eq!(got, "0.13.4");
    }

    /// sb-go's text, the output trimmed and quoted.
    #[test]
    fn other_output_is_refused() {
        for bad in ["", "uv", "uvx 0.13.4", "error: no\n"] {
            let err = parse_version("uv", bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("unexpected uv version output {:?}", bad.trim())
            );
        }
    }
}
