//! Prototype: can indicatif + portable-pty reproduce sb-go's task runner?
//! Drawn as the rounded tree picked from `--gallery`: a branch per step with
//! the times in one column, the running step's live output hanging under it,
//! the last lines of a failed step kept under it. Clean Ctrl-C, plain output
//! when piped. The run itself is simulated (apt-style output). Throwaway; see
//! FINDINGS.md.

mod gallery;

use std::collections::VecDeque;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use proto_theme as theme;
use rustix::process::{Pid, Signal, kill_process_group};

#[derive(Parser)]
struct Cli {
    /// Stream every output line instead of a live tail.
    #[arg(short, long)]
    verbose: bool,
    /// Add a failing step at the end.
    #[arg(long)]
    fail: bool,
    /// Add a 30s step to try Ctrl-C against.
    #[arg(long)]
    slow: bool,
    /// Print static snapshots of a run in several formats, to compare them.
    #[arg(long)]
    gallery: bool,
    /// When to use colour: auto (only on a terminal), always (also when
    /// piped, e.g. by the Saltbox installer script) or never.
    #[arg(long, value_enum, default_value_t = clap::ColorChoice::Auto)]
    color: clap::ColorChoice,
}

struct Step {
    running: &'static str,
    done: &'static str,
    cmd: Vec<String>,
}

const TAIL_LINES: usize = 3;
const FAILURE_TAIL: usize = 10;

fn sh(script: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script.into()]
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    apply_colour_choice(cli.color);
    if cli.gallery {
        gallery::print();
        return Ok(());
    }
    // One flag holding the stop signal that arrived (0 = none), registered
    // for signal-hook's shutdown signals as its flag module documents
    // (signal-hook 0.4.5 flag.rs, register_usize; consts::TERM_SIGNALS).
    // Either way the child's process group is stopped before we exit. A
    // second Ctrl-C is not flag::register_conditional_shutdown, which would
    // orphan the child; the TERM-then-KILL escalation below handles a child
    // that won't stop.
    let interrupted = Arc::new(AtomicUsize::new(0));
    for &signal in signal_hook::consts::TERM_SIGNALS {
        let value = usize::try_from(signal).unwrap_or(usize::MAX);
        signal_hook::flag::register_usize(signal, Arc::clone(&interrupted), value)?;
    }

    let mut steps = simulated_steps();
    if cli.slow {
        steps.push(Step {
            running: "Waiting on a long task (press Ctrl-C)",
            done: "Long task finished",
            cmd: sh("trap 'echo got TERM; exit 143' TERM; for i in $(seq 1 30); do echo \"waiting ($i/30)\"; sleep 1; done"),
        });
    }
    if cli.fail {
        steps.push(Step {
            running: "Installing an unavailable package",
            done: "Package installed",
            cmd: sh(r#"echo "Reading package lists... Done"; sleep 0.3
echo "Building dependency tree... Done"; sleep 0.3
echo "Reading state information... Done"; sleep 0.2
echo "E: Unable to locate package demo-unavailable"; exit 100"#),
        });
    }

    // indicatif decides whether to draw: its stderr target is hidden when
    // stderr isn't a terminal, or TERM is dumb or unset
    // (ProgressDrawTarget::term, indicatif 0.18.6 draw_target.rs). Our own
    // "live" flag follows that, so lines never go to a hidden target, where
    // MultiProgress::println does nothing.
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let live = !mp.is_hidden();
    let echo = live.then(QuietControlEcho::new).flatten();
    emit(
        &mp,
        live,
        &palette(theme::ACCENT)
            .bold()
            .apply_to("Saltbox (simulated run)")
            .to_string(),
    )?;
    for (i, step) in steps.iter().enumerate() {
        let last = i + 1 == steps.len();
        if let Err(e) = run_step(&mp, step, last, live, cli.verbose, &interrupted) {
            let signal = interrupted.load(Ordering::Relaxed);
            if signal != 0 {
                // 128 + the signal, the shell convention (130 for Ctrl-C, 143
                // for SIGTERM). An explicit exit, which signal-hook recommends
                // over re-raising the signal in a multi-threaded program
                // (low_level::emulate_default_handler).
                drop(echo);
                std::process::exit(128 + i32::try_from(signal).unwrap_or(0));
            }
            // A failed step is already shown in the tree, with its last
            // output; exit 1 without repeating it. Anything else (the PTY
            // could not be opened, ...) is reported as an error.
            if e.downcast_ref::<StepFailed>().is_some() {
                drop(echo);
                std::process::exit(1);
            }
            return Err(e);
        }
    }
    Ok(())
}

