# node's readFileSync(fd, "utf8") on a pipe

Verified 2026-11-27 for `crates/sb/tests/hangup-systemd.sh`, which reads
cargo's JSON messages from a pipe with node.

**Software:** node 24.22.3 (the official linux-x64 build, the one CI's
actions/setup-node used for `node-version: 24`), glibc 2.35; node's
sources at the tags v24.20.0, v24.21.0, v24.22.0 to v24.22.3, v26.7.0,
v26.8.0, v26.12.1, v26.13.0, v26.13.1 and on `main`.

**Method:**
- Read `ReadFileUtf8()` in `src/node_file.cc` at those tags, and the
  upstream issue and fix linked below.
- A writer that waits for node to start, writes lines of about 1 KiB one
  at a time with a pause between them until more than 64 KiB has gone,
  then writes the rest in one go, piped into
  `node -e 'require("fs").readFileSync(0, "utf8")'`; the same with
  `readFileSync(0)`. Then the same writer feeding the JSON lines of a
  `cargo test --no-run` of the test into hangup-systemd.sh's own node
  program, 100 times, and once each under gdb and under valgrind's
  memcheck, in a throwaway container; and the same lines read from a
  regular file.
- hangup-systemd.sh's `cargo test --no-run --message-format=json | node`
  pipeline as written, 200 times in a row, with each of the two reads.

## What node does

- `fs.readFileSync(fd, "utf8")` (or `"utf-8"`, also as `{ encoding }`,
  and with a path in place of the descriptor) runs
  the C++ binding `readFileUtf8`. Since v24.21.0 ("fs: use sized reads
  for large files in readFileUtf8") it appends reads shorter than 8 KiB
  to a string and, at the first full 8 KiB read, moves to a heap buffer
  of 64 KiB, copying the string into it. A pipe whose writer is slower
  than node, as cargo is, gives short reads; when more than 56 KiB came
  that way first, the copy runs past the 64 KiB buffer, and the next
  read's length underflows and runs further.
- The process then aborts in glibc's checks, at `free()` in
  `node::fs::ReadFileUtf8` (gdb), with `free(): invalid next size
  (normal)`, `double free or corruption (!prev)` or `malloc(): invalid
  size (unsorted)`; valgrind reports an invalid write by `memmove` in
  `ReadFileUtf8`, "0 bytes after a block of size 65,536 alloc'd" in
  `ReadFileUtf8`.
- The paced writer aborted it in every run; cargo's own pipeline in some
  runs, as often as cargo's writes leave node short reads; a regular file
  in none (its reads are full).
- `fs.readFileSync(fd)` with no encoding reads through the JavaScript
  path (`tryReadSync` into a new 8 KiB Buffer per read, joined with
  `Buffer.concat`), not `readFileUtf8`: no abort with either input;
  `.toString("utf8")` decodes the result.
- v24.20.0 and v26.7.0 have none of the sized-read code. v24.21.0 to
  v24.22.3 and v26.8.0 to v26.13.1 have the bug. It is
  https://github.com/nodejs/node/issues/66341, fixed on `main` by
  https://github.com/nodejs/node/pull/66939 ("fs: fix readFileSync utf8
  overflow on short reads", the heap buffer sized to what was already
  read), in no 24.x or 26.x release yet.

## What sb does with it

hangup-systemd.sh reads its stdin with `readFileSync(0)` and decodes it
with `.toString("utf8")`; no other script reads a pipe or stdin with
node.
