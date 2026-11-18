//! Child processes for sb's commands (docs/specs/exec.md): one builder,
//! [`Cmd`], so every module gets the same cancellation, timeouts,
//! logging, secret handling and errors.

mod group;
mod mask;
mod run;

pub use group::on_forced_exit;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use sb_cli_core::log::WITHHELD;
use sb_cli_core::theme::Profile;

/// Set for every child not attached, over the caller's own (spec,
/// "Environment rules"), so nothing it runs waits on a prompt no one can
/// answer: git (run by the child or by a tool it starts) asks no terminal,
/// and ssh (run by git, or by any child) sends every passphrase and
/// password prompt to an askpass that fails at once (exec-M1). ssh(1),
/// ENVIRONMENT: with `SSH_ASKPASS_REQUIRE=force` "the askpass program will
/// be used for all passphrase input regardless of whether DISPLAY is set"
/// (<https://man.openbsd.org/ssh.1#ENVIRONMENT>). UNVERIFIED for the
/// host-key confirmation until the platform note's container check.
const NO_PROMPTS: [(&str, &str); 3] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("SSH_ASKPASS", "/bin/false"),
    ("SSH_ASKPASS_REQUIRE", "force"),
];

/// A child process to run (spec, "A command"). Named apart from std's and
/// tokio's `Command`. Its `Debug` shows what the log shows (no secret, no
/// variable's value), so a `?cmd` field or a `dbg!` can't print one.
pub struct Cmd {
    program: OsString,
    args: Vec<Arg>,
    clear_env: bool,
    env: BTreeMap<String, Var>,
    cwd: Option<PathBuf>,
    stdin: Option<Vec<u8>>,
    timeout: Option<Duration>,
    grace: Duration,
}

/// How long a group gets between SIGTERM and SIGKILL (decision 1).
const GRACE: Duration = Duration::from_secs(5);

enum Arg {
    /// A literal in sb's code (`arg`), shown.
    Literal(&'static str),
    /// A value shown as given (`shown_value`).
    Shown(String),
    /// A credential (`secret`): `[REDACTED]`, and taken out of output.
    Secret(String),
}

impl Arg {
    /// What the child gets.
    fn text(&self) -> &str {
        match self {
            Self::Literal(literal) => literal,
            Self::Shown(text) | Self::Secret(text) => text,
        }
    }
}

enum Var {
    Set(String),
    Secret(String),
    Removed,
}

impl fmt::Debug for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cmd")
            .field("command", &self.shown())
            .field("env_cleared", &self.clear_env)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .field("stdin", &self.stdin.as_ref().map(Vec::len))
            .field("timeout", &self.timeout)
            .field("grace", &self.grace)
            // The program and its arguments are in `command`, secrets masked.
            .finish_non_exhaustive()
    }
}

/// The way a child runs (spec, "Ways to run"); the environment rules and
/// the log depend on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Way {
    Capture,
    Stream,
    /// sb's own stdin, stdout and stderr, and sb's group; git and ssh may
    /// ask the user, so `GIT_TERMINAL_PROMPT` and ssh's askpass are left
    /// alone.
    Attached,
}

impl Way {
    fn name(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Stream => "stream",
            Self::Attached => "attached",
        }
    }
}

/// The environment a way hands its spawner: whether to start from an
/// empty one, then each variable set (`Some`) or removed (`None`), in name
/// order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Environment<'a> {
    pub(crate) clear: bool,
    pub(crate) vars: Vec<(&'a str, Option<&'a str>)>,
}

