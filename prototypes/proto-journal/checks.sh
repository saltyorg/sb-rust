# Checks shared by compare.sh and compat.sh, sourced inside each
# container (as /checks.sh, with the binary at /pj). Each check compares
# journalctl's `-o json --all` output with proto-journal's, field by
# field; `fail` is set to 1 on any failure.
# shellcheck shell=bash disable=SC2034 # fail is read by the scripts that source this
fail=0

bad() { # what failed
  fail=1
  echo "  FAIL  $1"
}

check() { # name expected actual
  local out
  # An empty expected side would pass against an empty actual one: every
  # check here is built to have entries, so empty means the harness broke.
  if [ ! -s "$2" ]; then
    bad "$1: no expected entries"
  elif out=$(/pj compare "$2" "$3"); then
    echo "  ok    $1: $(echo "$out" | tail -1)"
  else
    fail=1
    echo "  FAIL  $1:"
    while IFS= read -r line; do printf '          %s\n' "$line"; done <<<"$out"
  fi
}

# The image's systemd major version against its row's (query.rs
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
  hc=$(/pj hash-check "$@") || bad "$name: hash-check"
  echo " $name: $(echo "$hc" | head -1 | sed 's/^[^:]*: //')"
  dc=$(/pj decode-all "$@") || bad "$name: decode-all"
  echo "  ${dc%, *}"
  echo "$dc" | grep -q ' 0 errors' || bad "$name: decode errors"
}

# The fixture checks: the whole source, `-u demo.service` last 500 and
# one page back, and field matches. `jsrc` and `psrc` name the same files
# to journalctl and to proto-journal (`--file F` or `-D D` /
# `--directory D`), word-split on purpose.
fixture_checks() { # name release jsrc psrc
  local name=$1 rel=$2 jsrc=$3 psrc=$4 cur
  # shellcheck disable=SC2086 # jsrc and psrc are word-split on purpose
  {
    journalctl $jsrc -o json --all -q > e.json || bad "$name: journalctl whole"
    /pj tail --release "$rel" $psrc -n 100000000 > a.json || bad "$name: pj whole"
    check "$name whole" e.json a.json
    journalctl $jsrc -u demo.service -o json --all -q -n 500 > e.json || bad "$name: journalctl -u"
    /pj tail --release "$rel" $psrc -u demo.service -n 500 > a.json || bad "$name: pj -u"
    check "$name -u last 500" e.json a.json
    cur=$(head -1 e.json | sed 's/.*"__CURSOR":"\([^"]*\)".*/\1/')
    journalctl $jsrc -u demo.service -o json --all -q -n 1000 | head -n -500 > e.json || bad "$name: journalctl page"
    /pj tail --release "$rel" $psrc -u demo.service -n 500 --before "$cur" > a.json || bad "$name: pj page"
    check "$name -u page back" e.json a.json
    # AND across fields, OR within one: _UID=0 co-occurs with both priorities.
    journalctl $jsrc -o json --all -q -n 300 _UID=0 PRIORITY=3 PRIORITY=5 > e.json || bad "$name: journalctl and/or"
    /pj tail --release "$rel" $psrc -n 300 --match _UID=0 --match PRIORITY=3 --match PRIORITY=5 > a.json || bad "$name: pj and/or"
    check "$name field match, AND and OR" e.json a.json
    # Only an OR on one field: no single-value term to drive the walk.
    journalctl $jsrc -o json --all -q -n 300 PRIORITY=3 PRIORITY=5 > e.json || bad "$name: journalctl or"
    /pj tail --release "$rel" $psrc -n 300 --match PRIORITY=3 --match PRIORITY=5 > a.json || bad "$name: pj or"
    check "$name field match, OR only" e.json a.json
    # A value carried twice by its entries: walks its entry array, which
    # systemd 249 writes with each such entry twice.
    journalctl $jsrc -o json --all -q -n 300 SAME=again > e.json || bad "$name: journalctl repeated value"
    # Whether this writer kept the repeated item (journalctl then prints it
    # twice). journal-remote 249 was found not to, so the matrix doesn't
    # exercise repeated array links; file.rs's crafted-file tests do.
    if grep -q '"SAME":\["again","again"\]' e.json; then
      echo "  note  $name: the writer kept the repeated item"
    fi
    /pj tail --release "$rel" $psrc -n 300 --match SAME=again > a.json || bad "$name: pj repeated value"
    check "$name field match, repeated value" e.json a.json
  }
}
