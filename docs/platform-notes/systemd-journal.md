# systemd's journal, as sb's reader relies on it

Behaviour verified while building `prototypes/proto-journal`, on
2026-11-08. Each claim was checked one of two ways:
- **By source:** systemd's sources at the tags named.
- **By container runs:** unprivileged `ubuntu:22.04` (systemd 249.11),
  `24.04` (255.4), `26.04` (259.9), `18.04` (237) and `20.04` (245)
  containers. Each used that release's own `systemd-journald` (run
  standalone, not as PID 1), `systemd-journal-remote` and `journalctl`.
  The checks are compare.sh, compat.sh, follow.sh and stress.sh.

## Matching (`journalctl -u`)

- **249 and 255** (logs-show.c, `add_matches_for_unit`): any of
  - `_SYSTEMD_UNIT=u`;
  - the coredump `MESSAGE_ID` with `_UID=0` and `COREDUMP_UNIT=u`;
  - `_PID=1` and `UNIT=u`;
  - `_UID=0` and `OBJECT_SYSTEMD_UNIT=u`.
- **257 and later** (`add_matches_for_unit_full`, in the v257 and v259
  tags): the coredump branch accepts the `systemd-coredump` user's uid
  or 0. It has no uid term at all when that user doesn't resolve (or
  resolves to 0), or when the files are given with `--file` or `-D`
  (journalctl-filter.c).
- **259.1 and later:** the PID 1 branch matches
  `_SYSTEMD_CGROUP=/init.scope` instead of `_PID=1`. Checked in
  logs-show.c at the systemd/systemd tags v259.1 to v259.9; the v259 tag
  itself still has `_PID=1`. So the rule changed inside a stable point
  release, and an Ubuntu stable update can change journalctl's unit
  match within an LTS. Through main (v263 in development) it is
  unchanged.
- **Root in a container** that can reach journald's sockets logs with
  `_UID=0`, so it matches the `_UID=0` groups, for journalctl and for sb
  alike.
- **Matches on the same field are ORed** by sd-journal; different fields
  are ANDed.

## Writing

- **Format flags:** 249 writes keyed hash and zstd (`0xc`), and ignores
  the per-file compression environment variable. 255 and 259 add compact
  mode (`0x1c`). 237 and 245 write 240-byte headers, unkeyed, with lz4
  or no compression.
- **Repeated values:** 237 and 245 link a value an entry carries twice
  into its entry array twice (`n_entries` counts both). 249's
  `journal_file_append_entry` keeps duplicate items too (source). 255
  and later drop them.
- **249 splits** some long `systemd-cat` lines into several entries, a
  different number per run.
- **249 writes an entry again** as the first of the next file when a
  rotation interrupts it: same times and XOR hash, the next seqnum, the
  same directory. journalctl shows both.
- **Upgrades:** 255 and 259 refuse to append to an older header and
  rename that file to a `.journal~`. 249 appends to it, then archives
  it.
- **Flush:** after a flush to /var, the archived runtime file in /run
  holds copies of the flushed entries. sd-journal shows an entry once
  when its location compares equal (same boot, monotonic time, realtime
  and XOR hash), keeping the copy from the file it opened first (/run
  before /var).
  - 255's flush gives the copies the runtime file's seqnum id with new,
    higher seqnums, so sd-journal's order calls them newer. When
    `/var/log/journal` exists before it starts, `journalctl -f` 255 then
    prints each flushed entry twice.
- **Wake-ups:** journald truncates a file to its own size after writing,
  on a timer, to raise inotify events. Followers wake on the order of
  journald's 250 ms timer after a write; measured in the containers:
  p50 140-150 ms, p95 about 260 ms.

## Reading

- **Symlinked journal files are followed:** v249 sd-journal.c's
  `dirent_is_journal_file` accepts `DT_LNK` entries, and they are opened
  with a plain `open`.
- **`journalctl -f -n 0` seeks to the end only once it has started.**
  Writes made before that are lost to it.
- **journalctl 259 doesn't watch a journal root directory** that didn't
  exist when it started. Entries written after a mid-follow flush that
  creates `/var/log/journal` are lost to it.
