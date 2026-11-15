//! `sb logs` through the built binary (tui spec, Testing Strategy, "End
//! to end"): a stand-in `systemctl` first on `PATH`, crafted journal
//! files through `SB_JOURNAL_DIR`, the run log in the test's directory.
//! Runs before the screen are piped; the screen runs on a PTY
//! (portable-pty), read through vt100: the picker's rows, Enter opens a
//! service's log, an appended entry appears while following, Esc back to
//! the picker, `q` exits 0, Ctrl-C exits 130, the terminal restored
//! (alternate screen left, mouse off, the PTY's termios read back in
//! canonical mode with echo), and no log content in the run log.

mod common;
mod logs_pty;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::prelude::*;
use logs_pty::{Pty, World};
use sb_docker::testing::{FakeEngine, Options};
use sb_host::journal::testing::crafted::{
    Crafted, D_ENTRY_ARRAY, D_ENTRY_OFFSET, D_N_ENTRIES, H_ARENA_SIZE, H_ENTRY_ARRAY_OFFSET,
    H_N_ENTRIES,
};

/// The unit whose log the tests open.
const APP: &str = "saltbox_managed_app";

/// A string the run log must never hold: it is only in a log's content.
const MARKER: &str = "content-marker-7f3a";

/// `list-units`: two Saltbox units and docker; another unit the default
/// filters leave out.
const UNITS: &str = r#"[
{"unit":"saltbox_managed_app.service","load":"loaded","active":"active","sub":"running","description":"app"},
{"unit":"saltbox_managed_backup.service","load":"loaded","active":"failed","sub":"failed","description":"backup"},
{"unit":"docker.service","load":"loaded","active":"active","sub":"running","description":"Docker"},
{"unit":"ssh.service","load":"loaded","active":"active","sub":"running","description":"ssh"}
]"#;

const UNIT_FILES: &str = r#"[
{"unit_file":"saltbox_managed_app.service","state":"enabled","vendor_preset":null},
{"unit_file":"saltbox_managed_backup.service","state":"enabled","vendor_preset":null},
{"unit_file":"docker.service","state":"enabled","vendor_preset":null}
]"#;

/// A world's journal files.
trait Journal {
    fn write_journal(&self);
    fn append(&self);
}

impl Journal for World {
    /// `APP`'s journal: three entries of its own and one of another unit.
    fn write_journal(&self) {
        let unit = format!("_SYSTEMD_UNIT={APP}.service");
        let marker = format!("MESSAGE=a line with {MARKER}");
        let entries: [&[&[u8]]; 4] = [
            &[
                b"MESSAGE=hello from app",
                unit.as_bytes(),
                b"SYSLOG_IDENTIFIER=app",
                b"SYSLOG_PID=42",
            ],
            &[b"MESSAGE=not this unit", b"_SYSTEMD_UNIT=docker.service"],
            &[
                marker.as_bytes(),
                unit.as_bytes(),
                b"SYSLOG_IDENTIFIER=app",
                b"SYSLOG_PID=42",
            ],
            &[
                b"MESSAGE=ready",
                unit.as_bytes(),
                b"SYSLOG_IDENTIFIER=app",
                b"SYSLOG_PID=42",
            ],
        ];
        journal_file(&self.journal().join("system.journal"), 1, &entries);
    }

    /// A second file with one more entry, as journald's next file would
    /// come (renamed into place, so the follow's watch sees it whole).
    fn append(&self) {
        let unit = format!("_SYSTEMD_UNIT={APP}.service");
        let entries: [&[&[u8]]; 1] = [&[
            b"MESSAGE=appended while following",
            unit.as_bytes(),
            b"SYSLOG_IDENTIFIER=app",
            b"SYSLOG_PID=42",
        ]];
        let part = self.dir.path().join("next.part");
        journal_file(&part, 10, &entries);
        std::fs::rename(&part, self.journal().join("user-1000.journal")).expect("rename");
    }
}

