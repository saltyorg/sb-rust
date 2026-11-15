//! sb's full-screen screens (capability map, `tui`; docs/specs/tui.md).
//!
//! The groundwork (the tui spec's slice 0): terminal setup and restore,
//! chained with cli-core's restore hook and panic path, and the async
//! event loop every screen runs on ([`Tui`] and [`App`]).
//!
//! The parts of slice 1 with no screen: the log-source protocol and the
//! journal adapter ([`source`]), and the service status rule
//! ([`status`]); then the viewer ([`viewer`]) and its lines ([`line`]),
//! the picker ([`picker`]) and `sb logs`' screens ([`logs`]), with the
//! inline fallback when stdout isn't a terminal ([`inline`]).

pub mod line;
pub mod picker;
mod save;
mod search;
pub mod source;
pub mod status;
mod term;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod viewer;

use std::fs::File;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit;
use sb_cli_core::theme::Theme;
use sb_docker::connect::Daemon;
use sb_docker::ps::Row as ContainerRow;
use sb_host::journal::{Journal, Uid, UnitName};
use sb_host::systemd::{Service, Systemd, default_filters};
use sb_term_ui::DisplayText;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

pub use picker::Filter;
pub use term::{App, Ending, Flow, Tui};

use crate::line::Format;
use crate::picker::{Choice, ListServices, Picker};
use crate::source::{Event, Generation, Notice, Outbox, Request, SourceHandle};
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
    /// Where the viewer's `s` saves: the run log's directory (tui spec,
    /// "Pasteable output").
    pub save_dir: PathBuf,
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
/// until `q` (the picker's Esc too), `p`, Ctrl-C or the run's token. Esc in
/// the viewer goes back to the picker, its source stopped (the handle's
/// drop). The Containers section is kept live meanwhile. After `p`, once
/// the screen is gone, the loaded lines go to stdout and the review line
/// to stderr (tui spec, "Pasteable output").
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored. A failure to restore is reported whatever the
/// ending, Ctrl-C's included (cli-L5: a requested exit code mustn't hide
/// a real error). After `p`, a failed write to stdout or stderr, as
/// cli-core's `OutputError` (a broken pipe is its quiet exit).
pub async fn logs(shutdown: &Shutdown, logs: Logs) -> anyhow::Result<Ending> {
    let mut tui = Tui::enter(shutdown)?;
    let shown = screens(&mut tui, logs, shutdown.token()).await;
    let left = tui.leave().await;
    tracing::info!(ending = ?shown.as_ref().ok().map(|s| s.0), "logs screen closed");
    let (ending, printed) = ended(shown, left)?;
    if let Some((text, count)) = printed {
        print(&text)?;
        tracing::info!(lines = count, "log printed");
    }
    Ok(ending)
}

/// The inline fallback (tui spec, "The inline fallback"): the picker on
/// `tty` (`/dev/tty`, stdout not being a terminal), then, once the screen
/// is gone, the chosen log on stdout: the newest lines, then new ones as
/// they come, each in `stream`'s theme (the run's colour choice for
/// stdout), until the run's token (Ctrl-C, now a signal, or SIGTERM) or
/// the reader goes. The source's notices go to stderr.
///
/// # Errors
///
/// As [`logs`] for the picker; then a failed write to stdout or stderr,
/// as cli-core's `OutputError` (a broken pipe is its quiet exit), or a
/// follow that can't start (`can't follow the log: <cause>`): the stream
/// would wait for nothing.
pub async fn inline(
    shutdown: &Shutdown,
    logs: Logs,
    tty: File,
    stream: Theme,
) -> anyhow::Result<Ending> {
    let token = shutdown.token();
    let mut tui = Tui::on_tty(shutdown, tty)?;
    let zone = logs.zone.clone();
    let mut screens = Screens::new(&tui, logs, token.clone());
    let picked = screens.pick(&mut tui).await;
    let left = tui.leave().await;
    tracing::info!(ending = ?picked.as_ref().ok().map(|p| p.0), "logs picker closed");
    let (ending, choice) = ended(picked, left)?;
    let Some(choice) = choice else {
        return Ok(ending);
    };
    let (name, kind, open) = screens.source(choice)?;
    // The picker's watch and refresh end with it.
    drop(screens);
    let word = kind.word();
    tracing::info!(kind = word, name, "source opened");
    let format = Format::new(zone, stream);
    let mut out = io::BufWriter::new(io::stdout());
    let ending = stream_log(open, &format, &token, &mut out, &mut io::stderr()).await;
    tracing::info!(kind = word, name, ending = ?ending.as_ref().ok(), "stream ended");
    ending
}

/// How a source is opened on an [`Outbox`].
type Open = Box<dyn FnOnce(Outbox) -> SourceHandle>;

/// The picker with what its choices are opened from: the run's journal,
/// the daemon and the Containers section's watch (held for the screens'
/// life: its drop ends the watch).
struct Screens {
    picker: Picker,
    journal: Option<Arc<Journal>>,
    coredump_uid: Option<Uid>,
    daemon: Option<Arc<Daemon>>,
    _watcher: Option<source::docker::Watcher>,
    format: Format,
    save_dir: PathBuf,
}

