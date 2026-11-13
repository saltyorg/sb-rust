# Checks shared by compare.sh and compat.sh, sourced inside each
# container (as /checks.sh, with the example binary at /sbj). Each check
# compares journalctl's `-o json --all` output with sb-journal's, field by
# field; `fail` is set to 1 on any failure. Where sb-journal differs from
# journalctl on purpose (docs/specs/journal.md, "Differences"), the check
# asserts that difference, never equality. The callers set `rel` (the row
# whose rules apply) and `want` (its systemd major).
# shellcheck shell=bash disable=SC2034,SC2154 # fail is read, and want set, by the scripts that source this
fail=0
# The pread figures of the last fixture_checks run: its -u tail, page back
# and first page forward.
preads_tail='' preads_back='' preads_forward=''

bad() { # what failed
  fail=1
  echo "  FAIL  $1"
}

ok() { # what passed
  echo "  ok    $1"
}

# A listed difference, asserted.
difference() { # what
  echo "  diff  $1"
}

# The systemd-coredump user's uid, for sb-journal's --coredump-uid (26.04's
# `-u`, decisions 1 and 4); empty when the user doesn't exist, and then the
# flag is left out.
coredump_uid() {
  cuid=$(id -u systemd-coredump 2>/dev/null) || cuid=''
}

# sb-journal's page under the row's rules, with the coredump uid when the
# user exists.
sbj_tail() { # release args...
  local rel=$1 extra=()
  shift
  [ -n "${cuid:-}" ] && extra=(--coredump-uid "$cuid")
  /sbj tail --release "$rel" "${extra[@]}" "$@"
}

check() { # name expected actual
  local out
  # An empty expected side would pass against an empty actual one: every
  # check here is built to have entries, so empty means the harness broke.
  if [ ! -s "$2" ]; then
    bad "$1: no expected entries"
  elif out=$(/sbj compare "$2" "$3"); then
    ok "$1: $(echo "$out" | tail -1)"
  else
    fail=1
    echo "  FAIL  $1:"
    while IFS= read -r line; do printf '          %s\n' "$line"; done <<<"$out"
  fi
}

# The first of these paths that is an executable file (a release's
# systemd binaries live in /usr/lib or /lib).
first_of() { # path...
  local p
  for p; do
    if [ -x "$p" ]; then
      echo "$p"
      return
    fi
  done
}

files() { # dir: each journal file, as journalctl would meet it
  find "$1" -name '*.journal*' -type f 2>/dev/null | sort | sed 's|.*/|  file: |'
}

# Fields are read from `-o json` lines by the example, through
# serde_json (`sbj fields`, `sbj select`), never by matching the text.

# Each entry's MESSAGE, space-separated.
msgs() { # file
  /sbj fields "$1" MESSAGE | paste -sd' ' -
}

# The cursor of a file's first (or last) entry.
first_cursor() { # file
  /sbj fields "$1" __CURSOR | sed -n 1p
}
last_cursor() { # file
  /sbj fields "$1" __CURSOR | sed -n '$p'
}

# sb-journal's flags (`--stats` on stderr) against journalctl: a matching
# entry lies beyond the page when journalctl's whole output for the query
# has one before (after) the page's entries (`sbj more`).
flags_check() { # name whole page stats [--before C | --after C]
  local name=$1 whole=$2 page=$3 stats=$4 want_flags got
  shift 4
  if ! want_flags=$(/sbj more "$whole" "$page" "$@" 2>&1); then
    bad "$name flags: $want_flags"
    return
  fi
  got=$(grep -o 'more_older=[a-z]* more_newer=[a-z]*' "$stats" | tail -1)
  if [ "$got" = "$want_flags" ]; then
    ok "$name flags: $got"
  else
    bad "$name flags: sb-journal ${got:-none}, journalctl's entries give $want_flags"
  fi
}

preads() { # stats: the page's pread count
  grep -o 'preads=[0-9]*' "$1" | tail -1 | cut -d= -f2
}

# Difference 1: 26.04's journalctl takes a coredump entry of any uid for
# files given with --file or -D, and for the local journal when the
# systemd-coredump user doesn't resolve; sb-journal still requires
# `_UID=0` (decisions 1 and 4). Whether this row and source hit it.
d1_applies() { # external (1 for --file or -D)
  [ "$want" -ge 257 ] && { [ "$1" = 1 ] || [ -z "${cuid:-}" ]; }
}

