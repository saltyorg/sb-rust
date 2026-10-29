//! Prototype: can indicatif + portable-pty reproduce sb-go's task runner?
//! Spinner per step, live output tail under the running step, output tail on
//! failure, clean Ctrl-C, plain output when piped. Throwaway; see FINDINGS.md.

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
    /// Path to ansible-playbook for the realistic step.
    #[arg(long, default_value = "ansible-playbook")]
    ansible: String,
    /// Print static snapshots of a run in several formats, to compare them.
    #[arg(long)]
    gallery: bool,
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

    let mut steps = vec![
        Step {
            running: "Checking Ansible",
            done: "Ansible available",
            cmd: vec![cli.ansible.clone(), "--version".into()],
        },
        Step {
            running: "Installing packages",
            done: "Packages installed",
            cmd: sh(
                r#"for i in 1 2 3 4 5 6; do printf '\033[32mok\033[0m: [localhost] => package-%s\n' "$i"; sleep 0.4; done
                       for p in 10 40 70 100; do printf '\rdownloading %s%%' "$p"; sleep 0.3; done; echo"#,
            ),
        },
    ];
    if cli.slow {
        steps.push(Step {
            running: "Long task (press Ctrl-C)",
            done: "Long task finished",
            cmd: sh("trap 'echo got TERM; exit 143' TERM; for i in $(seq 1 30); do echo tick $i; sleep 1; done"),
        });
    }
    if cli.fail {
        steps.push(Step {
            running: "Configuring service",
            done: "Service configured",
            cmd: sh(r#"for i in $(seq 1 14); do echo "task line $i"; done; printf '\033[31mfatal\033[0m: [localhost]: FAILED! => unreachable\n'; exit 2"#),
        });
    }

    // indicatif decides whether to draw: its stderr target is hidden when
    // stderr isn't a terminal, or TERM is dumb or unset
    // (ProgressDrawTarget::term, indicatif 0.18.6 draw_target.rs). Our own
    // "live" flag follows that, so lines never go to a hidden target, where
    // MultiProgress::println does nothing.
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let live = !mp.is_hidden();
    for step in &steps {
        if let Err(e) = run_step(&mp, step, live, cli.verbose, &interrupted) {
            let signal = interrupted.load(Ordering::Relaxed);
            if signal != 0 {
                // 128 + the signal, the shell convention (130 for Ctrl-C, 143
                // for SIGTERM). An explicit exit, which signal-hook recommends
                // over re-raising the signal in a multi-threaded program
                // (low_level::emulate_default_handler).
                std::process::exit(128 + i32::try_from(signal).unwrap_or(0));
            }
            return Err(e);
        }
    }
    Ok(())
}

fn run_step(
    mp: &MultiProgress,
    step: &Step,
    live: bool,
    verbose: bool,
    interrupted: &AtomicUsize,
) -> Result<()> {
    let spinner = mp.add(ProgressBar::new_spinner());
    spinner.set_style(ProgressStyle::with_template(&spinner_template())?);
    spinner.set_message(step.running);
    spinner.enable_steady_tick(Duration::from_millis(80));
    if !live {
        eprintln!("- {}", step.running);
    }
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
    cmd.env("ANSIBLE_FORCE_COLOR", "1");
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
                    if live {
                        mp.println(format!("    {line}"))?;
                    } else if console::colors_enabled() {
                        println!("    {line}");
                    } else {
                        println!("    {}", console::strip_ansi_codes(&line));
                    }
                }
                if history.len() == FAILURE_TAIL {
                    history.pop_front();
                }
                history.push_back(line);
                let start = history.len().saturating_sub(TAIL_LINES);
                for (bar, line) in tail_bars.iter().zip(history.iter().skip(start)) {
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
    let emit = |text: String| -> Result<()> {
        if live {
            mp.println(text)?;
        } else if console::colors_enabled_stderr() {
            eprintln!("{text}");
        } else {
            // Child output keeps its colours under the PTY; strip them when
            // console says stderr gets no colour (not a terminal, or
            // CLICOLOR=0): https://docs.rs/console/0.16.6/console/fn.strip_ansi_codes.html
            eprintln!("{}", console::strip_ansi_codes(&text));
        }
        Ok(())
    };
    if interrupted.load(Ordering::Relaxed) != 0 {
        emit(status_line(&Outcome::Interrupted, step.running, &elapsed))?;
        bail!("interrupted");
    }
    if status.success() {
        emit(status_line(&Outcome::Done, step.done, &elapsed))?;
        return Ok(());
    }
    emit(status_line(
        &Outcome::Failed {
            code: status.exit_code(),
            signal: status.signal().map(str::to_string),
        },
        step.running,
        &elapsed,
    ))?;
    for line in &history {
        emit(format!("    {line}"))?;
    }
    bail!("step failed: {}", step.running)
}

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

/// indicatif passes a template's style part to the same parser
/// (indicatif 0.18.6 style.rs).
fn spinner_template() -> String {
    format!(
        "{{spinner:.{a}}} {{msg:.{a}}} {{elapsed:.{m}}}",
        a = theme::ACCENT,
        m = theme::MUTED
    )
}

/// The live output tail under a running step. Child output keeps its own
/// colours inside; this only tones down uncoloured text.
fn tail_template() -> String {
    format!("    {{wide_msg:.{}}}", theme::MUTED)
}

/// The permanent line a finished step leaves behind.
fn status_line(outcome: &Outcome, text: &str, elapsed: &str) -> String {
    match outcome {
        Outcome::Done => format!(
            "{} {}",
            palette(theme::OK).apply_to(format!("✓ {text}")),
            palette(theme::MUTED).apply_to(elapsed)
        ),
        // portable-pty reports a signalled child as code 1 plus the signal
        // name (portable-pty 0.9.0 lib.rs, ExitStatus).
        Outcome::Failed {
            signal: Some(signal),
            ..
        } => palette(theme::PROBLEM)
            .apply_to(format!("✗ {text}: killed by signal ({signal})"))
            .to_string(),
        Outcome::Failed { code, .. } => palette(theme::PROBLEM)
            .apply_to(format!("✗ {text}: failed (exit {code})"))
            .to_string(),
        Outcome::Interrupted => palette(theme::WARN)
            .apply_to(format!("✗ {text}: interrupted"))
            .to_string(),
    }
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
            let line = status_line(&outcome, "Packages installed", "1s");
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
        assert!(!sgr(&status_line(&Outcome::Done, "x", "1s")).is_empty());
        console::set_colors_enabled(true);
        console::set_colors_enabled_stderr(false);
        assert!(sgr(&status_line(&Outcome::Done, "x", "1s")).is_empty());
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

    #[test]
    fn a_signalled_child_is_reported_by_signal() {
        let _lock = COLOUR_SWITCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
        let line = status_line(
            &Outcome::Failed {
                code: 1,
                signal: Some("Terminated".into()),
            },
            "Configuring service",
            "2s",
        );
        assert_eq!(line, "✗ Configuring service: killed by signal (Terminated)");
    }

    #[test]
    fn templates_use_palette_colours() {
        for template in [spinner_template(), tail_template()] {
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
