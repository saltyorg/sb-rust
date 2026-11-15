//! User Sessions and Last login (motd spec, Sections; decision 6;
//! Differences 7 and 8): logind's session list through `loginctl`, and
//! the last login through `last`, where sb-go read `who`, then `last -n
//! 5`, `lastlog` and `who -u` (`motd/info.go:151-332`).
//!
//! The arguments and the parsing were fixed by the container matrix
//! (`crates/motd/tests/matrix.sh`, `docs/platform-notes/motd-sources.md`)
//! on 22.04, 24.04 and 26.04:
//! - `loginctl list-sessions` writes JSON with `--output=json` on systemd
//!   249 and 255 (22.04, 24.04), and with `--json=short` on 259 (26.04),
//!   which takes `--output=json` without a word and writes its table;
//!   249 and 255 refuse `--json=`. Only 259 reports a session's `class`.
//! - `last -w -n 5` is util-linux's on 22.04 and 24.04 and wtmpdb's on
//!   26.04; both write sb-go's columns, wtmpdb with ` - ` before `still
//!   logged in` and `wtmpdb begins` for util-linux's `wtmp begins`. `-w`
//!   keeps a long user or host (an IPv6 address) whole where the default
//!   cuts it at 8 and 16 columns.
//!
//! - A session in `closing` (its user logged out and left a process
//!   running) isn't counted (the maintainer, 2026-11-15; Difference 7):
//!   one `loginctl show-session -p Id -p State <ids>` reads the listed
//!   sessions' states, on every release.
//!
//! Every child runs through sb-exec's `Cmd` under sb-go's 5 s, its text
//! through term-ui's `DisplayText`; the JSON through `serde_json`.
//! `last`'s columns are sb-go's field rules, as no crate reads them
//! (why not utmp-rs: stale, and wtmpdb's database is no utmp file; motd
//! spec, Tech Stack, "Considered and not used").

use std::collections::HashSet;
use std::path::PathBuf;

use sb_exec::Cmd;

use crate::Paint;
use crate::packages::child_output;

/// The two programs, found on `PATH`; the tests point them at stand-ins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    pub loginctl: PathBuf,
    pub last: PathBuf,
}

impl Tools {
    /// systemd's `loginctl` and the host's `last`.
    #[must_use]
    pub fn system() -> Self {
        Self {
            loginctl: PathBuf::from("loginctl"),
            last: PathBuf::from("last"),
        }
    }
}

/// The session classes that aren't a login (systemd 256's `class`: the
/// user's service manager and background sessions; Difference 7).
const NOT_LOGINS: [&str; 3] = ["manager", "manager-early", "background"];

/// sb-go's text for no session, or none it can count.
const NO_SESSIONS: &str = "No active sessions";

/// sb-go's text when no login can be shown.
const NO_LOGINS: &str = "No recent logins";

/// loginctl's JSON option on the release with os-release's `VERSION_ID`:
/// `--output=json` up to 24.04, `--json=short` after it and when the
/// release is unknown (the newest form), as the matrix found.
fn json_option(version_id: Option<&str>) -> &'static str {
    let major = version_id
        .and_then(|id| id.split('.').next())
        .and_then(|major| major.parse::<u32>().ok());
    match major {
        Some(major) if major <= 24 => "--output=json",
        _ => "--json=short",
    }
}

/// User Sessions: logind's sessions counted, `N active session(s)` with
/// the count in the value colour, or `No active sessions`. A session in
/// `closing`, a user who logged out and left a process running, is not
/// counted (the maintainer, 2026-11-15; Difference 7): not every
/// release's list carries the state (only 255's JSON has it), so one
/// `show-session` child reads every counted session's `State`, as
/// loginctl documents for parsable output
/// (<https://www.freedesktop.org/software/systemd/man/latest/loginctl.html>,
/// `show-session`; the states:
/// <https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html>).
pub async fn sessions(tools: &Tools, version_id: Option<&str>, paint: &Paint) -> String {
    let listed = child_output(
        Cmd::new(tools.loginctl.as_os_str())
            .arg("list-sessions")
            .arg(json_option(version_id)),
        "loginctl",
    )
    .await;
    let Some(ids) = login_sessions(&listed) else {
        return NO_SESSIONS.to_string();
    };
    let asked: Vec<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty())
        .collect();
    let states = if asked.is_empty() {
        String::new()
    } else {
        // `-p Id` keeps each answer with its session. loginctl stops at a
        // session that has gone since the list (exit 1, the matrix): then
        // no state is read and the listed sessions count.
        let mut cmd = Cmd::new(tools.loginctl.as_os_str())
            .arg("show-session")
            .arg("-p")
            .arg("Id")
            .arg("-p")
            .arg("State");
        for id in asked {
            cmd = cmd.value(id);
        }
        child_output(cmd, "loginctl show-session").await
    };
    match count_open(&ids, &states) {
        0 => NO_SESSIONS.to_string(),
        1 => format!("{} active session", paint.value("1")),
        count => format!("{} active sessions", paint.value(&count.to_string())),
    }
}