# journalctl's `-u` output without the entries Difference 1 is about:
# coredump entries of another uid than 0 (where it applies, sb-journal
# takes root's only: a local journal with a systemd-coredump user doesn't
# hit it). Prints how many it left out; fails if the selection does.
without_d1() { # in out
  /sbj select "$1" --keep-coredumps-of 0 > "$2" || return 1
  echo $(($(grep -c '^{' "$1") - $(grep -c '^{' "$2")))
}

# journalctl's `-u demo.service` output for jsrc into OUT, with Difference
# 1's entries left out where this row and source hit it. With `asserted`
# 1, at least one must have been (the fixtures have uid-1000 coredump
# decoys), or the difference isn't what this asserts; a page (0) may hold
# none.
unit_whole() { # name jsrc external asserted out [journalctl args...]
  local name=$1 jsrc=$2 external=$3 asserted=$4 out=$5 n
  shift 5
  # shellcheck disable=SC2086 # jsrc is word-split on purpose
  journalctl $jsrc -u demo.service -o json --all -q "$@" >"$out.raw" || bad "$name: journalctl -u"
  if ! d1_applies "$external"; then
    cp "$out.raw" "$out"
    return
  fi
  if ! n=$(without_d1 "$out.raw" "$out"); then
    bad "$name: sbj select"
    return
  fi
  if [ "$asserted" = 0 ]; then
    return
  elif [ "$n" -gt 0 ]; then
    difference "$name: Difference 1, $n coredump entries of another uid shown by journalctl only"
  else
    bad "$name: Difference 1 expected, but journalctl showed no coredump entry of another uid"
  fi
}

# The entries of a `-u` whole after the realtime `t`, at most `n`.
after_realtime() { # whole t n
  /sbj select "$1" --after-realtime "$2" | sed -n "1,$3p"
}

