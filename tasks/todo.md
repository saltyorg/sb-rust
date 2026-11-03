# config tasks

Plan: [tasks/plan.md](plan.md). Spec: [docs/specs/config.md](../docs/specs/config.md).
Every task: TDD where it changes behaviour, a code review (and the
library-reviewer for I/O, terminal and process code) before committing,
`scripts/check.sh full` before committing, Library-check and Review lines
in each commit, and the binary size measured: a change past the 5% ratchet
tolerance carries its ADR in the same commit (CONSTRAINTS.md). Never run sb
as root without `SB_LOG_DIR` in `target/` or a temporary directory. Never read or
change the real Saltbox checkout (`/srv/git/saltbox`), in tests or reviews:
fixtures through `SB_SALTBOX_DIR`; say so in every reviewer's brief. Root-gated
tests run only as root with `SB_TEST_ROOT=1` (check.sh sets it).

Carried forward (open items owned by later work):
- Taken by this plan:
  - term-ui criteria 4 and 6 with sb's own commands (moved here by docker
    decision 1): the failure block and the Ctrl-C endings and restore
    through the runner (Task 12).
  - From docker Task 4's review: control characters escaped in text sb
    echoes (the spec's issue text and remote messages: Tasks 6-9); term-ui's
    `Cell` itself stays carried (below).
- From cli-core:
  - Task 3: the rexpect PTY smoke test on the built binary moves to the
    first long-running command (validate-config's network checks can take
    seconds; Task 12 records whether it takes it or keeps it carried, with
    the reason).
  - Criterion 1: tab completion comes with the completion module.
  - The exec module removes `SB_RELAUNCHED` from child processes
    (`env_remove`), or an unprivileged sb started by one would refuse to
    relaunch (root.rs module docs).
  - sb-go exports COLORTERM for child processes (truecolor, 256color,
    unset for ansi and ascii); decide when the exec module starts children.
  - Check sudoers(5) for `--preserve-env` names outside env_keep; record
    the answer in docs/platform-notes/.
  - The first command that takes argument values (`install -e`) adds an
    end-to-end test that a real secret never reaches the run log.
  - Help: fang's 120-column wrap of long abouts comes with the first ported
    command whose long text needs it (cli-core Task 11); validate-config's
    long text is one short line, so it stays carried (Task 11 confirms).
- From term-ui's spec: prompts (criterion 10) are built with their first
  user, self-update or git's confirm (decision 1).
- From docker Task 4's review: term-ui's `Cell` passes control characters
  through; escape them when a command shows less trusted text.
- From the docker spec, for later docker plans: start/stop/restart (no
  `--verbose`, config decision 1; the unit-file check, timeouts and
  response cap); the picker's container list with the event stream.
- From the config spec, for the facts plan: the line-preserving writer,
  locking and drift detection; `sb update`'s call into validation.
- From the prototypes: the install script (tasks/prototypes/todo.md,
  "Later") depends on cli-core and the runner.

## Task 1: Schemas, dependencies, the musl build and crate choices

**Description:** Evidence, no production code, in a scratch project in a
temporary directory (removed after, build output included).
- **Schemas:** copy Saltbox's `schema/*.schema.yml` from the public
  upstream repository at a pinned commit (never the live checkout) into
  `crates/config/tests/fixtures/saltbox-schemas/` with a provenance note
  (repository URL, commit, GPL-3.0); run gitleaks
  over them (their `example:` values). Inventory every rule
  key and form, every `type`, `custom_validator` (is
  `validate_rclone_remote` used? decision 6) and `format`, every
  `additionalProperties` form, and every `pattern`: compiles under Rust's
  `regex`? same meaning as its ECMA-262 reading? anchored?
- **saphyr:** confirm 0.3.3 keeps scalar source text (representation
  mode) and that its parser's events carry positions for the duplicate pass.
- **Crates:** a maintained public-suffix crate with the ICANN and private
  sections; for `timezone`, a crate reading the system's tz database
  (not an embedded one), preferring `jiff` (already in the stack table)
  if its system database honours `TZDIR` so tests can point it at a
  fixture zoneinfo.
- **The musl build:** a production musl build of the scratch crate
  (reqwest with rustls and aws-lc-rs, serde-saphyr, regex, the two
  crates), built the way `scripts/check.sh` builds sb. proto-musl found
  plain builds fail once aws-lc-sys needs a musl C compiler, which this
  host lacks; record the outcome and the routes (a musl toolchain on the
  host and in CONSTRAINTS.md "Tools", `cross` for check.sh's size step,
  or `cargo-zigbuild`) for the maintainer. Measure each dependency's size
  in that build, so feature choices can weigh it.
- **cargo-deny:** the scratch crate's tree against sb's deny.toml.

Findings in `docs/platform-notes/saltbox-schemas.md` and
`docs/platform-notes/musl-c-dependencies.md` (versions, method; no host
details) and a spec amendment if anything contradicts the spec.

**Acceptance criteria:**
- [ ] The pinned schemas in the fixtures with provenance and clean docs and
      secret scans; their inventory recorded, each pattern's Rust behaviour
      recorded.
- [ ] saphyr confirmed; the two crates chosen with a reason; the new trees
      pass deny.toml (or the problem is named).
- [ ] The musl build's outcome, the routes and each dependency's size
      recorded for the maintainer.

**Verification:**
- [ ] The scratch project removed.
- [ ] The maintainer reviews the findings and picks the musl route
      (Checkpoint A).

**Dependencies:** None
**Files:** `crates/config/tests/fixtures/saltbox-schemas/`,
`docs/platform-notes/saltbox-schemas.md`,
`docs/platform-notes/musl-c-dependencies.md`, `docs/specs/config.md` (if
amended)
**Scope:** M

## Task 2: term-ui: a group that lets every sibling finish

**Description:** Decision 3. A group mode beside the existing one: no
sibling is stopped when another fails; `join` waits for all and returns
every result in spawn order; the parent concludes as the existing group
does ("sub-step failed" when any failed). The stopping group, its tests
and term-ui decision 7 stay as they are; term-ui's spec gains the mode
(an amendment for the maintainer at Checkpoint A).

**Acceptance criteria:**
- [ ] A failing sibling doesn't stop the others; all endings drawn and
      returned; a panic and Ctrl-C still follow term-ui's endings table.
- [ ] The existing group's tests pass unchanged; the spec amended.

**Verification:**
- [ ] `scripts/check.sh full`; library-reviewer on the diff.

**Dependencies:** None (beside Task 1)
**Files:** `crates/term-ui/src/runner.rs`, `crates/term-ui/tests/runner.rs`,
`docs/specs/term-ui.md`
**Scope:** S

## Task 3: One Saltbox root: `SB_SALTBOX_DIR`

**Description:** `SB_SALTBOX_DIR` (unset or empty: `/srv/git/saltbox`)
moves cli-core's inventory path (`config::INVENTORY`, read at startup for
the theme) with the root; cli-core's spec amended; `common::sb()` sets it
to a test directory with no Saltbox files, so no sb test reads the real
checkout from now on.

