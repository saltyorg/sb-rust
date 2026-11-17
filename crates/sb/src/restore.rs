//! `sb restore` (ops spec, "`sb restore` (hidden)"): sb-go's restore of a
//! user's backed-up config from Saltbox's restore service, on sb's
//! current-thread runtime.

use std::ffi::OsString;
use std::io::IsTerminal as _;

use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_ops::restore::{RestoreArgs, SERVICE};

/// `sb restore`: into the Saltbox root (`SB_SALTBOX_DIR`'s, ops
/// assumption 5), from the service or `SB_RESTORE_URL`'s, with a
/// `User-Agent` of `sb/<version>`; the screen when stdout is a terminal
/// (ops decision 32, as `sb logs` chooses its viewer).
///
/// # Errors
///
/// sb-ops' command's: an answer that can't be had, the client, the
/// screen's, no file restored, Ctrl-C on the screen, a failed write.
pub fn restore(args: &RestoreArgs, shutdown: &Shutdown) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let base = service(std::env::var_os("SB_RESTORE_URL"));
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    let user_agent = format!("sb/{}", build.version);
    super::block_on(sb_ops::restore::restore(
        args,
        &root,
        &base,
        &user_agent,
        std::io::stdout().is_terminal(),
        shutdown,
    ))?
}

/// The service's base: `SB_RESTORE_URL` when set and not empty (tests
/// and debugging, as config's `SB_DOCKERHUB_URL`; logged; sudo's relaunch
/// drops it), else [`SERVICE`].
fn service(var: Option<OsString>) -> String {
    match var
        .and_then(|value| value.into_string().ok())
        .filter(|v| !v.is_empty())
    {
        Some(url) => {
            tracing::info!(
                variable = "SB_RESTORE_URL",
                url = %sb_config::http::loggable_url(&url),
                "restore service overridden"
            );
            url
        }
        None => SERVICE.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_is_crs_unless_the_variable_moves_it() {
        assert_eq!(service(None), "https://crs.saltbox.dev");
        assert_eq!(service(Some("".into())), "https://crs.saltbox.dev");
        assert_eq!(service(Some("http://fake".into())), "http://fake");
    }
}