# The fixture checks: the whole source, `-u demo.service` whole, last 500,
# one page back and every page forward, with the flags; Difference 18's
# cursors; and field matches. `jsrc` and `psrc` name the same files to
# journalctl and to sb-journal (`--file F` or `-D D` / `--directory D`),
# word-split on purpose; `external` is 1 for both.
fixture_checks() { # name release jsrc psrc external
  local name=$1 rel=$2 jsrc=$3 psrc=$4 external=$5 cur pages
  # shellcheck disable=SC2086 # jsrc and psrc are word-split on purpose
  {
    journalctl $jsrc -o json --all -q > whole.json || bad "$name: journalctl whole"
    sbj_tail "$rel" $psrc -n 100000000 > a.json || bad "$name: sbj whole"
    check "$name whole" whole.json a.json

    unit_whole "$name" "$jsrc" "$external" 1 u.whole
    sbj_tail "$rel" $psrc -u demo.service -n 100000000 > a.json || bad "$name: sbj -u whole"
    check "$name -u whole" u.whole a.json

    if d1_applies "$external"; then
      tail -n 500 u.whole > e.json
    else
      journalctl $jsrc -u demo.service -o json --all -q -n 500 > e.json || bad "$name: journalctl -u"
    fi
    sbj_tail "$rel" $psrc -u demo.service -n 500 --stats > a.json 2> a.err || bad "$name: sbj -u"
    check "$name -u last 500" e.json a.json
    flags_check "$name -u last 500" u.whole a.json a.err
    preads_tail=$(preads a.err)

    cur=$(first_cursor e.json)
    if d1_applies "$external"; then
      tail -n 1000 u.whole | head -n 500 > e.json
    else
      journalctl $jsrc -u demo.service -o json --all -q -n 1000 | head -n -500 > e.json || bad "$name: journalctl page"
    fi
    sbj_tail "$rel" $psrc -u demo.service -n 500 --before "$cur" --stats > a.json 2> a.err || bad "$name: sbj page"
    check "$name -u page back" e.json a.json
    flags_check "$name -u page back" u.whole a.json a.err --before "$cur"
    preads_back=$(preads a.err)

    # Forward, page by page from the first entry, each against
    # journalctl --after-cursor (decision 10), until both are past the
    # end. The cursors are entries both show, so Difference 18 doesn't
    # arise; Difference 1's entries are left out of journalctl's pages.
    cur=$(first_cursor u.whole)
    pages=0
    preads_forward=''
    while :; do
      if [ "$pages" -ge 10 ]; then
        bad "$name -u forward: not past the end after $pages pages"
        break
      fi
      unit_whole "$name forward page $pages" "$jsrc" "$external" 0 e.all --after-cursor "$cur"
      head -n 500 e.all > e.json
      sbj_tail "$rel" $psrc -u demo.service -n 500 --after "$cur" --stats > a.json 2> a.err || bad "$name: sbj forward"
      [ -n "$preads_forward" ] || preads_forward=$(preads a.err)
      if [ ! -s e.json ]; then
        if [ -s a.json ]; then
          bad "$name -u forward past the end: sb-journal shows $(wc -l < a.json) entries, journalctl none"
        else
          ok "$name -u forward past the end: both empty"
        fi
        flags_check "$name -u forward past the end" u.whole a.json a.err --after "$cur"
        break
      fi
      check "$name -u forward page $pages" e.json a.json
      flags_check "$name -u forward page $pages" u.whole a.json a.err --after "$cur"
      cur=$(last_cursor a.json)
      pages=$((pages + 1))
    done
    [ "$pages" -ge 3 ] || bad "$name -u forward: $pages pages before the end, expected at least 3"

    d18_checks "$name" "$rel" "$jsrc" "$psrc" "$external"

    # AND across fields, OR within one: _UID=0 co-occurs with both priorities.
    journalctl $jsrc -o json --all -q -n 300 _UID=0 PRIORITY=3 PRIORITY=5 > e.json || bad "$name: journalctl and/or"
    sbj_tail "$rel" $psrc -n 300 --match _UID=0 --match PRIORITY=3 --match PRIORITY=5 > a.json || bad "$name: sbj and/or"
    check "$name field match, AND and OR" e.json a.json
    # Only an OR on one field: no single-value term to drive the walk.
    journalctl $jsrc -o json --all -q -n 300 PRIORITY=3 PRIORITY=5 > e.json || bad "$name: journalctl or"
    sbj_tail "$rel" $psrc -n 300 --match PRIORITY=3 --match PRIORITY=5 > a.json || bad "$name: sbj or"
    check "$name field match, OR only" e.json a.json
    # A value carried twice by its entries: walks its entry array, which
    # systemd 249 writes with each such entry twice.
    journalctl $jsrc -o json --all -q -n 300 SAME=again > e.json || bad "$name: journalctl repeated value"
    # Whether this writer kept the repeated item (journalctl then prints it
    # twice). journal-remote 249 was found not to, so the matrix doesn't
    # exercise repeated array links; file.rs's crafted-file tests do.
    if /sbj fields e.json SAME | grep -qxF '["again","again"]'; then
      echo "  note  $name: the writer kept the repeated item"
    fi
    sbj_tail "$rel" $psrc -n 300 --match SAME=again > a.json || bad "$name: sbj repeated value"
    check "$name field match, repeated value" e.json a.json
  }
}

# Difference 18: the entry after a vanished or non-matching cursor. sb
# shows it, as 24.04's and 26.04's journalctl do (255.1 and later test the
# cursor); 22.04's journalctl always steps past where the seek lands, and
# loses it. Uses u.whole and whole.json from fixture_checks.
d18_checks() { # name release jsrc psrc external
  local name=$1 rel=$2 jsrc=$3 psrc=$4 external=$5 cur t k
  # A non-matching cursor: entry 1222 is other.service's, no -u branch on
  # any release; after it, the -u entries after its realtime.
  t=$((1700000000000000 + 1222 * 1000000))
  after_realtime whole.json $((t - 1)) 1 > c.json
  cur=$(first_cursor c.json)
  # It is other.service's entry, which no -u group matches.
  [ "$(/sbj fields c.json _SYSTEMD_UNIT)" = other.service ] ||
    bad "$name: Difference 18's non-matching cursor isn't other.service's entry"
  if /sbj fields u.whole __CURSOR | grep -qxF "$cur"; then
    bad "$name: Difference 18's non-matching cursor matches -u"
  fi
  after_realtime u.whole "$t" 11 > d18.next
  d18_count "$name non-matching cursor"
  d18_check "$name non-matching cursor" "$rel" "$jsrc" "$psrc" "$external" "$cur"
  # A vanished cursor: the 100th -u entry's, its seqnum id another file's
  # (a copy in a file rotated or vacuumed away, or a /run copy flushed),
  # so no entry is the cursor's own and both seeks land on that entry.
  sed -n 100p u.whole > c.json
  cur=$(first_cursor c.json | sed 's/^s=[0-9a-f]*;/s=0123456789abcdef0123456789abcdef;/')
  sed -n 100,110p u.whole > d18.next
  d18_count "$name vanished cursor"
  d18_check "$name vanished cursor" "$rel" "$jsrc" "$psrc" "$external" "$cur"
}