/// The IDs of the sessions in loginctl's JSON whose class, where it has
/// one, is a login's (empty where a session has no ID); none when the
/// output isn't a JSON array (a failed or older loginctl, logged).
fn login_sessions(output: &str) -> Option<Vec<String>> {
    if output.is_empty() {
        return None;
    }
    let sessions: Vec<serde_json::Value> = match serde_json::from_str(output) {
        Ok(sessions) => sessions,
        Err(err) => {
            tracing::warn!("loginctl's session list not read: {err}");
            return None;
        }
    };
    Some(
        sessions
            .iter()
            .filter(|session| {
                session
                    .get("class")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|class| !NOT_LOGINS.contains(&class))
            })
            .map(|session| {
                session
                    .get("session")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
    )
}

/// The IDs `show-session -p Id -p State` reports as `closing`: one block
/// per session, blocks apart by an empty line. Why by hand: loginctl has
/// no JSON for `show-session` (its `--json=` covers the `list-*` verbs
/// only, loginctl(1)), and no crate in the tree reads systemd's
/// `KEY=VALUE` text (sb-host's `parse_show` reads systemctl's the same
/// way). The empty line between blocks and the stop at the first unknown
/// ID are loginctl's code, not its manual: `show_properties` in v249 and
/// `show_session` in v259
/// (<https://github.com/systemd/systemd/blob/v249/src/login/loginctl.c>,
/// <https://github.com/systemd/systemd/blob/v259/src/login/loginctl.c>),
/// and the container matrix saw both on 22.04, 24.04 and 26.04.
fn closing_sessions(output: &str) -> HashSet<&str> {
    let mut closing = HashSet::new();
    for block in output.split("\n\n") {
        let mut id = None;
        let mut state = None;
        for line in block.lines() {
            match line.split_once('=') {
                Some(("Id", value)) => id = Some(value),
                Some(("State", value)) => state = Some(value),
                _ => {}
            }
        }
        if let (Some(id), Some("closing")) = (id, state) {
            closing.insert(id);
        }
    }
    closing
}

/// The listed sessions that `states` doesn't report as closing.
fn count_open(ids: &[String], states: &str) -> usize {
    let closing = closing_sessions(states);
    ids.iter()
        .filter(|id| !closing.contains(id.as_str()))
        .count()
}

/// Last login: the first entry of `last -w -n 5` that is a login, in
/// sb-go's text, or `No recent logins`.
pub async fn last_login(tools: &Tools, paint: &Paint) -> String {
    let output = child_output(
        Cmd::new(tools.last.as_os_str())
            .arg("-w")
            .arg("-n")
            .arg("5"),
        "last",
    )
    .await;
    first_login(&output, paint).unwrap_or_else(|| NO_LOGINS.to_string())
}

/// sb-go's `isSkippableLastLine`, with wtmpdb's `wtmpdb begins` beside
/// util-linux's `wtmp begins` (decision 6: reboot, shutdown and wtmp
/// lines are passed over).
fn skippable(line: &str, fields: &[&str]) -> bool {
    match fields.first() {
        None => true,
        Some(first) => {
            ["reboot", "shutdown", "runlevel", "wtmp", "wtmpdb"].contains(first)
                || line.contains("wtmp begins")
        }
    }
}

/// sb-go's reading of `last`'s lines (`GetLastLogin`): the first line of
/// five or more fields that isn't skippable and has a date, as
/// `user at Day Mon D HH:MM (still logged in) from IP`,
/// `user at ... until HH:MM (duration) from IP` or `user at ... from IP`;
/// a source without a `.` or `:` reads `local`, the date then one field
/// earlier. The user, date, logout time and source are in the value
/// colour.
fn first_login(output: &str, paint: &Paint) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        let fields: Vec<&str> = line.split_whitespace().collect();
        if skippable(line, &fields) || fields.len() < 5 {
            continue;
        }
        let (from, at) = if fields[2].contains('.') || fields[2].contains(':') {
            (fields[2], 3)
        } else {
            ("local", 2)
        };
        let Some(date) = fields.get(at..at + 4) else {
            continue;
        };
        let user = paint.value(fields[0]);
        let date = paint.value(&date.join(" "));
        let from = paint.value(from);
        if line.contains("still logged in") {
            return Some(format!("{user} at {date} (still logged in) from {from}"));
        }
        let logout = fields
            .iter()
            .position(|field| *field == "-")
            .and_then(|dash| fields.get(dash + 1));
        return Some(match logout {
            Some(logout) => format!(
                "{user} at {date} until {} ({}) from {from}",
                paint.value(logout),
                duration(&fields)
            ),
            None => format!("{user} at {date} from {from}"),
        });
    }
    None
}