fn run_step(
    mp: &MultiProgress,
    step: &Step,
    last: bool,
    live: bool,
    verbose: bool,
    interrupted: &AtomicUsize,
) -> Result<()> {
    let spinner = mp.add(ProgressBar::new_spinner());
    spinner.set_style(ProgressStyle::with_template(&spinner_template())?);
    // The running step is the newest branch, so it takes the last-item glyph.
    spinner.set_prefix(faint(branch(true)));
    spinner.set_message(step.running);
    spinner.enable_steady_tick(Duration::from_millis(80));
    let tail_bars: Vec<ProgressBar> = if live && !verbose {
        (0..TAIL_LINES)
            .map(|_| {
                let b = mp.add(ProgressBar::new_spinner());
                b.set_style(
                    ProgressStyle::with_template(&tail_template()).expect("static template"),
                );
                b
            })
            .collect()
    } else {
        Vec::new()
    };

    let (rows, cols) = console::Term::stderr()
        .size_checked()
        .map_or((24, 80), |(r, c)| (r.max(10), c.max(40)));
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&step.cmd[0]);
    cmd.args(&step.cmd[1..]);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let pid = child.process_id();

    // (text, overwrite): overwrite=true means the segment ended in a bare \r,
    // so the next segment replaces it (progress counters) instead of appending.
    let (tx, rx) = mpsc::channel::<(String, bool)>();
    let mut reader = pair.master.try_clone_reader()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut splitter = LineSplitter::default();
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            for line in splitter.push(&buf[..n]) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        }
        for line in splitter.finish() {
            let _ = tx.send(line);
        }
    });

    let mut history: VecDeque<String> = VecDeque::with_capacity(FAILURE_TAIL);
    let mut last_overwrites = false;
    let mut killed_at: Option<Instant> = None;
    let mut escalated = false;
    let status = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((line, overwrite)) => {
                if last_overwrites {
                    history.pop_back();
                }
                last_overwrites = overwrite;
                if verbose && !overwrite {
                    let line = format!("{}{line}", faint(skip(last)));
                    if live {
                        mp.println(line)?;
                    } else if console::colors_enabled() {
                        println!("{line}");
                    } else {
                        println!("{}", console::strip_ansi_codes(&line));
                    }
                }
                if history.len() == FAILURE_TAIL {
                    history.pop_front();
                }
                history.push_back(line);
                let start = history.len().saturating_sub(TAIL_LINES);
                let shown = history.len() - start;
                for (i, (bar, line)) in tail_bars.iter().zip(history.iter().skip(start)).enumerate()
                {
                    bar.set_prefix(tail_prefix(i, shown));
                    bar.set_message(line.clone());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break child.wait()?,
        }
        if interrupted.load(Ordering::Relaxed) != 0 {
            match killed_at {
                None => {
                    if let Some(pgid) = pid.and_then(|p| Pid::from_raw(p as i32)) {
                        let _ = kill_process_group(pgid, Signal::TERM);
                    }
                    killed_at = Some(Instant::now());
                }
                // Escalate to the whole process group, as for TERM: portable-pty's
                // Child::kill signals only the child's own pid (and blocks for
                // up to ~200 ms), so grandchildren would survive.
                Some(t) if t.elapsed() > Duration::from_secs(3) && !escalated => {
                    if let Some(pgid) = pid.and_then(|p| Pid::from_raw(p as i32)) {
                        let _ = kill_process_group(pgid, Signal::KILL);
                    }
                    escalated = true;
                }
                Some(_) => {}
            }
        }
        if let Some(status) = child.try_wait()? {
            // Drain whatever the reader already queued before reporting.
            while let Ok((line, overwrite)) = rx.try_recv() {
                if last_overwrites {
                    history.pop_back();
                }
                last_overwrites = overwrite;
                history.push_back(line);
                if history.len() > FAILURE_TAIL {
                    history.pop_front();
                }
            }
            break status;
        }
    };

    for bar in &tail_bars {
        bar.finish_and_clear();
    }
    // Finished steps become permanent printed lines; indicatif only manages
    // live bars. Its MultiProgress docs describe this: a dropped bar is wiped
    // on the next println and not redrawn, so either keep the handle or print
    // the line yourself.
    // https://docs.rs/indicatif/0.18.6/indicatif/struct.MultiProgress.html
    // Elapsed in the same format as the live {elapsed} (indicatif 0.18.6
    // style.rs renders it with HumanDuration's alternate form).
    let elapsed = format!("{:#}", indicatif::HumanDuration(spinner.elapsed()));
    spinner.finish_and_clear();
    if interrupted.load(Ordering::Relaxed) != 0 {
        emit(
            mp,
            live,
            &step_line(true, &Outcome::Interrupted, step.running, &elapsed),
        )?;
        bail!("interrupted");
    }
    if status.success() {
        emit(
            mp,
            live,
            &step_line(last, &Outcome::Done, step.done, &elapsed),
        )?;
        return Ok(());
    }
    // A failure ends the run, so the failed step is the last branch and its
    // last output lines hang under it.
    let outcome = Outcome::Failed {
        code: status.exit_code(),
        signal: status.signal().map(str::to_string),
    };
    emit(mp, live, &step_line(true, &outcome, step.running, &elapsed))?;
    for (i, line) in history.iter().enumerate() {
        emit(
            mp,
            live,
            &format!("{}{line}", tail_prefix(i, history.len())),
        )?;
    }
    Err(StepFailed.into())
}

