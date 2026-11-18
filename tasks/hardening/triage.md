# Hardening plan triage: the record of the cut (2026-11-12)

The maintainer adopted this triage on 2026-11-12 and confirmed the drop
of config-M1 explicitly: it needs a file the server owner would have to
write themselves. The plan's decision 18 records the cut
([tasks/plan.md](plan.md)); the tasks are in [tasks/todo.md](todo.md);
the findings, by id, are in [tasks/findings.md](findings.md).

Later the same day the maintainer answered four open points (the plan's
decisions 19 to 22), which change what the tables below say:

- Task 35 is merged into Task 33 as one follow.sh scenario on real
  journald; the stress.sh run and its floors are gone, so 21 tasks
  remain. tests-H2 stays dropped. Its row's reason ("Task 35's handover
  check carries its own floors") no longer holds: the handover fix is
  shown by Task 33's scenario, and stress.sh keeps the weakness.
- Decision 14 is superseded, not narrowed: the maintainer chooses after
  each research task.
- Decision 7's drop is confirmed, and checkpoints have no report files.

On 2026-11-13 the maintainer approved eight module specs (tui, ansible,
completion, python-env, repo, release, ops and motd, in
[docs/specs/](../../docs/specs/)). The repo spec's exec amendment specifies
the fixes for exec-M1 and exec-L2, so both leave the DEFER table's
waiting state: their rows say where the fix is specified, and each is
fixed with the repo plan. The tui spec takes up the journal items and
the viewer's exec and sanitizer items, as the DEFER rule says.

Each open finding got one verdict:

- **KEEP**: fixed now. It stays in the plan, in the task named here.
- **DEFER**: real, but nothing uses the code yet. It left the plan. It
  comes back when its feature is specified: the TUI log viewer spec for
  the journal items and the items on exec's streaming, PTY, attached
  and mask paths; release preparation for the release items. Each row's
  reason names what it waits for.
- **DROP**: closed. No work, and no decline to confirm later.

A verdict the triage was unsure of is marked in the "was unsure"
column. The maintainer adopted those as they stand.

## What was counted

- tasks/findings.md holds 174 ids. 22 were closed before the triage and
  are left out: the 21 docs ids done in Tasks 1-6 (docs-H1, H2, M1-M6,
  L1-L4, L6-L12, L14, L17) and sb-L6 (declined, confirmed by decision
  16). The stricter half of docs-L14 is still a proposed decline in
  tasks/todo.md.
- 152 ids are classified below. An id that several reviewers gave to
  one defect is counted once per id, so the 29 KEEP ids are about 21
  distinct defects.

## Facts the verdicts rest on (read from the code)

- sb has six commands: `version`, `docker ps`, `docker start`,
  `docker stop`, `docker restart`, `validate-config`
  (crates/sb/src/main.rs).
- No sb command reaches the journal reader. sb-journal is wrapped by
  sb-host, and sb calls only sb-host's release check. So no journal
  finding can happen in the binary today. The ones that journals
  written by journald trigger are kept all the same: the viewer is the
  next piece of work and builds on them. The rest are deferred or
  dropped.
- exec has one caller: a captured `systemctl is-active` in
  crates/docker/src/control/service.rs. Nothing uses `stream`, `pty`,
  `attached`, exec's secret mask or streamed child output. Findings on
  those paths are deferred.
- Every runtime is current-thread (main.rs builds three, all
  `new_current_thread`).
- There is no release workflow; the only workflow is the journal
  matrix. The production musl build exists only as a size check.

## Counts

| Verdict | High | Medium | Low | unrated | Total | of which was unsure |
|---|---|---|---|---|---|---|
| KEEP | 6 | 18 | 12 | 1 | 37 | 11 |
| DEFER | 0 | 3 | 7 | 1 | 11 | 1 |
| DROP | 1 | 15 | 81 | 7 | 104 | 8 |
| Total | 7 | 36 | 100 | 9 | 152 | 20 |

On 2026-11-15 (Checkpoint R) exec-M1 and exec-L2 moved from DEFER to KEEP:
both were fixed in bdfc7e6 (module task M23).

On 2026-11-17 cli-core's cli-L5, journal's follow-L1, follow-L4 and
page-L5, and term-ui's san-I4 moved from DEFER to KEEP the same way:
the module build fixed each when its consumer arrived (the rows below).