/// The duration in parentheses: the first field opening one, joined with
/// the next when it doesn't close there, as sb-go joins them.
fn duration(fields: &[&str]) -> String {
    for (i, field) in fields.iter().enumerate() {
        if let Some(after) = field.strip_prefix('(') {
            return match (after.strip_suffix(')'), fields.get(i + 1)) {
                (Some(whole), _) => whole.to_string(),
                (None, Some(next)) => {
                    format!("{after} {}", next.strip_suffix(')').unwrap_or(next))
                }
                (None, None) => after.to_string(),
            };
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::tests::{SPAWNS, stand_in};
    use std::path::Path;

    /// The matrix's captures (`matrix.sh` with `SBM_FIXTURES`), one
    /// directory per release.
    fn fixture(release: &str, file: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/logins")
            .join(release)
            .join(file);
        std::fs::read_to_string(path).unwrap()
    }

    fn painted() -> Paint {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        Paint::new(&theme, true)
    }

    /// Stand-ins for both tools in `dir`, none there yet.
    fn tools(dir: &Path) -> Tools {
        Tools {
            loginctl: dir.join("loginctl"),
            last: dir.join("last"),
        }
    }

    #[test]
    fn the_system_tools_are_found_on_path() {
        let system = Tools::system();
        assert_eq!(system.loginctl, Path::new("loginctl"));
        assert_eq!(system.last, Path::new("last"));
    }

    #[test]
    fn loginctls_json_option_follows_the_release() {
        assert_eq!(json_option(Some("22.04")), "--output=json");
        assert_eq!(json_option(Some("24.04")), "--output=json");
        assert_eq!(json_option(Some("25.04")), "--json=short");
        assert_eq!(json_option(Some("26.04")), "--json=short");
        assert_eq!(json_option(Some("garbage")), "--json=short");
        assert_eq!(json_option(None), "--json=short");
    }

    /// Each release's session list, beside show-session's states: the
    /// logged-out login with a process left running is closing and isn't
    /// counted, so the one open SSH login is; 26.04's service manager
    /// session is not a login.
    #[test]
    fn each_releases_sessions_count_the_open_login() {
        for release in ["22.04", "24.04", "26.04"] {
            let ids = login_sessions(&fixture(release, "loginctl.json")).unwrap();
            assert_eq!(ids.len(), 2, "{release}");
            let states = fixture(release, "show-session");
            assert_eq!(closing_sessions(&states).len(), 1, "{release}");
            assert_eq!(count_open(&ids, &states), 1, "{release}");
        }
    }

    /// show-session's blocks: the closing sessions' IDs, whatever the
    /// order of a block's lines; a block without an ID names none.
    #[test]
    fn show_sessions_closing_sessions_are_found() {
        let states = "Id=c2\nState=active\n\nId=c3\nState=closing\n\n\
                      State=closing\nId=c4\n\nState=closing\n\nId=c6\nState=online\n";
        let closing = closing_sessions(states);
        assert_eq!(closing.len(), 2);
        assert!(closing.contains("c3") && closing.contains("c4"));
        assert!(closing_sessions("").is_empty());
    }

    /// The leftovers of logged-out users are dropped; a session
    /// show-session didn't answer for still counts.
    #[test]
    fn closing_sessions_are_not_counted() {
        let ids = ["c3", "c4", "c5", ""].map(String::from);
        let states = "Id=c3\nState=closing\n\nId=c4\nState=active\n";
        assert_eq!(count_open(&ids, states), 3);
        assert_eq!(count_open(&ids, ""), 4);
        assert_eq!(count_open(&[], states), 0);
    }

    #[test]
    fn session_classes_that_are_no_login_are_not_counted() {
        let json = r#"[{"class":"user"},{"class":"user"},{"class":"manager"},
            {"class":"manager-early"},{"class":"background"},{"class":"user-early"},
            {"class":"greeter"},{},{"class":7}]"#;
        assert_eq!(login_sessions(json).map(|ids| ids.len()), Some(6));
        // Only the service manager's: no login.
        assert_eq!(
            login_sessions(r#"[{"class":"manager"}]"#).map(|ids| ids.len()),
            Some(0)
        );
        assert_eq!(login_sessions("[]").map(|ids| ids.len()), Some(0));
    }

    /// No output (the child failed), loginctl's table, or JSON that isn't
    /// a list: nothing to count.
    #[test]
    fn output_that_is_no_session_list_counts_nothing() {
        assert_eq!(login_sessions("").map(|ids| ids.len()), None);
        assert_eq!(
            login_sessions("SESSION  UID USER\n     c3 1001 sbuser").map(|ids| ids.len()),
            None
        );
        assert_eq!(
            login_sessions(r#"{"session":"c3"}"#).map(|ids| ids.len()),
            None
        );
    }

    #[tokio::test]
    async fn sessions_read_as_sb_go_counts_them() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        let paint = painted();
        // The option is the release's, and it is the only one.
        stand_in(
            &tools.loginctl,
            "[ \"$*\" = 'list-sessions --json=short' ] || exit 1\n\
             echo '[{\"class\":\"user\"},{\"class\":\"manager\"}]'",
        );
        assert_eq!(
            sessions(&tools, Some("26.04"), &paint).await,
            format!("{} active session", paint.value("1"))
        );
        assert_eq!(sessions(&tools, Some("22.04"), &paint).await, NO_SESSIONS);
        stand_in(
            &tools.loginctl,
            "[ \"$*\" = 'list-sessions --output=json' ] || exit 1\necho '[{},{},{}]'",
        );
        assert_eq!(
            sessions(&tools, Some("22.04"), &paint).await,
            format!("{} active sessions", paint.value("3"))
        );
        stand_in(&tools.loginctl, "echo '[]'");
        assert_eq!(sessions(&tools, None, &paint).await, "No active sessions");
    }

    /// One show-session child asks for the counted sessions' states, and
    /// a closing one is dropped (the maintainer, 2026-11-15; Difference
    /// 7); the service manager's isn't asked for.
    #[tokio::test]
    async fn sessions_drop_the_closing_ones() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        let paint = painted();
        stand_in(
            &tools.loginctl,
            "case \"$*\" in\n\
             'list-sessions --json=short') echo '[{\"session\":\"c2\",\"class\":\"manager\"},\
             {\"session\":\"c3\",\"class\":\"user\"},{\"session\":\"c4\",\"class\":\"user\"},\
             {\"session\":\"c5\",\"class\":\"user\"}]' ;;\n\
             'show-session -p Id -p State c3 c4 c5') printf 'Id=c3\\nState=closing\\n\\nId=c4\\nState=active\\n\\nId=c5\\nState=active\\n' ;;\n\
             *) exit 1 ;;\n\
             esac",
        );
        assert_eq!(
            sessions(&tools, Some("26.04"), &paint).await,
            format!("{} active sessions", paint.value("2"))
        );
        // Every counted session closing: none.
        stand_in(
            &tools.loginctl,
            "case \"$1\" in\n\
             list-sessions) echo '[{\"session\":\"24911\"}]' ;;\n\
             show-session) printf 'Id=24911\\nState=closing\\n' ;;\n\
             esac",
        );
        assert_eq!(sessions(&tools, Some("22.04"), &paint).await, NO_SESSIONS);
        // show-session failing (a session gone between the two calls):
        // the listed sessions count, as before the states were read.
        stand_in(
            &tools.loginctl,
            "case \"$1\" in\n\
             list-sessions) echo '[{\"session\":\"24911\"},{\"session\":\"24912\"}]' ;;\n\
             show-session) printf 'Id=24911\\nState=closing\\n'; exit 1 ;;\n\
             esac",
        );
        assert_eq!(
            sessions(&tools, Some("24.04"), &paint).await,
            format!("{} active sessions", paint.value("2"))
        );
    }

    /// No session to count: show-session isn't run.
    #[tokio::test]
    async fn no_session_runs_no_show_session() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        let ran = dir.path().join("ran");
        stand_in(
            &tools.loginctl,
            &format!(
                "case \"$1\" in\nlist-sessions) echo '[{{\"session\":\"c2\",\"class\":\"manager\"}}]' ;;\n\
                 *) touch '{}' ;;\nesac",
                ran.display()
            ),
        );
        assert_eq!(sessions(&tools, None, &Paint::plain()).await, NO_SESSIONS);
        assert!(!ran.exists());
    }

    /// loginctl missing, failing or hanging: sb-go's text for no session.
    #[tokio::test]
    async fn sessions_without_an_answer_say_no_active_sessions() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        assert_eq!(sessions(&tools, None, &Paint::plain()).await, NO_SESSIONS);
        stand_in(&tools.loginctl, "echo '[{}]'; exit 1");
        assert_eq!(sessions(&tools, None, &Paint::plain()).await, NO_SESSIONS);
        stand_in(&tools.loginctl, "echo '[{}]'; exec sleep 60");
        assert_eq!(sessions(&tools, None, &Paint::plain()).await, NO_SESSIONS);
    }

    /// Each release's `last -w -n 5` reads as sb-go reads `last -n 5`:
    /// the first login, still open.
    #[test]
    fn each_releases_last_login_is_the_open_one() {
        for (release, date) in [
            ("22.04", "Sun Nov 15 09:00"),
            ("24.04", "Sun Nov 15 09:00"),
            ("26.04", "Sun Nov 15 09:01"),
        ] {
            assert_eq!(
                first_login(&fixture(release, "last"), &Paint::plain()).as_deref(),
                Some(format!("sbuser at {date} (still logged in) from 127.0.0.1").as_str()),
                "{release}"
            );
        }
    }

    #[test]
    fn a_closed_login_shows_its_end_and_duration() {
        let last = "reboot   system boot  6.8.0-1-generic Sat Nov 14 23:10   still running\n\
                    sbuser   pts/0        2001:db8::1:2:3:4 Sat Nov 14 23:10 - 23:52  (00:42)\n";
        assert_eq!(
            first_login(last, &Paint::plain()).as_deref(),
            Some("sbuser at Sat Nov 14 23:10 until 23:52 (00:42) from 2001:db8::1:2:3:4")
        );
        let paint = painted();
        assert_eq!(
            first_login(last, &paint).unwrap(),
            format!(
                "{} at {} until {} (00:42) from {}",
                paint.value("sbuser"),
                paint.value("Sat Nov 14 23:10"),
                paint.value("23:52"),
                paint.value("2001:db8::1:2:3:4")
            )
        );
    }

    /// A console login has no source: `local`, the date one field earlier.
    #[test]
    fn a_login_without_a_source_is_local() {
        assert_eq!(
            first_login(
                "root     tty1   Sat Nov 14 23:10 - crash  (1+02:03)\n",
                &Paint::plain()
            )
            .as_deref(),
            Some("root at Sat Nov 14 23:10 until crash (1+02:03) from local")
        );
    }

    /// util-linux's other endings read as sb-go reads them, its oddities
    /// kept: `gone - no logout` takes `no` for the logout time.
    #[test]
    fn util_linuxs_other_endings_read_as_sb_go_reads_them() {
        let plain = Paint::plain();
        for (line, shown) in [
            (
                "sbuser   pts/0        192.0.2.9         Sat Nov 14 23:10   gone - no logout",
                "sbuser at Sat Nov 14 23:10 until no () from 192.0.2.9",
            ),
            (
                "sbuser   pts/0        192.0.2.9         Sat Nov 14 23:10 - down   (00:05)",
                "sbuser at Sat Nov 14 23:10 until down (00:05) from 192.0.2.9",
            ),
            (
                "sbuser   pts/0        192.0.2.9         Sat Nov 14 23:10 - crash  (1+00:05)",
                "sbuser at Sat Nov 14 23:10 until crash (1+00:05) from 192.0.2.9",
            ),
            (
                "sbuser   pts/0        192.0.2.9         Sat Nov 14 23:10   still running",
                "sbuser at Sat Nov 14 23:10 from 192.0.2.9",
            ),
        ] {
            assert_eq!(first_login(line, &plain).as_deref(), Some(shown), "{line}");
        }
    }

    /// sb-go's other forms: no dash, a split duration, no duration.
    #[test]
    fn odd_lines_read_as_sb_go_reads_them() {
        let plain = Paint::plain();
        assert_eq!(
            first_login("sbuser pts/0 192.0.2.9 Sat Nov 14 23:10 gone\n", &plain).as_deref(),
            Some("sbuser at Sat Nov 14 23:10 from 192.0.2.9")
        );
        assert_eq!(
            first_login("sbuser pts/0 192.0.2.9 Sat Nov 14 23:10 -\n", &plain).as_deref(),
            Some("sbuser at Sat Nov 14 23:10 from 192.0.2.9")
        );
        assert_eq!(
            first_login(
                "sbuser pts/0 192.0.2.9 Sat Nov 14 23:10 - 23:11 (2 days)\n",
                &plain
            )
            .as_deref(),
            Some("sbuser at Sat Nov 14 23:10 until 23:11 (2 days) from 192.0.2.9")
        );
        assert_eq!(
            first_login(
                "sbuser pts/0 192.0.2.9 Sat Nov 14 23:10 - 23:11 (open\n",
                &plain
            )
            .as_deref(),
            Some("sbuser at Sat Nov 14 23:10 until 23:11 (open) from 192.0.2.9")
        );
        assert_eq!(
            first_login("sbuser pts/0 192.0.2.9 Sat Nov 14 23:10 - 23:11\n", &plain).as_deref(),
            Some("sbuser at Sat Nov 14 23:10 until 23:11 () from 192.0.2.9")
        );
        // Too few fields for a date: the next line is read.
        assert_eq!(
            first_login(
                "sbuser pts/0 192.0.2.9 Sat Nov\nroot tty1 Sat Nov 14 23:10\n",
                &plain
            )
            .as_deref(),
            Some("root at Sat Nov 14 23:10 from local")
        );
    }

    /// Reboots, shutdowns, run levels and both wtmp trailers are passed
    /// over; with nothing else, there is no login.
    #[test]
    fn records_that_are_no_login_are_passed_over() {
        let last = "\n\
                    reboot   system boot  6.8.0-1-generic Sat Nov 14 23:10   still running\n\
                    shutdown system down  6.8.0-1-generic Sat Nov 14 23:09 - 23:10  (00:01)\n\
                    runlevel (to lvl 5)   6.8.0-1-generic Sat Nov 14 23:10 - 23:10  (00:00)\n\
                    a b c\n\
                    a b\n\
                    \n\
                    wtmp begins Sat Nov 14 23:10:53 2026\n\
                    wtmpdb begins Tue Jul 21 11:18:23 2026\n\
                    x: wtmp begins Tue Jul 21 11:18:23 2026\n";
        assert_eq!(first_login(last, &Paint::plain()), None);
        assert!(skippable("", &[]));
        assert!(!skippable("sbuser pts/0", &["sbuser", "pts/0"]));
    }

    #[tokio::test]
    async fn last_login_runs_last_with_sb_gos_count_and_whole_names() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        stand_in(
            &tools.last,
            "[ \"$*\" = '-w -n 5' ] || exit 1\n\
             echo 'sbuser   pts/0        127.0.0.1        Sat Nov 14 23:10   still logged in'",
        );
        assert_eq!(
            last_login(&tools, &Paint::plain()).await,
            "sbuser at Sat Nov 14 23:10 (still logged in) from 127.0.0.1"
        );
    }

    /// last missing, failing, hanging or silent: `No recent logins`.
    #[tokio::test]
    async fn last_login_without_an_answer_says_no_recent_logins() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        assert_eq!(last_login(&tools, &Paint::plain()).await, NO_LOGINS);
        assert_eq!(NO_LOGINS, "No recent logins");
        for body in [
            "echo 'sbuser pts/0 192.0.2.9 Sat Nov 14 23:10 gone'; exit 1",
            "echo 'sbuser pts/0 192.0.2.9 Sat Nov 14 23:10 gone'; exec sleep 60",
            "true",
        ] {
            stand_in(&tools.last, body);
            assert_eq!(
                last_login(&tools, &Paint::plain()).await,
                NO_LOGINS,
                "{body}"
            );
        }
    }

    /// A name from `last` is made safe to draw.
    #[tokio::test]
    async fn last_logins_text_goes_through_display_text() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        stand_in(
            &tools.last,
            "printf 'ev\\033]0;t\\007il pts/0 192.0.2.9 Sat Nov 14 23:10 gone\\n'",
        );
        assert_eq!(
            last_login(&tools, &Paint::plain()).await,
            "evil at Sat Nov 14 23:10 from 192.0.2.9"
        );
    }
}