**Acceptance criteria:**
- [ ] The theme reads the inventory under `SB_SALTBOX_DIR` (a test); unset
      or empty means the default; the cli-core spec amended.

**Verification:**
- [ ] `scripts/check.sh full`.

**Dependencies:** None (beside Tasks 1-2)
**Files:** `crates/cli-core/src/config.rs`, `crates/sb/src/main.rs`,
`crates/sb/tests/common/mod.rs`, `docs/specs/cli-core.md`
**Scope:** S

## Checkpoint A: Evidence
- [ ] Task 1's findings reviewed; the musl route picked (an ADR if
      check.sh's gate changes); the spec amended and approved if needed.
- [ ] Task 2's term-ui spec amendment reviewed.

## Task 4: Crate and strict schema loading

**Description:** `crates/config` (`sb-config`) in the workspace and
deny.toml (`sb-config` added to `sb-cli-core`'s and `sb-term-ui`'s
wrappers, its own entry allowing only `sb`). Schemas loaded with
serde-saphyr into typed rules (`deny_unknown_fields`): every rule key of
the spec (`description` and `example` accepted and ignored), sb-go's type
and custom-validator names without `validate_rclone_remote`, formats,
`additionalProperties` as a rule or a bool, `pattern` compiled at load;
any unknown key, name or format, a malformed value or a pattern that
doesn't compile is an error naming the schema file and key; files up to
1 MiB. cargo-deny's gate reads sb's graph only, which includes sb-config
from Task 11; until then `cargo deny --manifest-path
crates/config/Cargo.toml --all-features check` runs here, recorded.