d18_count() { # name: d18.next holds 11 entries
  local k
  k=$(grep -c '^{' d18.next)
  [ "$k" = 11 ] || bad "$1: Difference 18 fixture has $k entries after the cursor"
}

# d18.next holds the 11 -u entries from the first one after the cursor.
d18_check() { # name release jsrc psrc external cursor
  local name=$1 rel=$2 jsrc=$3 psrc=$4 external=$5 cur=$6
  # shellcheck disable=SC2086 # psrc is word-split on purpose
  sbj_tail "$rel" $psrc -u demo.service -n 10 --after "$cur" > a.json || bad "$name: sbj"
  head -n 10 d18.next > e.json
  check "$name, sb-journal shows the entry after it" e.json a.json
  unit_whole "$name" "$jsrc" "$external" 0 e.all --after-cursor "$cur"
  head -n 10 e.all > j.json
  if [ "$want" -ge 255 ]; then
    check "$name, journalctl $want shows it too" e.json j.json
  else
    tail -n 10 d18.next > e.json
    if /sbj compare e.json j.json >/dev/null; then
      difference "$name: Difference 18, journalctl $want loses the entry after it"
    else
      bad "$name: journalctl $want doesn't lose just the entry after it (Difference 18):"
      /sbj compare e.json j.json | sed 's/^/          /'
    fi
  fi
}

# The image's systemd major version against its row's (rules.rs
# RELEASES). A canary, another image read under a row's rules, only
# reports it.
systemd_check() { # image release want
  local got
  got=$(journalctl --version | head -1 | awk '{ print $2 }')
  if [ "$1" = "$2" ] && [ "$got" != "$3" ]; then
    bad "$1 ships systemd $got; its RELEASES row says $3"
  elif [ "$got" != "$3" ]; then
    echo "  note  canary: $1 ships systemd $got, read with $2's rules (systemd $3)"
  fi
}

integrity() { # name source...: hash-check and decode-all, no errors
  local name=$1 hc dc
  shift
  hc=$(/sbj hash-check "$@") || bad "$name: hash-check"
  echo " $name: $(echo "$hc" | head -1 | sed 's/^[^:]*: //')"
  dc=$(/sbj decode-all "$@") || bad "$name: decode-all (errors)"
  echo "  ${dc%, *}"
}

