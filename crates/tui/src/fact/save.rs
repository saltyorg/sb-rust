//! A save off the screen's thread (tui spec, editors, `sb fact`, Save):
//! sb-config's `Session::save` is synchronous and asks its hooks while it
//! runs, so it runs on its own thread (cli-core's `spawn::thread`, the
//! run's span carried) with the session moved in. Its hooks send the
//! screen what to show and wake it ([`Tui::waker`](crate::Tui::waker)):
//! the "waiting for Saltbox" dialog's time on each try, a drift that the
//! screen answers through a reply channel, and at the end the session
//! back with each role's outcome. The screen's cancel and the run's stop
//! (Ctrl-C, SIGTERM) are flags the hooks read.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use sb_config::facts::apply::{Change, RoleOutcome};
use sb_config::facts::session::{Drift, DriftChoice, SaveHooks, Session, Wait};
use tokio::sync::Notify;

/// What a running save tells the screen.
pub(super) enum Event {
    /// Saltbox holds `role`'s lock; the save has waited `waited`.
    Waiting { role: String, waited: Duration },
    /// `role` changed on disk; the save waits for the answer on `reply`.
    Drift {
        role: String,
        drift: Box<Drift>,
        reply: Sender<DriftChoice>,
    },
    /// The save is over: the session, and each role's outcome.
    Done {
        session: Box<Session>,
        outcomes: BTreeMap<String, RoleOutcome>,
    },
}

/// What [`Running::next`] found.
pub(super) enum Next {
    Event(Event),
    /// Nothing yet.
    Quiet,
    /// The thread ended without its `Done`.
    Gone,
}

/// A save under way. Dropped before it is over, it stops the save after
/// the role being committed, as Ctrl-C does.
#[derive(Debug)]
pub(super) struct Running {
    events: Receiver<Event>,
    /// Set by the waiting dialog's Cancel.
    cancel: Arc<AtomicBool>,
    /// Set by Ctrl-C or SIGTERM: the role being committed finishes first.
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The hooks the session asks during the save.
struct Hooks {
    events: Sender<Event>,
    wake: Option<Arc<Notify>>,
    cancel: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Hooks {
    fn send(&self, event: Event) -> bool {
        let sent = self.events.send(event).is_ok();
        if let Some(wake) = &self.wake {
            wake.notify_one();
        }
        sent
    }
}

impl SaveHooks for Hooks {
    fn waiting(&mut self, role: &str, waited: Duration) -> Wait {
        let _ = self.send(Event::Waiting {
            role: role.to_owned(),
            waited,
        });
        if self.cancel.load(Ordering::SeqCst) {
            Wait::Cancel
        } else {
            Wait::Keep
        }
    }

    /// The screen's answer; Cancel (keep the changes, write nothing) when
    /// the screen has gone or the run is stopping.
    fn drift(&mut self, role: &str, drift: &Drift) -> DriftChoice {
        let (reply, answer) = mpsc::channel();
        let asked = self.send(Event::Drift {
            role: role.to_owned(),
            drift: Box::new(drift.clone()),
            reply,
        });
        if !asked {
            return DriftChoice::Cancel;
        }
        answer.recv().unwrap_or(DriftChoice::Cancel)
    }

    fn stopped(&mut self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

impl Running {
    /// Save `changes` with `session` on a thread of its own, waking the
    /// screen through `wake` at each event.
    ///
    /// # Errors
    ///
    /// The thread couldn't be started; the session comes back with the
    /// error.
    pub(super) fn start(
        session: Session,
        changes: Vec<Change>,
        wake: Option<Arc<Notify>>,
    ) -> Result<Self, (Box<Session>, std::io::Error)> {
        let (events, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let mut hooks = Hooks {
            events,
            wake,
            cancel: Arc::clone(&cancel),
            stop: Arc::clone(&stop),
        };
        // The session moves in and back out with the outcomes; a thread
        // that can't start hands it back through this slot (std's
        // `Builder::spawn` drops the closure when it fails).
        let slot = Arc::new(Mutex::new(Some(session)));
        let held = Arc::clone(&slot);
        let started = sb_cli_core::spawn::thread(
            std::thread::Builder::new().name("fact-save".into()),
            move || {
                let taken = held.lock().unwrap_or_else(PoisonError::into_inner).take();
                let Some(mut session) = taken else {
                    return;
                };
                let outcomes = session.save(&changes, &mut hooks);
                let _ = hooks.send(Event::Done {
                    session: Box::new(session),
                    outcomes,
                });
            },
        );
        match started {
            Ok(thread) => Ok(Self {
                events: receiver,
                cancel,
                stop,
                thread: Some(thread),
            }),
            Err(err) => {
                let session = slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .expect("the thread never ran, so the session is still in its slot");
                Err((Box::new(session), err))
            }
        }
    }

    /// The next event the save has sent; [`Next::Gone`] when its thread
    /// ended without `Done` (it panicked: the session went with it).
    pub(super) fn next(&self) -> Next {
        match self.events.try_recv() {
            Ok(event) => Next::Event(event),
            Err(TryRecvError::Empty) => Next::Quiet,
            Err(TryRecvError::Disconnected) => Next::Gone,
        }
    }

    /// The waiting dialog's Cancel: the wait ends at the next try.
    pub(super) fn cancel_wait(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Ctrl-C or SIGTERM: the save stops after the role it is committing,
    /// and a drift question still open is answered Cancel (its reply
    /// dropped with the event).
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Wait for the save's thread to end, taking its events (the drift
    /// questions among them answered Cancel by their drop). The session
    /// comes back if the save got to the end.
    pub(super) fn finish(mut self) -> Option<(Box<Session>, BTreeMap<String, RoleOutcome>)> {
        let mut done = None;
        // The thread ends after its last event, `Done`: reading until the
        // channel closes waits for it and drops each drift's reply.
        while let Ok(event) = self.events.recv() {
            if let Event::Done { session, outcomes } = event {
                done = Some((session, outcomes));
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        done
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}
