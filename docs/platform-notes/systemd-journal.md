# systemd's journal, as sb's reader relies on it

Behaviour first verified while building the proto-journal prototype
(2026-11-08; its findings are in
[docs/prototypes/proto-journal.md](../prototypes/proto-journal.md)), and
verified again against sb's reader, the `sb-journal` crate, in its plan's
Tasks 5 to 11 (2026-11-10 and 2026-11-11). Each claim was checked one of
two ways:
- **By source:** systemd's sources at the tags named.
- **By container runs:** unprivileged `ubuntu:22.04` (systemd
  249.11-0ubuntu3.23), `24.04` (255.4-1ubuntu8.18), `26.04`
  (259.9-0ubuntu0.1), `18.04` (237) and `20.04` (245) containers. Each
  used that release's own `systemd-journald` (run standalone, not as
  PID 1), `systemd-journal-remote` and `journalctl`. The checks are
  crates/journal/tests/compare.sh, compat.sh, follow.sh and stress.sh
  (the prototype ran their first versions); the last run, on the crate
  after its security audit, passed all of them on 22.04, 24.04 and 26.04,
  with the 18.04 and 20.04 files read through compat.sh.

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
  into its entry array twice (`n_entries` counts both). The v249 tag's
  `journal_file_append_entry` keeps duplicate items too, but 22.04's
  249.11 drops them, as 255 and later do: systemd-stable v249.11's
  journal-file.c has `remove_duplicate_entry_items`, and 22.04's source
  package (249.11-0ubuntu3.23) is that file unpatched (checked
  2026-11-12, below).
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
    prints each flushed entry twice. Where the /run file stays after the
    flush (a container's overlayfs /run; on a host /run is tmpfs and
    journald removes it), sb's page shows the flushed entries twice on
    255 (follow.sh, 2026-11-13). Its merge is `journalctl -n`'s loop, so
    by the same order journalctl does too (read from the code, not run).