/// Turns off the terminal's echo of control characters (`ECHOCTL`) while the
/// runner draws, and restores it when dropped. Without this, pressing Ctrl-C
/// echoes `^C` among indicatif's lines, its line count goes wrong, and a
/// stale spinner line is left above the "interrupted" line. Measured in
/// tmux: one stale line without this, none with it; turning on `NOFLSH`
/// instead made no difference. Ctrl-C still sends SIGINT; only the echo goes.
/// Uses rustix's termios wrappers (tcgetattr/tcsetattr on stdin, the
/// controlling terminal). Why not crossterm's `enable_raw_mode`: raw mode
/// clears ISIG, so Ctrl-C would stop sending SIGINT. UNVERIFIED pattern: no
/// indicatif or console documentation covers this; checked against tmux as
/// above. (The PTY harness's emulator, pyte, doesn't show the echo, so it
/// can't check this.)
struct QuietControlEcho {
    original: rustix::termios::Termios,
}

impl QuietControlEcho {
    fn new() -> Option<Self> {
        let stdin = rustix::stdio::stdin();
        let original = rustix::termios::tcgetattr(stdin).ok()?;
        let mut quiet = original.clone();
        quiet
            .local_modes
            .remove(rustix::termios::LocalModes::ECHOCTL);
        rustix::termios::tcsetattr(stdin, rustix::termios::OptionalActions::Now, &quiet).ok()?;
        Some(Self { original })
    }
}

impl Drop for QuietControlEcho {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            rustix::stdio::stdin(),
            rustix::termios::OptionalActions::Now,
            &self.original,
        );
    }
}

/// A step exited unsuccessfully; already reported in the tree.
#[derive(Debug)]
struct StepFailed;

impl std::fmt::Display for StepFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("step failed")
    }
}

impl std::error::Error for StepFailed {}

#[derive(Debug)]
enum Outcome {
    Done,
    Failed { code: u32, signal: Option<String> },
    Interrupted,
}