impl Cmd {
    /// A command running `program` (a name looked up in `PATH`, or a path),
    /// with sb's environment, working directory and no stdin.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            clear_env: false,
            env: BTreeMap::new(),
            cwd: None,
            stdin: None,
            timeout: None,
            grace: GRACE,
        }
    }

    /// A literal argument written in sb's code; the log shows it.
    #[must_use]
    pub fn arg(mut self, literal: &'static str) -> Self {
        self.args.push(Arg::Literal(literal));
        self
    }

    /// A value the log and errors show as given: the default for every
    /// value sb computes or passes on (paths, versions, names, branches,
    /// a user's ansible arguments; the maintainer, 2026-11-14 and
    /// 2026-11-15, spec decision 14). A credential (a token, a password,
    /// an API key) goes through [`Cmd::secret`] or [`Cmd::secret_env`]
    /// instead.
    #[must_use]
    pub fn shown_value(mut self, value: impl Into<String>) -> Self {
        self.args.push(Arg::Shown(value.into()));
        self
    }

    /// A credential: `[REDACTED]` in the log and errors, and taken out of
    /// the child's output. argv is readable by other users in
    /// `/proc/<pid>/cmdline`, so pass a secret through the environment or
    /// stdin where the tool allows.
    #[must_use]
    pub fn secret(mut self, secret: impl Into<String>) -> Self {
        self.args.push(Arg::Secret(secret.into()));
        self
    }

    /// Set a variable; the log names it, never its value.
    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), Var::Set(value.into()));
        self
    }

    /// Set a variable holding a credential: its value is also taken out of
    /// the child's output.
    #[must_use]
    pub fn secret_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), Var::Secret(value.into()));
        self
    }

    /// Remove an inherited variable.
    #[must_use]
    pub fn env_remove(mut self, name: impl Into<String>) -> Self {
        self.env.insert(name.into(), Var::Removed);
        self
    }

    /// Start from an empty environment (sb-go's `WithEnv`): variables set
    /// before are dropped, those set after are kept, as std's
    /// `Command::env_clear`
    /// (<https://doc.rust-lang.org/std/process/struct.Command.html#method.env_clear>).
    #[must_use]
    pub fn env_clear(mut self) -> Self {
        self.clear_env = true;
        self.env.clear();
        self
    }

    /// Run in `dir` (sb's own working directory otherwise). It must be an
    /// existing directory when the child starts.
    #[must_use]
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// End the child if it runs longer than `limit`, as a cancellation
    /// does (SIGTERM, the grace, SIGKILL), its outcome timed out.
    #[must_use]
    pub fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    /// The time between SIGTERM and SIGKILL for this command (5 s
    /// otherwise).
    #[must_use]
    pub fn grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Write `bytes` to the child's stdin, then close it (null otherwise).
    #[must_use]
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// The environment for `way`: the caller's changes, then
    /// [`NO_PROMPTS`] for every child not attached (spec, "Environment
    /// rules"), whatever the caller set for them.
    pub(crate) fn environment(&self, way: Way) -> Environment<'_> {
        self.environment_for(way, sb_cli_core::theme::current().profile())
    }

    /// [`Cmd::environment`] for a colour `profile`: an attached child also
    /// gets `COLORTERM` from it (decision 3): `truecolor` or `256color`, and an
    /// inherited one removed for `ansi` and `ascii`, so the child matches
    /// what sb itself draws.
    pub(crate) fn environment_for(&self, way: Way, profile: Profile) -> Environment<'_> {
        let colorterm = match (way, profile) {
            (Way::Capture | Way::Stream, _) => None,
            (Way::Attached, Profile::TrueColor) => Some(Some("truecolor")),
            (Way::Attached, Profile::Ansi256) => Some(Some("256color")),
            (Way::Attached, Profile::Ansi | Profile::Ascii) => Some(None),
        };
        let no_prompt = way != Way::Attached;
        let ruled = |name: &str| no_prompt && NO_PROMPTS.iter().any(|(rule, _)| *rule == name);
        let mut vars: Vec<(&str, Option<&str>)> = self
            .env
            .iter()
            .filter(|(name, _)| !ruled(name))
            .map(|(name, var)| {
                let value = match var {
                    Var::Set(value) | Var::Secret(value) => Some(value.as_str()),
                    Var::Removed => None,
                };
                (name.as_str(), value)
            })
            .collect();
        if no_prompt {
            vars.extend(NO_PROMPTS.iter().map(|&(rule, value)| (rule, Some(value))));
        }
        if let Some(value) = colorterm {
            vars.retain(|(name, _)| *name != "COLORTERM");
            vars.push(("COLORTERM", value));
        }
        vars.sort_unstable_by_key(|(name, _)| *name);
        Environment {
            clear: self.clear_env,
            vars,
        }
    }

    /// The command as the log and errors show it, for a caller that prints
    /// it (ansible's `Executing...` line): the program, literals and shown
    /// values in place, `[REDACTED]` for each secret. The arguments are
    /// joined with spaces, unquoted, as sb-go prints its command
    /// (`strings.Join(command, " ")`). Why not `shell_words::join` or
    /// `shlex`: they quote, and the line would then differ from sb-go's.
    #[must_use]
    pub fn shown(&self) -> String {
        let mut shown = self.program.to_string_lossy().into_owned();
        for arg in &self.args {
            shown.push(' ');
            match arg {
                Arg::Literal(literal) => shown.push_str(literal),
                Arg::Shown(value) => shown.push_str(value),
                Arg::Secret(_) => shown.push_str(WITHHELD),
            }
        }
        shown
    }

    /// How many arguments [`Cmd::shown`] masks (the secrets), for the log.
    fn masked(&self) -> usize {
        self.args
            .iter()
            // Exhaustive, so a new kind of argument must say whether it
            // counts.
            .filter(|arg| match arg {
                Arg::Secret(_) => true,
                Arg::Literal(_) | Arg::Shown(_) => false,
            })
            .count()
    }

    /// The secrets to take out of this command's output and errors.
    pub(crate) fn mask(&self) -> mask::Mask {
        let args = self.args.iter().filter_map(|arg| match arg {
            Arg::Secret(secret) => Some(secret.as_str()),
            Arg::Literal(_) | Arg::Shown(_) => None,
        });
        let vars = self.env.values().filter_map(|var| match var {
            Var::Secret(secret) => Some(secret.as_str()),
            Var::Set(_) | Var::Removed => None,
        });
        mask::Mask::new(args.chain(vars))
    }

    /// The run-log record of a child starting (spec, "Logging"): the
    /// command as shown, the count of masked arguments, the names of
    /// changed variables, the working directory and the way.
    pub(crate) fn log_start(&self, way: Way) {
        let command = self.shown();
        let masked = self.masked();
        let environment = self.environment(way);
        let changed: Vec<&str> = environment.vars.iter().map(|(name, _)| *name).collect();
        let cwd = self
            .cwd
            .as_ref()
            .map_or_else(|| ".".to_owned(), |dir| dir.display().to_string());
        let timeout_ms = self.timeout.map(millis);
        tracing::info!(
            command,
            masked,
            env = changed.join(","),
            env_cleared = environment.clear,
            cwd,
            way = way.name(),
            timeout_ms,
            grace_ms = millis(self.grace),
            "child started"
        );
    }
}