# Decision 11 and Difference 20: two boots in two files, a1 and a2 on
# boot 0xa…, b1 and b2 on 0xb…, written by journal-remote into /boots,
# whose headers name this machine. `overlap` puts a at 100 and 300 s, b
# at 200 and 400 s (a clock set back across a reboot); `apart` puts a at
# 100 and 200 s, b at 300 and 400 s. Boot a's newest tail (300 or 200 s)
# is older than b's. `machines` is `overlap` imported from two source
# machines, each entry naming its own in `_MACHINE_ID=` (0x11… for a,
# 0x22… for b): 26.04 (259.9) takes the tail entry's machine for the boot
# table (issue #34169), so its boots compare equal and realtime
# interleaves them, where 24.04 (255.4) compares the headers' machine and
# orders them by boot. `machines-untagged` leaves `_MACHINE_ID=` off each
# file's last entry, the tail, so 26.04 keeps the header's too. Then the
# `own boot` fixture (own_boot_checks).
boot_checks() { # release remote
  local rel=$1 remote=$2 kind j src order d20 jc_walk sb_walk cur
  local ma=() mb=()
  mkdir -p /boots
  for kind in overlap apart machines machines-untagged; do
    if [ "$kind" = apart ]; then
      set -- 100,200 300,400
    else
      set -- 100,300 200,400
    fi
    ma=() mb=()
    case $kind in
      machines)
        ma=(--machine 11111111111111111111111111111111)
        mb=(--machine 22222222222222222222222222222222)
        ;;
      machines-untagged)
        ma=(--machine 11111111111111111111111111111111 --untagged-tail)
        mb=(--machine 22222222222222222222222222222222 --untagged-tail)
        ;;
    esac
    /sbj gen-boot --boot aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --prefix a --realtime-s "$1" "${ma[@]}" > a.export
    /sbj gen-boot --boot bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb --prefix b --realtime-s "$2" "${mb[@]}" > b.export
    rm -f "/boots/$kind-a.journal" "/boots/$kind-b.journal"
    "$remote" --output="/boots/$kind-a.journal" - < a.export >/dev/null 2>&1 || bad "boots $kind: journal-remote a"
    "$remote" --output="/boots/$kind-b.journal" - < b.export >/dev/null 2>&1 || bad "boots $kind: journal-remote b"
    src="--file /boots/$kind-a.journal --file /boots/$kind-b.journal"
    # The order between the boots: 249 by realtime; 255 and 259 by the
    # boot table (boot a's newest tail first) where the clocks overlap,
    # except 259 over two machines' tail entries, by realtime. Difference
    # 20 is where the table orders boots whose clocks overlap.
    if [ "$kind" = apart ]; then
      order="a1 a2 b1 b2"
    elif [ "$want" -lt 255 ] || { [ "$kind" = machines ] && [ "$want" -ge 259 ]; }; then
      order="a1 b1 a2 b2"
    else
      order="a1 a2 b1 b2"
    fi
    if [ "$kind" != apart ] && [ "$order" = "a1 a2 b1 b2" ]; then d20=yes; else d20=no; fi
    # shellcheck disable=SC2086 # src is word-split on purpose
    {
      journalctl $src -o json --all -q > bw.json || bad "boots $kind: journalctl whole"
      if [ "$(msgs bw.json)" = "$order" ]; then
        ok "boots $kind: journalctl $want orders them $order"
      else
        bad "boots $kind: journalctl $want orders them $(msgs bw.json), decision 11 says $order"
      fi
      sbj_tail "$rel" $src -n 100 > a.json || bad "boots $kind: sbj whole"
      check "boots $kind whole" bw.json a.json
      journalctl $src -o json --all -q -n 2 > e.json || bad "boots $kind: journalctl tail"
      sbj_tail "$rel" $src -n 2 --stats > a.json 2> a.err || bad "boots $kind: sbj tail"
      check "boots $kind tail 2" e.json a.json
      flags_check "boots $kind tail 2" bw.json a.json a.err
      journalctl $src -o json --all -q -n 4 > jtail.json || bad "boots $kind: journalctl tail 4"
      # The pages back below take journalctl's tail walk for its order.
      check "boots $kind: journalctl's tail 4 is its whole" bw.json jtail.json
      for j in 1 2 3 4; do
        sed -n "${j}p" bw.json > c.json
        cur=$(first_cursor c.json)
        # A page back: what journalctl's tail walk has before the entry.
        sbj_tail "$rel" $src -n 10 --before "$cur" --stats > a.json 2> a.err || bad "boots $kind: sbj back"
        head -n $((j - 1)) jtail.json > e.json
        if [ "$j" = 1 ]; then
          [ -s a.json ] && bad "boots $kind back from 1: sb-journal shows entries before the first"
        else
          check "boots $kind page back from $j" e.json a.json
        fi
        flags_check "boots $kind page back from $j" bw.json a.json a.err --before "$cur"
        # A page forward: sb-journal's is the entries after it in the
        # release's order, each once (decision 11).
        sbj_tail "$rel" $src -n 10 --after "$cur" --stats > a.json 2> a.err || bad "boots $kind: sbj forward"
        tail -n +$((j + 1)) bw.json > e.json
        if [ "$j" = 4 ]; then
          [ -s a.json ] && bad "boots $kind forward from 4: sb-journal shows entries after the last"
        else
          check "boots $kind page forward from $j" e.json a.json
        fi
        flags_check "boots $kind page forward from $j" bw.json a.json a.err --after "$cur"
        journalctl $src -o json --all -q --after-cursor "$cur" > j.json || bad "boots $kind: journalctl forward"
        d20_page "$kind" "$d20" "$j" e.json j.json
      done
      # Paging forward one entry at a time from a1.
      cur=$(first_cursor bw.json)
      sb_walk='' jc_walk=''
      for _ in 1 2 3 4; do
        sbj_tail "$rel" $src -n 1 --after "$cur" > a.json || bad "boots $kind: sbj walk"
        [ -s a.json ] || break
        sb_walk="${sb_walk:+$sb_walk }$(msgs a.json)"
        cur=$(last_cursor a.json)
      done
      cur=$(first_cursor bw.json)
      for _ in 1 2 3 4; do
        journalctl $src -o json --all -q --after-cursor "$cur" | head -n 1 > j.json
        [ -s j.json ] || break
        jc_walk="${jc_walk:+$jc_walk }$(msgs j.json)"
        cur=$(last_cursor j.json)
      done
    }
    if [ "$sb_walk" = "${order#a1 }" ]; then
      ok "boots $kind: sb-journal one at a time from a1: $sb_walk"
    else
      bad "boots $kind: sb-journal one at a time from a1: $sb_walk, expected ${order#a1 }"
    fi
    if [ "$d20" = yes ]; then
      if [ "$jc_walk" = "a2 b2" ]; then
        difference "boots $kind: Difference 20, journalctl $want one at a time from a1 loses b1: $jc_walk"
      else
        bad "boots $kind: journalctl $want one at a time from a1: $jc_walk, Difference 20 predicts a2 b2"
      fi
    elif [ "$jc_walk" = "$sb_walk" ]; then
      ok "boots $kind: journalctl $want one at a time from a1: $jc_walk"
    else
      bad "boots $kind: journalctl $want one at a time from a1: $jc_walk, sb-journal $sb_walk"
    fi
  done
  own_boot_checks "$rel" "$remote"
}