/// A journal file at `path` of one entry per element of `entries`, each
/// carrying the payloads given, seqnums from `first`, one second apart
/// and ten minutes ago: sb-journal's crafted builder, as its
/// `journal_of` lays the objects out, with times set.
fn journal_file(path: &Path, first: u64, entries: &[&[&[u8]]]) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after 1970")
        .as_secs();
    let base = (now - 600) * 1_000_000;
    let mut c = Crafted::new();
    c.hash_table(7);
    let main = c.array(&[], entries.len(), 0);
    let mut objects: Vec<(Vec<u8>, u64)> = Vec::new();
    let mut items_of = Vec::new();
    for payloads in entries {
        let mut items = Vec::new();
        for &payload in *payloads {
            let d = if let Some(&(_, d)) = objects.iter().find(|(p, _)| p == payload) {
                d
            } else {
                let d = c.data(payload);
                objects.push((payload.to_vec(), d));
                d
            };
            items.push(d);
        }
        items_of.push(items);
    }
    let offsets: Vec<u64> = items_of
        .iter()
        .zip(first..)
        .map(|(items, seqnum)| {
            let e = c.entry(seqnum, items);
            // The entry's realtime, after the object header and seqnum.
            c.set(e + 24, base + seqnum * 1_000_000);
            e
        })
        .collect();
    for (k, &e) in (0..).zip(&offsets) {
        c.set_array_item(main, k, e);
    }
    for &(_, d) in &objects {
        let listing: Vec<u64> = offsets
            .iter()
            .zip(&items_of)
            .filter(|(_, items)| items.contains(&d))
            .map(|(&e, _)| e)
            .collect();
        c.set(d + D_ENTRY_OFFSET, listing[0]);
        c.set(d + D_N_ENTRIES, listing.len() as u64);
        let rest = c.array(&listing[1..], 0, 0);
        c.set(d + D_ENTRY_ARRAY, rest);
        if listing.len() > 1 {
            c.set_tail(d, rest, listing.len() as u64 - 1);
        }
    }
    c.set(H_N_ENTRIES, offsets.len() as u64);
    c.set(H_ENTRY_ARRAY_OFFSET, main);
    c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
    std::fs::write(path, &c.b).expect("journal file");
}

/// sb-go's short text stays (the command contract); the long one
/// describes the picker (tui spec, "Help texts").
#[test]
fn help_is_pinned() {
    let help = common::sb()
        .args(["logs", "--help", "--color=never"])
        .assert()
        .success();
    insta::assert_snapshot!(
        "logs_help",
        String::from_utf8_lossy(&help.get_output().stdout)
    );
    common::sb().args(["logs", "x"]).assert().code(2);
}

