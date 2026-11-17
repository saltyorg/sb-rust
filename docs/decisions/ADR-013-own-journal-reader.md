# ADR-013: Own the systemd journal reader

## Status

Accepted (maintainer: owning the reader as its own crate, 2026-11-08; the sb-journal spec, 2026-11-09)

## Date

2026-11-09

## Context

`sb logs` reads a service's journal: its newest entries, pages back and
forward, and new lines as they arrive. sb-go polls `journalctl -o json
--cursor` (cmd/logs.go). CLAUDE.md's library-first rule says sb uses a
maintained dependency for this kind of I/O rather than writing its own,
and diverging needs evidence. The host spec (decisions 4 and 6) chose a
small pure-Rust reader of the journal files, proven first in
`prototypes/proto-journal` (removed in the plan's Task 12; in git
history); the journal spec (docs/specs/journal.md) makes
it the `sb-journal` crate, wrapped by sb-host.

The evidence is proto-journal's FINDINGS.md ("Why not the alternatives",
"How it was checked"), moved to
[docs/prototypes/proto-journal.md](../prototypes/proto-journal.md) in the
plan's Task 12:

- **sdjournal 0.1.22** was run hands-on, and was exact once worked around,
  but:
  - it skips `.journal~` files (left by unclean shutdowns, which
    journalctl reads) by default, with no error, so their entries are
    missing from its output;
  - its follow engine can die on a rotation race, and the subscriber only
    sees a disconnect;
  - it can't page back from a cursor;
  - its `match_unit` isn't journalctl's `-u` rule;
  - it carries a RustSec advisory through ruzstd 0.8;
  - its bus factor is 1;
  - it adds about 640 KB to the binary.
- **netdata's systemd-journal-sdk** reads files through a 32 MiB mmap
  window with a SIGBUS handler, and links the C `zstd` crate. Both go
  against the reader's pread-only, no-SIGBUS, pure-Rust constraint (and
  `unsafe_code = "forbid"`). It wasn't run hands-on.
- **The libsystemd `systemd` crate** can't link into the static musl
  binary (capability map, decision 6), and systemd's musl support is
  "incomplete".
- **A `journalctl` child** is exact by definition, but it is a host CLI
  (the maintainer prefers pure Rust), and its JSON has quirks of its own:
  ANSI lines arrive as byte arrays, fields of 4096 bytes or more are
  `null` without `--all`, `-- Boot --` lines appear without `-q`, and
  `-f -n 0` loses writes made while it starts.

The prototype's own reader was checked value for value against each
supported release's journalctl (22.04: systemd 249, 24.04: 255, 26.04:
259) in containers: writer variants, `-u`, pages back, files from older
releases, the follow across rotations and flushes, and live stress. Its
fuzz harness ran without a panic or a hang. It is about 240 KiB of code
in the static binary, decompressors included.

What stays a library: the hashes' siphash24 (siphasher), the three
decompressors, inotify and
file opening (rustix), the merge (itertools), timestamps (jiff) and the
errors (thiserror). Of the decompressors, only lzma-rust2 without its
`optimization` feature is `forbid(unsafe_code)` crate-wide (deny.toml
bans the feature). lz4_flex with `safe-decode` forbids `unsafe` in its
`decompress_safe` module, the decoder sb calls, but not crate-wide.
ruzstd 0.9.1 uses `unsafe` in its ring buffer and decode buffer. sb's own
code stays `unsafe_code = "forbid"`; the `unsafe` in ruzstd is the
dependency's, under its maintainers' review and RustSec, as for any other
crate in the tree. What is ours: the file format's parsing, with every
offset and size checked, lookup3's `hashlittle2` (its "why not hashers"
note is in `hash.rs`), the walks, the per-release unit rules and the
follow.

## Decision

sb owns its journal reader, `sb-journal`, ported from proto-journal's
tested code (the journal spec and tasks/journal/plan.md). It reads
the files with `pread` only; sb's code never uses mmap or `unsafe`. It
starts no child and supports
exactly the formats and `-u` rules of the releases sb runs on (sb refuses
the others at startup). Where it differs from journalctl, the spec's
"Differences from journalctl" lists it and a test pins it; any other
difference is a bug or a question for the maintainer.

Two of those differences are decisions, not limits:

