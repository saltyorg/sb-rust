# logrotate and sb's run log

sb writes every root run's record to one file, `/var/log/sb/sb.log`
(directory root-only, 0700). sb never rotates or prunes it; Saltbox's
logrotate does (cli-core spec, decision 11 in the capability map).

## Proposed rule (for Saltbox's logrotate role)

```
/var/log/sb/sb.log {
    weekly
    rotate 8
    compress
    delaycompress
    missingok
    notifempty
    nocreate
}
```

Retention (`weekly`, `rotate 8`) is a suggestion; Saltbox's own convention
for its other logs wins. The rest matters:

- **No `postrotate` signal and no `copytruncate`.** sb opens the file
  afresh on every run and exits, so it never needs telling to reopen. A
  SIGHUP would end a running command: sb handles it as a hangup and
  cancels the run, as it does SIGTERM.
- **`delaycompress`.** A run that overlaps a rotation keeps writing to the
  renamed `sb.log.1` until it exits. Compressing that copy straight away
  would remove it under the running sb and lose the end of its record. The
  delay lasts one rotation: a second rotation soon after (`logrotate -f`,
  or a repeat on the same day with `dateext`) compresses it anyway.
- **`nocreate`, written out** (Ubuntu's `/etc/logrotate.conf` sets
  `create` for every rule). sb creates `sb.log` on its next run. With
  `create`, a run that opens the file between logrotate's rename and its
  create makes logrotate fail with "destination already exists" and move
  that run's file aside as `sb.log-<date>.backup`, which no rule rotates.
  The file's mode follows root's umask (0644 under 022); the root-only
  directory is what keeps it private.
- **Names.** sb recognises numbered copies and the default `dateext`
  names. `extension`, a `dateformat` that doesn't start with `-` or `.`,
  and `olddir` inside `/var/log/sb` give names or a directory sb doesn't
  count as its own, which only matters if the directory ever loses its
  0700 mode (sb then refuses to log there, with a warning).

## Verified behaviour

logrotate 3.21.0, tracing-appender 0.2.5, on Ubuntu, with the rule above
pointed at a scratch directory and `logrotate -f` run by hand:

- A writer that opened `sb.log` before the rotation and wrote after it
  ended up in `sb.log.1`, its lines together. With `nocreate` there is no
  `sb.log` until the next writer, which creates it (append mode, as
  tracing-appender opens it).
- On the following rotation `sb.log.1` became `sb.log.2.gz`, so
  compression waits one cycle.
- tracing-appender opens the file in append mode (`OpenOptions::append`,
  `rolling.rs`), so runs that overlap each other append whole lines
  rather than overwrite.

sb accepts logrotate's names for rotated copies (`sb.log.1`,
`sb.log.2.gz`, and `sb.log-YYYYMMDD` with `dateext`) as its own when it
checks an existing log directory.
