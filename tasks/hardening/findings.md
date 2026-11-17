# Re-review findings (2026-11-11, at 96e940a)

The condensed findings of the thirteen fresh-context reviews behind
[tasks/plan.md](plan.md). Every item has a stable id, `<area>-<rating><n>`,
which the plan's traceability table and the Low sweeps in
[tasks/todo.md](todo.md) cite. Ratings: `H` High, `M` Medium, `L` Low,
`I` Info, `T` a test gap, `N` a note with no rating. The numbers are the
reviewers' own, so some areas start at 2 or skip a number; items a
reviewer left unnumbered take the next free number. Where several
reviewers found one defect, each item names the others ("same as").

"CONFIRMED" means the reviewer reproduced it with a probe; "reasoned"
means read from the code.

Areas: `docker`, `cli` (cli-core), `san` (the sanitizer and the
untrusted-text sinks), `follow` (the follow and its async glue), `page`
(paging and the public API), `sb` (the binary and its seams), `docs`
(documents and gates), `file` (the journal file layer), `exec`, `ui`
(term-ui's runner and tables), `tests` (test evidence), `fwd` (forward
paging and boot order), `config`.


## docker (done): no Critical/High
- docker-M1 CONFIRMED (same as san-H1 and ui-M4): escape sequences in container labels (traefik port label; image LABEL) reach the ps table raw. ps.rs:280-283, term-ui table.rs:31,164. Fix: escape_controls on name + traefik values in ps::table (table only), test.
- docker-M2 reasoned: wait/poll ending without final job status (Timeout 295s vs controller 300/600s, one failed poll, POST timeout; the outcomes Timeout, Poll, StatusCheck, DecodeStatus, MissingStatus and a Send timeout) gives no "controller may still be working" notice; restart then never starts -> containers left down. control/commands.rs:141-163, client.rs:67-81. sb-go parity; needs spec decision (notice for every non-terminal ending; tolerate transient poll failure).
- docker-L1 confirmed (part of san-M2): daemon error text unescaped in run log (connect.rs:190, `%cause`); cli-core log.rs:184-186 comment claims escaping.
- docker-L2: DOCKER_HOST userinfo/control chars echoed (connect.rs:59-61,85-91,157).
- docker-L3: tcp://host with no port and a bare unix:// are passed to bollard (connect.rs:52-58), which only strips the scheme: port 80 where docker uses 2375, and an empty socket path where docker uses the default. Irrelevant on Saltbox; spec note.
- docker-L4 uncertain: keep-alive reuse across the 5 s polls vs the controller's IdleTimeout (control/mod.rs:156-159 keeps the default pool): a poll can hit a connection the server is closing, hyper-util retries only a request it had not written yet, and one such failure ends the wait (docker-M2). The controller's server timeouts are not recorded. Fix: check them, or pool_max_idle_per_host(0).
- docker-L5: list entry without Id skipped silently (connect.rs:164-166).
- docker-L6: fakes thinner than real answers (testing/mod.rs:40-49 builds list entries with only Id, Names and State; the fixtures carry only State, Config.Labels and NetworkSettings.Ports), so a bollard model mismatch on its closed enums (RestartPolicy.Name, CgroupnsMode, Isolation, mount propagation, list Ports[].Type, list Health) shows only in the opt-in live test; add full-shape fixture (API 1.55 shape, documentation addresses).
- docker-L7: the fake's `refuse_unversioned` models a guess: testing/mod.rs:314-319 and 363-365 invent the daemon's message and keep the unversioned ping served. The code says so; the test proves "an error, not a panic" and nothing more.
- docker-L8: the first of several names is used as the display name (ps.rs:193-199); with legacy links the list's `Names` can start with an alias (`/other/alias`). sb-go parity; Saltbox does not use links.
- docker-L9: guideline notes, no bug: `ControlError` is an enum with `String` payloads and `PartialEq` (M-ERRORS-CANONICAL-STRUCTS), defensible for an app crate whose tests match variants; `Daemon` exposes `bollard::Docker` as a pub field (M-DONT-LEAK-TYPES), in an internal workspace crate.