On 2026-11-18 exec-L6 moved from DEFER to KEEP, fixed once `attached`
had callers (`sb edit`, ansible's runs), and exec-M3 and exec-L4 from
DEFER to DROP: `Cmd::pty` was removed unused (exec decision 15). The
same day's re-triage kept san-L2 and ui-L5 deferred, with reasons.

## KEEP (fixed now)

"Task" is the task that holds the id in the cut plan.

| id | severity | crate | task | description | reason | was unsure |
|---|---|---|---|---|---|---|
| san-H1 | High | term-ui, docker | 8 | `docker ps` prints container label values raw (same defect as docker-M1, ui-M4) | Labels come from any pulled image and reach root's terminal; one small fix in `Table` |  |
| docker-M1 | Medium | docker | 8 | Escape sequences in container labels reach the ps table | Same defect as san-H1 |  |
| ui-M4 | Medium | term-ui | 8 | `Table` writes cells raw | Same defect as san-H1 |  |
| san-M1 | Medium | cli-core | 9 | Error block prints error text raw | Daemon, controller and remote-service text reach it; raw escapes need a hostile or broken peer, but the fix is small and at one place | yes |
| san-M2 | Medium | cli-core | 10 | Run-log record can be split or forged by a newline or a Display field | A plain multi-line error already breaks the one-line record; forging needs hostile text. Keep the formatter escape, cut the source-scan backstop | yes |
| cli-M3 | Medium | cli-core | 9, 10 | Error text unescaped at the screen and log choke points | Same defect as san-M1 and san-M2 | yes |
| docker-L1 | Low | docker | 10 | Daemon error text unescaped in the run log | Part of san-M2; closed by the formatter fix with no work of its own | yes |
| exec-H1 | High | exec, cli-core | 12 | A dropped SSH session kills sb with no cancel and no log ending | SSH drops are ordinary; a `docker restart` cut between stop and start leaves containers down |  |
| cli-M1 | Medium | cli-core | 12 | SIGHUP unhandled | Same defect as exec-H1 |  |
| cli-M2 | Medium | cli-core | 13 | A SIGTERM sent to the whole session arrives twice under sudo and forces the exit | Reasoned, not yet probed: a session stop or shutdown signals sudo and sb alike, and sudo relays its copy; cleanup and terminal restore are then skipped |  |
| sb-M2 | Medium | sb, cli-core | 17 | A panic leaves nothing in the run log | Only on an sb bug, but then the log is the only evidence a user can send; a small hook | yes |
| ui-M3 | Medium | term-ui | 17 | A panic in a concurrent step is overdrawn by the live tree | Only on an sb bug; without it the panic message is lost from the screen too | yes |
| docker-M2 | Medium | docker | 18 | `docker restart` can end without starting the containers and without a notice | A slow stop past sb's wait, or one failed poll, is ordinary; containers stay down silently |  |
| ui-M2 | Medium | term-ui | 19 | Typing or Enter during a live run leaves stale duplicate lines | Any user can do it; visibly wrong output |  |
| ui-M1 | Medium | term-ui | 20 | A live tree taller than the terminal hides the running step | Reproduced with validate-config's failure blocks on 80x24 |  |
| ui-L3 | Low | term-ui | 21 | Table wrap artefacts at narrow widths (blank lines, emptied cells) | `docker ps` on a narrow terminal is ordinary; visibly wrong output |  |
| ui-L4 | Low | term-ui | 21 | Notices and kept output lines cut with an ellipsis in the finished tree | Seen on any 80-column session, but the full text is in the log and it is a look the maintainer still has to choose | yes |
| tests-H1 | High | journal | 27, 28 | Compact files (the 24.04 and 26.04 default) untested above object decoding | A test gap, not a defect; kept only so the kept journal fixes are tested in the layout servers write. Builder mode only, no mutant hunt | yes |
| follow-H1 | High | journal | 32, 33, 34, 35 | A page shows an entry its end doesn't count; the follow shows it again | Happens on a journal being written with a filtered query, which is the viewer's normal case. No sb command reaches the reader yet |  |
| page-H1 | High | journal | 32, 33, 34, 35 | Same doubled entry, found through paging | Same defect as follow-H1 |  |
| fwd-M2 | Medium | journal | 32, 33, 34, 35 | Same doubled entry, found through forward paging | Same defect as follow-H1 |  |
| page-M1 | Medium | journal | 36 | A page whose chosen entries all fail to decode returns no cursor to page past | One fix with page-M2 |  |
| page-M2 | Medium | journal | 36 | `more_newer` stays true when the newest matching entry can't be shown | One oversized or damaged newest entry is enough; a viewer never reaches the tail and never follows |  |
| fwd-M1 | Medium | journal | 37 | A forward page loops on the cursor's own boot | Needs a journald restart and a backward clock step in one boot (a package upgrade plus a time sync can do it); the effect is an endless loop | yes |
| file-M1 | Medium | journal | 38 | A forward walk loses every later array after one bogus item | Needs a damaged file; damage after an unclean shutdown is real but this exact shape is unproven. The fix is one line | yes |
| config-H1 | High | config | 42, 43 | validate-config refuses multi-line flow YAML that Ansible accepts | Users write such layouts by hand; sb fails a file Saltbox runs fine |  |
| config-L1 | Low | config | 42, 43 | sb passes a tab-only line or tab-indented top level that Ansible rejects | Tabs are the common YAML mistake; validate-config passes a file Saltbox then fails on |  |
| config-L2 | Low | config | 46 | A secret under a misplaced structure is echoed | A wrong indent in the accounts file is an ordinary mistake; the secret then lands on screen and in the log |  |
| config-L11 | Low | sb | 46 (was 52) | Ctrl-C may lag when DNS is dead | A server with broken DNS is where validate-config gets run; untested claim, and a second Ctrl-C forces the exit | yes |
| exec-M1 | Medium | exec | 7, 14 | A piped child prompting on the terminal hangs sb for ever | exec's only caller is a captured `systemctl is-active`; matters with the first command that runs git or ssh. **Fix specified 2026-11-13** (repo spec's exec amendment: `SSH_ASKPASS_REQUIRE=force`, `SSH_ASKPASS=/bin/false`, exec spec "Environment rules"); **fixed 2026-11-13 in bdfc7e6** (module task M23) |
| exec-L2 | Low | exec | 15 | `stream` loses a final unterminated line when a grandchild holds the pipe | No command uses `stream` yet. **Fix specified 2026-11-13** (repo spec's exec amendment, exec spec "Ways to run"); **fixed 2026-11-13 in bdfc7e6** (module task M23) |
| san-I4 | unrated | term-ui | 25 | No size bound in `from_bytes`; the viewer needs its own cap | Already a recorded requirement for the viewer spec; **fixed 2026-11-13 in 32497d8** (module task M66: the journal adapter's 4 MiB output limit) |  |
| cli-L5 | Low | cli-core | 22 | A requested exit code swallows a real error | Latent; no command combined the two then; **fixed 2026-11-13 in 9eafa73** (module task M68: a failed restore after Ctrl-C is still reported) |  |
| follow-L1 | Low | journal | 40 | A waited read is retried only at the next inotify event | The retry timing belongs to the viewer's wake loop; decide it there; **fixed 2026-11-15 in 24a7f01** (module task M73) |  |
| follow-L4 | Low | journal | 40 | Follow fails outright on inotify exhaustion | Correct behaviour; only the viewer's wording was open; **fixed 2026-11-15 in 24a7f01** (module task M73) |  |
| page-L5 | Low | journal | 39 | Release and source passed twice, nothing ties them | An API addition with no consumer then (decision 10 names it); **fixed 2026-11-13 in 172d0b1** (module task M65: `Journal::unit_query`) |  |
| exec-L6 | Low | exec | 15 | `attached` doesn't re-check the token after the hold | Deferred until `attached` had a caller. **Re-triaged 2026-11-18**: reachable now, as `sb edit` and ansible's `install` and `diag` runs are attached: a Ctrl-C delivered just before the hold is decided as the hold starts (`Shutdown::hold` settles it), and the editor or playbook was then started and stopped at once. **Fixed 2026-11-18** (`fix(exec): start no attached child once the hold sees an interrupt`; `tests/attached_late_signal.rs`) |  |

## DEFER (waits for its consumer)

"Task" is the task that held the id before the cut.

| id | severity | crate | task | description | reason | was unsure |
|---|---|---|---|---|---|---|
| exec-M2 | Medium | exec | 11 | A secret split by an escape evades the mask | No command registers a secret with exec or streams child output yet. 2026-11-18: commands stream child output now (apt, git, uv, the hooks), but none registers a secret with exec |  |
| san-L3 | Low | exec | 11 | Mask runs before sanitizing | Same defect as exec-M2 (and its 2026-11-18 note) |  |
| exec-L5 | Low | exec | 23 | No mask for a secret sent on stdin | An API addition with no consumer |  |
| san-L2 | Low | term-ui | 25 | Kept SGR codes include font select and aren't always closed | Only through streamed child output, which no command produced then (decision 17 already says how). **Re-triaged 2026-11-18**: still not reached in practice. The streamed children (git, apt-get and the dpkg maintainer scripts its install, remove and autoremove run, uv, the Git hooks command) relay text only from sources that already run as root on the server (Ubuntu's packages, Saltbox's repository and its commit subjects, the pinned requirements) or from GitHub over TLS (git's `remote:` lines); their own colour, when a config file or a root shell forces it on a pipe, is closed standard codes. Only SGR codes reach the screen, and only with sb's colour on; the run log escapes every line. Stays deferred, with findings.md's fix, for a child relaying coloured text from an untrusted source |  |
| san-I1 | unrated | term-ui | 21 | Step labels and titles drawn unsanitized | Every caller passes constants or escaped paths; matters with the first caller that passes foreign text |  |
| ui-L1 | Low | term-ui | 21 | Labels and title unsanitized | Same as san-I1 |  |
| ui-L5 | Low | term-ui | 21 | Long output lines stored whole and re-measured each frame | Only through streamed child output. **Re-triaged 2026-11-18**: not reachable through exec: every streamed line is cut to 4 KiB (exec's `LINE`) before it reaches `Step::output`, and the only other caller (completion's refresh) passes sb's own short lines. Stays deferred for a caller of `Step::output` with unbounded foreign text |  |
| page-L9 | Low | journal | 39 | API ergonomics (PartialEq, constructors, cursor parsing) | API changes with no consumer |  |
| sb-M1 | Medium | sb | 48 | No gate runs the production musl binary | Gate work, but it guards what users will actually run; do it when the first release is prepared | yes |
| sb-M4 | Medium | sb | 48 | The cross build drops the version variables | Bites at the first release build (wrong `sb version`); no release pipeline exists yet |  |
| sb-L8 | Low | sb | 48 | Cross image named by a mutable tag | Release preparation, as the finding itself says |  |

## DROP (closed)

"Task" is the task that held the id before the cut.

| id | severity | crate | task | description | reason | was unsure |
|---|---|---|---|---|---|---|
| exec-M3 | Medium | exec | 15 | `pty` with a cleared environment can't find a bare program name | Deferred until `pty` had a user; none came, and `Cmd::pty` was removed unused on 2026-11-18 (the maintainer; exec decision 15): the defect went with the code |  |
| exec-L4 | Low | exec | 23 | A PTY child that reads twice hangs until its timeout | As exec-M3: `Cmd::pty` removed unused on 2026-11-18 (exec decision 15) |  |
| docker-L2 | Low | docker | 10 | `DOCKER_HOST` userinfo and control characters echoed | Saltbox uses the local socket; nobody sets a `DOCKER_HOST` with credentials |  |
| docker-L3 | Low | docker | 24 | `tcp://host` with no port and bare `unix://` passed on | The finding says it is irrelevant on Saltbox |  |
| docker-L4 | Low | docker | 24 | Keep-alive reuse against the controller's idle timeout | Unconfirmed; docker-M2's tolerance of a few failed polls covers the effect | yes |
| docker-L5 | Low | docker | 24 | A list entry without an id is skipped silently | The daemon doesn't send one |  |
| docker-L6 | Low | docker | 24 | Fakes thinner than real daemon answers | Test quality; the opt-in live test covers the real shape, though a new daemon enum value would show only there | yes |
| docker-L7 | Low | docker | 24 | The fake's refusal of unversioned paths is a guess | Test note; already a proposed decline |  |
| docker-L8 | Low | docker | 24 | First of several names used as the display name | Matches sb-go; Saltbox doesn't use links |  |
| docker-L9 | Low | docker | 24 | Guideline notes on error and type shape | A reviewer note asking for no change |  |
| cli-L1 | Low | cli-core | 22 | Log directory check follows symlinks | Needs a local attacker in the syslog group planting a link |  |
| cli-L2 | Low | cli-core | 22 | An empty root-owned `SB_LOG_DIR` is silently chmodded | A test and debugging override |  |
| cli-L3 | Low | cli-core | 13 | Forced exit and hard escape leave no log record | A diagnostic nicety; the user knows they forced it |  |
| cli-L4 | Low | cli-core | 22 | Empty `SB_LOG_DIR` not treated as unset | A test and debugging override |  |
| cli-L6 | Low | cli-core | 22 | The details pointer is printed when log writes stopped | Needs a full disk mid-run; the effect is a pointer to a short log |  |
| cli-L7 | Low | cli-core | 22 | sudo argv lacks `--` | The executable is sb's own absolute path; it can't look like an option |  |
| cli-T1 | unrated | cli-core | 10, 13, 22 | Test gaps | Test quality; the kept fixes bring their own tests |  |
| san-L1 | Low | term-ui | 25 | `cut` splits SGR forms the console crate doesn't know | Cosmetic, and only through streamed child output |  |
| san-L4 | Low | cli-core | 10 | Stale comment in log.rs | A comment; corrected in passing in Task 10 |  |
| san-I2 | unrated | term-ui | 8, 25 | Bidi and zero-width characters pass to the screen | Accepted by the journal spec; the table half needs a hostile image and forges nothing (supersedes decision 7) |  |
| san-I3 | unrated | term-ui | 25 | A sequence split across lines shows its payload as text | No control byte results; already a proposed decline |  |
| san-T1 | unrated | term-ui | 8, 25 | Sanitizer test gaps | Test quality |  |
| follow-N1 | unrated | journal | 32, 33 | Mirror loss: a counted entry not yet linked is missed | Far rarer and the reviewer would decline it; comes free if Task 33 takes the re-read shape, otherwise not worth its own work | yes |
| follow-L2 | Low | journal | 40 | A file opened mid-follow can lose leading entries | Microsecond window; the reviewer says decline |  |
| follow-L3 | Low | journal | 40 | A follow that lost every watch goes silent | Needs the journal directories removed under a running follow | yes |
| follow-L5 | Low | journal | 40 | First batch memory when resuming from an old end | A doc line |  |
| follow-L6 | Low | journal | 40 | Double directory listing per rescan | No visible effect |  |
| follow-L7 | Low | journal | 40 | Tight 1 s deadlines in tests | Test quality |  |
| page-L1 | Low | journal | 39 | Limit notices can vanish under the ten-error cap | Needs more than ten read errors in one page |  |
| page-L2 | Low | journal | 39 | `omitted` counts repeats | A count in a notice |  |
| page-L3 | Low | journal | 39 | `decode` preallocates outside the limits | No visible effect |  |
| page-L4 | Low | journal | 39 | `PageEnd` is the journal's end, not the page's | A doc line; written with Task 33 |  |
| page-L6 | Low | journal | 39 | Subdirectory name rule looser than sd-journal's | Imported directories only; already a proposed decline |  |
| page-L7 | Low | journal | 39 | Spec drift | Documentation |  |
| page-L8 | Low | journal | 28 | Weak tests | Test quality |  |
| page-N1 | unrated | journal | 38 | Possible unbounded entry-array chain walk | Crafted files only |  |
| file-L2 | Low | journal | 38 | Zero-capacity entry arrays accepted | Crafted files only |  |
| file-L3 | Low | journal | 38 | An I/O error reported as corruption | Needs a failing disk; the effect is the wrong error kind |  |
| file-L4 | Low | journal | 38 | `read_up_to` doesn't retry EINTR | Reads of regular local files aren't interrupted |  |
| file-L5 | Low | journal | 27, 28 | Weak tests and missing hash vectors | Test quality |  |
| fwd-L3 | Low | journal | 37 | Boot table unbounded in boot ids | A real server has hundreds of boots, not millions |  |
| fwd-L4 | Low | journal | 39 | Per-boot start by one key, not strict order | Microsecond ties |  |
| fwd-L5 | Low | journal | 39 | Forward page across a flush repeats entries | journalctl does the same; a doc line |  |
| docs-L5 | Low | journal | 39 | Rustdoc and platform note disagree on 257 to 259.0 | Documentation only |  |
| docs-L13 | Low | scripts | 49 | check.sh skips silently when not root (the open part) | Gate script |  |
| docs-L15 | Low | docs | none | CLAUDE.md's two stale lines | The maintainer's own file; already deferred with no task |  |
| docs-L16 | Low | docs | Checkpoint C | CONSTRAINTS "Last reviewed" | A gate record |  |
| exec-M4 | Medium | exec | 16 | Tight wall-clock bounds in exec's tests | Test quality; no effect on a server |  |
| exec-L1 | Low | exec | 15 | `timeout(Duration::MAX)` panics | No caller passes it |  |
| exec-L3 | Low | exec | 23 | A group id re-tested after it was seen empty | Needs a pid wrap within two seconds |  |
| exec-L7 | Low | exec | 23 | The forced-exit hook logs between kills under the lock | No visible effect |  |
| exec-L8 | Low | exec | 23 | PTY CLOEXEC set after open | Unreachable on the current-thread runtime |  |
| exec-L9 | Low | exec | 23 | Mask edge cases for very short and encoded secrets | By spec; no Saltbox secret is that short |  |
| ui-L2 | Low | term-ui | 17 | `Saved` has no Drop | Needs a panic during the runner's setup |  |
| ui-L6 | Low | term-ui | 26 | Step record logged under the drawing lock | No visible effect |  |
| ui-L7 | Low | term-ui | 26 | Test gaps and hygiene | Test quality |  |
| ui-L8 | Low | term-ui | 26 | Parent printed before a late child is added | Unreachable on the current-thread runtime |  |
| sb-M3 | Medium | sb | 49 | Half of sb's integration tests skip without the root variable | Gate and test tooling |  |
| sb-M5 | Medium | sb | 50 | Overflow checks off in the production profile | The file-layer review found no overflow; only crafted files could reach one | yes |
| sb-M6 | Medium | sb | 51 | Startup builds the command tree twice and parses up to three times | Startup is inside its ratchet; nobody sees it |  |
| sb-L1 | Low | sb | 52 | `sb --color=never` alone exits 2 where bare `sb` exits 0 | Harmless; decision 17 only wanted it written down |  |
| sb-L2 | Low | sb | 52 | Help ignores a closed or full stdout | Harmless |  |
| sb-L3 | Low | sb | 52 | Service URL overrides honoured silently | Test overrides; sudo drops them |  |
| sb-L4 | Low | sb | 52 | Non-UTF-8 overrides handled two ways | Test overrides |  |
| sb-L5 | Low | sb | 51 | Unused dependency in sb's manifest | Housekeeping |  |
| sb-L7 | Low | host | 48, 52 | A comment's rationale is wrong for static musl | A comment; no command reaches the lookup |  |
| sb-L9 | Low | sb | 52 | A version test asserts only failure | Test quality |  |
| sb-L10 | Low | sb, host | 49 | Ungated tests need a supported host | Test quality |  |
| sb-L11 | Low | sb | 50 | Nothing says the panic strategy must stay unwind | A comment |  |
| sb-L12 | Low | sb | 52 | mimalloc guideline not followed | A reviewer note; already a proposed decline |  |
| tests-H2 | High | journal | 29 | stress.sh passes when every live snapshot fails | A test script, not sb; Task 35's handover check carries its own floors. The only High dropped | yes |
| tests-M1 | Medium | journal | 31 | Fuzz seeds are six formats, not eight | Test tooling |  |
| tests-M2 | Medium | journal | 29 | compare.sh labels variants it never checks | Test tooling |  |
| tests-M3 | Medium | journal | 31 | The fuzz harness never checks anything was read | Test tooling |  |
| tests-M4 | Medium | journal | 31 | The fuzz harness misses several read paths | Test tooling |  |
| tests-M5 | Medium | journal | 31 | The fuzz memory bound sees resident pages only | Test tooling |  |
| tests-M6 | Medium | journal | 29 | compare.sh and compat.sh ignore reported read errors | Test tooling |  |
| tests-M7 | Medium | journal | 30 | follow.sh's overflow scenario doesn't isolate an overflow | Test tooling |  |
| tests-M8 | Medium | journal | 30 | follow.sh checks sets, not order or count | Test tooling |  |
| tests-L1 | Low | journal | 30 | follow.sh pgrep PIDs unchecked | Test tooling |  |
| tests-L2 | Low | journal | 30 | Restart race false-fail | Test tooling |  |
| tests-L3 | Low | journal | 29 | stress.sh fixed sleep | Test tooling |  |
| tests-L4 | Low | journal | 41 | compare.sh live row can fail falsely | Test tooling |  |
| tests-L5 | Low | journal | 29 | stepback_checks returns with a note on 22.04 | Test tooling |  |
| tests-L6 | Low | journal | 29 | `compare` passes on zero expected entries | Test tooling |  |
| tests-L7 | Low | journal | 29 | compare.sh's variant count unasserted | Test tooling |  |
| tests-L8 | Low | journal | 41 | "Vanished cursor" fixtures name an existing entry | Test quality |  |
| tests-L9 | Low | journal | 41 | Difference 15 has no divergent test | Test quality |  |
| tests-L10 | Low | journal | 41 | Crafted files less realistic than journald's | Test quality |  |
| tests-L11 | Low | journal | 29, 31 | Workflow gaps | Test tooling; CI is unpushed |  |
| tests-L12 | Low | journal | 41 | compat.sh leaves root-owned files | Test tooling |  |
| tests-L13 | Low | journal | 29 | stress.sh's ready loop has no failure branch | Test tooling |  |
| tests-L14 | Low | journal | 30 | follow.sh's vacuum scenario deletes only read files | Test tooling |  |
| config-M1 | Medium | config, cli-core | 44 | A config file under 1 MiB exhausts memory or the stack | Needs alias bombs, 150 KB of duplicate keys or 20,000 nesting levels; only someone who already has sudo can write the file | yes; drop confirmed by the maintainer |
| config-M2 | Medium | config | 45 | `ALL_PROXY` honoured where Go ignores it | sudo drops the variable; left is a root shell where the owner exported a proxy on purpose | yes |
| config-L3 | Low | config | 46 | `hide` removes only verbatim echoes of a secret | Neither real service echoes credentials |  |
| config-L4 | Low | config | 46 | Debug derives print secrets | Nothing logs them |  |
| config-L5 | Low | config | 47 | No User-Agent sent | No observed effect on either service |  |
| config-L6 | Low | config | 47 | Pattern Unicode classes unguarded | The shipped patterns don't use them |  |
| config-L7 | Low | config | 47 | An unparsable Retry-After-Ms skips Retry-After | The service doesn't send one |  |
| config-L8 | Low | config | 47 | Schema loading laxer than documented | Harmless; the schema is sb's own |  |
| config-L9 | Low | config | 47 | An empty key's path is blank in the message | Cosmetic |  |
| config-L10 | Low | config | 47 | UTF-16 files refused | No Saltbox file is UTF-16; the refusal is clear |  |
| config-T1 | unrated | config | 42-45 | Test gaps | Test quality; the differential test that decision 3 asks for stays inside Task 43 |  |

## What the cut did to Tasks 7-52

The surviving tasks keep their numbers. 22 of 46 are left: 9 whole and
13 reduced.

Whole:

| Task | Ids |
|---|---|
| 12: a hangup cancels cleanly | exec-H1, cli-M1 |
| 18: `docker restart` never ends silently | docker-M2 |
| 19: echo off during a live run | ui-M2 |
| 20: a live tree taller than the terminal | ui-M1 |
| 32: the handover: facts, probe, options | follow-H1, page-H1, fwd-M2 |
| 33: a page and its follow give every entry once | follow-H1, page-H1, fwd-M2 (follow-N1 only if it comes free) |
| 36: a page can always be paged past | page-M1, page-M2 |
| 42: YAML: the corpus and the options | config-H1, config-L1 |
| 43: sb accepts the YAML Ansible accepts | config-H1, config-L1 (with decision 3's differential test) |

Reduced:

| Task | Ids left | What went |
|---|---|---|
| 8: table cells are sanitized | san-H1, docker-M1, ui-M4 | san-I2 (bidi in cells), san-T1 (the alphabet test) |
| 9: the error block escapes error text | san-M1, cli-M3 (screen) | nothing; the whole task was an unsure KEEP |
| 10: no logged value can forge a record | san-M2, cli-M3 (log), docker-L1 | docker-L2, cli-T1, the source scan and its allowlist; san-L4's comment is corrected in passing |
| 13: a relayed signal isn't a second signal | cli-M2 | cli-L3 (forced exits logged), cli-T1 |
| 17: panics reach the log and the screen | sb-M2, ui-M3 | ui-L2 |
| 21: table wrap and uncut lines | ui-L3, ui-L4 | ui-L1, san-I1, ui-L5 (deferred) |
| 27: the builder writes compact files | tests-H1 (the builder mode) | file-L5's mutants, the keyed mode unless a kept fix needs it, the named-mutant runs |
| 28: suites over both layouts | tests-H1 (only the suites the kept fixes touch) | file-L5's hash vectors, page-L8 |
| 34: the handover across files | follow-H1, page-H1, fwd-M2: the rotation and flush pins, Difference 13's wording | the archived file copied under a new inode, unless Task 32 shows journald does that |
| 35: stress.sh checks the handover | follow-H1 (evidence on real journald) | nothing yet; it may shrink to one follow.sh scenario run as Task 33's verification |
| 37: a forward page on the cursor's own boot | fwd-M1 | fwd-L3 |
| 38: forward walks over damaged arrays | file-M1 | file-L2, file-L3, file-L4, page-N1 |
| 46: secrets hidden wherever their key sits | config-L2, and config-L11 from Task 52 | config-L3, config-L4 |

Gone:

| Task | Why |
|---|---|
| 7, 14: a child that prompts on the terminal | exec-M1 deferred to the first command that runs git or ssh |
| 11: secrets masked after escapes are stripped | exec-M2, san-L3 deferred: no command masks or streams |
| 15: exec's smaller defects | exec-M3, L2, L6 deferred; exec-L1 dropped |
| 16: exec's timing-bound tests | exec-M4 dropped (test quality) |
| 22: Low sweep, cli-core | all dropped; cli-L5 deferred |
| 23: Low sweep, exec | all dropped or deferred |
| 24: Low sweep, docker | all dropped |
| 25: Low sweep, the sanitizer | san-L2, san-I4 deferred; the rest dropped |
| 26: Low sweep, term-ui's runner | all dropped |
| 29, 30, 31: matrix scripts and fuzz harness | test tooling (tests-H2 is the one High in here) |
| 39: Low sweep, journal paging and API | dropped or deferred; page-L4's doc line goes into Task 33 |
| 40: Low sweep, the journal follow | dropped; follow-L1, L4 deferred to the viewer |
| 41: Low sweep, journal test evidence | test quality |
| 44: config files can't exhaust memory | config-M1 needs a crafted file; drop confirmed by the maintainer |
| 45: proxies as Go reads them | config-M2 dropped |
| 47: Low sweep, config | all dropped |
| 48: the production binary run by the gate | sb-M1, sb-M4, sb-L8 deferred to release preparation |
| 49: skipped tests counted, gate as root | gate tooling |
| 50: overflow checks | sb-M5 dropped |
| 51: startup parses once | sb-M6 not visible |
| 52: Low sweep, the sb binary | all dropped; config-L11 moved to Task 46 |

Dropped ids that still leave a trace in a kept task, with no work of
their own:

- san-L4: the stale comment is corrected in passing in Task 10.
- page-L4: the `PageEnd` doc line is written with Task 33.
- follow-N1: Task 33 covers the mirror case only if the followed option
  gives it for free.
- config-T1: the differential test that decision 3 asks for stays in
  Tasks 42 and 43.

## What the cut did to the maintainer's earlier decisions

| Decision | Effect of the cut |
|---|---|
| 1: fix all High and Medium before the viewer spec | Superseded. One High (tests-H2) and 14 Mediums are dropped (tests-M1 to M8, exec-M4, sb-M3, sb-M5, sb-M6, config-M1, config-M2); 5 Mediums are deferred (exec-M1, M2, M3, sb-M1, sb-M4). |
| 4: Lows are not ignored; each is a criterion or in a sweep | Superseded. Its test ("a Low that can happen on a real server is fixed") is the one the triage applied. 80 Lows are dropped at once, and the sweeps that carried them are gone. |
| 7: bidi and zero-width characters escaped in table cells | Superseded: the table half of san-I2 is dropped. |
| 10: API names | Half unused. `Page::oldest_chosen` and `newest_chosen` stay (Task 36). `Journal::unit_query` (page-L5) is deferred. The `PageEnd` doc line is written with Task 33. |
| 13: checkpoints as reports; every checkpoint runs the plan id check | Narrowed. The plan id check is retired. Checkpoints T, D, B, Y and C cover the surviving tasks only. |
| 14: research tasks take their recommendation | Narrowed to Tasks 32, 34 and 42/43. Tasks 7 and 14 are gone. |
| 15: sweeps commit fixes and collect declines | Narrowed. No sweep remains. The DROP table above replaces the sweeps' declines; the one running list stays for a decline proposed later. |
| 17: answers given ahead of their tasks | Narrowed to Tasks 18, 19 and 20. The answers for Tasks 25, 45, 47, 48, 50 and 52 are unused. |
| Checkpoint C's criteria | Changed: every KEEP id is fixed or declined with the maintainer's confirmation. The fresh review of the whole range is gone. |

Not affected: 2, 3, 5, 6, 8, 9, 11, 12, 16.

Also affected: the plan's "named expected failures" lived in Tasks
29-31. With them gone, no later task has an entry to remove.