# The `own boot` fixture (the hardening plan's decision 33, Difference
# 21): one boot, 0xc…, written to two files of two seqnum ids with its
# realtime clock set back between them. File b holds b1 and b2 (monotonic
# 1.001 and 1.002 s, realtimes 300 and 400 s), file x holds x1 to x4
# (monotonic 1.003 to 1.006 s, realtimes 10 to 40 s); each entry names
# source machine 0x11… in `_MACHINE_ID=`. `filtered` reads file bc, b's
# entries and c1 (monotonic 1.007 s, realtime 500 s, no `_MACHINE_ID=`),
# by `_MACHINE_ID=11…`: the query's seek in bc finds c1's place in the
# boot but no matching entry from it. After an entry, sb-journal shows
# the entries after it, on every row, in either file order, and paging
# one entry at a time from x1 ends at x4. journalctl 249 and 259.9 show
# the same; 255 seeks file b by realtime (no matching entry of it reaches
# the cursor's monotonic time), lands before the cursor, shows b1 and b2
# then the cursor's entry, and pages for ever.
own_boot_checks() { # release remote
  local rel=$1 remote=$2 kind layout src name order who i cur got want_jc walk jc_walk f
  local boot=cccccccccccccccccccccccccccccccc m=11111111111111111111111111111111
  local jf=() sf=()
  mkdir -p /own
  /sbj gen-boot --boot "$boot" --prefix b --realtime-s 300,400 --machine "$m" > b.export
  /sbj gen-boot --boot "$boot" --prefix c --realtime-s 500 --monotonic-skip 6 > c.export
  cat b.export c.export > bc.export
  /sbj gen-boot --boot "$boot" --prefix x --realtime-s 10,20,30,40 --monotonic-skip 2 --machine "$m" > x.export
  rm -f /own/b.journal /own/bc.journal /own/x.journal
  for f in b bc x; do
    "$remote" --output="/own/$f.journal" - < "$f.export" >/dev/null 2>&1 || bad "own boot: journal-remote $f"
  done
  order=(b1 b2 x1 x2 x3 x4)
  for kind in plain filtered; do
    if [ "$kind" = plain ]; then
      jf=()
      sf=()
      set -- "b x" "x b"
    else
      jf=("_MACHINE_ID=$m")
      sf=(--match "_MACHINE_ID=$m")
      set -- "bc x" "x bc"
    fi
    for layout; do
      name="own boot $kind $layout"
      src="--file /own/${layout% *}.journal --file /own/${layout#* }.journal"
      # shellcheck disable=SC2086 # src is word-split on purpose
      {
        journalctl $src -o json --all -q "${jf[@]}" > ow.json || bad "$name: journalctl whole"
        if [ "$(msgs ow.json)" = "${order[*]}" ]; then
          ok "$name: journalctl $want orders them ${order[*]}"
        else
          bad "$name: journalctl $want orders them $(msgs ow.json), expected ${order[*]}"
        fi
        sbj_tail "$rel" $src -n 100 "${sf[@]}" > a.json || bad "$name: sbj whole"
        check "$name whole" ow.json a.json
        for i in 0 1 2 3 4 5; do
          who=${order[$i]}
          sed -n "$((i + 1))p" ow.json > c.json
          cur=$(first_cursor c.json)
          tail -n +$((i + 2)) ow.json > e.json
          sbj_tail "$rel" $src -n 10 "${sf[@]}" --after "$cur" --stats > a.json 2> a.err || bad "$name: sbj forward"
          if [ "$i" = 5 ]; then
            [ -s a.json ] && bad "$name: sb-journal shows entries after x4"
          else
            check "$name page forward from $who" e.json a.json
          fi
          flags_check "$name page forward from $who" ow.json a.json a.err --after "$cur"
          journalctl $src -o json --all -q --after-cursor "$cur" "${jf[@]}" > j.json || bad "$name: journalctl forward"
          got=$(msgs j.json)
          if [ "$want" = 255 ] && [ "$i" -ge 2 ]; then
            want_jc="b1 b2 ${order[*]:$i}"
            if [ "$got" = "$want_jc" ]; then
              difference "$name: Difference 21, journalctl 255 after $who: '$got', sb-journal '$(msgs e.json)'"
            else
              bad "$name: journalctl 255 after $who: '$got', Difference 21 predicts '$want_jc'"
            fi
          elif [ "$got" = "$(msgs e.json)" ]; then
            ok "$name: journalctl $want after $who: '$got', as sb-journal"
          else
            bad "$name: journalctl $want after $who: '$got', sb-journal '$(msgs e.json)'"
          fi
        done
        # Paging forward one entry at a time from x1, at most six pages.
        walk='' jc_walk=''
        sed -n 3p ow.json > c.json
        cur=$(first_cursor c.json)
        for _ in 1 2 3 4 5 6; do
          sbj_tail "$rel" $src -n 1 "${sf[@]}" --after "$cur" > a.json || bad "$name: sbj walk"
          [ -s a.json ] || break
          walk="${walk:+$walk }$(msgs a.json)"
          cur=$(last_cursor a.json)
        done
        cur=$(first_cursor c.json)
        for _ in 1 2 3 4 5 6; do
          journalctl $src -o json --all -q --after-cursor "$cur" "${jf[@]}" | head -n 1 > j.json
          [ -s j.json ] || break
          jc_walk="${jc_walk:+$jc_walk }$(msgs j.json)"
          cur=$(last_cursor j.json)
        done
      }
      if [ "$walk" = "x2 x3 x4" ]; then
        ok "$name: sb-journal one at a time from x1: $walk"
      else
        bad "$name: sb-journal one at a time from x1: $walk, expected x2 x3 x4"
      fi
      if [ "$want" = 255 ]; then
        if [ "$jc_walk" = "b1 b2 x1 b1 b2 x1" ]; then
          difference "$name: Difference 21, journalctl 255 one at a time from x1 never ends: $jc_walk ..."
        else
          bad "$name: journalctl 255 one at a time from x1: $jc_walk, Difference 21 predicts b1 b2 x1 b1 b2 x1"
        fi
      elif [ "$jc_walk" = "$walk" ]; then
        ok "$name: journalctl $want one at a time from x1: $jc_walk"
      else
        bad "$name: journalctl $want one at a time from x1: $jc_walk, sb-journal $walk"
      fi
    done
  done
}