/// `duration` in whole milliseconds, for the log.
pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// How a child ended and how long it ran (spec, "Outcome and errors").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub ending: Ending,
    pub elapsed: Duration,
}

/// How a child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// It exited with this code.
    Exited(i32),
    /// A signal killed it (the signal's number).
    Signalled(i32),
    /// The token was cancelled while it ran, whatever it then did.
    Interrupted,
    /// It ran past its timeout, whatever it then did.
    TimedOut,
}

impl Ending {
    /// How it ended, short, for a failed step's times column: `exit 3`,
    /// the signal's name (signal-hook's), `interrupted` or `timed out`.
    pub fn reason(self) -> String {
        match self {
            Self::Exited(code) => format!("exit {code}"),
            Self::Signalled(signal) => signal_hook::low_level::signal_name(signal)
                .map_or_else(|| format!("signal {signal}"), str::to_owned),
            Self::Interrupted => "interrupted".to_owned(),
            Self::TimedOut => "timed out".to_owned(),
        }
    }
}

impl fmt::Display for Ending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Exited(code) => write!(f, "exited with code {code}"),
            Self::Signalled(signal) => match signal_hook::low_level::signal_name(signal) {
                Some(name) => write!(f, "was killed by {name}"),
                None => write!(f, "was killed by signal {signal}"),
            },
            Self::Interrupted => f.write_str("was interrupted"),
            Self::TimedOut => f.write_str("timed out"),
        }
    }
}

