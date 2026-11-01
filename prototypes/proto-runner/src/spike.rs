//! term-ui Task 1: can the runner draw nested and concurrent async steps
//! without blinking? (term-ui spec, decisions 2-4.)
//!
//! One live tree model, drawn two ways:
//! - `pool`: a fixed pool of indicatif bars created once and reused, with
//!   the cursor-moving redraw (`MultiProgress::set_move_cursor`), which
//!   proto-runner measured at 0 line erases with a constant bar count;
//! - `single`: one bar whose message is the whole live area, several lines.
//!
//! Finished steps are printed for good once their place in the tree is
//! known (decision 3: the newest finished line stays live until the next
//! step starts or its parent ends); a subtree stays live until its parent
//! ends. Steps are async on tokio (decision 4); the frames are drawn by a
//! tokio interval task, the bars by indicatif.
//!
//! Why not indicatif's tree examples (examples/multi-tree*.rs): each node is
//! its own bar inserted as it starts, which changes the bar count and so
//! rules out the cursor-moving redraw (its docs: the number of bars must not
//! change). Tree prefixes come from termtree's glyphs drawn line by line, as
//! main.rs's `glyphs` explains (termtree renders only finished trees).
//!
//! `SPIKE_STYLE` picks design options, comma-separated `key=value`:
//! `notice=label|bullet|branch|guide|child|inline`, `tail=muted|faint|last`,
//! `spin=braille|dots|arc`, `conc=full|compact`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use super::{COLUMN, faint, glyphs, palette, theme};

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum Shape {
    /// Three steps, one after another, with notices.
    Seq,
    /// A step with sub-steps, one of them with its own sub-steps.
    Nested,
    /// A step whose two sub-steps run at the same time.
    Concurrent,
    /// Everything in one short run, for recordings: both kinds of notice,
    /// output, two steps in parallel, a final step.
    Tour,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum Strategy {
    /// A fixed pool of bars, reused every frame.
    Pool,
    /// One bar holding the whole live area.
    Single,
}

/// Bars in the pool: enough for the widest live area these shapes draw.
const POOL: usize = 14;
/// Output lines kept per step: sb-go's 8 (`liveTaskOutputLines`), the
/// reviewed proto-runner tail. How many show is capped in `tail_shown`.
const TAIL: usize = 8;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DOTS: [&str; 4] = ["∙∙∙", "●∙∙", "∙●∙", "∙∙●"];
const ARC: [&str; 6] = ["◜", "◠", "◝", "◞", "◡", "◟"];

/// A design option from `SPIKE_STYLE` (`key=value,…`), or `default`.
fn style(key: &str, default: &'static str) -> String {
    std::env::var("SPIKE_STYLE")
        .ok()
        .and_then(|all| {
            all.split(',').find_map(|kv| {
                kv.split_once('=')
                    .filter(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            })
        })
        .unwrap_or_else(|| default.to_string())
}

fn spinner(frame: usize) -> &'static str {
    match style("spin", "braille").as_str() {
        "dots" => DOTS[(frame / 2) % DOTS.len()],
        "arc" => ARC[frame % ARC.len()],
        _ => SPINNER[frame % SPINNER.len()],
    }
}

#[derive(Clone, PartialEq)]
enum Status {
    Running,
    Done,
    Failed(String),
    /// Stopped because a sibling failed or Ctrl-C arrived.
    Stopped(&'static str),
}

struct Node {
    parent: Option<usize>,
    depth: usize,
    running: String,
    done: String,
    status: Status,
    started: Instant,
    elapsed: Option<Duration>,
    tail: VecDeque<String>,
    notices: Vec<(bool, String)>,
}

#[derive(Default)]
struct Tree {
    nodes: Vec<Node>,
    printed: usize,
    ended: bool,
}

type Shared = Arc<Mutex<Tree>>;

fn lock(tree: &Shared) -> std::sync::MutexGuard<'_, Tree> {
    tree.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Tree {
    fn add(&mut self, parent: Option<usize>, running: &str, done: &str) -> usize {
        let depth = parent.map_or(0, |p| self.nodes[p].depth + 1);
        self.nodes.push(Node {
            parent,
            depth,
            running: running.into(),
            done: done.into(),
            status: Status::Running,
            started: Instant::now(),
            elapsed: None,
            tail: VecDeque::new(),
            notices: Vec::new(),
        });
        self.nodes.len() - 1
    }

    fn finish(&mut self, id: usize, status: Status) {
        let node = &mut self.nodes[id];
        if node.status == Status::Running {
            node.status = status;
            node.elapsed = Some(node.started.elapsed());
        }
    }

    /// Whether node `id` is the last child of its parent, once known: a
    /// later sibling says no; a finished parent (or the run's end, at the
    /// top) says yes.
    fn last(&self, id: usize) -> Option<bool> {
        let parent = self.nodes[id].parent;
        if self.nodes[id + 1..].iter().any(|n| n.parent == parent) {
            return Some(false);
        }
        let parent_done = match parent {
            Some(p) => self.nodes[p].status != Status::Running,
            None => self.ended,
        };
        parent_done.then_some(true)
    }

    /// The prefix in front of node `id`'s branch: one column per ancestor,
    /// "│   " under an ancestor that isn't last, spaces under one that is.
    fn indent(&self, id: usize) -> String {
        let mut columns = Vec::new();
        let mut at = self.nodes[id].parent;
        while let Some(p) = at {
            let g = glyphs();
            columns.push(match self.last(p) {
                Some(true) => format!("{}{}", g.last_skip, g.skip_indent),
                _ => format!("{}{}", g.middle_skip, g.skip_indent),
            });
            at = self.nodes[p].parent;
        }
        columns.reverse();
        columns.concat()
    }

    fn line(&self, id: usize, frame: usize) -> String {
        let g = glyphs();
        let node = &self.nodes[id];
        let branch = match self.last(id) {
            Some(true) => format!("{}{}", g.last_item, g.item_indent),
            _ => format!("{}{}", g.middle_item, g.item_indent),
        };
        let lead = format!("{}{}", self.indent(id), branch);
        let elapsed = format!(
            "{}s",
            node.elapsed
                .unwrap_or_else(|| node.started.elapsed())
                .as_secs()
        );
        let (left, right) = match &node.status {
            Status::Running => (
                palette(theme::ACCENT)
                    .apply_to(format!("{} {}", spinner(frame), node.running))
                    .to_string(),
                palette(theme::MUTED).apply_to(elapsed).to_string(),
            ),
            Status::Done => (
                {
                    let done = palette(theme::OK)
                        .apply_to(format!("✓ {}", node.done))
                        .to_string();
                    // notice=inline: the first info notice follows the done label.
                    match node.notices.iter().find(|(warn, _)| !warn) {
                        Some((_, text)) if style("notice", "label") == "inline" => {
                            format!(
                                "{done} {}",
                                palette(theme::MUTED).apply_to(format!("· {text}"))
                            )
                        }
                        _ => done,
                    }
                },
                palette(theme::MUTED).apply_to(elapsed).to_string(),
            ),
            Status::Failed(why) => (
                palette(theme::PROBLEM)
                    .apply_to(format!("✗ {}", node.running))
                    .to_string(),
                palette(theme::PROBLEM)
                    .apply_to(format!("{why} · {elapsed}"))
                    .to_string(),
            ),
            Status::Stopped(why) => (
                palette(theme::WARN)
                    .apply_to(format!("✗ {}", node.running))
                    .to_string(),
                palette(theme::WARN)
                    .apply_to(format!("{why} · {elapsed}"))
                    .to_string(),
            ),
        };
        let room = COLUMN.saturating_sub(
            console::measure_text_width(&lead) + console::measure_text_width(&right) + 1,
        );
        format!(
            "{}{} {right}",
            faint(&lead),
            console::pad_str(&left, room, console::Alignment::Left, None)
        )
    }

    /// Lines hanging under node `id`: its notices, and while it runs (or if
    /// it failed) its newest output lines.
    fn under(&self, id: usize) -> Vec<String> {
        let node = &self.nodes[id];
        let g = glyphs();
        let child_lead = format!(
            "{}{}",
            self.indent(id),
            match self.last(id) {
                Some(true) => format!("{}{}", g.last_skip, g.skip_indent),
                _ => format!("{}{}", g.middle_skip, g.skip_indent),
            }
        );
        let mut tail_lines = Vec::new();
        // The spec keeps an interrupted step's last lines too.
        if matches!(
            node.status,
            Status::Running | Status::Failed(_) | Status::Stopped("interrupted")
        ) {
            let tail = style("tail", "muted");
            let colour = if tail == "faint" {
                theme::FAINT
            } else {
                theme::MUTED
            };
            let skip = if tail == "last" && node.status == Status::Running {
                node.tail.len().saturating_sub(1)
            } else {
                // Only running steps share the height; a finished one keeps its tail.
                node.tail
                    .len()
                    .saturating_sub(if node.status == Status::Running {
                        self.tail_shown()
                    } else {
                        TAIL
                    })
            };
            tail_lines.extend(
                node.tail
                    .iter()
                    .skip(skip)
                    .map(|l| format!("{}{}", faint(&child_lead), palette(colour).apply_to(l))),
            );
        }
        // A notice closes the branch only when nothing follows it under
        // the step: no output lines and no sub-steps.
        let followed = !tail_lines.is_empty() || self.nodes.iter().any(|n| n.parent == Some(id));
        let notice = style("notice", "bullet");
        let shown: Vec<&(bool, String)> = node
            .notices
            .iter()
            .filter(|(warn, _)| *warn || notice != "inline" || node.status != Status::Done)
            .collect();
        let count = shown.len();
        let mut lines: Vec<String> = shown
            .into_iter()
            .enumerate()
            .map(|(i, (warn, text))| {
                let last = i + 1 == count && !followed;
                // The mark: label words, or bullets (› info, orange ! warning).
                let mark = match (notice.as_str(), warn) {
                    ("label", true) => {
                        format!("{} {text}", palette(theme::WARN).apply_to("warning:"))
                    }
                    ("label", false) => {
                        format!("{} {text}", palette(theme::MUTED).apply_to("info:"))
                    }
                    (_, true) => format!("{} {text}", palette(theme::WARN).apply_to("!")),
                    (_, false) => palette(theme::MUTED)
                        .apply_to(format!("› {text}"))
                        .to_string(),
                };
                // What ties the notice to its step.
                let tie = match notice.as_str() {
                    "branch" => if last { "╰╴" } else { "├╴" }.to_string(),
                    "guide" => "┆ ".to_string(),
                    "child" => if last { "╰── " } else { "├── " }.to_string(),
                    _ => String::new(),
                };
                format!("{}{mark}", faint(format!("{child_lead}{tie}")))
            })
            .collect();
        lines.extend(tail_lines);
        lines
    }

    /// Output lines shown per running step: up to 8, with all running
    /// steps' tails together within half the terminal's height (the
    /// reviewed proto-runner rule, shared between parallel steps).
    fn tail_shown(&self) -> usize {
        // The reviewed proto-runner rule: the terminal's height, else 24.
        let rows = console::Term::stderr()
            .size_checked()
            .map_or(24, |(rows, _)| usize::from(rows));
        let running = self
            .nodes
            .iter()
            .filter(|n| n.status == Status::Running && !n.tail.is_empty())
            .count()
            .max(1);
        (rows / 2 / running).clamp(1, TAIL)
    }

    /// Whether node `id` and everything under it are finished.
    fn settled(&self, id: usize) -> bool {
        self.nodes[id].status != Status::Running
            && self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.parent == Some(id))
                .all(|(c, _)| self.settled(c))
    }

    /// Takes the lines that can be printed for good: in start order, each
    /// settled node whose place (middle or last) is known.
    fn take_printable(&mut self, frame: usize) -> Vec<String> {
        let mut out = Vec::new();
        while self.printed < self.nodes.len() {
            let id = self.printed;
            // A parent prints with its subtree, once all of it is settled.
            let top_settled = {
                let mut root = id;
                while let Some(p) = self.nodes[root].parent {
                    if p < self.printed {
                        break;
                    }
                    root = p;
                }
                self.settled(root)
            };
            if !top_settled || self.last(id).is_none() {
                break;
            }
            out.push(self.line(id, frame));
            out.extend(self.under(id));
            self.printed += 1;
        }
        out
    }

    fn live(&self, frame: usize) -> Vec<String> {
        let compact = style("conc", "full") == "compact";
        let mut out = Vec::new();
        for id in self.printed..self.nodes.len() {
            let parent_running = self.nodes[id]
                .parent
                .is_some_and(|p| self.nodes[p].status == Status::Running);
            // conc=compact: a running parent's sub-steps stay hidden (failed
            // ones aside) while it runs; its line counts them instead.
            if compact && parent_running && !matches!(self.nodes[id].status, Status::Failed(_)) {
                continue;
            }
            let mut line = self.line(id, frame);
            if compact && self.nodes[id].status == Status::Running {
                let running = self
                    .nodes
                    .iter()
                    .filter(|n| n.parent == Some(id) && n.status == Status::Running)
                    .count();
                if running > 0 {
                    line = format!(
                        "{line} {}",
                        palette(theme::MUTED).apply_to(format!("· {running} running"))
                    );
                }
            }
            out.push(line);
            out.extend(self.under(id));
        }
        out
    }
}

/// How the live area reaches the terminal.
enum Area {
    Pool(MultiProgress, Vec<ProgressBar>),
    Single(MultiProgress, ProgressBar, std::cell::Cell<usize>),
}

/// indicatif pads each line to the terminal's width in its cursor-moving
/// redraw (indicatif 0.18.6 draw_target.rs `draw_to_term`), but it drops
/// empty lines (style.rs pushes only non-empty ones), so a blank line would
/// shrink the area, and that redraw never clears lines that fall off the
/// bottom (`MultiProgress::set_move_cursor`: the number of lines must not
/// change). A blank line is therefore one space, and the area never shrinks
/// while running.
fn line_or_blank(line: Option<&String>) -> String {
    line.filter(|l| !l.is_empty())
        .cloned()
        .unwrap_or_else(|| " ".to_string())
}

impl Area {
    fn new(strategy: Strategy) -> Self {
        let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
        let style = ProgressStyle::with_template("{msg}").expect("template");
        match strategy {
            Strategy::Pool => {
                mp.set_move_cursor(true);
                let bars = (0..POOL)
                    .map(|_| mp.add(ProgressBar::new_spinner().with_style(style.clone())))
                    .collect();
                Area::Pool(mp, bars)
            }
            Strategy::Single => {
                mp.set_move_cursor(true);
                let bar = mp.add(ProgressBar::new_spinner().with_style(style));
                Area::Single(mp, bar, std::cell::Cell::new(0))
            }
        }
    }

    fn mp(&self) -> &MultiProgress {
        match self {
            Area::Pool(mp, _) | Area::Single(mp, _, _) => mp,
        }
    }

    fn draw(&self, printed: &[String], live: &[String]) {
        for line in printed {
            let _ = self.mp().println(line);
        }
        match self {
            Area::Pool(_, bars) => {
                for (i, bar) in bars.iter().enumerate() {
                    bar.set_message(line_or_blank(live.get(i)));
                }
            }
            // The area never shrinks while running: blank padded lines keep
            // its height at the tallest it has been.
            Area::Single(_, bar, tallest) => {
                tallest.set(tallest.get().max(live.len()));
                let lines: Vec<String> = (0..tallest.get())
                    .map(|i| line_or_blank(live.get(i)))
                    .collect();
                bar.set_message(lines.join("\n"));
            }
        }
    }

    fn clear(self) {
        // SPIKE_CLEAR picks how the live area is cleared at the end:
        // off (proto-runner: cursor-moving redraw off for the final clear),
        // keep (leave it on), blank (empty every line, then clear).
        let how = std::env::var("SPIKE_CLEAR").unwrap_or_else(|_| "off".into());
        if how == "blank" {
            self.draw(&[], &[]);
        }
        if how == "keep" || how == "blank" {
            match self {
                Area::Pool(_, bars) => bars.into_iter().for_each(|b| b.finish_and_clear()),
                Area::Single(_, bar, _) => bar.finish_and_clear(),
            }
            return;
        }
        match self {
            Area::Pool(mp, bars) => {
                mp.set_move_cursor(false);
                for bar in bars {
                    bar.finish_and_clear();
                }
            }
            Area::Single(mp, bar, _) => {
                mp.set_move_cursor(false);
                bar.finish_and_clear();
            }
        }
    }
}

/// One step's simulated work: output lines every `pace`, `lines` of them,
/// then success, or with `fail` a failure after two lines (early, so a
/// longer sibling is still running and gets stopped). Stops early on `stop`.
async fn work(
    tree: &Shared,
    id: usize,
    lines: usize,
    pace: Duration,
    fail: bool,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<(), ()> {
    let lines = if fail { lines.min(2) } else { lines };
    for n in 1..=lines {
        // https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled
        if stop
            .run_until_cancelled(tokio::time::sleep(pace))
            .await
            .is_none()
        {
            return Err(());
        }
        let mut t = lock(tree);
        let node = &mut t.nodes[id];
        node.tail.push_back(format!("checking item {n} of {lines}"));
        while node.tail.len() > TAIL {
            node.tail.pop_front();
        }
    }
    let mut t = lock(tree);
    if fail {
        t.finish(id, Status::Failed("invalid token".into()));
        Err(())
    } else {
        t.finish(id, Status::Done);
        Ok(())
    }
}

fn step(tree: &Shared, parent: Option<usize>, running: &str, done: &str) -> usize {
    lock(tree).add(parent, running, done)
}

async fn scenario(
    shape: Shape,
    fail: bool,
    tree: Shared,
    stop: tokio_util::sync::CancellationToken,
) -> Result<(), ()> {
    let ms = Duration::from_millis;
    let a = step(&tree, None, "Reading accounts.yml", "accounts.yml read");
    work(&tree, a, 4, ms(250), false, &stop).await?;
    lock(&tree).nodes[a]
        .notices
        .push((false, "3 accounts".into()));
    match shape {
        Shape::Tour => {
            let b = step(&tree, None, "Checking settings.yml", "settings.yml checked");
            lock(&tree).nodes[b]
                .notices
                .push((true, "unknown key: downloads.extra".into()));
            work(&tree, b, 11, ms(180), false, &stop).await?;
            let c = step(&tree, None, "Checking credentials", "credentials valid");
            let cf = step(
                &tree,
                Some(c),
                "Checking the Cloudflare token",
                "Cloudflare token valid",
            );
            let dh = step(
                &tree,
                Some(c),
                "Checking the Docker Hub login",
                "Docker Hub login valid",
            );
            // As sb-go's errgroup: the first failure stops the other sibling.
            let group = stop.child_token();
            let (t1, t2) = (tree.clone(), tree.clone());
            let (g1, g2) = (group.clone(), group.clone());
            let a1 = tokio::spawn(async move {
                let r = work(&t1, cf, 14, ms(180), fail, &g1).await;
                if r.is_err() {
                    g1.cancel();
                }
                r
            });
            let a2 = tokio::spawn(async move {
                let r = work(&t2, dh, 9, ms(180), false, &g2).await;
                if r.is_err() {
                    g2.cancel();
                }
                r
            });
            let (r1, r2) = (a1.await.unwrap_or(Err(())), a2.await.unwrap_or(Err(())));
            let (sibling, parent) = if stop.is_cancelled() {
                ("interrupted", "interrupted")
            } else {
                ("stopped", "sub-step failed")
            };
            let mut t = lock(&tree);
            for id in [cf, dh] {
                t.finish(id, Status::Stopped(sibling));
            }
            t.finish(
                c,
                if r1.is_ok() && r2.is_ok() {
                    Status::Done
                } else {
                    Status::Stopped(parent)
                },
            );
            drop(t);
            r1.and(r2)?;
        }
        Shape::Seq => {
            let b = step(&tree, None, "Checking settings.yml", "settings.yml checked");
            lock(&tree).nodes[b]
                .notices
                .push((true, "unknown key: downloads.extra".into()));
            work(&tree, b, 6, ms(250), fail, &stop).await?;
        }
        Shape::Nested => {
            let v = step(&tree, None, "Validating config files", "config files valid");
            let s = step(
                &tree,
                Some(v),
                "Checking settings.yml",
                "settings.yml checked",
            );
            let schema = step(&tree, Some(s), "Checking the schema", "schema matches");
            work(&tree, schema, 4, ms(250), false, &stop).await?;
            let rules = step(&tree, Some(s), "Checking custom rules", "custom rules pass");
            let r = work(&tree, rules, 4, ms(250), fail, &stop).await;
            let why = if stop.is_cancelled() {
                "interrupted"
            } else {
                "sub-step failed"
            };
            lock(&tree).finish(
                s,
                if r.is_ok() {
                    Status::Done
                } else {
                    Status::Stopped(why)
                },
            );
            if r.is_err() {
                let why = if stop.is_cancelled() {
                    "interrupted"
                } else {
                    "sub-step failed"
                };
                lock(&tree).finish(v, Status::Stopped(why));
            }
            r?;
            let adv = step(
                &tree,
                Some(v),
                "Checking adv_settings.yml",
                "adv_settings.yml checked",
            );
            work(&tree, adv, 4, ms(250), false, &stop).await?;
            lock(&tree).finish(v, Status::Done);
        }
        Shape::Concurrent => {
            let c = step(&tree, None, "Checking credentials", "credentials valid");
            let cf = step(
                &tree,
                Some(c),
                "Checking the Cloudflare token",
                "Cloudflare token valid",
            );
            let dh = step(
                &tree,
                Some(c),
                "Checking the Docker Hub login",
                "Docker Hub login valid",
            );
            // As sb-go's errgroup: the first failure stops the other sibling.
            let group = stop.child_token();
            let (t1, t2) = (tree.clone(), tree.clone());
            let (g1, g2) = (group.clone(), group.clone());
            let cf_task = tokio::spawn(async move {
                let r = work(&t1, cf, 10, ms(250), fail, &g1).await;
                if r.is_err() {
                    g1.cancel();
                }
                r
            });
            let dh_task = tokio::spawn(async move {
                let r = work(&t2, dh, 5, ms(250), false, &g2).await;
                if r.is_err() {
                    g2.cancel();
                }
                r
            });
            let (r1, r2) = (
                cf_task.await.unwrap_or(Err(())),
                dh_task.await.unwrap_or(Err(())),
            );
            {
                let mut t = lock(&tree);
                let (sibling, parent) = if stop.is_cancelled() {
                    ("interrupted", "interrupted")
                } else {
                    ("stopped", "sub-step failed")
                };
                for id in [cf, dh] {
                    t.finish(id, Status::Stopped(sibling));
                }
                t.finish(
                    c,
                    if r1.is_ok() && r2.is_ok() {
                        Status::Done
                    } else {
                        Status::Stopped(parent)
                    },
                );
            }
            r1.and(r2)?;
        }
    }
    let z = step(&tree, None, "Writing the report", "report written");
    work(&tree, z, 3, ms(250), false, &stop).await
}

/// Runs a spike scenario; erases are counted from a `script` recording
/// (`grep -o $'\e\[2K' | wc -l`).
pub(crate) fn run(shape: Shape, strategy: Strategy, fail: bool) -> Result<()> {
    // Without this, a Ctrl-C keypress echoes ^C after a full-width line and
    // shifts the redraw by a row (proto-runner's guard, main.rs).
    let quiet = console::Term::stderr()
        .is_term()
        .then(super::QuietControlEcho::new)
        .flatten();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_time()
        .enable_io()
        .build()?;
    runtime.block_on(async move {
        let tree: Shared = Arc::default();
        let stop = tokio_util::sync::CancellationToken::new();
        let _ = writeln_stderr(
            &palette(theme::TEXT)
                .apply_to("sb validate-config")
                .to_string(),
        );
        let area = Area::new(strategy);
        let work = tokio::spawn(scenario(shape, fail, tree.clone(), stop.clone()));
        let ctrl_c = {
            let stop = stop.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    stop.cancel();
                }
            })
        };
        let mut tick = tokio::time::interval(Duration::from_millis(80));
        // A redraw loop skips frames it missed rather than bursting them.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut frame = 0;
        loop {
            tick.tick().await;
            frame += 1;
            let (printed, live) = {
                let mut t = lock(&tree);
                if work.is_finished() {
                    let interrupted = stop.is_cancelled();
                    for id in 0..t.nodes.len() {
                        t.finish(
                            id,
                            Status::Stopped(if interrupted {
                                "interrupted"
                            } else {
                                "stopped"
                            }),
                        );
                    }
                    t.ended = true;
                }
                (t.take_printable(frame), t.live(frame))
            };
            area.draw(&printed, &live);
            if lock(&tree).ended {
                break;
            }
        }
        ctrl_c.abort();
        area.clear();
        // The spec's codes: 130 after Ctrl-C, 1 after a failure.
        let code = match (stop.is_cancelled(), work.await) {
            (true, _) => 130,
            (false, Ok(Ok(()))) => 0,
            _ => 1,
        };
        drop(quiet);
        if code != 0 {
            std::process::exit(code);
        }
        Ok(())
    })
}

fn writeln_stderr(line: &str) -> std::io::Result<()> {
    console::Term::stderr().write_line(line)
}