**Acceptance criteria:**
- [ ] The pinned schemas load; a test per kind of strictness error, each
      naming the file and key, failing against a stub first.
- [ ] The crate's own cargo-deny run passes.

**Verification:**
- [ ] `cargo test -p sb-config`; `scripts/check.sh full`.

**Dependencies:** Checkpoint A
**Files:** `Cargo.toml`, `deny.toml`, `crates/config/Cargo.toml`,
`crates/config/src/lib.rs`, `crates/config/src/schema.rs`
**Scope:** M

## Task 5: Config parsing: positions, duplicate keys, scalars

**Description:** Files read up to 1 MiB, the first document only, an
empty file as an empty mapping, a non-mapping root as one issue, a syntax
error as an issue with line and column; merge keys (`<<`) not followed
(an unknown field later, pinned by a test). The duplicate pass over
saphyr-parser's events first: every duplicate with its key path, its
position and every earlier one's. Plain scalars resolved from their source
text by yaml.v3's rules, a test at each edge. `Issue` (file relative to the
root, line, column, key path, message), sorting by position.

**Acceptance criteria:**
- [ ] Every duplicate reported with positions; the scalar edges and parse
      cases tested, each failing against a stub first.

**Verification:**
- [ ] `cargo test -p sb-config`; `scripts/check.sh full`.

**Dependencies:** Task 4
**Files:** `crates/config/src/parse.rs`, `crates/config/src/issue.rs`,
`crates/config/tests/fixtures/`
**Scope:** M

## Task 6: The rules engine

**Description:** sb-go's rules over the parsed tree, collecting every
issue (the spec's list, each quirk pinned by a test; `pattern` an
unanchored search on strings; `additionalProperties` as the spec defines;
unknown keys errors; "is required" at the parent key, 1:1 at the top;
"unknown field" at the key; control characters escaped in echoed values).
sb-go's `yaml_schema_test.go` cases ported; proto-yaml's many-errors case
copied into the fixtures with every issue found; a valid synthetic config
set passes the pinned schemas.

**Acceptance criteria:**
- [ ] Every rule and quirk has a test that failed against a stub first;
      the many-errors case yields every issue, sorted, with positions; the
      valid set yields none.

**Verification:**
- [ ] `cargo test -p sb-config`; `scripts/check.sh full`.

**Dependencies:** Task 5
**Files:** `crates/config/src/rules.rs`, `crates/config/tests/fixtures/`
**Scope:** M

## Task 7: The sync validators and secrets

**Description:** The type validators and their `validate_*` names, sb-go's
messages and its `custom_validators_test.go` tables ported. Validators
that touch the host take their paths as parameters: `timezone`'s tz
database (the chosen crate, tests pointing it at a fixture zoneinfo),
`rclone_template`'s file (tests use tempdirs); check sb-go for
`directory_path` and `ssh_key_or_url`. The secret fields never appear in
an issue or warning: an issue names the key, never the value. The crate's
cargo-deny run repeated for the tz crate.

**Acceptance criteria:**
- [ ] Every validator's cases (sb-go's included) pass without reading host
      paths; the password warning is a warning.
- [ ] No secret value in any issue or warning (a known-secret test per
      field); the crate's cargo-deny run passes.

**Verification:**
- [ ] `cargo test -p sb-config`; `scripts/check.sh full`.

**Dependencies:** Task 6
**Files:** `crates/config/src/validators.rs`, `crates/config/src/secret.rs`
**Scope:** M

## Task 8: The HTTP fake, the client and Docker Hub