/// What [`Cmd::capture`] returns: the outcome and the child's output,
/// secrets taken out. stdout keeps its first 1 MiB, stderr its last (its
/// tail holds the error); `truncated` says either was cut. A text can
/// run a few bytes past 1 MiB: a character split by the cut becomes
/// U+FFFD, and a secret across it the placeholder.
#[derive(Debug)]
#[must_use = "exec returns Ok for a child that didn't exit 0: `check()` it, or read its outcome"]
pub struct Captured {
    pub outcome: Outcome,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    command: String,
}

/// How many of stderr's last lines a failure carries, each cut at
/// [`LINE_BYTES`].
const FAILED_LINES: usize = 10;

/// The longest line a failure carries (term-ui's cut for streamed lines,
/// exec spec "Lines").
const LINE_BYTES: usize = 4096;

impl Captured {
    /// The output if the child exited 0, else [`Failed`] with stderr's last
    /// lines.
    ///
    /// # Errors
    ///
    /// [`Failed`] when the child didn't exit 0.
    pub fn check(self) -> Result<Self, Failed> {
        if self.outcome.ending == Ending::Exited(0) {
            return Ok(self);
        }
        let lines: Vec<&str> = self.stderr.lines().collect();
        let last = lines[lines.len().saturating_sub(FAILED_LINES)..]
            .iter()
            .map(|line| line[..line.floor_char_boundary(LINE_BYTES)].to_owned())
            .collect();
        Err(failure(&self.command, self.outcome.ending, last))
    }
}

/// The failure of a child that didn't exit 0.
fn failure(command: &str, ending: Ending, last_lines: Vec<String>) -> Failed {
    Failed::new(format!("{command} {ending}"), ending.reason(), last_lines)
}

/// What [`Cmd::attached`] returns: the outcome. Its output went to sb's
/// own terminal, so there is no tail to keep.
#[derive(Debug)]
#[must_use = "exec returns Ok for a child that didn't exit 0: `check()` it, or read its outcome"]
pub struct Ran {
    pub outcome: Outcome,
    command: String,
}

impl Ran {
    /// The result if the child exited 0, else [`Failed`] with no lines of
    /// its own: what it printed is on the terminal already.
    ///
    /// # Errors
    ///
    /// [`Failed`] when the child didn't exit 0.
    pub fn check(self) -> Result<Self, Failed> {
        if self.outcome.ending == Ending::Exited(0) {
            return Ok(self);
        }
        Err(failure(&self.command, self.outcome.ending, Vec::new()))
    }

    /// Decision 5's pass-through, for an attached run whose own code sb
    /// passes on (an `ansible-playbook` run): exit 0 is `Ok`; otherwise
    /// cli-core's quiet `Exit`, so the command stops: for an interrupted
    /// run with no line (cli-core ends sb by the signal, its own block
    /// shown), else after one line on `out` naming the program and its
    /// outcome (cli-core's `report`, the arguments masked as in
    /// [`Ran::check`], no log pointer), with the child's own code, 128 plus a signal's
    /// number, or 1 for a timeout.
    ///
    /// # Errors
    ///
    /// The quiet [`Exit`](sb_cli_core::exit::Exit) above when the child
    /// didn't exit 0.
    pub fn pass_through(
        self,
        out: &mut impl std::io::Write,
    ) -> Result<Self, sb_cli_core::exit::Exit> {
        let code = match self.outcome.ending {
            Ending::Exited(0) => return Ok(self),
            Ending::Interrupted => return Err(sb_cli_core::exit::Exit(1)),
            Ending::Exited(code) => u8::try_from(code).unwrap_or(1),
            Ending::Signalled(signal) => sb_cli_core::cancel::signal_exit_code(signal),
            Ending::TimedOut => 1,
        };
        if let Err(failed) = self.check() {
            // cli-core's error block and write (its own code, 1, isn't the
            // one passed through), with no run-log pointer: the child's
            // output is on the terminal above and not in the run log (the
            // maintainer, 2026-11-18). An attached child that couldn't
            // start is the caller's error, and keeps the pointer.
            let _ = sb_cli_core::exit::report(&failed, None, out);
        }
        Err(sb_cli_core::exit::Exit(code))
    }
}

