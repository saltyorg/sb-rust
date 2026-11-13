# Reading the journal through libsystemd

Checked 2026-11-12 for the maintainer's question whether sb should read
the journal through libsystemd instead of its own reader, sb-journal
(ADR-013). The answer was to keep sb-journal and the static musl
binary (hardening plan, decision 27). This note keeps the evidence, so
the question isn't reopened without new facts.

## The crates

- **`libsystemd` 0.7.2** (pure Rust, docs.rs/libsystemd/0.7.2): modules
  activation, credentials, daemon (notify, watchdog), id128, logging,
  sysusers, unit. Its `logging` module only sends entries to journald's
  socket; it has no journal reader (no open, match, seek, cursor or
  wait). A reading pull request (lucab/libsystemd-rs #40, 2019) was not
  merged; issue #39 asking for `sd_journal_*` is open.
- **`systemd` 0.11.1 / `libsystemd-sys` 0.10.0** (github.com/codyps/rust-systemd,
  bindings to the C library): wraps open, the matches, every seek,
  next and previous, data, cursors, `query_unique` and the wait calls.
  Not wrapped or not declared: `sd_journal_step_one`, `get_seqnum`,
  `enumerate_available_data`, the fd opens. `Journal` is `!Send` and
  `!Sync`, as sd-journal requires one owning thread. Its entry
  iteration decodes lossily, and three places panic on journal content
  (a non-UTF-8 field in `Display`, a data object without `=`, a failed
  fd). The default data threshold truncates fields over 64 KiB.
  Licence LGPL-2.1-or-later WITH GCC-exception-2.0. One maintainer.

## Measured

Method: a glibc build of sb and a small probe on the `systemd` crate,
built in an `ubuntu:22.04` container (glibc 2.35) and run in
`ubuntu:22.04`, `ubuntu:24.04` and `ubuntu:26.04` containers with
journald started as `crates/journal/tests/` starts it.

- **The glibc build of sb** builds with aws-lc-sys, needs GLIBC_2.34 at
  most, links only libc, libm, libgcc_s and the loader, and runs on all
  three releases. It is 118 KB smaller than the musl binary; `sb --help`
  starts 0.1 to 0.6 ms slower, with about 2.4 MB more peak memory.
- **Paging** through libsystemd matches journalctl cursor for cursor
  (tail and one page back of a unit), and it reads every writer variant,
  `.journal~` files and older releases' files. Compact files from 24.04
  or 26.04 fail on 22.04 with EPROTONOSUPPORT, as for journalctl 249.
- **Following** on 255 and 259 needs `sd_journal_step_one`, which a
  binary built against 249 can't link; without it a filtered follow
  from the tail returns nothing. Reached through `dlsym`, it needs
  `unsafe`, which sb's workspace forbids.
- **Across a flush** the follow inherits journalctl's behaviour: 255
  doubles entries and 259 loses them. With `/run/log` on tmpfs, as on a
  host, a reader opened before `/var/log/journal` existed lost 3,500 to
  3,800 entries on all three releases; journalctl 259 lost the same.
- **SIGBUS:** sd-journal reads through mmap; only journalctl installs a
  handler, which libsystemd doesn't export, so a truncated file would
  end the process.
- `-u` unit matching is built by journalctl itself (logs-show.c), not
  libsystemd; a libsystemd reader would still carry sb's per-release
  rules.
- libsystemd0 is present on every supported release (apt depends on it).

## Consequence for sb-journal

The matrix containers' `/run` is overlayfs, where journald keeps the
/run file after a flush (`rm_rf` refuses a physical file system); on a
host `/run` is tmpfs and the file goes. Task 39's flush round runs with
a tmpfs `/run/log` too, so sb-journal is tested where readers lost
entries.