## cli-core (done): no Critical/High
- cli-M1 reasoned (same as exec-H1): SIGHUP unhandled (cancel.rs:510; spec cli-core.md:349 says so). Dropped SSH kills sb: no cancel, no "run finished", piped children orphaned. sb-go parity. Fix: treat like SIGTERM + amend spec. Spec decision.
- cli-M2 reasoned: group-wide SIGTERM counted twice under sudo relay (cancel.rs:277-284, 519-531) -> ForcedExit, cleanup skipped. Live once cli-M1 fixed. Fix: ignore repeat within ~250 ms; probe.
- cli-M3 (same as san-M1 on screen and san-M2 in the log): error text unescaped at choke point: exit.rs:165-175, 197-203 (screen), log.rs:272-277 (log: tracing escapes ESC etc but not \n \r -> forged "run finished" record). Fix: escape per line in shown_chain/render_block + log wrapper; test.
- cli-L1: log dir check follows symlinks (log.rs:156-178); /var/log is root:syslog 0775. Fix: symlink_metadata + openat O_NOFOLLOW.
- cli-L2: empty root-owned SB_LOG_DIR silently chmod 0700, sticky dropped (log.rs:170-178, 293-306).
- cli-L3: forced exit / hard escape leave no log trace (cancel.rs:521-531).
- cli-L4 (same as the SB_LOG_DIR part of sb-L4): empty SB_LOG_DIR not treated as unset (log.rs:44-46, main.rs:189).
- cli-L5: requested exit code swallows a real error (exit.rs:78-80); latent.
- cli-L6: the `details:` pointer is printed whenever the run log was opened, also when its writes stopped during the run, so it can point at a log that lacks the failure; the spec says "when the run is logged" (spec line).
- cli-L7: sudo argv lacks `--` before the executable (root.rs:82-87).
- cli-T1 test gaps: symlinked/empty log dir, \n in log, decide(Some(SIGINT), true, SIGTERM).