/// What [`Cmd::stream`] returns: the outcome, and stderr's last 64 KiB
/// (secrets taken out) for a caller matching on it (apt's retry). The
/// lines themselves went to the step.
#[derive(Debug)]
#[must_use = "exec returns Ok for a child that didn't exit 0: `check()` it, or read its outcome"]
pub struct Streamed {
    pub outcome: Outcome,
    pub stderr_tail: String,
    command: String,
}

impl Streamed {
    /// The result if the child exited 0, else [`Failed`] with no lines of
    /// its own: the step's failure block shows its last 10 lines.
    ///
    /// # Errors
    ///
    /// [`Failed`] when the child didn't exit 0.
    pub fn check(self) -> Result<Self, Failed> {
        if self.outcome.ending == Ending::Exited(0) {
            return Ok(self);
        }
        Err(failure(&self.command, self.outcome.ending, Vec::new()))
    }
}

/// A child that didn't exit 0: its command (arguments masked as in the
/// log) and how it ended, the short reason (`exit 3`, `SIGTERM`) and, from
/// `capture`, stderr's last lines. term-ui's type (the maintainer,
/// Checkpoint A), so a step failing with it shows the reason in its times
/// column.
pub use sb_term_ui::ChildFailure as Failed;

/// A child that couldn't be started (or waited for), naming the program.
#[derive(Debug, thiserror::Error)]
#[error("could not run {program}")]
pub struct SpawnError {
    program: String,
    #[source]
    cause: SpawnCause,
}

