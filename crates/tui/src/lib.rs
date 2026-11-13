//! sb's full-screen screens (capability map, `tui`; docs/specs/tui.md).
//!
//! The groundwork (the tui spec's slice 0): terminal setup and restore,
//! chained with cli-core's restore hook and panic path, and the async
//! event loop every screen runs on ([`Tui`] and [`App`]).
//!
//! The parts of slice 1 with no screen: the log-source protocol and the
//! journal adapter ([`source`]), and the service status rule
//! ([`status`]); then the viewer ([`viewer`]) and its lines ([`line`]),
//! the picker ([`picker`]) and `sb logs`' screens ([`logs`]).

pub mod line;
pub mod picker;
pub mod source;
pub mod status;
mod term;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod viewer;

use std::io;
use std::sync::Arc;

use jiff::Timestamp;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::theme::Theme;
use sb_host::journal::{Journal, Uid, UnitName};
use sb_host::systemd::Service;

pub use term::{App, Ending, Flow, Tui};

use crate::line::Format;
use crate::picker::Picker;
use crate::viewer::{Kind, Leave, Viewer};

/// What `sb logs` shows: the services listed before the screen, and the
/// run's one journal (tui spec, "Sources": one `Journal::open` per run).
#[derive(Debug)]
pub struct Logs {
    pub services: Vec<Service>,
    pub journal: Journal,
    /// The systemd-coredump user's uid for the unit queries, if it
    /// resolved (sb-host's `coredump_uid`).
    pub coredump_uid: Option<Uid>,
    /// The run's theme: the screens' roles and the log spans' profile.
    pub theme: Theme,
    /// The zone line times are shown in (`line::local_zone`).
    pub zone: jiff::tz::TimeZone,
}

/// `sb logs`' screens (tui spec, "The picker" and "The viewer"): the
/// picker, and the viewer of the service chosen in it, until `q` (the
/// picker's Esc too), Ctrl-C or the run's token. Esc in the viewer goes
/// back to the picker, its source stopped (the handle's drop).
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored. A failure to restore is reported whatever the
/// ending, Ctrl-C's included (cli-L5: a requested exit code mustn't hide
/// a real error).
pub async fn logs(shutdown: &Shutdown, logs: Logs) -> io::Result<Ending> {
    let mut tui = Tui::enter(shutdown)?;
    let ending = screens(&mut tui, logs).await;
    let left = tui.leave().await;
    tracing::info!(ending = ?ending.as_ref().ok(), "logs screen closed");
    ended(ending, left)
}

/// The picker and the viewers, in turn, on `tui`.
async fn screens(tui: &mut Tui, logs: Logs) -> io::Result<Ending> {
    let journal = Arc::new(logs.journal);
    let mut picker = Picker::new(&logs.services, Timestamp::now(), logs.theme.clone());
    let format = Format::new(logs.zone.clone(), logs.theme.clone());
    loop {
        let ending = tui.run(&mut picker).await?;
        if ending != Ending::Quit {
            return Ok(ending);
        }
        let Some(name) = picker.take_choice() else {
            return Ok(ending);
        };
        // A name the journal's rule refuses can't be read; systemd's own
        // names all pass.
        let unit = unit_of(&name).map_err(io::Error::other)?;
        tracing::info!(kind = "service", name, "source opened");
        let mut viewer = Viewer::open(&name, Kind::Service, format.clone(), tui.waker(), |o| {
            source::spawn_journal(Arc::clone(&journal), &unit, logs.coredump_uid, o)
        });
        let ending = tui.run(&mut viewer).await;
        let leave = viewer.leave();
        drop(viewer);
        tracing::info!(kind = "service", name, "source closed");
        match (ending?, leave) {
            (Ending::Quit, Some(Leave::Back)) => {}
            (ending, _) => return Ok(ending),
        }
    }
}

/// The unit of a service sb-host listed as `name` (without `.service`,
/// so a dotted name is still the service's, as sb-host's `show` names it).
fn unit_of(name: &str) -> Result<UnitName, sb_host::journal::QueryError> {
    UnitName::new(&format!("{name}.service"))
}

/// How a run of the screens ended, once the terminal is restored: the
/// screens' error first, then the restore's, then the ending.
fn ended(ending: io::Result<Ending>, left: io::Result<()>) -> io::Result<Ending> {
    let ending = ending?;
    left?;
    Ok(ending)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sb-host's names come without `.service`; a dotted one is still a
    /// service (the journal's rule would read `a.b` as a full unit name).
    #[test]
    fn a_listed_name_is_its_service_unit() {
        assert_eq!(
            unit_of("saltbox_managed_app").expect("a unit").as_str(),
            "saltbox_managed_app.service"
        );
        assert_eq!(
            unit_of("saltbox_managed_remote.example")
                .expect("a unit")
                .as_str(),
            "saltbox_managed_remote.example.service"
        );
        assert!(unit_of("bad name").is_err());
    }

    /// cli-L5: a restore that failed after Ctrl-C is still an error; the
    /// screens' own error comes first.
    #[test]
    fn a_failed_restore_wins_over_any_ending() {
        let failed = || io::Error::other("restore");
        let ended_with = |ending, left| ended(ending, left).map_err(|e| e.to_string());
        assert_eq!(
            ended_with(Ok(Ending::Interrupted), Err(failed())),
            Err("restore".to_owned())
        );
        assert_eq!(
            ended_with(Err(io::Error::other("draw")), Err(failed())),
            Err("draw".to_owned())
        );
        assert_eq!(
            ended_with(Ok(Ending::Interrupted), Ok(())),
            Ok(Ending::Interrupted)
        );
        assert_eq!(ended_with(Ok(Ending::Quit), Ok(())), Ok(Ending::Quit));
    }
}