## sanitizer / untrusted-text sinks (security, done): DisplayText itself sound (3M fuzz, linear time)
- san-H1 CONFIRMED (same as docker-M1 and ui-M4, rated High here): ps table label cells raw. ps.rs:61-80, 273-286; table.rs:107-112, 166-173. Fix at the choke point: Cell::new / table.rs:111 through DisplayText(colour off). Spec gap: tables an unstated exception in term-ui.md.
- san-M1 CONFIRMED (same as cli-M3, screen): error_block raw. exit.rs:143,150-175,191-201. Fix: escape_controls per line in shown_chain.
- san-M2 CONFIRMED (same as cli-M3, log; includes docker-L1): run-log injection via Display fields; tracing escapes ESC etc only for message/error fields, never \n \r; %display nothing. Sites: log.rs:274, connect.rs:190, exec run.rs:261, pty.rs:106, sb main.rs:196, LogTracer deps. Fix: escape at call sites + test grepping `%` fields; custom formatter for \n\r; fix log.rs:183-186 comment.
- san-L1 confirmed: tree.rs:514-523 `cut` splits SGRs console doesn't recognise (colon forms, >5 digits) -> dangling CSI, width over-count. Cosmetic.
- san-L2 reasoned: kept SGR not limited to colour (text.rs:211-218, 258): 7 reverse, 8 conceal (journal decision 2 accepts this), 5 blink and 10-20 font select are kept; on the Linux virtual console SGR 11/12 change the character mapping and `ESC[0m` does not undo it, so the tree glyphs stay garbled after sb exits. And not always closed (tree.rs:403-405; runner.rs:832-840, 875-877, 1265-1268): a warn notice's text sits outside the paint, and Plain mode writes kept codes with no reset, so an unclosed `ESC[8m` runs into following lines. Latent today apart from the Linux-console case through streamed child output. Fix: append reset to any drawn line that kept an SGR; consider an allowlist of params (0-9, 21-29, 30-49, 38/48/58 forms, 90-107) that drops 10-20.
- san-L3 reasoned (same as exec-M2): exec masks secrets before sanitizing: `hun\x1b[1mter2` -> log holds hunter2. run.rs:538-544, runner.rs:866. Fix: sanitize then mask.
- san-L4 (part of san-M2): stale comment log.rs:183-186.
- san-I1 Info (same as ui-L1): labels/title drawn unsanitized (runner.rs:320, 480; tree.rs:340); all current callers pass constants or escape_controls-ed paths. The API contract should say so, or sanitize (M-STRONG-TYPES-GUARD).
- san-I2 Info: bidi and zero-width characters pass to the screen; accepted (journal spec, display decision 2). In the log they are escaped (the Debug form), so screen and log differ in the safe direction.
- san-I3 Info: a sequence split across lines shows its printable payload: exec splits at `\n` and each line is its own from_bytes call, so `ESC]52;c;AAAA\nBBBB BEL` shows `BBBB` as plain text on the second line. No control bytes result.
- san-I4 Info: Step::output and from_bytes have no size bound of their own; exec caps lines at 4096 bytes (run.rs:31). A zero-width line of pure SGRs is never shortened by `cut` (100,000 SGRs survive cut(80)), so with the cap the worst case is about 4 KB x 8 tail lines per frame. The log viewer needs its own size cap before from_bytes on journal fields.
- san-T1 test gaps (the reviewer's I5): no test feeds sanitized output through `cut` or through a table; `no_short_input_gets_a_control_through` uses a 19-symbol alphabet with no CAN/SUB, DEL, `:` or 8-bit DCS/APC introducers.

## follow + async glue (done)
- follow-H1 CONFIRMED by probe (same as page-H1 and fwd-M2): an entry appended between a page's header read and that file's walk is shown by the page (query walks use the DATA object's live list; `within` grows the arena) but not counted in PageEnd.n_entries -> the follow's first batch shows it again. walk.rs:703-760, 641-690, 327-336; file.rs:355-380; follow.rs:248-254. Contradicts "0 lost, 0 doubled". Tests write strictly after the page returns. PageEnd.newest has the same skew. Fix: bound the page's walk to the header's view (drop offsets past the main list's item n_entries-1; from the main array on 249); crafted test with header 2 / lists 3.
- follow-N1 mirror loss (reasoned, far rarer; no rating): header read after main-list link but before item links -> page misses, follow starts past. Reviewer would decline unless cheap.
- follow-L1: a waited read retried only at next inotify event (follow.rs:584-605 etc).
- follow-L2: file opened mid-follow can lose leading entries if `last` advanced (Copies::Leading); microsecond window; decline.
- follow-L3: follow losing all watches mid-run goes silent (follow.rs:976-982, 1193-1201).
- follow-L4: Journal::follow fails outright on inotify exhaustion (EMFILE/ENOSPC) — correct, viewer should word it.
- follow-L5: first batch memory = limit x files when resuming from an old/default PageEnd; doc line.
- follow-L6: double directory listing per rescan; dirs map keeps removed watches.
- follow-L7: PROMPTLY 1 s deadlines may flake under shared CPU (host journal.rs:465); no host test of FollowWait across a rescan.
- follow-M1 CONFIRMED on real journald (hardening Task 34, 2026-11-12; predates it): a follow running through a flush shows the last burst before it twice. journald stamps every entry of one event-loop pass with the same times (journald-server.c's sd_event_now), and `is_old` (follow.rs) takes an entry at `last`'s stamp as old only if it equals `last`, so the /var copies of the burst's other entries end the rescan's leading region and come again. Contradicts Difference 2. Fix: keep the identities of the entries shown at `last`'s stamp and drop their copies.

## paging + public API (done)
- page-H1 CONFIRMED independently (same as follow-H1 and fwd-M2): page shows an entry written during the page, PageEnd doesn't count it, follow repeats it. Also via After paged to tail. Fix: `lasts` keeps raw offset of main-list item n_entries-1; Walk::next drops driver offsets above it; forward.rs same bound.
- page-M1 CONFIRMED: page whose chosen entries all fail to decode returns no records and no cursor -> caller can't page past (walk.rs:384-454, lib.rs:421-435). Fix: expose oldest_chosen/newest_chosen cursors on Page.
- page-M2 CONFIRMED (pinned by tests walk.rs:2111-2125): more_newer stays true at tail when newest matching entry can't be shown; After pages from there empty forever -> viewer never reaches "at tail, follow". page-M1's cursors fix it.
- page-L1 confirmed: limit notices (OutputLimit, HeldLimit, TooLarge, ListChanging) go through the ten-error cap and can vanish (walk.rs:202,416,434,593). Fix: outside the cap, like add_open.
- page-L2 confirmed: `omitted` counts repeats (lib.rs:337-349): 14 distinct -> 10 shown + omitted=32.
- page-L3: decode preallocates Vec::with_capacity(picked.len()) outside limits (walk.rs:391).
- page-L4: PageEnd is the journal's end, not the page's; follow from a Before page skips entries; docs lib.rs:430-445. Doc/rename.
- page-L5: release and source passed twice (Query::unit, Journal::open), nothing ties them. Fix: Journal::unit_query.
- page-L6: Source::Directory takes any subdirectory with a 32-byte name as a machine-id directory (the scan in walk.rs), where sd-journal takes only a name `id128_is_valid` accepts; so sb can read a subdirectory `journalctl -D` skips, or skip one it reads.
- page-L7 spec drift: Record "its file's directory"; "0 doubled"; "ten distinct".
- page-L8 weak tests, which assert less than their names claim: walk.rs:1190-1203 (`a_failed_lookup_leaves_the_other_groups` checks only that one message is present and that some error exists), 961-972 (the `newest_of` helper drops the "more" flag), 1442-1456 (`the_output_limit_keeps_the_newest_across_files` checks a count and a lower bound, not which entries), 1184 (an error count with no check of which errors).
- page-L9 API: Record no PartialEq; Position Copy; Query::and consumes on error; BadCursor is a QueryError; Cursor::from_str rejects trailing newline; Page/Record no constructor outside testing.
- page-N1 flag for the file.rs reviewer (no rating): possible unbounded entry-array chain walk at EntriesRev::new.

## sb binary + seams (done): no Critical/High
- sb-M1 confirmed: no gate runs the production musl binary; check.sh:258-268 only stats it; startup timed on glibc release. Fix: smoke set via SB_TEST_BINARY on the artifact (version, --help, docker ps and validate-config against the fakes); time startup on it.
- sb-M2 reasoned (the log half of ui-M3): a panic leaves nothing in the run log (no panic hook; main.rs:203-216). Fix: hook logging message+location.
- sb-M3 confirmed: 44 of 87 sb integration tests return early `ok` without SB_TEST_ROOT=1 (tests/root_gate/mod.rs:9-27). Fix: CI as root, fail if unset.
- sb-M4 reasoned: cross build drops SB_VERSION/SB_GIT_COMMIT/SB_DISABLE_SELF_UPDATE (option_env!, build_info.rs:44-49; no Cross.toml passthrough) -> wrong version shipped.
- sb-M5 reasoned: production profile has overflow-checks off, tests have them on; sb-journal parses untrusted binary. Fix: overflow-checks = true in release+production; re-measure ratchet.
- sb-M6 reasoned: per-run startup builds Cli::command() twice, parses up to 3x, renders help for every command (main.rs:132, 261-272; help.rs:46-66). Fix: parse plain first.
- sb-L1: `sb --color=never` no subcommand exits 2 vs bare `sb` help 0 (main.rs:282); cobra prints help for both. The docker group's case is recorded in its spec; the root's is not. Fix: treat MissingSubcommand at the root as help, or record the difference.
- sb-L2: help ignores closed/full stdout: `sb --help | true`, bare `sb` and `sb --help > /dev/full` exit 0, where `sb version` gives 141 and 1 (main.rs:283-288). The spec's 141 rule names no exception. Fix: write help through the output_error path, or record the exception.
- sb-L3: service URL overrides (CLOUDFLARE_BASE_URL, SB_DOCKERHUB_URL, SB_DOCKER_CONTROLLER_URL; main.rs:393-403, 491-509) honoured in production silently, plain http included: a root shell with a leftover export sends real credentials there with nothing on screen (visible only in the log). sudo's env_reset drops them for relaunched runs. Fix: a runner notice when one is set.
- sb-L4: non-UTF-8 overrides handled two ways: SB_DOCKER_CONTROLLER_URL is an error; CLOUDFLARE_BASE_URL, SB_DOCKERHUB_URL and DOCKER_HOST silently fall back to the real service (main.rs:441, 492). SB_LOG_DIR="" isn't treated as unset, unlike the other overrides (that part: same as cli-L4).
- sb-L5: colorchoice-clap unused in sb (crates/sb/Cargo.toml:22); main.rs uses cli-core's re-export.
- sb-L6 (same as the E1 part of docs-L12): the E1 advisory ignores match nothing in the shipped graph (deny.toml:86-91; two advisory-not-detected). syntect isn't in sb yet, so when tui-markdown lands nothing fails at that moment. Fix: add the ignores with the dependency.
- sb-L7: getpwnam_r rationale wrong for static musl (crates/host/src/journal.rs:115-134): the comment prefers it because NSS and systemd backends answer, but static musl reads /etc/passwd (and nscd) only. Ubuntu's systemd-coredump user is in /etc/passwd, so it works, but tests run on glibc. Fix: correct the comment and cover it in the sb-M1 smoke run.
- sb-L8: cross image by mutable tag (`:0.2.5`), not digest; no Cross.toml to pin one. Worth fixing before the first release.
- sb-L9: `there_is_no_version_flag` asserts only `.failure()` (crates/sb/tests/version.rs:27-30); it passes on exit 101 or on an unsupported host. Fix: assert code 2 and the message.
- sb-L10: the 36 ungated tests need a supported Ubuntu host and fail with "not an Ubuntu distribution" elsewhere; no end-to-end test covers a missing /etc/os-release or a missing VERSION_ID; `ubuntu_release_reads_the_version_id_check_support_reads` (crates/host/src/lib.rs:208-215) asserts nothing off such a host (M-TAUTOLOGICAL-TESTS).
- sb-L11: nothing in Cargo.toml says the panic strategy must stay unwind: the terminal restore on Drop, catch_unwind in the restore hooks (cli-core/src/cancel.rs:99) and resume_unwind in the runner depend on it. Fix: a comment on the profile.
- sb-L12: M-MIMALLOC-APPS is not followed; the shipped binary uses musl's allocator. Weigh it only with a measurement on journal paging; it adds a C dependency.

## docs + gates (done)
- docs-H1: floor-guard.mjs:89-92 only inspects .rs and CONSTRAINTS.md. Blind to check.sh thresholds (41-44), MUTATION_TESTERS, deny.toml (ignores, wrappers), Cargo.toml [workspace.lints], rust-toolchain.toml, new tool configs (.cargo/mutants.toml, clippy.toml, .gitleaksignore), tests/*.sh (PREADS_*_MAX compare.sh:37-39). Its deny.toml / clippy::all alternatives (line 80) can never fire.
- docs-H2: check.sh full has no acknowledgement path for an accepted floor finding; it was run with the guard skipped (the journal plan's task list, now tasks/journal/todo.md, 425-427 and 588-592; dee2ecb). Fix: expected-findings list (rule:file) passed to the guard, logged.
- docs-M1: check-commit-messages.sh wired to nothing (no hook, not in check.sh). Fix: check.sh full runs it; CONSTRAINTS row; Review: line into the Floor.
- docs-M2: guard false negatives in Rust: cfg_attr(test, mutants::skip), cfg_attr(.., coverage(off)), split cfg_attr expect, #[cfg(any())] on a test, nested cfg_attr ignore; assert_cmd chains (.assert() .success() .code()) not assertions; deleted src/*.rs with unit tests unflagged; bullet continuation lines; appended looser alternative threshold.
- docs-M3: matrix + fuzz (exactness gates, criteria 3-4) are in no gate; CI unpushed. Fix: CONSTRAINTS note on manual cadence.
- docs-M4: nothing fails when a new crate lacks a deny.toml entry or [lints] workspace = true; tool versions unchecked.
- docs-M5: Checkpoint A record contradicts itself (the journal plan, now tasks/journal/plan.md:275, unticked; its task list, tasks/journal/todo.md, 277-280 ticked and 633-635 saying it is unticked).
- docs-M6: capability map stack table has no nix row (host.md:364-366 requires; criterion 7 marked met); rustix row lacks sb-host `event`.
- docs-L1: journal.md Project Structure omits boots.rs.
- docs-L2: Tech Stack misdescribes rustix (no openat/fstat).
- docs-L3: "nine scenarios" (11).
- docs-L4: "about 3,800 lines" stale (~4,530); ADR-013 upkeep omits BootRule.
- docs-L5: UnitRule::Pid1's rustdoc (rules.rs:320-322) says "249 to 259.0 ... coredumps only from root", while docs/platform-notes/systemd-journal.md says the coredump branch accepts the systemd-coredump user's uid from 257 on; the two disagree on 257 to 259.0.
- docs-L6: plan.md "decisions 1-9", "API sketch".
- docs-L7: journal.md:376-379 still says `clean`.
- docs-L8: journal.md's decision 2 says C0 controls other than tab and newline are dropped, but the sanitizer's rule (term-ui.md, "Escape handling") applies a carriage return: what follows the last one on its line is kept.
- docs-L9: the capability map's term-ui row is stale: it still describes "ANSI stripping of child output when not a terminal" and doesn't name the shared sanitizer (`DisplayText`), which now runs on every line.
- docs-L10: host.md:382 asks for "a deny.toml entry allowing only `sb-host`", but deny.toml:77 lists sb-journal among its own wrappers too (its self dev-dependency).
- docs-L11: deny.toml:71's comment says host is "only the supported-release check so far", which is stale now that sb-host wraps the journal reader.
- docs-L12: E1 + 0BSD don't bite (the E1 part: same as sb-L6).
- docs-L13: check.sh: git diff without --no-ext-diff/prefix flags; no --locked; CARGO_TARGET_DIR; non-root skips silently; dead branches; x86_64 only.
- docs-L14: commit-msg script: "Merge " subject bypass; lowercase first char only.
- docs-L15: CLAUDE.md stale ("do not exist yet"; systemd-run example lacks OOMPolicy).
- docs-L16: CONSTRAINTS "Last reviewed".
- docs-L17: todo housekeeping: done items in open list; no follow-up for MUTATION_TESTERS when sb reaches sb-journal; host.md has no plan entry.

## journal file layer (done): APPROVE, no Critical/High; no panic/overflow/unbounded decompression found
- file-M1 CONFIRMED: EntriesFwd keeps `self.last` when leaving an out-of-order array (file.rs:1825-1835), so one bogus high item loses every later array going forward; EntriesRev resets (file.rs:1192). After and tail pages disagree. Fix: last = None; tests both directions (deleting the Rev reset passes all 434 tests).
- file-L2 confirmed: zero-capacity entry arrays accepted (file.rs:1028-1032, 1106-1131); 1M 24-byte arrays -> 1M preads + 16 MB before first yield; bisect vs walk disagree. systemd check_object refuses. Fix: refuse in array_head.
- file-L3: `within` turns I/O error into "outside the arena" Corrupt (file.rs:366-379); is_io branches misled. Fix: Result.
- file-L4: read_up_to doesn't retry EINTR (file.rs:2215-2224).
- file-L5 weak tests (mutation-confirmed): hash.rs:40 `>12`→`>=12` survives; vectors: jenkins_hash64("MESSAGE=test")=0x7b2c506b6c9b3b4e; "PRIORITY=6\n_UID=0\n_GID=0"=0x5f0be856f41790ff (from systemd lookup3.c). file.rs:1108/1721 arena cap on n; :1023 alignment; :1533 left_items; :631 ENTRY_SIZE_MAX; :1789 dead code.

## exec (done)
- exec-H1 CONFIRMED (same as cli-M1): ssh drop -> SIGHUP kills sb, piped children (own process group, run.rs:470) orphaned PPID 1 and keep running. sb-go didn't have this (shared group). PTY and attached children unaffected. Fix: SIGHUP -> Act::Cancel, or a spec decision that apt survives a disconnect.
- exec-M1 CONFIRMED: piped child touching the controlling tty (ssh passphrase/host-key prompt under git, getpass) gets SIGTTIN/SIGTTOU, stops; sb waits silently forever without a timeout (run.rs:470, 323-353). Fix: setsid for piped children (fail fast), or detect stopped leader, or (as pty.rs already does) `waitid` with WSTOPPED, or at minimum doc + required timeouts + GIT_SSH_COMMAND='ssh -oBatchMode=yes' or SSH_ASKPASS_REQUIRE=never. GIT_TERMINAL_PROMPT=0 covers git's own prompt, not ssh's.
- exec-M2 CONFIRMED (same as san-L3): secret split by escape reassembled after mask -> screen and log (run.rs:519, 538-544; pty.rs:359; runner.rs:866). Real trigger: tools colouring part of a token. Breaks success criterion 6.
- exec-M3 CONFIRMED: pty with env_clear() can't find a bare program name; error not NotFound (pty.rs:138-147, 281-300).
- exec-M4: tight wall-clock test bounds (stream.rs:201, pty.rs:183,215,247, groups.rs:163-164).
- exec-L1 confirmed: timeout/grace(Duration::MAX) panics (run.rs:55, 322).
- exec-L2 confirmed: stream loses final unterminated line when a grandchild holds the pipe (run.rs:494, 373-377).
- exec-L3: group id re-tested after seen empty (group.rs:216-222, 85-91): it stays in `LIVE` up to 2 s after `finish` saw no members, so a later "has members" can only be another group that reused the id. Needs a pid wrap. Fix: remove it from `LIVE` once seen empty after the reap, with a `gone` flag Drop honours.
- exec-L4: PTY child reading twice hangs until its timeout (pty.rs:157): the single end-of-file is used up by the first read. By spec ("first read"); the consequence is that every `pty` caller needs a timeout; say so in the doc comment.
- exec-L5: no mask for stdin secrets (lib.rs:215, 286-296): the docs at lib.rs:146-149 recommend stdin for credentials, but only `secret()` and `secret_env()` feed the mask, so a tool that echoes the stdin credential in an error reaches the log. Fix: a `secret_stdin` or a mask-only registration.
- exec-L6: attached re-check token after hold (run.rs:223-234).
- exec-L7: forced-exit hook logs between kills under lock (group.rs:80-93).
- exec-L8: portable-pty sets CLOEXEC after `openpty`, not atomically (pty.rs:124-126): a spawn on another thread in that window would inherit the slave and hold the PTY open. Not reachable on today's current-thread runtime; note it for when spawns go parallel.
- exec-L9: mask edge cases (mask.rs:45-64; by spec): a 1 or 2 byte secret without a newline masks every occurrence, which gives away the secret and inflates output about 10 times; a secret with a leading newline and a body under 4 bytes is never masked in line-split output; encoded forms (URL-encoded, base64) are not matched.

## term-ui runner/UI (done): no Critical/High; 4 Medium reproduced in tmux
- ui-M1 CONFIRMED: live subtree taller than the terminal hides the running step (tree.rs:499-507, runner.rs:112-129; indicatif stops drawing past height). 30 sub-steps on 80x12; validate-config on 80x24 with failure blocks. Fix: collapse settled middle lines to "… N finished"; or print settled children for good (changes decision 3 -> maintainer).
- ui-M2 CONFIRMED: typing/Enter during a live run leaves stale duplicate lines (tty.rs:50-51 only clears ECHOCTL). Fix: also clear ECHO in Saved::quiet (verified with stty -echo); PTY test.
- ui-M3 CONFIRMED (its log half is sb-M2): panic in a group sibling loses its message (runner.rs:1040-1072, 947-955, 1246-1261); frames keep redrawing; not in run log either (no panic hook). Fix: set stop flag in the sibling panic guard unconditionally; cli-core logs panics (sb-M2).
- ui-M4 CONFIRMED at table (same as docker-M1 and san-H1): Table writes cells raw (table.rs:110-113). Fix in Cell::new/render via DisplayText(false), keep \n.
- ui-L1 (same as san-I1): labels/title unsanitized (runner.rs:52-66, 320, 480; tree.rs:280-311; latent; callers static): a done label holding `ESC[2J` wiped the screen, and a `\n` in a label breaks the tree. Fix: one_line in Labels::new and Labels::failure; add labels to the spec's list.
- ui-L2: Saved has no Drop; quiet() before tokio::spawn that can panic (runner.rs:355-396).
- ui-L3 confirmed: tabled wrap artefacts at narrow widths (blank line at exact multiples; keep_words breaks padded binding; below min width cells emptied) (table.rs:128-151).
- ui-L4: notices and kept output lines cut with … in the finished tree (tree.rs:402, 438), where error lines deliberately aren't; on an 80-column session the most diagnostic line can end in …, with the full text only in the log. Fix: the same `fit` closure for notices and tail when not live.
- ui-L5: long output lines stored whole, re-measured every frame (tree.rs:178-184, 438); a child writing multi-megabyte lines costs CPU and memory. Fix: cap a stored line, for example at 4096 columns.
- ui-L6: step record logged under the drawing lock (runner.rs:476-478).
- ui-L7: test gaps and hygiene: no test covers ui-M1 to ui-M4 or ui-L1 (the in-memory screens are always larger than the tree, and no PTY test sends input); tests/runner.rs:25-36 runs `script` without a timeout, unlike signals.rs:19, so a hang would hang the suite; tests/live.rs:13 uses Target::Stderr, so run from a real terminal it goes live and changes the developer's tty modes; tests/runner.rs:225 does not remove NO_COLOR for the `--colour` case; the ignored src/.runner.rs.pending-snap and src/.tree.rs.pending-snap hold stale entries that a later `cargo insta accept` could apply; delete them.
- ui-L8: parent printed before late child added (runner.rs:756-766, tree.rs:133-143; multi-thread runtime only): `is_open()` and `begin()` take the lock separately. sb's current-thread runtime can't hit it. Fix: check `is_running(parent)` inside `begin` under the same lock.

## test evidence (done)
- tests-H1 (mutation-confirmed): compact files untested above object decoding in unit tests: 5 mutants in the compact branches of bisect and EntriesFwd survive (file.rs:1417, 1547, 1746, 1813, 1857). crafted::journal is regular/unkeyed/uncompressed (testing.rs:39-41). 24.04/26.04 write compact by default. Matrix covers it but is manual. Fix: compact (+keyed) mode in the crafted builder; run walk/forward/boot/follow suites over both.
- tests-H2: stress.sh passes when every live snapshot fails (stress.sh:144-153, 161-169): tail exits 0, noisy snapshots skipped unbounded, total may be 0. Fix: require k>0, total>0, clean≈2k; fail on any noisy snapshot whose stderr isn't a listed transient pattern.
- tests-M1: fuzz seeds are 6 formats not 8 (249 ignores SYSTEMD_JOURNAL_COMPRESS; 3 of 22.04 seeds are 0xc); distinct() compares bytes. Missing: 240-byte headers (237/245), non-compact 255, 259, journald-written. Fix: key distinctness on header size+flags+writer; seeds from 20.04/18.04, 24.04 COMPACT=0, 26.04.
- tests-M2: compare.sh labels variants it never checks (compare.sh:91-124; 22.04's 7 rounds = 2 formats). Fix: assert expected flag bits per release, explicit skip note.
- tests-M3: fuzz never checks anything was read (examples/fuzz.rs:163-164). Fix: unmutated seeds must read clean with expected count; records floor.
- tests-M4: fuzz reads miss realtime/monotonic bisects, per-boot seek, vanished cursor, match path on drains, small Limits (Batch::more), multi-file merge.
- tests-M5: fuzz memory bound sees resident pages only (VmHWM); thin headroom ~13 of 16 MiB incl. harness copies. Fix: RLIMIT_AS = base VmSize + margin.
- tests-M6: compare.sh/compat.sh ignore sb-journal's reported read errors (checks.sh:172-252). Fix: fail unless stderr empty or stats line.
- tests-M7: follow.sh overflow scenario doesn't isolate overflow (follow.sh:247-261).
- tests-M8: follow.sh checks sets not order, never the written count (follow.sh:135-146, 312-324).
- tests-L1: follow.sh:116 pgrep PIDs unchecked.
- tests-L2: restart race false-fail (follow.sh:239-244).
- tests-L3: stress.sh:134 fixed 3 s sleep.
- tests-L4: compare.sh:153-158: the live row compares two reads while journald still runs, so a write between them fails it falsely; compat.sh stops journald first.
- tests-L5: stepback_checks returns with a note for a 22.04 writer (checks.sh:507-510) should fail.
- tests-L6: `compare` passes on zero expected entries (checks.sh:48, tools.rs:142-152).
- tests-L7: compare.sh's variant loop count is unasserted: nothing checks that all seven variants ran, and a command reading stdin inside the loop would eat the list.
- tests-L8: Difference 18 "vanished cursor" fixtures still name an existing entry (walk.rs:2365-2366, checks.sh:279-280), no same-seqnum-id gap case.
- tests-L9: Difference 15 no divergent test.
- tests-L10: crafted realism: XOR hashes and item hashes always zero; header ids, state and tail fields zero; one boot id; seven data hash buckets; no TAG objects, and sealing has no test anywhere (the spec lists it as UNVERIFIED); lookup3 pinned at lengths 0 and 30 only (the other tail lengths only through the matrix's hash-check; see file-L5).
- tests-L11: workflow: compat.sh lacks `if: !cancelled()` (line 118), fuzz + seeds not in CI.
- tests-L12: compat.sh leaves root-owned files in target/journal-compat, so a second run by a non-root user fails at `rm -rf`; seeds.sh chowns its output, compat.sh doesn't.
- tests-L13: stress.sh:93-97: the ready loop has no failure branch; a writer that never gets ready is caught only later, indirectly.
- tests-L14: follow.sh's vacuum scenario only deletes files already read to the end; only the unit test at follow.rs:1624 covers a file deleted unread.

## forward paging + boot order (done): bisect, seek, boot table match systemd line by line; no Critical/High
- fwd-M1 CONFIRMED by probe: a single-boot file of the cursor's OWN boot keeps 259's realtime fallback when the monotonic bisect misses (forward.rs:284 -> seek_259 :220, :336-341, :386-389) -> After(e) returns [b1,b2,e,f] incl. entries before the cursor and the cursor's own entry; paging one at a time loops forever with no order cycle. All three releases. journalctl 259.9 shows only f (exact-match first step, not emulated). Needs two seqnum ids in one boot + backward clock step (249-written leftovers, journal-remote). Fix: in boot_starts return starts when the file holds the cursor's boot (`|| starts.contains_key(&c.boot_id)`), or emulate 259.9's exact-match step; correct docs forward.rs:28-32, 81-86; journal.md:785-793, 871-874. Property probe: 5 modes x 300 seeds x 2 releases 0 failures; the 6th mode fails 210/600 on this pattern.
- fwd-M2 CONFIRMED (same as follow-H1 and page-H1; the third independent confirmation of the page/follow double; also via After).
- fwd-L3: field_values / boot_starts unbounded in _BOOT_ID values (file.rs:875-927, forward.rs:253-283); cap and fall back.
- fwd-L4: per-boot starts "at or after" by one key, not strictly after by Cursor::order (forward.rs:266, 278); microsecond ties.
- fwd-L5: forward page across a flush repeats entries on 24.04/26.04, as journalctl does; not listed for After (Differences 2, 13 cover the follow).

## config (done): differential-tested against sb-go/yaml.v3/PyYAML/libyaml; rules, scalars, validators, PSL, positions, secrets sound (0 mismatches over ~60k cases)
- config-H1 CONFIRMED: saphyr-parser 0.3.3 refuses multi-line flow layouts Ansible and sb-go accept (closing bracket at the key's column after a quoted item or nested flow; empty `[\n]`; multi-line quoted scalars; tab after colon). parse.rs:57-59, 193; cli-core config.rs:176-185. validate-config exits 1 on a file Saltbox runs fine; appdata_path falls back to /opt on parse failure (read from code). Not in the Differences list. Fix: lenient/libyaml-compatible parser or list the difference; standing differential test.
- config-M1 CONFIRMED x3: <1 MiB file exhausts memory/crashes: alias copies of a large scalar (OOM at 3 GB; kills `sb version` too via cli-core startup inventory load); quadratic duplicate-key messages (150 KB -> OOM); stack overflow in cli-core's inventory read with 20,000 nesting levels (no depth guard there). Files group-writable, sb root.
- config-M2 CONFIRMED: ALL_PROXY honoured by reqwest Proxy::System (http.rs:56-58), Go ignores; socks5 breaks both checks, http ALL_PROXY silently routes credentials. Fix: explicit HTTPS_PROXY/HTTP_PROXY/NO_PROXY only.
- config-L1: sb passes tab-only line / tab-indented top level that Ansible rejects.
- config-L2: secret under misplaced structure echoed (`user:\n  - pass: !!int x`), rules.rs:140-157, secret.rs:20.
- config-L3: `hide` only removes verbatim echoes (secret.rs:41): a service echoing the token URL-encoded, base64, uppercased or JSON-escaped gets through to screen and log. Neither real service echoes credentials; defence in depth. Fix: also hide the percent-encoded and JSON-escaped forms.
- config-L4: Debug derives print secrets (tree::Node, Data, scalar::Value); nothing logs them today. cloudflare::Check's Debug (cloudflare.rs:53-60) prints the reqwest client that run::Settings' Debug deliberately leaves out; drop the `http` field there.
- config-L5: no User-Agent is sent (http.rs:53); Go sends `Go-http-client/1.1` and cloudflare-go its own. Fix: `.user_agent("sb/<version>")`.
- config-L6: pattern Unicode classes with no guard (schema.rs:233-247, rules.rs:376-382): `^\d+$` accepts Arabic digits. The pinned patterns don't use `\d`, `\w` or `\s`, but a later schema would be silently laxer than its ECMA reading. Fix: refuse or translate those classes at load.
- config-L7: an unparsable Retry-After-Ms skips Retry-After (cloudflare.rs:125-127 returns straight to backoff; cloudflare-go v7.13.1's parseRetryAfterHeader continues to Retry-After). Fix: fall through.
- config-L8: strict schema loading is laxer than documented: `required: "true"`, `min_length: "1"`, `description: 5` and `pattern: 5` all load. Harmless.
- config-L9: an empty key's path is blank (rules.rs:179-185): input `: abc` gives `hetzner_vlan.yml:1:1: unknown field` with no key shown, while parse.rs:165 shows `""` for the same key. Fix: share the join.
- config-L10: UTF-16 files are refused as "isn't UTF-8"; yaml.v3 and PyYAML read them; not listed as a difference.
- config-L11: exit may lag after Ctrl-C when DNS is dead (not tested): the runtime drop in crates/sb/src/main.rs:512-521 waits for hyper's blocking getaddrinfo thread.
- config-T1 test gaps: nothing compares parser acceptance with libyaml or yaml.v3 (config-H1, config-L1); nothing bounds memory for alias-copied bytes or duplicate-message size (config-M1); `Proxy::System` has no behavioural test, only `Via` and `Off` (config-M2); the parity tables are hand-ported, and a differential harness would keep them honest.