- **New objects:** journald raises the header's `arena_size` before it
  writes a new object's header and links it (`journal_file_append_object`
  calls `journal_file_allocate` first; journal-file.c at the v249, v255
  and v259 tags). A reader that sized its reads by an earlier header can
  therefore find a linked object past the arena it knew, and has to read
  the header again before reading that object whole. sb-journal Task 11's
  security audit hit this with a race probe reading a live journal while
  journald wrote (fixed in the crate's `object_head`).
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
    255.4 orders them by boot. Checked by source (the v259.9 tag's
    src/libsystemd/sd-journal/sd-journal.c, and the commit's own test,
    `merge_interleaving` in test-journal-interleaving.c), and since
    sb-journal Task 6 by container runs (compare.sh's `machines`
    fixtures, `ubuntu:22.04` 249.11-0ubuntu3.23, `24.04`
    255.4-1ubuntu8.18, `26.04` 259.9-0ubuntu0.1): two boots in two files
    written by that release's journal-remote, a1 and a2 at 100 and 300 s
    naming one source machine, b1 and b2 at 200 and 400 s naming another,
    both headers this machine. 259.9's journalctl orders them a1 b1 a2
    b2 (realtime), 255.4's a1 a2 b1 b2 (the boot table), 249's a1 b1 a2
    b2; with `_MACHINE_ID=` left off each file's last entry, 259.9 orders
    them a1 a2 b1 b2 too (the header's machine). The forward pages then
    match sb-journal's where realtime orders them, and Difference 20
    where the table does.
- **One boot in two seqnum spaces, its clock set back between them**
  (sb-journal hardening, Task 37; 2026-11-13; container runs on the same
  three images, files written by each release's journal-remote, which
  gives each output file its own seqnum id). File b holds b1 and b2 at
  monotonic 1.001 and 1.002 s, realtimes 300 and 400 s; file x holds x1
  to x4 at monotonic 1.003 to 1.006 s, realtimes 10 to 40 s; one boot
  id. All three order the whole b1 b2 x1 x2 x3 x4. `--after-cursor`
  x3: 249.11 and 259.9 show x4; 255.4 shows b1 b2 x3 x4 (file b sought
  by realtime once its monotonic bisect finds nothing, that landing the
  lowest, and shown as it isn't the cursor's entry). Paging one entry
  at a time from x1, 249.11 and 259.9 end at x4, and 255.4 goes b1 b2
  x1 b1 b2 x1 ... for ever. The same in either `--file` order. After
  b1 or b2 all three show the later entries. The same again filtered
  by `_MACHINE_ID=`, with file b also holding c1 (monotonic 1.007 s,
  realtime 500 s) without that field, where the query's monotonic seek
  in file b finds c1's place but no matching entry from it. 259.9's
  answer comes from its exact-cursor first step (issue #31516, above).
  sb-journal follows 249 and 259.9 there on every row (Difference 21).
- **The coredump term of `-u` on the local journal** (same runs): with
  the `systemd-coredump` package installed, 259.9 takes coredump
  entries of that user's uid or root's; 249 and 255 take root's only.
- **A damaged entry-array item** (Checkpoint B of the hardening plan,
  2026-11-13; journalctl 249.11, 255.4 and 259.9 from the releases'
  `systemd` packages, in fresh `ubuntu:22.04`, `:24.04` and `:26.04`
  containers). A 300-entry file written by systemd-journal-remote 249
  (crates/journal/tests/seeds.sh's `default` variant) had item 13 of the
  middle of its five main entry arrays (26 items) set to the offset of
  a DATA object, the array's first entry's first item. Read with
  `journalctl --file F -o json --no-pager`:
  - `--after-cursor` the entry after the damaged slot (seqnum 0x1b):
    no entry and no message, exit 0, on all three;
  - `--after-cursor` the entry before it (0x19): 273 entries on 249,
    274 on 255 and 259; the whole file: 298 on 249, 299 on 255 and 259;
  - `-n 10` and `-n 100` print 10 and 100 entries; `-n 500` prints
    none, on all three.

  sb-journal's comparison CLI on the same file: no entry after 0x1b
  (with "object ... is type 1, wanted 3" reported), 274 after 0x19, and
  a tail of 286 with that error and "entry array ... is out of order"
  (Difference 19).
- **The varlink API:** v260 and later serve
  `io.systemd.JournalAccess.GetEntries`, with no cursor; v262 adds
  `follow`. It is not available on 22.04 to 26.04.

## The order journald links an entry in, for a reader of a live file

What a page and the follow after it can rely on while journald writes
(sb-journal hardening, Task 32; verified 2026-11-12). Checked three ways:
- **By source:** systemd's tags v249, v255 and v259, and the stable
  tags the releases build on, systemd-stable v249.11 and v255.4 and
  systemd v259.9. Line numbers are the v249, v255 and v259 tags', in
  src/libsystemd/sd-journal/journal-file.c unless another file is named.
- **Against the packages:** in unprivileged `ubuntu:22.04`, `24.04` and
  `26.04` containers, with deb-src enabled, `apt-get source systemd`,
  which unpacks the source package and applies its patches. The files
  cited here were compared with the tags byte for byte, and
  debian/patches was searched for patches that touch them.
- **By a run under writes:** a reader racing that release's own
  journald, below.

### The link order

- **An entry's items are written before it is linked anywhere**
  (`journal_file_append_entry_internal`, v259 2390-2399). A full fence
  separates them from the links: `__sync_synchronize()` on v249 (1942),
  `__atomic_thread_fence(__ATOMIC_SEQ_CST)` on v255 (2230) and v259
  (2251), at the top of `journal_file_link_entry`.
- **A main-list item is written, then `n_entries` raised**
  (`link_entry_into_array`): v249 1827-1828 and 1857-1872; v255
  2100-2101 and 2132-2150; v259 the same function, at 2092.
- **The entry is linked into the main list and counted, then the
  header's tail fields are set, then each value's list is linked**
  (`journal_file_link_entry`): v249 1944-1966; v255 2232-2266; v259
  2253-2286.
- **A value's item is written, then that list's count raised**
  (`link_entry_into_array_plus_one`): v249 1892-1906; v255 2175-2189.
- **The compact and regular layouts link in the same order:** one
  `link_entry_into_array`, with `write_entry_array_item` choosing the
  item's width (v255 2060 and 2089-2103).
- **So, with journald appending entry by entry, only the newest counted
  entry can be half linked for a reader:** counted in the header, and
  not yet in all of its values' lists. The run below finds that case
  under load, and finds no other.
- **The exception: a link that fails stays partly linked for good.** v249
  returns at the first item that fails (1962-1965). v255 and v259 go on
  past `-E2BIG` and return other errors (2257-2265; 2277-2285). That is
  the journal spec's Difference 12.
- **Entry offsets rise with the main-list index** in files journald
  writes, since each object is appended at the tail
  (`journal_file_append_object`: v249 1069; v255 1242-1259; v259
  1250-1267).

### journalctl on a file being written

- **Without `--follow` it reads the live counts at each step:** the
  header's (`journal_file_next_entry`: v249 2902; v255 3532; v259 3551)
  and a value's (v249 2975, `journal_file_next_entry_for_data`; v255
  3234 and v259 3253, `generic_array_bisect_for_data`). So it shows
  entries written after it opened the file.
- **With `-n N` it seeks to the tail once and shows at most N lines**
  (v255 src/journal/journalctl.c 2510-2514 and 1958; v255.4's 2520-2524
  are the same).

### Flush

- **A flush copy keeps the entry's boot id and both times**
  (`journal_file_copy_entry`: v249 3882-3955; v255 4436-4497; v259
  4453-4515). Its xor hash is computed again from the payloads of the
  original's stored items, so it is the original's, with two exceptions
  (checked by source at v255, 2026-11-12):
  - **An entry written with a duplicate `FIELD=value`:** the original's
    hash runs over every field it was given, so the two copies of the
    duplicate cancel out (journal-file.c `journal_file_append_entry`, v255
    2590-2593). Its stored items drop the duplicate
    (`remove_duplicate_entry_items`, 2604, on 255 and later and on 22.04's
    249.11), and the copy's hash runs over those, so it differs.
  - **An item whose DATA object is bad** (`-EADDRNOTAVAIL`, `-EBADMSG`)
    is skipped by the copy (v255 4463-4466) and left out of its hash.

  sb-journal matches a flush copy by these stamps and that hash, so such
  a copy isn't matched, and is shown again.
- **/run is removed only when every entry iterated was copied**, on all
  three releases. The flush walks the runtime journal with
  `SD_JOURNAL_FOREACH`, and any read or write error jumps to `finish`,
  where `rm_rf` of the runtime directory runs only when `r >= 0`:
  - v249 src/journal/journald-server.c `server_flush_to_var`, 1115-1220
    (loop 1152, `finish` 1195, `rm_rf` 1202);
  - v255 journald-server.c, same function, 1252-1373 (1289, 1347, 1354);
  - v259 src/journal/journald-manager.c `manager_flush_to_var`, 1301-1453
    (loop 1347, `finish` 1405, `rm_rf` 1418). v259 also removes every
    other runtime directory the walk read (1430-1435).
  - v259 differs before the walk: it closes the runtime journal first
    (1329) and resets the seqnum state (1333, issue #30092), and it opens
    the runtime files with `SD_JOURNAL_ASSUME_IMMUTABLE`.
  - The function is the same at v249.11 (22.04's package, below), v255.4
    and v259.9.
  - The removal is `rm_rf(..., REMOVE_ROOT)` without `REMOVE_PHYSICAL`,
    which refuses a directory on a disk file system (src/shared/rm-rf.c,
    `is_physical_fs`: anything but tmpfs, ramfs and cgroupfs, at v249 and
    v259). On a host /run is a tmpfs. In the matrix containers it is
    overlayfs (`stat -f` on /run/log/journal in `ubuntu:22.04` and
    `ubuntu:26.04`). There, after `journalctl --flush` of a volatile
    standalone journald, 22.04 (249.11-0ubuntu3.23) kept /run's
    system.journal, and 26.04 (259.9-0ubuntu0.1) removed it, which this
    reading doesn't explain (follow.sh's `between-flush`, 2026-11-12).

### The packages against the tags

- **22.04, systemd 249.11-0ubuntu3.23:** journal-file.c and journalctl.c
  are systemd-stable v249.11's, byte for byte. journald-server.c is
  v249.11's plus two Ubuntu patches,
  `debian/Re-enable-journal-forwarding-to-syslog.patch` and
  `debian/Don-t-enable-audit-by-default.patch`. They change two of
  `server_init`'s defaults, not the flush. v249.11 differs from the v249
  tag in journal-file.c only in `journal_file_append_entry_internal`'s
  failure path (the tag reverts the entry's seqnum when a link fails;
  v249.11 doesn't) and in dropping duplicate items. Every function cited
  above is unchanged.
- **24.04, systemd 255.4-1ubuntu8.18:** journal-file.c, journald-server.c
  and journalctl.c are systemd-stable v255.4's, byte for byte. None of
  the package's 85 patches touches them. v255.4 differs from the v255
  tag in journal-file.c only in `journal_file_enable_post_change_timer`,
  and in journalctl.c only in the `--after-cursor` step (Difference 18).
- **26.04, systemd 259.9-0ubuntu0.1:** journal-file.c,
  journald-manager.c, journalctl.c and journalctl-show.c are systemd
  v259.9's, byte for byte. None of the package's 19 patches touches
  them. v259.9 differs from the v259 tag in journal-file.c in the hash
  table's setup, `journal_file_append_data`, decompression's size bound,
  the bisect's `test_object_*` functions (a zero item is `-EBADMSG`, not
  an assertion) and `journal_file_open`. None of these is cited above.

### The run under writes

On each release, journald standalone (Storage=persistent, one file, no
rotation, no rate limit) took 40,000 lines from each of four
`systemd-cat` writers, three with one `SYSLOG_IDENTIFIER` and one with
another: 160,000 entries. Meanwhile a reader (sb-journal's own file
layer) looped on the active file:
1. read the header (n0) and the main list up to it;
2. looked up the shared `SYSLOG_IDENTIFIER=` value and took the newest
   64 entries of its list;
3. read the header again (n1) and the main list up to it.

It checked three things:
- every entry found in the list is in the main list below n1, which
  would fail if a value's list could get an entry before the header
  counted it;
- whether the newest entry counted at n0 carried the value and was
  missing from the list read after it (the half-linked entry above);
- every earlier counted entry inside the list's window that carried the
  value was listed.

As a check that the race was really exercised, it also counted the
listed entries past n0 (written during the round).

| Release | Rounds | Listed entries checked | In a list, not counted | Past n0 (race seen) | Newest counted, not yet listed | Earlier counted, not listed |
|---|---|---|---|---|---|---|
| 22.04 (249.11) | 3,001,963 | 192,001,837 | 0 | 1,369 | 8,214 | 0 |
| 24.04 (255.4) | 4,547,056 | 290,887,931 | 0 | 10,450 | 632 | 0 |
| 26.04 (259.9) | 13,295,210 | 850,808,353 | 0 | 8,368 | 4,394 | 0 |

So:
- An entry found in a value's list is always counted by a header read
  after the lookup.
- An entry in a list can be past a header read before it (the double
  that hardening finding follow-H1 describes).
- The newest counted entry is often not yet in its value's list (the
  mirror case, follow-N1): 0.01% to 0.27% of rounds under this load, on
  every release.
- No entry before the newest was ever found half linked.

The run doesn't prove the order. It failed to refute it hundreds of
millions of times on each release, with the race shown to occur.

On 22.04, standalone journald kept writing the /run file after
`journalctl --flush` in this setup, so that file was the one read there;
on 24.04 too; on 26.04 the /var file.

### The one assumption no reading settles

A reader's `pread` sees journald's stores, made through a shared
`mmap` of the same file, in program order. Both go through the same
page cache, and journald's fence orders the entry's items before its
links. The store order of the links among themselves (main list, count,
values' lists) on another CPU rests on the architecture. x86-64's
memory model (total store order) keeps stores in program order to
every other CPU, so the order above holds there. That is the only
architecture sb ships for. On a weaker model it would need fences
journald doesn't issue between those stores.