/// No services: sb-go's message, extended to both kinds, exit 0, before
/// any screen (so piped too).
#[test]
fn no_services_print_the_message_and_exit_0() {
    let engine = FakeEngine::start(Options {
        inspects: Vec::new(),
        ..Options::default()
    })
    .expect("the fake Engine API");
    let world = World::new("[]", "[]").on_docker(engine.host());
    let out = world.piped(&["logs"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "No matching systemd services or Docker containers found.\n"
    );
    assert!(out.status.success(), "{out:?}");
}

/// No services and Docker unreachable: not "nothing found" (Docker's
/// containers weren't seen), so the picker opens with the Containers
/// section's one row, which needs a terminal.
#[test]
fn no_services_and_no_docker_open_the_picker_with_the_unreachable_row() {
    let world = World::new("[]", "[]");
    let out = world.without_terminal(&["logs"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Docker unreachable: "), "{}", pty.contents());
    pty.send(b"\r");
    pty.send(b"q");
    assert!(pty.exit().success());
    pty.assert_restored();
    assert!(
        world.log().contains("Docker unreachable"),
        "{}",
        world.log()
    );
}

/// A failing `systemctl`: host's error in the error block, exit 1.
#[test]
fn a_failing_service_list_is_an_error() {
    let world = World::with_script("echo 'Failed to connect to bus' >&2\nexit 1\n");
    let out = world.piped(&["logs"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to list systemd services"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

/// Services but no terminal at all (no `/dev/tty`, stdout a pipe): the
/// error, exit 1, nothing on stdout.
#[test]
fn no_terminal_is_an_error_with_nothing_on_stdout() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let out = world.without_terminal(&["logs"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sb logs needs a terminal to choose a log"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

/// A journal directory that can't be read: the error block before the
/// screen, exit 1.
#[test]
fn an_unreadable_journal_is_an_error_before_the_screen() {
    let world = World::new(UNITS, UNIT_FILES);
    std::fs::remove_dir(world.journal()).expect("no journal");
    let mut pty = Pty::spawn(&["logs"], world.envs());
    let status = pty.exit();
    assert_eq!(status.exit_code(), 1, "{}", pty.contents());
    assert!(
        pty.contents().contains("read the journal directory"),
        "{}",
        pty.contents()
    );
    assert!(!pty.parser.screen().alternate_screen());
}

/// The picker lists the Saltbox services, Enter opens one's log, a new
/// entry appears while following, Esc goes back, `q` exits 0 with the
/// terminal restored; the run log has the source and none of its
/// content.
#[test]
fn the_picker_opens_a_services_log_which_follows() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    // The whole screen: its last row is drawn last.
    assert!(pty.showing("Docker unreachable: "), "{}", pty.contents());
    let screen = pty.contents();
    // The failed unit heads the list, under Needs attention.
    assert!(screen.contains("› saltbox_managed_backup"), "{screen}");
    assert!(screen.contains("saltbox_managed_app"), "{screen}");
    assert!(screen.contains("✗ failed"), "{screen}");
    assert!(!screen.contains("ssh"), "{screen}");
    // No daemon in the test's environment: the Containers section's row.
    assert!(screen.contains("Docker unreachable: "), "{screen}");
    assert!(pty.parser.screen().alternate_screen());
    // Past Needs attention and docker: the app.
    pty.send(b"jj\r");
    assert!(pty.showing("app[42]: hello from app"), "{}", pty.contents());
    assert!(pty.showing("--- following ---"), "{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("app[42]: ready"), "{screen}");
    assert!(!screen.contains("not this unit"), "{screen}");
    world.append();
    assert!(
        pty.showing("appended while following"),
        "{}",
        pty.contents()
    );
    pty.send(b"\x1b");
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    pty.assert_restored();
    let log = world.log();
    assert!(log.contains("source opened"), "{log}");
    assert!(log.contains(APP), "{log}");
    assert!(!log.contains(MARKER), "{log}");
}

/// Ctrl-C in the viewer: exit 130, quietly, the terminal restored.
#[test]
fn ctrl_c_exits_130_with_the_terminal_restored() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"jj\r");
    assert!(pty.showing("hello from app"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
    assert!(!pty.contents().contains("ERROR"), "{}", pty.contents());
}

/// Ctrl-C in the picker: exit 130 too.
#[test]
fn ctrl_c_in_the_picker_exits_130() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
}

/// `q` in the viewer quits sb, exit 0, rather than going back.
#[test]
fn q_in_the_viewer_quits() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"jj\r");
    assert!(pty.showing("hello from app"), "{}", pty.contents());
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    pty.assert_restored();
}

/// How a search for `hello` lights the app's first line on screen with
/// `extra` set (no value removes a variable): whether `hello` is drawn in
/// reverse video, `from` after it not, and `hello`'s background.
fn hello_lit(args: &[&str], extra: &[(&str, Option<&str>)]) -> (bool, bool, vt100::Color) {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut envs = world.envs();
    // A developer's CLICOLOR settings would change the colour-on cases.
    envs.push(("CLICOLOR".to_owned(), None));
    envs.push(("CLICOLOR_FORCE".to_owned(), None));
    envs.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned))),
    );
    let mut pty = Pty::spawn(args, envs);
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"jj\r");
    assert!(pty.showing("app[42]: hello from app"), "{}", pty.contents());
    pty.send(b"/hello\r");
    assert!(pty.showing("/hello 1/1"), "{}", pty.contents());
    let screen = pty.parser.screen();
    let (row, text) = screen
        .rows(0, 100)
        .enumerate()
        .find(|(_, text)| text.contains("app[42]: hello from app"))
        .expect("the line");
    let at = |word: &str| {
        let col = text.find(word).expect("the word");
        let row = u16::try_from(row).expect("a row");
        let col = u16::try_from(col).expect("a column");
        screen.cell(row, col).expect("a cell").clone()
    };
    let (hello, from) = (at("hello"), at("from"));
    // The column is a byte offset: right while the row's lead is ASCII.
    assert_eq!((hello.contents(), from.contents()), ("h", "f"));
    pty.send(b"q");
    assert!(pty.exit().success());
    (hello.inverse(), from.inverse(), hello.bgcolor())
}