impl Screens {
    /// The picker of `logs` on `tui`; `token` is the run's.
    fn new(tui: &Tui, logs: Logs, token: CancellationToken) -> Self {
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
        let (daemon, watcher) = match logs.docker {
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
        Self {
            picker: picker.with_filter(logs.filter),
            journal,
            coredump_uid,
            daemon,
            _watcher: watcher,
            format: Format::new(logs.zone, logs.theme),
            save_dir: logs.save_dir,
        }
    }

    /// The picker on `tui` until it ends: the source chosen when it quit
    /// with one.
    async fn pick(&mut self, tui: &mut Tui) -> io::Result<(Ending, Option<Choice>)> {
        // A list that came while the viewer showed: the viewer took the
        // wake it came with.
        self.picker.wake();
        let ending = tui.run(&mut self.picker).await?;
        let choice = match ending {
            Ending::Quit => self.picker.take_choice(),
            Ending::Interrupted | Ending::Cancelled => None,
        };
        Ok((ending, choice))
    }

    /// The chosen source's name and kind, and how it is opened.
    fn source(&self, choice: Choice) -> io::Result<(String, Kind, Open)> {
        Ok(match choice {
            Choice::Service(name) => {
                // A name the journal's rule refuses can't be read;
                // systemd's own names all pass.
                let unit = unit_of(&name).map_err(io::Error::other)?;
                let journal = self
                    .journal
                    .clone()
                    .ok_or_else(|| io::Error::other("no journal"))?;
                let coredump_uid = self.coredump_uid;
                let open: Open =
                    Box::new(move |o| source::spawn_journal(journal, &unit, coredump_uid, o));
                (name, Kind::Service, open)
            }
            Choice::Container(container) => {
                let daemon = self
                    .daemon
                    .clone()
                    .ok_or_else(|| io::Error::other("no daemon"))?;
                let name = container.name.clone();
                let open: Open = Box::new(move |o| source::spawn_docker(daemon, container, o));
                (name, Kind::Container, open)
            }
        })
    }
}

/// The picker and the viewers, in turn, on `tui`; `token` is the run's.
/// With the ending, the loaded lines and their count after `p`.
async fn screens(
    tui: &mut Tui,
    logs: Logs,
    token: CancellationToken,
) -> io::Result<(Ending, Option<(String, usize)>)> {
    let mut screens = Screens::new(tui, logs, token);
    loop {
        let (ending, choice) = screens.pick(tui).await?;
        let Some(choice) = choice else {
            return Ok((ending, None));
        };
        let (name, kind, open) = screens.source(choice)?;
        let word = kind.word();
        tracing::info!(kind = word, name, "source opened");
        let mut viewer = Viewer::open(&name, kind, screens.format.clone(), tui.waker(), open)
            .saving_in(screens.save_dir.clone());
        let ending = tui.run(&mut viewer).await;
        let leave = viewer.leave();
        let printed = (leave == Some(Leave::Print)).then(|| viewer.plain());
        drop(viewer);
        tracing::info!(kind = word, name, "source closed");
        match (ending?, leave) {
            (Ending::Quit, Some(Leave::Back)) => {}
            (ending, _) => return Ok((ending, printed)),
        }
    }
}

/// `p`'s output: `text` on stdout, then the review line on stderr.
fn print(text: &str) -> anyhow::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(exit::output_error)?;
    writeln!(io::stderr(), "{}", save::REVIEW).map_err(exit::output_error)?;
    Ok(())
}