1. **The coredump rule is stricter than 26.04's journalctl** (spec
   decisions 1 and 4). journalctl 259 matches a unit's coredumps by the
   systemd-coredump user's uid, and drops the uid term when that user
   doesn't resolve or for files given with `--file`/`-D`. sb keeps
   `_UID=0` in those cases, for every source, so a coredump written by
   another user isn't shown as the unit's. The 26.04 matrix expects its
   uid-1000 coredump decoy not to match.
2. **The display keeps every colour and drops every other escape** (spec
   decision 2). journalctl's JSON hands out the raw bytes; sb's viewer
   shows them through one sanitizer (`DisplayText` in sb-term-ui): SGR
   passes when colour is on, and every other escape sequence, C0 other
   than tab and newline, DEL and C1 is dropped. Bidi and zero-width
   characters aren't marked. The crate itself hands out raw bytes.

## Upkeep

- **Each new Ubuntu LTS** is a `RELEASES` row: its VERSION_ID, its
  systemd version's `-u` rule and `-o json` shape, and any new
  incompatible header flag its journald writes (an unknown flag is an
  error naming the file, never a silent skip). The row comes with its
  container-matrix rows, run against that release's journalctl, and with
  sb-host's `SUPPORTED_RUNTIME_RELEASES`, which a test keeps naming the
  same releases.
- **The scheduled matrix** (the plan's CI workflow, daily, with interim
  and devel canaries once the maintainer pushes and authorizes it) runs
  the comparison against current images, so a change in a release's
  journald or journalctl shows up before sb users meet it.
- **A matrix mismatch that isn't a listed difference** goes to the
  maintainer; it is never pinned as a new difference silently.
- The decompressors and siphasher are updated through the usual
  dependency checks (cargo-deny, RustSec).

## Alternatives considered

- **sdjournal:** silently missing `.journal~` entries, a follow that can
  die on rotation, no paging back, a looser unit match, an advisory
  through ruzstd 0.8, a bus factor of 1, and nearly three times the
  binary cost. Rejected.
- **systemd-journal-sdk:** mmap with a SIGBUS handler and C zstd, against
  the pread-only and pure-Rust constraints. Rejected.
- **The libsystemd crate:** doesn't link into the static musl binary.
  Rejected.
- **A `journalctl` child, as sb-go does:** exact by definition and no
  code of ours, but a host CLI, JSON quirks to work around (the 4096-byte
  `null`s, byte-array lines), lost writes at a follow's start, and a
  process per page. Rejected in the host spec's decision 4.

## Consequences

- About 3,800 lines of reader code are ours to maintain, under the
  CONSTRAINTS bar (coverage, mutation score, the fuzz harness), with the
  container matrix as the exactness check.
- Every `Library-check:` line on sb-journal cites this ADR for what
  stays custom.
- A release sb doesn't know can't be read wrongly: sb refuses to run
  there, and the reader refuses unknown header flags.
- The 26.04 coredump rule and the display policy differ from
  journalctl by decision; changing either is a spec amendment.

## Amendments

- 2026-11-12, hardening plan Task 6 (docs-L4): a `RELEASES` row also
  carries the release's `BootRule`, the order between boots its
  journalctl uses (journal spec, decision 11): `Realtime` for 249,
  `NewestTail` for 255, `NewestTailEntryMachine` for 259.9. A new LTS row
  needs its systemd's rule checked against that release's sd-journal
  source and the container matrix, as for the `-u` rule. The decision
  above is unchanged.
- 2026-11-12, hardening plan Task 6 (docs-L4): the reader is about
  4,710 lines, not 3,800 (Consequences): the nine reader modules' code
  lines outside their tests, without blank or comment lines or the
  `testing` feature's modules; re-counted at the hardening plan's
  Checkpoint C. Re-counted 2026-11-17, at that checkpoint, by the same
  method: about 5,150 lines (5,149). The same method gives 4,714 at the
  2026-11-12 figure's commit (619f25a). The decision above is
  unchanged.
- 2026-11-12, hardening plan Task 6 (docs-L8): in decision 2 above,
  carriage return is applied, not dropped: trailing ones are ignored,
  then what follows the last one on its line is kept (journal spec,
  decision 2 and "Display (decision 2)"). The rest of the C0 rule
  stands.