/// A match is drawn in reverse video with colour off (`NO_COLOR`,
/// `--color=never`) and under the 16 colours, in `SearchHit`'s background
/// under truecolor and 256 (the maintainer, 2026-11-14; tui spec,
/// decision 23).
#[test]
fn a_match_is_reversed_with_colour_off_and_in_sixteen_colours() {
    let reversed = (true, false, vt100::Color::Default);
    assert_eq!(hello_lit(&["logs"], &[]), reversed, "NO_COLOR");
    assert_eq!(
        hello_lit(&["logs", "--color=never"], &[("NO_COLOR", None)]),
        reversed,
        "--color=never"
    );
    assert_eq!(
        hello_lit(
            &["logs"],
            &[("NO_COLOR", None), ("SB_COLOR_PROFILE", Some("ansi"))]
        ),
        reversed,
        "ansi"
    );
    for profile in ["truecolor", "ansi256"] {
        let (inverse, _, bg) = hello_lit(
            &["logs"],
            &[("NO_COLOR", None), ("SB_COLOR_PROFILE", Some(profile))],
        );
        assert!(!inverse, "{profile}");
        assert_ne!(bg, vt100::Color::Default, "{profile}");
    }
}

/// A dotted service name opens its own log (sb-host lists it without
/// `.service`).
#[test]
fn a_dotted_service_name_opens_its_log() {
    let units = r#"[{"unit":"saltbox_managed_a.b.service","load":"loaded","active":"active","sub":"running","description":"a.b"}]"#;
    let files =
        r#"[{"unit_file":"saltbox_managed_a.b.service","state":"enabled","vendor_preset":null}]"#;
    let world = World::new(units, files);
    let unit = "_SYSTEMD_UNIT=saltbox_managed_a.b.service";
    let entries: [&[&[u8]]; 1] = [&[b"MESSAGE=dotted unit line", unit.as_bytes()]];
    journal_file(&world.journal().join("system.journal"), 1, &entries);
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Services (1)"), "{}", pty.contents());
    pty.send(b"\r");
    assert!(pty.showing("dotted unit line"), "{}", pty.contents());
    pty.send(b"q");
    assert!(pty.exit().success());
}

/// A service's change shows within 6 s, without input: the picker reads
/// the services again every 5 s (tui spec, success criterion 4), and the
/// run log has the change but none of the refresh's own records.
#[test]
fn a_service_change_shows_within_six_seconds() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Needs attention (1)"), "{}", pty.contents());
    let recovered = UNITS.replace(
        r#""active":"failed","sub":"failed""#,
        r#""active":"active","sub":"running""#,
    );
    std::fs::write(world.dir.path().join("units.json"), recovered).expect("units");
    let changed = std::time::Instant::now();
    assert!(pty.showing("all healthy"), "{}", pty.contents());
    assert!(
        changed.elapsed() < std::time::Duration::from_secs(6),
        "{:?}",
        changed.elapsed()
    );
    assert!(
        !pty.contents().contains("Needs attention"),
        "{}",
        pty.contents()
    );
    pty.send(b"q");
    assert!(pty.exit().success());
    // The refresh that saw it wrote none of its read's records (tui spec,
    // "Verification notes": the refresh's run log): the systemctl
    // children logged are the first load's four, and the change is
    // logged.
    let log = world.log();
    let children = log
        .lines()
        .filter(|line| line.contains("child started") && line.contains("systemctl"))
        .count();
    assert_eq!(children, 4, "{log}");
    assert_eq!(log.matches("systemd services listed").count(), 1, "{log}");
    assert!(
        log.lines()
            .any(|line| line.contains("service state changed")
                && line.contains("saltbox_managed_backup")
                && line.contains("active=active")),
        "{log}"
    );
}