/// Splits a child's output into lines. `(text, overwrite)`: overwrite means
/// the segment ended in a bare `\r`, so the next one replaces it (progress
/// counters) instead of appending.
///
/// Custom, pending evaluation: none of indicatif, console or portable-pty
/// splits a byte stream into lines. A terminal emulator crate (vt100) would
/// also handle cursor and erase sequences, but changes how the output tail is
/// built; it is an open item for the runner module (capability map).
/// Bytes are split before decoding, so a character cut across two reads
/// arrives whole (`\n`/`\r` never occur inside a UTF-8 sequence).
#[derive(Default)]
struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    fn push(&mut self, bytes: &[u8]) -> Vec<(String, bool)> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(i) = self.pending.iter().position(|&b| b == b'\n' || b == b'\r') {
            let bare_cr = self.pending[i] == b'\r';
            if bare_cr && i + 1 == self.pending.len() {
                break; // wait: this \r may be the first half of \r\n
            }
            let crlf = bare_cr && self.pending[i + 1] == b'\n';
            let end = if crlf { i + 2 } else { i + 1 };
            let segment: Vec<u8> = self.pending.drain(..end).collect();
            let text = String::from_utf8_lossy(&segment[..i]).into_owned();
            if !text.is_empty() {
                out.push((text, bare_cr && !crlf));
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<(String, bool)> {
        let rest_bytes = std::mem::take(&mut self.pending);
        let rest = String::from_utf8_lossy(&rest_bytes);
        let text = rest.trim_end_matches(['\n', '\r']).to_string();
        if text.is_empty() {
            Vec::new()
        } else {
            vec![(text, rest.ends_with('\r'))]
        }
    }
}

/// A palette colour as a console style. ratatui's `Color` prints as
/// `#RRGGBB` (ratatui-core 0.1.2 style/color.rs, Display), which console's
/// dotted style syntax parses as a 24-bit colour (console 0.16.6 utils.rs,
/// `Style::from_dotted_str`). console writes RGB as 24-bit SGR whatever the
/// terminal reports, and leaves output plain when colours are off (piped).
pub(crate) fn palette(colour: impl std::fmt::Display) -> console::Style {
    // The lines go to stderr, so colour follows stderr's setting, as
    // indicatif does for its own bars (ProgressBar::set_style).
    console::Style::from_dotted_str(&colour.to_string()).for_stderr()
}

/// Applies `--color` to console's colour switches, which every style here
/// and indicatif's bars follow. `auto` keeps console's own detection
/// (terminal or not, NO_COLOR, dumb terminals, CLICOLOR/CLICOLOR_FORCE).
/// The value type is clap's own ColorChoice, the auto/always/never
/// convention of git, ls and cargo. Why not the colorchoice crate (in the
/// tree through clap): it sets anstream's global, which console and
/// indicatif don't read.
fn apply_colour_choice(choice: clap::ColorChoice) {
    let enabled = match choice {
        clap::ColorChoice::Always => true,
        clap::ColorChoice::Never => false,
        clap::ColorChoice::Auto => return,
    };
    console::set_colors_enabled(enabled);
    console::set_colors_enabled_stderr(enabled);
}

/// Prints a permanent line above the live bars, or straight to stderr when
/// indicatif isn't drawing (piped, or a dumb terminal).
fn emit(mp: &MultiProgress, live: bool, text: &str) -> Result<()> {
    if live {
        mp.println(text)?;
    } else if console::colors_enabled_stderr() {
        eprintln!("{text}");
    } else {
        // Child output keeps its colours under the PTY; strip them when
        // console says stderr gets no colour (not a terminal, or
        // CLICOLOR=0): https://docs.rs/console/0.16.6/console/fn.strip_ansi_codes.html
        eprintln!("{}", console::strip_ansi_codes(text));
    }
    Ok(())
}

/// Column where every step's right-hand text (elapsed, exit code) ends.
const COLUMN: usize = 64;
/// Width the running step's elapsed time is right-aligned in.
const ELAPSED_WIDTH: usize = 6;

/// The rounded tree picked from the gallery: termtree's glyphs with
/// ratatui's rounded corner for the last branch.
/// https://docs.rs/termtree/1.0.0/termtree/struct.GlyphPalette.html
/// termtree only draws a finished tree in one go, so the live runner borrows
/// its glyph set and draws the branches line by line through indicatif
/// prefixes, the pattern of indicatif's own tree example:
/// https://github.com/console-rs/indicatif/blob/main/examples/multi-tree-ext.rs
fn glyphs() -> termtree::GlyphPalette {
    termtree::GlyphPalette {
        last_item: ratatui::symbols::line::ROUNDED_BOTTOM_LEFT,
        ..termtree::GlyphPalette::new()
    }
}

/// A step's branch: "├── ", or "╰── " for the last one.
fn branch(last: bool) -> String {
    let g = glyphs();
    format!(
        "{}{}",
        if last { g.last_item } else { g.middle_item },
        g.item_indent
    )
}

/// What sits in front of a step's children: "│   " under a middle branch,
/// spaces under the last.
fn skip(last: bool) -> String {
    let g = glyphs();
    format!(
        "{}{}",
        if last { g.last_skip } else { g.middle_skip },
        g.skip_indent
    )
}

/// Prefix for line `i` of `n` hanging under the running or failed (last)
/// step: ├── for each, ╰── for the newest.
fn tail_prefix(i: usize, n: usize) -> String {
    faint(format!("{}{}", skip(true), branch(i + 1 == n)))
}

fn faint(text: impl std::fmt::Display) -> String {
    palette(theme::FAINT).apply_to(text).to_string()
}

/// The running step: branch, spinner and name, then the elapsed time
/// right-aligned so it ends in `COLUMN`, like the finished lines. Width and
/// alignment are indicatif's own template syntax ({key:<width} and
/// {key:>width}), which pads by display width; `!` truncates a name that is
/// too long, so the time stays in its column. Its style part goes to the same
/// parser as `palette`. https://docs.rs/indicatif/0.18.6/indicatif/#templates
fn spinner_template() -> String {
    let branch_width = console::measure_text_width(&branch(true));
    let name = COLUMN - branch_width - 2 - 1 - ELAPSED_WIDTH;
    format!(
        "{{prefix}}{{spinner:.{a}}} {{msg:<{name}!.{a}}} {{elapsed:>{ELAPSED_WIDTH}.{m}}}",
        a = theme::ACCENT,
        m = theme::MUTED
    )
}

/// One line of the live output tail: the command's output as it is, with
/// its own colours, and uncoloured text in the terminal's default colour.
/// Only the tree prefix is styled (before it is set).
fn tail_template() -> String {
    "{prefix}{wide_msg}".to_string()
}

/// The permanent line a finished step leaves: its branch, the status and
/// name, and the right-hand text ending in `COLUMN` (console's pad_str pads
/// by display width): https://docs.rs/console/0.16.6/console/fn.pad_str.html
fn step_line(last: bool, outcome: &Outcome, text: &str, elapsed: &str) -> String {
    let (left, right) = match outcome {
        Outcome::Done => (
            palette(theme::OK).apply_to(format!("✓ {text}")).to_string(),
            palette(theme::MUTED).apply_to(elapsed).to_string(),
        ),
        // portable-pty reports a signalled child as code 1 plus the signal
        // name (portable-pty 0.9.0 lib.rs, ExitStatus).
        Outcome::Failed {
            signal: Some(signal),
            ..
        } => (
            palette(theme::PROBLEM)
                .apply_to(format!("✗ {text}"))
                .to_string(),
            palette(theme::PROBLEM)
                .apply_to(format!("killed by signal ({signal}) · {elapsed}"))
                .to_string(),
        ),
        Outcome::Failed { code, .. } => (
            palette(theme::PROBLEM)
                .apply_to(format!("✗ {text}"))
                .to_string(),
            palette(theme::PROBLEM)
                .apply_to(format!("exit {code} · {elapsed}"))
                .to_string(),
        ),
        Outcome::Interrupted => (
            palette(theme::WARN)
                .apply_to(format!("✗ {text}"))
                .to_string(),
            palette(theme::WARN)
                .apply_to(format!("interrupted · {elapsed}"))
                .to_string(),
        ),
    };
    let branch = branch(last);
    let room = COLUMN.saturating_sub(
        console::measure_text_width(&branch) + console::measure_text_width(&right) + 1,
    );
    format!(
        "{}{} {right}",
        faint(branch),
        console::pad_str(&left, room, console::Alignment::Left, None)
    )
}

/// A simulated install with apt-style output, so the runner can be judged
/// without touching the system. Progress uses `\r` updates, as apt's does.
fn simulated_steps() -> Vec<Step> {
    vec![
        Step {
            running: "Checking the system",
            done: "System checked",
            cmd: sh(r#"echo "Ubuntu 24.04 (noble), x86_64"; sleep 0.4
echo "Disk: 41 GiB free on /"; sleep 0.4
echo "Memory: 7.6 GiB"; sleep 0.3"#),
        },
        Step {
            running: "Updating package lists",
            done: "Package lists updated",
            cmd: sh(r#"for line in \
  "Hit:1 http://archive.example.org/ubuntu noble InRelease" \
  "Get:2 http://archive.example.org/ubuntu noble-updates InRelease [126 kB]" \
  "Get:3 http://security.example.org/ubuntu noble-security InRelease [126 kB]" \
  "Get:4 http://archive.example.org/ubuntu noble-updates/main amd64 Packages [612 kB]" \
  "Fetched 864 kB in 2s (432 kB/s)"; do echo "$line"; sleep 0.5; done
printf 'Reading package lists... 0%%\r'; sleep 0.3
printf 'Reading package lists... 64%%\r'; sleep 0.3
echo "Reading package lists... Done""#),
        },
        Step {
            running: "Installing packages",
            done: "Packages installed",
            cmd: sh(r#"echo "Reading package lists... Done"; sleep 0.2
echo "Building dependency tree... Done"; sleep 0.2
echo "The following NEW packages will be installed: curl jq unzip"; sleep 0.4
for p in "curl 8.5.0 [227 kB]" "jq 1.7.1 [65.7 kB]" "unzip 6.0 [174 kB]"; do
  echo "Get: http://archive.example.org/ubuntu noble/main amd64 $p"; sleep 0.4
done
for pct in 10 25 40 55 70 85 100; do
  bar=$(printf '%*s' $((pct / 5)) '' | tr ' ' '#')
  printf 'Progress: [%3d%%] [%-20s]\r' "$pct" "$bar"; sleep 0.35
done
echo
for p in curl jq unzip; do
  echo "Unpacking $p ..."; sleep 0.3
  echo "Setting up $p ..."; sleep 0.3
done
echo "Processing triggers for man-db ...""#),
        },
        Step {
            running: "Enabling services",
            done: "Services enabled",
            // Coloured like `systemctl status`, to show a command's own colours
            // coming through the tail.
            cmd: sh(
                r#"echo "Created symlink multi-user.target.wants/demo.service -> demo.service"; sleep 0.5
printf '\033[0;1;32m●\033[0m demo.service - Demo service\n'; sleep 0.4
printf '     Active: \033[0;1;32mactive (running)\033[0m since just now\n'; sleep 0.6"#,
            ),
        },
    ]
}
#[cfg(test)]
mod tests {
    use super::*;

    /// console's colour switches are process-wide and tests run in
    /// parallel, so tests that set them take this lock.
    static COLOUR_SWITCHES: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Parameter lists of the SGR sequences in `text`, found with console's
    /// own ANSI iterator. The parameters are split here: no direct dependency
    /// parses SGR parameters, and anstyle-parse (in the tree through clap) is
    /// a VTE parser that would still leave the parameter walk to us.
    fn sgr(text: &str) -> Vec<Vec<u16>> {
        console::AnsiCodeIterator::new(text)
            .filter(|(_, is_code)| *is_code)
            .filter_map(|(code, _)| {
                code.strip_prefix("\x1b[")?
                    .strip_suffix('m')
                    .map(str::to_string)
            })
            .map(|p| p.split(';').filter_map(|n| n.parse().ok()).collect())
            .collect()
    }

    /// True when an SGR parameter list sets a named (16-colour) colour.
    /// `38;2;r;g;b` and `48;2;r;g;b` are skipped as whole groups, so a named
    /// colour mixed into the same sequence is still caught.
    fn sets_named_colour(params: &[u16]) -> bool {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                38 | 48 if params.get(i + 1) == Some(&2) => i += 5,
                38 | 48 if params.get(i + 1) == Some(&5) => i += 3,
                30..=37 | 40..=47 | 90..=97 | 100..=107 => return true,
                _ => i += 1,
            }
        }
        false
    }

    #[test]
    fn named_colour_check_catches_mixed_sequences() {
        assert!(sets_named_colour(&[38, 2, 1, 2, 3, 31]));
        assert!(!sets_named_colour(&[38, 2, 31, 32, 33]));
        assert!(!sets_named_colour(&[1]));
    }

    /// Truecolor is the design target (capability map, decision 10): the
    /// runner's own lines use palette RGB values, never named ANSI colours.
    #[test]
    fn status_lines_use_only_truecolor() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled_stderr(true);
        for outcome in [
            Outcome::Done,
            Outcome::Failed {
                code: 2,
                signal: None,
            },
            Outcome::Interrupted,
        ] {
            let line = step_line(false, &outcome, "Packages installed", "1s");
            let codes = sgr(&line);
            assert!(!codes.is_empty(), "{outcome:?} is unstyled: {line:?}");
            assert!(
                !codes.iter().any(|c| sets_named_colour(c)),
                "{outcome:?}: {codes:?}"
            );
        }
    }

    /// The status lines go to stderr, so their colour follows stderr's
    /// colour setting (console's for_stderr), not stdout's.
    #[test]
    fn status_line_colour_follows_stderr() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(true);
        assert!(!sgr(&step_line(false, &Outcome::Done, "x", "1s")).is_empty());
        console::set_colors_enabled(true);
        console::set_colors_enabled_stderr(false);
        assert!(sgr(&step_line(false, &Outcome::Done, "x", "1s")).is_empty());
    }

    /// Every gallery format is truecolor too, and none is empty.
    #[test]
    fn gallery_uses_only_truecolor() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled_stderr(true);
        let text = gallery::lines().join("\n");
        let codes = sgr(&text);
        assert!(codes.len() > 50, "gallery looks unstyled");
        assert!(
            !codes.iter().any(|c| sets_named_colour(c)),
            "named colour in the gallery"
        );
        for name in [
            "Current",
            "Dot leaders",
            "Step counter",
            "Badges",
            "Timeline",
            "Grouped",
            "Minimal",
            "Tree",
        ] {
            assert!(text.contains(name), "{name} missing");
        }
    }

    /// The tree formats: each tree is unbroken (every line after its root
    /// starts with a tree glyph or the indent under one), and every line
    /// carrying a time or exit code ends in the same column.
    #[test]
    fn tree_formats_are_connected_and_times_line_up() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled_stderr(false);
        let trees = gallery::tree_blocks();
        assert!(
            trees.len() >= 6,
            "expected a running and a failing tree per tree format"
        );
        for block in trees {
            let (root, rest) = block.split_first().expect("a root line");
            assert!(!root.trim().is_empty());
            for line in rest {
                let t = line.trim_start_matches(' ');
                assert!(
                    t.starts_with(['├', '└', '╰', '│']),
                    "loose line in tree: {line:?}\n{block:#?}"
                );
            }
            let ends: Vec<usize> = rest
                .iter()
                .filter(|l| l.ends_with('s') && l.contains(|c: char| c.is_ascii_digit()))
                .map(|l| console::measure_text_width(l))
                .collect();
            assert!(ends.len() >= 3, "{block:#?}");
            assert!(
                ends.windows(2).all(|w| w[0] == w[1]),
                "times don't line up: {ends:?}\n{block:#?}"
            );
        }
    }

    /// `--color=always` keeps colour when piped (the Saltbox installer
    /// script's case), `never` drops it, `auto` leaves console's detection.
    #[test]
    fn colour_flag_overrides_detection() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cli = Cli::try_parse_from(["proto-runner", "--color=always"]).expect("parses");
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
        apply_colour_choice(cli.color);
        assert!(console::colors_enabled() && console::colors_enabled_stderr());
        apply_colour_choice(clap::ColorChoice::Never);
        assert!(!console::colors_enabled() && !console::colors_enabled_stderr());
        console::set_colors_enabled_stderr(true);
        apply_colour_choice(clap::ColorChoice::Auto);
        assert!(
            console::colors_enabled_stderr(),
            "auto must not change detection"
        );
        let default = Cli::try_parse_from(["proto-runner"]).expect("parses");
        assert_eq!(default.color, clap::ColorChoice::Auto);
    }

    #[test]
    fn a_signalled_child_is_reported_by_signal() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
        let line = step_line(
            true,
            &Outcome::Failed {
                code: 1,
                signal: Some("Terminated".into()),
            },
            "Configuring service",
            "2s",
        );
        assert!(line.starts_with("╰── ✗ Configuring service "), "{line:?}");
        assert!(
            line.ends_with(" killed by signal (Terminated) · 2s"),
            "{line:?}"
        );
    }

    /// The live runner draws the rounded tree picked from the gallery:
    /// finished steps branch with ├──, the last with ╰──, and the right-hand
    /// text always ends in the same column as the running step's elapsed.
    #[test]
    fn step_lines_form_a_rounded_tree_with_aligned_times() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled_stderr(false);
        let lines = [
            step_line(false, &Outcome::Done, "System checked", "1s"),
            step_line(false, &Outcome::Done, "Packages installed", "1m 4s"),
            step_line(
                true,
                &Outcome::Failed {
                    code: 100,
                    signal: None,
                },
                "Configuring the firewall",
                "3s",
            ),
            step_line(true, &Outcome::Interrupted, "Waiting", "12s"),
        ];
        assert!(lines[0].starts_with("├── ✓ System checked"));
        assert!(lines[2].starts_with("╰── ✗ Configuring the firewall"));
        assert!(lines[2].ends_with("exit 100 · 3s"), "{}", lines[2]);
        assert!(lines[3].ends_with("interrupted · 12s"), "{}", lines[3]);
        for line in &lines {
            assert_eq!(console::measure_text_width(line), COLUMN, "{line:?}");
        }
    }

    /// The live tail hangs under the running (last) step: indented past its
    /// branch, ├── for each line and ╰── for the newest.
    #[test]
    fn tail_lines_branch_under_the_running_step() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled_stderr(false);
        let prefixes: Vec<String> = (0..3).map(|i| tail_prefix(i, 3)).collect();
        assert_eq!(prefixes, ["    ├── ", "    ├── ", "    ╰── "]);
        assert_eq!(tail_prefix(0, 1), "    ╰── ");
    }

    /// Command output keeps its own colours and is otherwise drawn in the
    /// terminal's default colour: the tail template styles nothing but the
    /// tree prefix (which is styled before it is set).
    #[test]
    fn tail_template_leaves_command_output_unstyled() {
        assert_eq!(tail_template(), "{prefix}{wide_msg}");
        ProgressStyle::with_template(&tail_template()).expect("valid template");
    }

    #[test]
    fn templates_use_palette_colours() {
        for template in [spinner_template()] {
            assert!(template.contains('#'), "{template}");
            for name in ["yellow", "green", "red", "cyan", "dim"] {
                assert!(!template.contains(name), "{template} uses {name}");
            }
            ProgressStyle::with_template(&template).expect("valid template");
        }
    }

    /// A character split across two reads must arrive whole.
    #[test]
    fn splitter_keeps_characters_split_across_reads() {
        let mut splitter = LineSplitter::default();
        let bytes = "café ✓\n".as_bytes();
        let cut = bytes.iter().position(|&b| b == 0xC3).expect("é") + 1;
        let mut lines = splitter.push(&bytes[..cut]);
        lines.extend(splitter.push(&bytes[cut..]));
        assert_eq!(lines, vec![("café ✓".to_string(), false)]);
    }

    #[test]
    fn splitter_marks_carriage_return_overwrites() {
        let mut splitter = LineSplitter::default();
        let mut lines = splitter.push(b"one\r\ntwo\n10%\r");
        lines.extend(splitter.push(b"20%\r"));
        // A \r at the end of a read may be half of \r\n, so it waits for
        // the next byte: here more text, so it was an overwrite.
        lines.extend(splitter.push(b"done"));
        lines.extend(splitter.finish());
        assert_eq!(
            lines,
            vec![
                ("one".to_string(), false),
                ("two".to_string(), false),
                ("10%".to_string(), true),
                ("20%".to_string(), true),
                ("done".to_string(), false),
            ]
        );
    }
}