- **`journalctl -o json`:**
  - without `--all`, a field of 4096 bytes or more is `null`;
  - a value that isn't printable UTF-8 (ESC counts as a control) is an
    array of byte values;
  - a repeated field is an array of its values;
  - `_BOOT_ID` is printed once, from the entry header;
  - v255 and later add `__SEQNUM` and `__SEQNUM_ID`;
  - without `-q`, v249 prints `-- Journal begins at … --`.
- **zstd payloads:** systemd checks the frame's declared content size
  (`decompress_blob_zstd`). ruzstd checks neither the size nor a
  checksum, so a reader must check the size itself.
- **`journalctl --after-cursor`** (verified 2026-11-10, sb-journal Task
  5, by container runs of crates/journal/tests/compare.sh and compat.sh
  on `ubuntu:22.04` (systemd 249.11-0ubuntu3.23), `24.04`
  (255.4-1ubuntu8.18) and `26.04` (259.9-0ubuntu0.1), and by source at
  the tags named):
  - 249 seeks and then always steps once past its landing
    (`sd_journal_next_skip(j, 1 + after_cursor)`), so after a cursor whose
    entry doesn't match the query, or has gone (another seqnum id), the
    first matching entry after it is lost.
  - 255.4 and 259.9 step past the landing only when it is the cursor's
    own entry (`sd_journal_test_cursor`), and lose nothing there. The
    v255 tag still has the 249 code; systemd-stable v255.1 cherry-picked
    the change ("journalctl: don't skip over messages not matching the
    cursor", systemd-stable commit d68d9019c483, upstream 4207a5577a37,
    issue #30288). Ubuntu's 255.4 builds on that stable release, so the
    change is in its upstream tarball, not in debian/patches, and its
    changelog doesn't mention it.
  - Each file is sought by one key: seqnum in the cursor's seqnum space,
    else monotonic time in the cursor's boot, else realtime. On a file
    whose realtime steps back (written by 249's journal-remote:
    10, 50, 5 and 45 s; a cursor at 40 s of another boot), 255.4 and 259.9
    land on 50 s and show 50, 5, 45; 249 shows 5, 45.
  - Over two boots of one machine in two files whose clocks overlap
    (a1, a2 at 100 and 300 s; b1, b2 at 200 and 400 s), 255.4 and 259.9
    order the whole a1, a2, b1, b2 (boot table) and 249 a1, b1, a2, b2.
    After a2 both 255.4 and 259.9 show only b2; after b1, 255.4 shows
    a2, b1, b2 (the lowest landing first, a2, which isn't the cursor's
    entry) and 259.9 shows b2.
  - 259.9 differs from the v259 tag there: systemd-stable v259.6 to
    v259.9 added "sd-journal: prefer exact cursor match after seek"
    (issue #31516), which takes the file whose landing is the cursor's
    exact entry (seqnum id and seqnum) as the first step. v259.9 also
    added "sd-journal: prefer machine ID from the tail entry" (issue
    #34169; systemd's v259-stable branch, commit 2d8b0b6a0aaf, cherry-picked
    from upstream ae940730c6f2; v259.8 doesn't have it, and neither does
    255.4). `journal_file_read_tail_timestamp` then sets the file's
    `newest_machine_id`, which `compare_boot_ids` compares, from the tail
    entry's `_MACHINE_ID=` (`journal_file_entry_get_machine_id`): it reads
    the entry's items in order and takes the first `_MACHINE_ID=` DATA
    object, passing over items it can't read as DATA objects (`-EBADMSG`,
    `-EADDRNOTAVAIL`). It keeps the header's id when the entry has none,
    the first one isn't 32 characters of a non-null id, another read
    error ends the walk, or the times came from an archived file's header
    rather than an entry. journal-remote writes the receiving machine
    into each file's header and keeps each entry's own `_MACHINE_ID=`, so
    on 259.9 imports of several machines interleave by realtime where
    255.4 orders them by boot. Checked by source only (the v259.9 tag's
    src/libsystemd/sd-journal/sd-journal.c, and the commit's own test,
    `merge_interleaving` in test-journal-interleaving.c); the matrix's
    fixtures carry no `_MACHINE_ID=`.
- **The coredump term of `-u` on the local journal** (same runs): with
  the `systemd-coredump` package installed, 259.9 takes coredump
  entries of that user's uid or root's; 249 and 255 take root's only.
- **The varlink API:** v260 and later serve
  `io.systemd.JournalAccess.GetEntries`, with no cursor; v262 adds
  `follow`. It is not available on 22.04 to 26.04.