# journalctl's page after the j-th entry of a boot fixture: sb-journal's
# (e.json) where the clocks don't overlap or no boot table orders them;
# where the table orders boots whose clocks overlap (`d20` yes), what
# Difference 20 predicts from journalctl's seek by one key per file.
d20_page() { # kind d20 j expected journalctl
  local kind=$1 d20=$2 j=$3 expected=$4 got predicted
  got=$(msgs "$5")
  if [ "$d20" = yes ]; then
    # 255's first step is the lowest landing (after b1: a2, sought by
    # realtime, and not the cursor's entry, so shown); 259.9's is the
    # cursor's exact entry when a file lands on it (issue #31516).
    case "$want:$j" in
      *:1) predicted="a2 b1 b2" ;;
      *:2) predicted="b2" ;;
      255:3) predicted="a2 b1 b2" ;;
      *:3) predicted="b2" ;;
      *:4) predicted="" ;;
    esac
    if [ "$got" != "$predicted" ]; then
      bad "boots $kind: journalctl $want after entry $j: '$got', Difference 20 predicts '$predicted'"
    elif [ "$got" = "$(msgs "$expected")" ]; then
      ok "boots $kind: journalctl $want after entry $j: '$got', as sb-journal"
    else
      difference "boots $kind: Difference 20, journalctl $want after entry $j: '$got', sb-journal '$(msgs "$expected")'"
    fi
  elif [ "$got" = "$(msgs "$expected")" ]; then
    ok "boots $kind: journalctl $want after entry $j: '$got', as sb-journal"
  else
    bad "boots $kind: journalctl $want after entry $j: '$got', sb-journal '$(msgs "$expected")'"
  fi
}

