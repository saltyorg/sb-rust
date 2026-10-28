//! Prototype: can indicatif + portable-pty reproduce sb-go's task runner?
//! Spinner per step, live output tail under the running step, output tail on
//! failure, clean Ctrl-C, plain output when piped. Throwaway; see FINDINGS.md.

use std::collections::VecDeque;
use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
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
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))?;

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

    let tty = std::io::stderr().is_terminal();
    let mp = MultiProgress::with_draw_target(if tty {
        ProgressDrawTarget::stderr()
    } else {
        ProgressDrawTarget::hidden()
    });
    for step in &steps {
        run_step(&mp, step, tty, cli.verbose, &interrupted)?;
    }
    Ok(())
}

fn run_step(
    mp: &MultiProgress,
    step: &Step,
    tty: bool,
    verbose: bool,
    interrupted: &AtomicBool,
) -> Result<()> {
    let spinner = mp.add(ProgressBar::new_spinner());
    spinner.set_style(ProgressStyle::with_template(
        "{spinner:.yellow} {msg:.yellow} {elapsed:.dim}",
    )?);
    spinner.set_message(step.running);
    spinner.enable_steady_tick(Duration::from_millis(80));
    if !tty {
        eprintln!("- {}", step.running);
    }
    let tail_bars: Vec<ProgressBar> = if tty && !verbose {
        (0..TAIL_LINES)
            .map(|_| {
                let b = mp.add(ProgressBar::new_spinner());
                b.set_style(
                    ProgressStyle::with_template("    {wide_msg:.dim}").expect("static template"),
                );
                b
            })
            .collect()
    } else {
        Vec::new()
    };

    let cols =
        rustix::termios::tcgetwinsize(rustix::stdio::stderr()).map_or(80, |w| w.ws_col.max(40));
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
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
        let mut pending = String::new();
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.push_str(&String::from_utf8_lossy(&buf[..n]));
            while let Some(i) = pending.find(['\n', '\r']) {
                let bare_cr = pending.as_bytes()[i] == b'\r';
                if bare_cr && i + 1 == pending.len() {
                    break; // wait: this \r may be the first half of \r\n
                }
                let crlf = bare_cr && pending.as_bytes()[i + 1] == b'\n';
                let end = if crlf { i + 2 } else { i + 1 };
                let seg: String = pending.drain(..end).collect();
                let text = seg.trim_end_matches(['\n', '\r']).to_string();
                if !text.is_empty() && tx.send((text, bare_cr && !crlf)).is_err() {
                    return;
                }
            }
        }
        if !pending.is_empty() {
            let _ = tx.send((pending.trim_end_matches(['\n', '\r']).to_string(), false));
        }
    });

    let mut history: VecDeque<String> = VecDeque::with_capacity(FAILURE_TAIL);
    let mut last_overwrites = false;
    let mut killed_at: Option<Instant> = None;
    let status = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((line, overwrite)) => {
                if last_overwrites {
                    history.pop_back();
                }
                last_overwrites = overwrite;
                if verbose && !overwrite {
                    if tty {
                        mp.println(format!("    {line}"))?;
                    } else {
                        println!("    {line}");
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
        if interrupted.load(Ordering::Relaxed) {
            match killed_at {
                None => {
                    if let Some(pgid) = pid.and_then(|p| Pid::from_raw(p as i32)) {
                        let _ = kill_process_group(pgid, Signal::TERM);
                    }
                    killed_at = Some(Instant::now());
                }
                Some(t) if t.elapsed() > Duration::from_secs(3) => {
                    child.kill()?;
                }
                Some(_) => {}
            }
        }
        if let Some(status) = child.try_wait()? {
            // Drain whatever the reader already queued before reporting.
            while let Ok((line, _)) = rx.try_recv() {
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
    // live bars. Leaving finished bars in the MultiProgress lost them once
    // their handles dropped and later redraws overwrote their lines.
    let elapsed = format!("{:.0?}", spinner.elapsed());
    spinner.finish_and_clear();
    let emit = |text: String| -> Result<()> {
        if tty {
            mp.println(text)?;
        } else {
            eprintln!("{text}");
        }
        Ok(())
    };
    if interrupted.load(Ordering::Relaxed) {
        emit(format!(
            "{}",
            console::style(format!("✗ {}: interrupted", step.running)).red()
        ))?;
        bail!("interrupted");
    }
    if status.success() {
        emit(format!(
            "{} {}",
            console::style(format!("✓ {}", step.done)).green(),
            console::style(elapsed).dim()
        ))?;
        return Ok(());
    }
    emit(format!(
        "{}",
        console::style(format!(
            "✗ {}: failed (exit {})",
            step.running,
            status.exit_code()
        ))
        .red()
    ))?;
    for line in &history {
        emit(format!("    {line}"))?;
    }
    bail!("step failed: {}", step.running)
}