**Description:** The fake first: a local HTTP server (axum, as sb-docker's)
behind an `sb-config` `testing` feature, scripted per test (answers,
Cloudflare's error JSON, 429 and 5xx with `Retry-After` and
`x-should-retry`, held answers), with its own tests; it also exposes the
fixture sets (copied into a tempdir per test, as sb-docker's fixtures). The
client: one reqwest client, rustls with the system's roots, bodies read up
to 1 MiB, remote text escaped and cut at 2000 characters; the check API
takes its base URLs, timeouts and a proxy setting as parameters: `System`
(reqwest's own handling of the proxy variables, `NO_PROXY` and lowercase
names included; production), `Off` (unit tests; setting variables in-process
needs `unsafe`, which the workspace forbids) or `Via(url)` (the proxy test),
cited from reqwest's proxy docs. Only sb reads `CLOUDFLARE_BASE_URL` and
`SB_DOCKERHUB_URL`; it reads no proxy variable itself. So no unit test can
reach a real service or a developer's proxy. Docker Hub: structure checks as
accounts.yml issues, the login with an encoded JSON body (a password with
`"` and `\`), no retries, `message` or `details`, a timeout. The crate's
cargo-deny run with `--all-features`.

**Acceptance criteria:**
- [ ] The fake's behaviours tested; Docker Hub's outcomes against it
      (success, failure text, the encoding, a timeout, skipped without
      credentials); the body cap and the text cut tested; a proxy test:
      the client, given the fake as its proxy, sends `CONNECT
      <host>.invalid:443` for an `https://` request (the fake refuses it,
      so no TLS is needed).
- [ ] No token in any output, error or log record (a known-secret test);
      the crate's cargo-deny run passes.

**Verification:**
- [ ] `cargo test -p sb-config`; library-reviewer on the diff;
      `scripts/check.sh full`.

**Dependencies:** Task 5 (beside Tasks 6-7)
**Files:** `crates/config/Cargo.toml`, `crates/config/src/http.rs`,
`crates/config/src/dockerhub.rs`, `crates/config/src/testing.rs`
**Scope:** M

## Task 9: The Cloudflare check

**Description:** Structure checks as accounts.yml issues (a mixed or
half-set credential, no `user.domain`); the zone from the chosen
public-suffix crate (both sections; a private-section domain tested);
`/user` for a global key, then zones and SSL; sb-go's guidance after any
zone or SSL failure (refused, not found, network error, timeout), with the
status and Cloudflare's messages, escaped and cut; SSL `flexible` or `off`
an error; cloudflare-go's retries (two more attempts with backoff on
connection errors, 408, 409, 429 and 5xx, honouring `Retry-After` and
`x-should-retry`), backoff injectable.

**Acceptance criteria:**
- [ ] Each outcome against the fake, each failure's guidance text, the
      retries and their limits, flexible SSL, a timeout; skipped without
      credentials.
- [ ] No token in any output, error or log record (a known-secret test);
      the crate's cargo-deny run (`--all-features`) passes with the
      public-suffix crate.

**Verification:**
- [ ] `cargo test -p sb-config`; library-reviewer on the diff;
      `scripts/check.sh full`.

**Dependencies:** Task 8
**Files:** `Cargo.toml`, `crates/config/Cargo.toml`,
`crates/config/src/cloudflare.rs`, `crates/config/src/testing.rs`
**Scope:** M

## Task 10: The validation run in sb-config

**Description:** The file table in sb-go's order (`crates/config/src/run.rs`):
a missing required file or schema as that file's issue, motd.yml and
localhost.yml skipped and logged when missing, localhost.yml duplicate keys
only, a file over 1 MiB an issue; the credential checks run after
accounts.yml's own validation and only for a section with no issues of its
own (an issue elsewhere in accounts.yml doesn't skip them), skipped without
credentials (the notice text). The run drives term-ui's steps itself
(sb-config depends on sb-term-ui): the parent step, a step per file with
its issues as the failure block, the credential checks as Task 2's group
under accounts.yml's step, notices and warnings; sb's command only builds
the runner and calls it, so nothing is decided twice. It logs each file,
its issues, each check's outcome and timing, never a secret (the log test
in its own test binary, as tracing caches callsite interest). Unit tests
on tempdir fixture sets through term-ui's in-memory target.

**Acceptance criteria:**
- [ ] Each table row and the check gating tested on fixture sets, failing
      against a stub first; the log records present with no known secret.

**Verification:**
- [ ] `cargo test -p sb-config`; `scripts/check.sh full`.

**Dependencies:** Tasks 7, 9
**Files:** `crates/config/Cargo.toml` (sb-term-ui's `testing` feature
for tests), `crates/config/src/run.rs`, `crates/config/tests/run_log.rs`,
`crates/config/tests/fixtures/`
**Scope:** M

## Task 11: `sb validate-config` in sb

**Description:** The command (sb-go's help texts, no arguments, no
`--verbose`, `needs_root`); sb's runtime built after parsing for it too,
within the startup budget; the runner titled "sb validate-config", handed
to Task 10's run with the Saltbox root from Task 3's one function (the
same for the theme and validation), the HTTP settings from the
environment; exit 0 or 1.
`common::sb()` (and the nobody helper) set `CLOUDFLARE_BASE_URL` and
`SB_DOCKERHUB_URL` to a loopback port bound and closed (refused at once;
another test could rebind it in between, a low risk noted in the helper)
unless a test points them at its fake, and remove `HTTP_PROXY`,
`HTTPS_PROXY`, `ALL_PROXY` (each case), `NO_PROXY`, `SSL_CERT_FILE` and
`SSL_CERT_DIR`. The basic end-to-end cases (root-gated): a valid fixture
set (exit 0), a broken one (exit 1). reqwest, rustls, serde-saphyr, regex
and the two crates first enter sb's binary here: this commit carries the
size ADR (Proposed), attributing each, and check.sh's musl step follows
the route picked at Checkpoint A; cargo-deny now checks the trees through
sb.

**Acceptance criteria:**
- [ ] The valid and broken fixture runs pass through the binary; `sb
      validate-config -v` exit 2; the contract test passes.
- [ ] `sb --help` within its ratchet, `sb version` within 10 ms; the size
      ADR written; `cargo deny check` passes; the musl step works.

**Verification:**
- [ ] `scripts/check.sh full` (root tests included); library-reviewer on
      the diff.

**Dependencies:** Tasks 2, 3, 10
**Files:** `Cargo.lock`, `crates/sb/Cargo.toml`, `crates/sb/src/main.rs`,
`crates/sb/tests/common/mod.rs`, `crates/sb/tests/nobody/mod.rs`,
`crates/sb/tests/validate_config.rs`, `docs/decisions/` (the size ADR),
`scripts/check.sh` and `CONSTRAINTS.md` (the musl route, if it changes
them)
**Scope:** M

## Checkpoint B: The command works
- [ ] `scripts/check.sh full` passes.
- [ ] The maintainer runs a release build on a real host's config
      (standalone binary, `SB_LOG_DIR` set).

## Task 12: End-to-end: the runner's look, failures, signals, the log

**Description:** Through the built sb, root-gated, on fixtures and the
fake: the steps with the credential checks concurrent and both finishing
when one fails, warnings and skipped checks as notices, each failed file's
block (term-ui criterion 4 with sb's own command); SIGINT and SIGTERM while
a check is held open, on a PTY, within seconds: 130 / 143, terminal
restored (term-ui criterion 6); piped plain output, NO_COLOR and
`--color=never`; the run log's records and no known secret in it or any
output. Records whether this takes cli-core's rexpect smoke test or keeps
it carried, with the reason.

**Acceptance criteria:**
- [ ] Success criteria 5-9 covered through the binary; term-ui's carried
      criteria 4 and 6 closed; the rexpect decision recorded.

**Verification:**
- [ ] `scripts/check.sh full`.

**Dependencies:** Checkpoint B
**Files:** `crates/sb/tests/validate_config.rs`,
`crates/sb/tests/validate_config_signals.rs`
**Scope:** M

## Task 13: Success criteria, parity and measurements

**Description:** Walk the spec's success criteria with evidence; the
end-to-end suite against the production musl binary (`SB_TEST_BINARY`);
measure startup and the binary and complete the size ADR. The parity run,
recorded so the maintainer can repeat it: sb-go's side effects checked
first (logging, update checks, its sudo relaunch dropping
`CLOUDFLARE_BASE_URL`), then sb-go as root in `unshare -m
--propagation private` with a fixture set bind-mounted over
`/srv/git/saltbox`, a marker file from the fixture set checked at that path
inside the namespace before sb-go starts (abort otherwise), any log
location in a temporary directory, its Cloudflare check at the fake, no Docker Hub
credentials in that set; differences only the spec's list. The maintainer
runs it on a real host's config (success criterion 11).

**Acceptance criteria:**
- [ ] Each success criterion has a test or a recorded check; the parity
      procedure and result recorded.
- [ ] Size and startup recorded; the size ADR complete for acceptance.

**Verification:**
- [ ] `scripts/check.sh full`; the maintainer's run.

**Dependencies:** Task 12
**Files:** `tasks/todo.md`, `docs/decisions/`
**Scope:** S

## Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes.
- [ ] Spec success criteria met or carried by name; the maintainer has run
      `sb validate-config` on real config.