# Decision 10's emulated bisect, on a file whose realtime steps back
# within one boot (only journald 249 and older write one): realtimes 10,
# 50, 5 and 45 s, read with a cursor at 40 s of another boot and seqnum
# id, so the seek is a realtime bisect. 255's and 259's bisect lands on
# 50, and 24.04's and 26.04's journalctl show 50, 5, 45; sb-journal does
# on every row. 22.04's journalctl lands there too and steps past it, and
# shows 5, 45 (Difference 18).
stepback_checks() { # name release file
  local name=$1 rel=$2 f=$3 cur got
  journalctl --file "$f" -o json --all -q > sw.json || bad "$name: journalctl whole"
  if [ "$(msgs sw.json)" != "s1 s2 s3 s4" ]; then
    echo "  note  $name: the writer didn't keep the stepped-back clock ($(msgs sw.json)); not checked"
    return
  fi
  sbj_tail "$rel" --file "$f" -n 10 > a.json || bad "$name: sbj whole"
  check "$name whole" sw.json a.json
  cur="s=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee;i=1;b=dddddddddddddddddddddddddddddddd;m=1;t=$(printf '%x' $((1700000000000000 + 40 * 1000000)));x=0"
  sbj_tail "$rel" --file "$f" -n 10 --after "$cur" > a.json || bad "$name: sbj forward"
  if [ "$(msgs a.json)" = "s2 s3 s4" ]; then
    ok "$name: sb-journal after 40 s lands on 50 s: s2 s3 s4"
  else
    bad "$name: sb-journal after 40 s: $(msgs a.json), decision 10's bisect predicts s2 s3 s4"
  fi
  journalctl --file "$f" -o json --all -q --after-cursor "$cur" > j.json || bad "$name: journalctl forward"
  got=$(msgs j.json)
  if [ "$want" -ge 255 ]; then
    if [ "$got" = "s2 s3 s4" ]; then
      ok "$name: journalctl $want after 40 s: $got, as sb-journal"
    else
      bad "$name: journalctl $want after 40 s: $got, decision 10's bisect predicts s2 s3 s4"
    fi
  elif [ "$got" = "s3 s4" ]; then
    difference "$name: Difference 18, journalctl $want after 40 s steps past its landing: $got"
  else
    bad "$name: journalctl $want after 40 s: '$got', Difference 18 predicts s3 s4"
  fi
}
