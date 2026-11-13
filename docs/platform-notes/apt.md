# apt's wait for the dpkg lock

Verified 2026-11-13 for the `host` module (modules plan Task M9; host
spec, "apt", decision 3 and difference 1).

**Software:** apt 2.4.15 (Ubuntu 22.04), apt 2.8.4 (Ubuntu 24.04) and
apt 3.2.1 (Ubuntu 26.04), each in the `ubuntu:<release>` image, amd64;
perl and util-linux's `flock` from the same images.

**Method:** a throwaway container per release (`docker run --rm
--network none`), never the host's apt. Inside it, as root:

1. A Perl process takes an `fcntl` write lock (`F_SETLKW`, `F_WRLCK`,
   the whole file) on `/var/lib/dpkg/lock-frontend`, as apt and dpkg
   take it, and holds it for 10 s.
2. While it holds it: `flock -n /var/lib/dpkg/lock-frontend true`, sb-go's
   kind of check.
3. `apt-get -o Debug::Locking=1 install -y apt`, an installed package,
   with its output to a file (not a terminal).
4. Again with the lock held for 10 s: the same command with
   `-o DPkg::Lock::Timeout=240`, timed.

The `-s` (simulate) form isn't used: it sets `Debug::NoLocking` and never
waits.

## What apt does

The same on all three releases:

- **sb-go's poll can't see the lock:** `flock -n` succeeds (exit 0) while
  the `fcntl` lock is held. On Linux the two kinds of lock don't see each
  other, so sb-go's `WaitForAptLock` went straight on.
- **Without the timeout:** apt-get fails at once, exit 100, with `E:
  Could not get lock /var/lib/dpkg/lock-frontend. It is held by process
  <pid> (perl)`.
- **With `DPkg::Lock::Timeout=240`:** apt-get waits and then succeeds
  (exit 0, after 9 s with the lock released at about 9 s). It prints
  `Waiting for cache lock: Could not get lock
  /var/lib/dpkg/lock-frontend. It is held by process <pid> (perl)...`
  once a second.
- **On a non-terminal output** each waiting line ends with a newline;
  there is no carriage return in the output. A step shows them as plain
  lines.

So `install`, `remove` and `autoremove` wait for the dpkg lock through
apt's own option (host spec, decision 3), and difference 1 holds.

## The package lists' lock

**Method:** the same containers; the Perl process holds the `fcntl` lock
on `/var/lib/apt/lists/lock` instead, and `apt-get -o
DPkg::Lock::Timeout=240 update` runs (no network: it stops at the lock
before any download), then `apt-get update` with stdout and stderr in
separate files.

- `update` fails at once (exit 100, 0 s) even with
  `DPkg::Lock::Timeout`, so the option doesn't cover this lock (host
  spec, decision 5).
- Its error is `E: Could not get lock /var/lib/apt/lists/lock. It is
  held by process <pid> (perl)`, on stderr only, on all three releases:
  the text `update_lists` matches in the stderr tail to wait and try
  again.

## Names apt ignores

**Method:** `apt-config dump Dir::Ignore-Files-Silently` in the same three
images (apt's built-in defaults, set in apt-pkg/init.cc).

All three list the same patterns: `~$`, `\.disabled$`, `\.bak$`,
`\.dpkg-[a-z]+$`, `\.ucf-[a-z]+$`, `\.save$`, `\.orig$` and
`\.distUpgrade$`. `add_repositories` writes its temporary files under a
name ending in `~`, so apt never reads a half-written file in
`sources.list.d` and doesn't warn about one.

## Not covered

- Translated messages: the containers have no apt translations, and sb
  runs apt-get under the inherited locale (host spec, "apt"), so on a
  host with another language and apt's translations installed the
  lock and mirror texts wouldn't match. sb-go is the same.
