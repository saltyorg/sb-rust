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
mod search;
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
use sb_docker::connect::Daemon;
use sb_docker::ps::Row as ContainerRow;
use sb_host::journal::{Journal, Uid, UnitName};
use sb_host::systemd::{Service, Systemd, default_filters};
use tokio_util::sync::CancellationToken;

pub use picker::Filter;
pub use term::{App, Ending, Flow, Tui};

use crate::line::Format;
use crate::picker::{Choice, ListServices, Picker};
use crate::source::{Outbox, SourceHandle};
use crate::viewer::{Kind, Leave, Viewer};

/// What `sb logs` and `sb docker logs` show: the services and containers
/// listed before the screen, the run's one journal (tui spec, "Sources":
/// one `Journal::open` per run) and its Docker daemon.
#[derive(Debug)]
pub struct Logs {
    /// The services, with the journal their logs are read from, or why
    /// they couldn't be listed (`sb docker logs` shows that as the
    /// Services section's one row; `sb logs` ends before the screen).
    pub services: Result<Services, String>,
    pub docker: Docker,
    /// The kinds shown.
    pub filter: Filter,
    /// The run's theme: the screens' roles and the log spans' profile.
    pub theme: Theme,
    /// The zone line times are shown in (`line::local_zone`).
    pub zone: jiff::tz::TimeZone,
}

/// The services listed, and where their logs are read.
#[derive(Debug)]
pub struct Services {
    pub list: Vec<Service>,
    pub journal: Journal,
    /// The systemd-coredump user's uid for the unit queries, if it
    /// resolved (sb-host's `coredump_uid`).
    pub coredump_uid: Option<Uid>,
}

/// The containers listed before the screen, or why Docker couldn't be
/// reached (tui spec, "First load before the screen").
#[derive(Debug)]
pub enum Docker {
    Listed {
        daemon: Arc<Daemon>,
        rows: Vec<ContainerRow>,
    },
    /// `sb logs` opens without containers, the section's one row saying
    /// why.
    Unreachable(String),
}

/// The logs commands' screens (tui spec, "The picker" and "The
/// viewer"): the picker, and the viewer of the source chosen in it,
/// until `q` (the picker's Esc too), Ctrl-C or the run's token. Esc in
/// the viewer goes back to the picker, its source stopped (the handle's
/// drop). The Containers section is kept live meanwhile.
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored. A failure to restore is reported whatever the
/// ending, Ctrl-C's included (cli-L5: a requested exit code mustn't hide
/// a real error).
pub async fn logs(shutdown: &Shutdown, logs: Logs) -> io::Result<Ending> {
    let mut tui = Tui::enter(shutdown)?;
    let ending = screens(&mut tui, logs, shutdown.token()).await;
    let left = tui.leave().await;
    tracing::info!(ending = ?ending.as_ref().ok(), "logs screen closed");
    ended(ending, left)
}

/// The picker and the viewers, in turn, on `tui`; `token` is the run's.
async fn screens(tui: &mut Tui, logs: Logs, token: CancellationToken) -> io::Result<Ending> {
    let now = Timestamp::now();
    let (services, journal, coredump_uid, no_services) = match logs.services {
        Ok(s) => (s.list, Some(Arc::new(s.journal)), s.coredump_uid, None),
        Err(cause) => (Vec::new(), None, None, Some(cause)),
    };
    let mut picker = Picker::new(&services, now, logs.theme.clone());
    picker = match no_services {
        // The list is read again every 5 s while the picker shows (tui
        // spec, "Live updates").
        None => picker.with_refresh(list_services(token), tui.waker()),
        Some(cause) => picker.with_no_services(&cause),
    };
    // The watcher is held for the screens' life: its drop ends the watch.
    let (daemon, _watcher) = match logs.docker {
        Docker::Listed { daemon, rows } => {
            let (watcher, updates) = source::docker::watch(Arc::clone(&daemon), tui.waker());
            picker = picker.with_containers(&rows, now).with_updates(updates);
            (Some(daemon), Some(watcher))
        }
        Docker::Unreachable(cause) => {
            picker = picker.with_unreachable(&cause);
            (None, None)
        }
    };
    picker = picker.with_filter(logs.filter);
    let format = Format::new(logs.zone.clone(), logs.theme.clone());
    loop {
        // A list that came while the viewer showed: the viewer took the
        // wake it came with.
        picker.wake();
        let ending = tui.run(&mut picker).await?;
        if ending != Ending::Quit {
            return Ok(ending);
        }
        let Some(choice) = picker.take_choice() else {
            return Ok(ending);
        };
        let (name, kind, open): (String, Kind, Box<dyn FnOnce(Outbox) -> SourceHandle>) =
            match choice {
                Choice::Service(name) => {
                    // A name the journal's rule refuses can't be read;
                    // systemd's own names all pass.
                    let unit = unit_of(&name).map_err(io::Error::other)?;
                    let journal = journal
                        .clone()
                        .ok_or_else(|| io::Error::other("no journal"))?;
                    let open =
                        Box::new(move |o| source::spawn_journal(journal, &unit, coredump_uid, o));
                    (name, Kind::Service, open)
                }
                Choice::Container(container) => {
                    let daemon = daemon
                        .clone()
                        .ok_or_else(|| io::Error::other("no daemon"))?;
                    let name = container.name.clone();
                    let open = Box::new(move |o| source::spawn_docker(daemon, container, o));
                    (name, Kind::Container, open)
                }
            };
        let word = kind.word();
        tracing::info!(kind = word, name, "source opened");
        let mut viewer = Viewer::open(&name, kind, format.clone(), tui.waker(), open);
        let ending = tui.run(&mut viewer).await;
        let leave = viewer.leave();
        drop(viewer);
        tracing::info!(kind = word, name, "source closed");
        match (ending?, leave) {
            (Ending::Quit, Some(Leave::Back)) => {}
            (ending, _) => return Ok(ending),
        }
    }
}

/// The picker's 5 s read of the services: sb-host's list with the
/// default filters, as before the screen, ended by the run's `token`.
fn list_services(token: CancellationToken) -> ListServices {
    Box::new(move || {
        let token = token.clone();
        Box::pin(async move {
            Systemd::new()
                .services(&default_filters(), &token)
                .await
                .map_err(|err| err.to_string())
        })
    })
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