/// Why a child couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum SpawnCause {
    /// The working directory isn't an existing directory.
    #[error("no such working directory: {}", .0.display())]
    NoDirectory(PathBuf),
    /// This way of running takes no stdin bytes (an attached child reads
    /// sb's own stdin).
    #[error("a {0} child takes no stdin bytes")]
    StdinRefused(&'static str),
    /// An attached child can't start while a term-ui runner draws: they
    /// would share the terminal.
    #[error("a term-ui runner is drawing, so an attached child can't have the terminal")]
    RunnerLive,
    /// The working directory couldn't be looked at (a parent sb can't
    /// search, say).
    #[error("working directory {}", dir.display())]
    Directory {
        dir: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    /// The operating system refused (not found, not executable, ...).
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl SpawnError {
    /// Why it couldn't run.
    #[must_use]
    pub fn cause(&self) -> &SpawnCause {
        &self.cause
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literals and shown values show; only a secret is masked (spec,
    /// decision 14).
    #[test]
    fn only_secrets_are_masked() {
        let cmd = Cmd::new("git")
            .arg("-C")
            .shown_value("some/repo")
            .arg("fetch")
            .secret("hunter2-not-real");
        assert_eq!(cmd.shown(), "git -C some/repo fetch [REDACTED]");
        assert_eq!(cmd.masked(), 1);
        assert_eq!(Cmd::new("true").shown(), "true");
        assert_eq!(Cmd::new("true").masked(), 0);
    }

    /// A shown value is in the rendering as given and isn't counted as
    /// masked; it's in no secret mask, and the child gets it as it is.
    #[test]
    fn a_shown_value_is_shown_as_given() {
        let cmd = Cmd::new("ansible-playbook")
            .arg("--tags")
            .shown_value("plex,sonarr")
            .shown_value(r#"--extra-vars={"a": 1}"#)
            .shown_value("some/path");
        assert_eq!(
            cmd.shown(),
            r#"ansible-playbook --tags plex,sonarr --extra-vars={"a": 1} some/path"#
        );
        assert_eq!(cmd.masked(), 0);
        let argv: Vec<&str> = cmd.args.iter().map(Arg::text).collect();
        assert_eq!(
            argv,
            [
                "--tags",
                "plex,sonarr",
                r#"--extra-vars={"a": 1}"#,
                "some/path"
            ]
        );
        assert_eq!(cmd.mask().longest(), 0);
    }

    #[test]
    fn durations_are_logged_in_whole_milliseconds() {
        assert_eq!(millis(Duration::from_micros(1_500_900)), 1500);
        assert_eq!(millis(Duration::MAX), u64::MAX);
    }

    #[test]
    fn the_grace_is_5_s_unless_given() {
        assert_eq!(Cmd::new("x").grace, Duration::from_secs(5));
        let cmd = Cmd::new("x").grace(Duration::from_millis(50));
        assert_eq!(cmd.grace, Duration::from_millis(50));
        assert_eq!(Cmd::new("x").timeout, None);
    }

    #[test]
    fn a_reason_is_how_it_ended_short() {
        assert_eq!(Ending::Exited(3).reason(), "exit 3");
        assert_eq!(Ending::Signalled(15).reason(), "SIGTERM");
        assert_eq!(Ending::Signalled(200).reason(), "signal 200");
        assert_eq!(Ending::Interrupted.reason(), "interrupted");
        assert_eq!(Ending::TimedOut.reason(), "timed out");
    }

    #[test]
    fn a_signal_without_a_name_shows_its_number() {
        assert_eq!(Ending::Signalled(15).to_string(), "was killed by SIGTERM");
        assert_eq!(
            Ending::Signalled(200).to_string(),
            "was killed by signal 200"
        );
    }

    /// Debug shows what the log shows: a `?cmd` in a tracing field or a
    /// `dbg!` must not print a secret, a variable's value or stdin; a
    /// shown value is in it, as in the log.
    #[test]
    fn debug_shows_no_secret() {
        let cmd = Cmd::new("x")
            .arg("literal")
            .secret("arg-secret-zz")
            .shown_value("shown-zz")
            .env("PLAIN", "plain-zz")
            .secret_env("T", "env-secret-zz")
            .env_remove("GONE")
            .stdin("stdin-secret-zz");
        let shown = format!("{cmd:?}");
        for hidden in [
            "arg-secret-zz",
            "plain-zz",
            "env-secret-zz",
            "stdin-secret-zz",
        ] {
            assert!(!shown.contains(hidden), "{hidden}: {shown}");
        }
        assert_eq!(
            shown,
            r#"Cmd { command: "x literal [REDACTED] shown-zz", env_cleared: false, env: ["GONE", "PLAIN", "T"], cwd: None, stdin: Some(15), timeout: None, grace: 5s, .. }"#
        );
    }

    #[test]
    fn a_path_program_shows_in_full() {
        let cmd = Cmd::new("/srv/ansible/venv/bin/python").arg("-V");
        assert_eq!(cmd.shown(), "/srv/ansible/venv/bin/python -V");
    }

    #[test]
    fn children_not_attached_get_no_terminal_prompt_from_git() {
        let cmd = Cmd::new("uv").env("A", "1");
        assert_eq!(
            cmd.environment(Way::Capture),
            Environment {
                clear: false,
                vars: vec![
                    ("A", Some("1")),
                    ("GIT_TERMINAL_PROMPT", Some("0")),
                    ("SSH_ASKPASS", Some("/bin/false")),
                    ("SSH_ASKPASS_REQUIRE", Some("force")),
                ],
            }
        );
    }

    /// The rule wins over the caller: a child without a terminal must not
    /// wait on git's prompt.
    #[test]
    fn the_rule_wins_over_the_callers_own_value() {
        for cmd in [
            Cmd::new("git").env("GIT_TERMINAL_PROMPT", "1"),
            Cmd::new("git").env_remove("GIT_TERMINAL_PROMPT"),
        ] {
            assert_eq!(
                cmd.environment(Way::Capture).vars,
                [
                    ("GIT_TERMINAL_PROMPT", Some("0")),
                    ("SSH_ASKPASS", Some("/bin/false")),
                    ("SSH_ASKPASS_REQUIRE", Some("force")),
                ]
            );
        }
    }

    #[test]
    fn changes_are_kept_by_name_the_last_one_winning() {
        let cmd = Cmd::new("x")
            .env("B", "1")
            .secret_env("A", "s")
            .env_remove("C")
            .env("B", "2");
        assert_eq!(
            cmd.environment(Way::Capture).vars,
            [
                ("A", Some("s")),
                ("B", Some("2")),
                ("C", None),
                ("GIT_TERMINAL_PROMPT", Some("0")),
                ("SSH_ASKPASS", Some("/bin/false")),
                ("SSH_ASKPASS_REQUIRE", Some("force")),
            ]
        );
    }

    /// As std's `Command::env_clear`: earlier changes go, later ones stay.
    #[test]
    fn clearing_drops_earlier_changes_only() {
        let cmd = Cmd::new("x").env("A", "1").env_clear().env("B", "2");
        assert_eq!(
            cmd.environment(Way::Capture),
            Environment {
                clear: true,
                vars: vec![
                    ("B", Some("2")),
                    ("GIT_TERMINAL_PROMPT", Some("0")),
                    ("SSH_ASKPASS", Some("/bin/false")),
                    ("SSH_ASKPASS_REQUIRE", Some("force")),
                ],
            }
        );
    }

    #[test]
    fn secrets_come_from_secret_arguments_and_variables_only() {
        let cmd = Cmd::new("x")
            .arg("literal")
            .shown_value("value")
            .secret("arg-secret")
            .env("PLAIN", "plain")
            .secret_env("TOKEN", "env-secret");
        let mask = cmd.mask();
        assert_eq!(
            mask.text("literal value arg-secret plain env-secret"),
            "literal value [REDACTED] plain [REDACTED]"
        );
    }

    #[test]
    fn the_way_is_named_in_the_log() {
        assert_eq!(Way::Capture.name(), "capture");
        assert_eq!(Way::Stream.name(), "stream");
    }

    /// An attached child gets COLORTERM from the profile, over an
    /// inherited or given one; the other ways leave it alone.
    #[test]
    fn colorterm_follows_the_profile_for_attached_children() {
        let cmd = Cmd::new("x").env("COLORTERM", "given");
        let colorterm = |way, profile| {
            cmd.environment_for(way, profile)
                .vars
                .into_iter()
                .find(|(name, _)| *name == "COLORTERM")
        };
        assert_eq!(
            colorterm(Way::Attached, Profile::TrueColor),
            Some(("COLORTERM", Some("truecolor")))
        );
        assert_eq!(
            colorterm(Way::Attached, Profile::Ansi256),
            Some(("COLORTERM", Some("256color")))
        );
        assert_eq!(
            colorterm(Way::Attached, Profile::Ansi),
            Some(("COLORTERM", None))
        );
        assert_eq!(
            colorterm(Way::Attached, Profile::Ascii),
            Some(("COLORTERM", None))
        );
        assert_eq!(
            colorterm(Way::Stream, Profile::TrueColor),
            Some(("COLORTERM", Some("given")))
        );
        assert_eq!(
            colorterm(Way::Capture, Profile::Ansi256),
            Some(("COLORTERM", Some("given")))
        );
    }

    /// An attached child gets COLORTERM, as above, and git may
    /// ask on sb's terminal: no `GIT_TERMINAL_PROMPT=0`, a given one kept.
    #[test]
    fn an_attached_child_gets_colorterm_and_may_prompt() {
        let cmd = Cmd::new("git").env("GIT_TERMINAL_PROMPT", "1");
        assert_eq!(
            cmd.environment_for(Way::Attached, Profile::TrueColor).vars,
            [
                ("COLORTERM", Some("truecolor")),
                ("GIT_TERMINAL_PROMPT", Some("1"))
            ]
        );
        assert_eq!(
            cmd.environment_for(Way::Attached, Profile::Ascii).vars,
            [("COLORTERM", None), ("GIT_TERMINAL_PROMPT", Some("1"))]
        );
        assert_eq!(Way::Attached.name(), "attached");
    }

    /// The pass-through's code: the child's own, 128 plus a signal's
    /// number, 1 for a timeout; exit 0 is `Ok`; an interrupted run is a
    /// quiet 1 with no line.
    #[test]
    fn the_pass_through_code_follows_the_ending() {
        let ran = |ending| Ran {
            outcome: Outcome {
                ending,
                elapsed: std::time::Duration::ZERO,
            },
            command: "tool".into(),
        };
        let code = |ending| {
            let mut out = Vec::new();
            let code = ran(ending).pass_through(&mut out).err().map(|exit| exit.0);
            (code, String::from_utf8_lossy(&out).into_owned())
        };
        assert_eq!(code(Ending::Exited(0)), (None, String::new()));
        // Interrupted: an error (the caller stops), quietly, no line: sb
        // ends by the signal.
        assert_eq!(code(Ending::Interrupted), (Some(1), String::new()));
        let (four, line) = code(Ending::Exited(4));
        assert_eq!(four, Some(4));
        assert_eq!(line, "✗ tool exited with code 4\n");
        assert_eq!(code(Ending::Signalled(9)).0, Some(137));
        assert_eq!(code(Ending::TimedOut).0, Some(1));
    }

    /// ssh, run by git or by any child not attached, asks no terminal:
    /// its prompts go to a failing askpass (exec-M1), whatever the
    /// caller set; an attached child keeps the caller's own.
    #[test]
    fn ssh_asks_no_terminal_unless_attached() {
        let cmd = Cmd::new("git")
            .env("SSH_ASKPASS", "/usr/bin/given")
            .env_remove("SSH_ASKPASS_REQUIRE");
        for way in [Way::Capture, Way::Stream] {
            let vars = cmd.environment_for(way, Profile::Ascii).vars;
            assert!(
                vars.contains(&("SSH_ASKPASS", Some("/bin/false"))),
                "{way:?}: {vars:?}"
            );
            assert!(
                vars.contains(&("SSH_ASKPASS_REQUIRE", Some("force"))),
                "{way:?}: {vars:?}"
            );
            assert_eq!(
                vars.iter().filter(|(n, _)| n.starts_with("SSH_")).count(),
                2
            );
        }
        let attached = cmd.environment_for(Way::Attached, Profile::Ascii).vars;
        assert!(
            attached.contains(&("SSH_ASKPASS", Some("/usr/bin/given"))),
            "{attached:?}"
        );
        assert!(
            attached.contains(&("SSH_ASKPASS_REQUIRE", None)),
            "{attached:?}"
        );
        let plain = Cmd::new("git");
        let plain = plain.environment_for(Way::Attached, Profile::Ascii).vars;
        assert!(
            !plain.iter().any(|(n, _)| n.starts_with("SSH_")),
            "{plain:?}"
        );
    }

    #[test]
    fn a_streamed_child_gets_no_terminal_prompt_from_git() {
        assert_eq!(
            Cmd::new("git").environment(Way::Stream).vars,
            [
                ("GIT_TERMINAL_PROMPT", Some("0")),
                ("SSH_ASKPASS", Some("/bin/false")),
                ("SSH_ASKPASS_REQUIRE", Some("force")),
            ]
        );
    }
}