/// Piped stdout (tui spec, "The inline fallback"): the picker on the
/// terminal (`/dev/tty`), then the chosen log on stdout, plain, with its
/// time and origin: the tail, then an appended entry; Ctrl-C, a signal
/// once the screen is gone, ends it by SIGINT, the terminal restored and
/// none of the log in the run log.
#[test]
fn piped_stdout_picks_on_the_terminal_and_streams_the_log() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let out = world.dir.path().join("out.txt");
    let out_arg = out.display().to_string();
    let mut pty = Pty::spawn_shell(r#"exec "$0" logs > "$1""#, &[&out_arg], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    assert!(pty.parser.screen().alternate_screen());
    pty.send(b"jj\r");
    let text = logs_pty::file_holding(&out, "app[42]: ready");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text:?}");
    for (line, message) in
        lines
            .iter()
            .zip(["hello from app", &format!("a line with {MARKER}"), "ready"])
    {
        // `2026-11-13T10:04:05+0000 app[42]: <message>`.
        let (time, rest) = line.split_once(' ').expect("a time");
        assert_eq!(time.len(), 24, "{line}");
        assert_eq!(rest, format!("app[42]: {message}"));
    }
    assert!(!text.contains('\x1b'), "{text:?}");
    world.append();
    logs_pty::file_holding(&out, "app[42]: appended while following");
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.signal(), Some("Interrupt"), "{}", pty.contents());
    pty.assert_restored();
    let log = world.log();
    assert!(log.contains("stream ended"), "{log}");
    assert!(!log.contains(MARKER), "{log}");
}

/// A reader that goes away ends the stream with cli-core's quiet 141, at
/// the next line written.
#[test]
fn a_closed_reader_ends_the_stream_with_141() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let out = world.dir.path().join("out.txt");
    let status = world.dir.path().join("status");
    let script = r#"{ "$0" logs; echo $? > "$2"; } | head -n 2 > "$1""#;
    let args = [out.display().to_string(), status.display().to_string()];
    let mut pty = Pty::spawn_shell(script, &[&args[0], &args[1]], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"jj\r");
    logs_pty::file_holding(&out, "hello from app");
    // head has gone after two lines; the next one written meets the
    // closed pipe.
    world.append();
    assert_eq!(logs_pty::file_holding(&status, "\n"), "141\n");
    assert!(pty.exit().success());
    pty.assert_restored();
    // Quiet: no error block.
    assert!(!pty.contents().contains('✗'), "{}", pty.contents());
}

/// `p` (tui spec, "Pasteable output"): the screen goes, the loaded lines
/// go to stdout, the review line to stderr, exit 0.
#[test]
fn p_prints_the_loaded_lines_and_the_review_line() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let err = world.dir.path().join("err.txt");
    let err_arg = err.display().to_string();
    let mut pty = Pty::spawn_shell(r#"exec "$0" logs 2> "$1""#, &[&err_arg], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"jj\r");
    assert!(pty.showing("app[42]: ready"), "{}", pty.contents());
    pty.send(b"p");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    pty.assert_restored();
    let screen = pty.contents();
    for message in ["hello from app", "ready"] {
        assert!(screen.contains(&format!("app[42]: {message}")), "{screen}");
    }
    assert!(!screen.contains("--- following ---"), "{screen}");
    assert_eq!(
        std::fs::read_to_string(&err).expect("stderr"),
        "Review it for domains, IPs and tokens before sharing it.\n"
    );
    let log = world.log();
    assert!(log.contains("log printed lines=3"), "{log}");
    assert!(!log.contains(MARKER), "{log}");
}

/// `s`: a new 0600 file in the run log's directory with the loaded lines,
/// and the notice on the screen; the run log has the path and the count,
/// none of the text.
#[test]
fn s_saves_the_loaded_lines_in_the_log_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"jj\r");
    assert!(pty.showing("app[42]: ready"), "{}", pty.contents());
    pty.send(b"s");
    // The notice wraps where the path's length puts it.
    assert!(
        pty.showing("saved the 3 lines loaded in the viewer"),
        "{}",
        pty.contents()
    );
    assert!(pty.showing("sharing it."), "{}", pty.contents());
    let logs = world.dir.path().join("logs");
    let saved: Vec<_> = std::fs::read_dir(&logs)
        .expect("the log directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| {
            path.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .starts_with("logs-service-saltbox_managed_app-")
            })
        })
        .collect();
    assert_eq!(saved.len(), 1, "{saved:?}");
    let mode = std::fs::metadata(&saved[0])
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let text = std::fs::read_to_string(&saved[0]).expect("the file");
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(
        text.contains(&format!("app[42]: a line with {MARKER}")),
        "{text}"
    );
    pty.send(b"q");
    assert!(pty.exit().success());
    let log = world.log();
    assert!(log.contains("log saved"), "{log}");
    assert!(log.contains("lines=3"), "{log}");
    assert!(!log.contains(MARKER), "{log}");
}