/// The inline stream: the source `open` opens, its tail and then its
/// follow written to `out` line by line in `format`, its notices to
/// `err`, until `token` is cancelled ([`Ending::Cancelled`]). The events
/// come from the channel itself (tokio's `recv`,
/// <https://docs.rs/tokio/1.54.1/tokio/sync/mpsc/struct.UnboundedReceiver.html#method.recv>),
/// so a source that ends is seen; the screen's wake isn't needed here.
async fn stream_log(
    open: Open,
    format: &Format,
    token: &CancellationToken,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> anyhow::Result<Ending> {
    let (tx, mut events) = mpsc::unbounded_channel();
    let source = open(Outbox::new(
        tx,
        Arc::new(Notify::new()),
        Generation::default(),
    ));
    source.send(Request::Tail);
    source.send(Request::Follow(true));
    loop {
        let tagged = tokio::select! {
            biased;
            () = token.cancelled() => return Ok(Ending::Cancelled),
            tagged = events.recv() => tagged,
        };
        let Some(first) = tagged else {
            anyhow::bail!("the log's source stopped");
        };
        // Every event waiting, then one flush.
        let waiting = std::iter::from_fn(|| events.try_recv().ok());
        for tagged in std::iter::once(first).chain(waiting) {
            match tagged.event {
                Event::Page { lines, .. } | Event::Followed(lines) => {
                    for line in &lines {
                        save::write_line(out, line, format).map_err(exit::output_error)?;
                    }
                }
                Event::Notice(Notice::CantFollow(cause)) => {
                    out.flush().map_err(exit::output_error)?;
                    anyhow::bail!("can't follow the log: {cause}");
                }
                Event::Notice(notice) => {
                    // stdout's lines first, so the notice falls in order.
                    out.flush().map_err(exit::output_error)?;
                    let shown = DisplayText::from_bytes(notice.to_string().as_bytes(), false);
                    writeln!(err, "{}", shown.as_str()).map_err(exit::output_error)?;
                }
            }
        }
        out.flush().map_err(exit::output_error)?;
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
fn ended<T>(ending: io::Result<T>, left: io::Result<()>) -> io::Result<T> {
    let ending = ending?;
    left?;
    Ok(ending)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::source::PageKind;
    use crate::testing::{FakeSource, fake_source, line};

    /// How the stream ended, what it wrote on stdout and stderr, and the
    /// requests its source got, after `drive` has driven the fake source
    /// (the stream's token in hand).
    async fn streamed(
        drive: impl AsyncFnOnce(&mut FakeSource, &CancellationToken),
    ) -> (Result<Ending, String>, String, String, Vec<Request>) {
        let slot = Arc::new(Mutex::new(None));
        let held = Arc::clone(&slot);
        let open: Open = Box::new(move |outbox| {
            let (handle, fake) = fake_source(outbox);
            *held.lock().expect("the slot") = Some(fake);
            handle
        });
        let format = Format::new(jiff::tz::TimeZone::UTC, Theme::plain());
        let token = CancellationToken::new();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut requests = Vec::new();
        let (ending, ()) = tokio::join!(
            stream_log(open, &format, &token, &mut out, &mut err),
            async {
                // The stream opened its source at its first poll.
                tokio::task::yield_now().await;
                let mut fake = slot.lock().expect("the slot").take().expect("opened");
                drive(&mut fake, &token).await;
                requests.extend(std::iter::from_fn(|| fake.request()));
            }
        );
        let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("utf-8");
        (
            ending.map_err(|e| e.to_string()),
            text(out),
            text(err),
            requests,
        )
    }

    /// Turns enough for the stream to take what was sent: `join!` polls
    /// its branches in turn, either first.
    async fn turns() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn tail_page(lines: Vec<source::Line>) -> Event {
        Event::Page {
            kind: PageKind::Tail,
            oldest: lines.first().map(|l| l.position.clone()),
            newest: lines.last().map(|l| l.position.clone()),
            lines,
            more_older: true,
            more_newer: false,
        }
    }

    /// The tail, then followed lines on stdout; a notice on stderr; the
    /// token ends it as cancelled.
    #[tokio::test]
    async fn the_stream_writes_the_tail_then_the_follow_until_the_token() {
        let (ending, out, err, requests) = streamed(async |fake, token| {
            assert!(fake.send(tail_page(vec![line(1, "a"), line(2, "b")])));
            turns().await;
            assert!(fake.send(Event::Notice(Notice::Stopped)));
            assert!(fake.send(Event::Followed(vec![line(3, "c\x1b]0;t\x07")])));
            turns().await;
            token.cancel();
        })
        .await;
        assert_eq!(ending, Ok(Ending::Cancelled));
        assert_eq!(out, "a\nb\nc\n");
        assert_eq!(err, "container stopped; waiting for it to start again\n");
        assert_eq!(requests, [Request::Tail, Request::Follow(true)]);
    }

    /// A follow that can't start ends the stream: it would wait for
    /// nothing. What came before it is written.
    #[tokio::test]
    async fn a_follow_that_cant_start_ends_the_stream() {
        let (ending, out, err, _) = streamed(async |fake, _| {
            assert!(fake.send(tail_page(vec![line(1, "a")])));
            assert!(fake.send(Event::Notice(Notice::CantFollow("no watches".into()))));
            assert!(fake.send(Event::Followed(vec![line(2, "late")])));
        })
        .await;
        assert_eq!(ending, Err("can't follow the log: no watches".to_owned()));
        assert_eq!(out, "a\n");
        assert_eq!(err, "");
    }

    /// A source that goes is an error, not a stream waiting forever.
    #[tokio::test]
    async fn a_source_that_stops_ends_the_stream() {
        let (ending, out, _, _) = streamed(async |fake, _| {
            assert!(fake.send(Event::Followed(vec![line(1, "a")])));
            // The fake holds the stream's sender: dropping it closes the
            // channel.
            let (_, gone) = fake_source(Outbox::new(
                mpsc::unbounded_channel().0,
                Arc::new(Notify::new()),
                Generation::default(),
            ));
            drop(std::mem::replace(fake, gone));
        })
        .await;
        assert_eq!(ending, Err("the log's source stopped".to_owned()));
        assert_eq!(out, "a\n");
    }

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
