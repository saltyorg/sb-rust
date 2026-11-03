# ADR-009: Binary size with `sb validate-config`

## Status

Proposed (config plan Task 11; for the maintainer at config Checkpoint B)

## Date

2026-11-03

## Context

CONSTRAINTS.md records the production musl binary at most 3595968 bytes
([ADR-007](ADR-007-docker-binary-size.md)), growing at most 5% per
change. sb-config has been built and tested on its own since config
Task 4; config Task 11 links it into sb with `sb validate-config`, and
with it the HTTPS client for the credential checks, the schema loader,
the public-suffix list and term-ui's runner:

| Step | Bytes | Change |
|---|---|---|
| ADR-007 | 3595968 | — |
| config Task 11 (`sb validate-config`) | 11641312 | +223.7% |

The file grew by 8045344 bytes (about 7857 KiB). The production build now
runs through `cross` ([ADR-008](ADR-008-musl-builds-through-cross.md)).
`cargo bloat --release --crates` (code only, on the host's release
build, so only roughly the production build's fat-LTO figures) puts
the new crates' code at about 5 MiB:

| Crates | Code | Why sb needs them |
|---|---|---|
| aws-lc-sys, rustls, webpki | about 1.9 MiB | TLS for the Cloudflare and Docker Hub checks (the capability map's stack: rustls with aws-lc-rs) |
| reqwest, with hyper-rustls | about 0.4 MiB | the HTTPS client; hyper and tokio were already in for bollard |
| psl | about 0.6 MiB | the public-suffix list, to find the zone Cloudflare is asked about |
| regex, regex-automata, regex-syntax, aho-corasick | about 0.7 MiB | the schemas' `pattern` rules, as Go's regexp |
| serde-saphyr, granit-parser, annotate-snippets | about 0.7 MiB | strict schema loading (config decision 5; ADR-004 measured the same set) |
| saphyr-parser, saphyr | about 0.2 MiB | the config files with positions and duplicate keys |
| sb-config | about 0.3 MiB | the rules, validators and run |
| jiff | about 0.1 MiB | `timezone` as Go's LoadLocation |

The rest of the file's growth is read-only data that comes with this
code: the public-suffix list and its tables, aws-lc's constant tables,
regex's Unicode tables. Then there are unwinding tables, which grow with
the code. term-ui's runner adds little: most of it was already linked
for `sb docker ps`'s tables.

`sb --help` and `sb version` don't build the runtime, but the bigger file
costs a little at startup. Medians on the release build: 1.1 ms for
`--help` and 1.3 ms for `version`, against ADR-005's recorded 0.76 ms
for `--help`. That is within the ratchet's 1 ms tolerance and far
under the 10 ms budget. The production binary measures 0.9 and 1.1 ms.
sb is 25.5% of sb-go's 45711520-byte binary, below CONSTRAINTS.md's
ceiling.

## Decision

Record 11641312 bytes as the production binary's limit.

## Alternatives considered

- **ring instead of aws-lc-rs:** smaller, but it compiles C and assembly
  for the target too (proto-musl), so it doesn't avoid the cross build.
  The capability map chose aws-lc-rs, rustls's default provider. A
  switch would be its own ADR, measured.
- **The system's OpenSSL (native-tls):** a static musl build would
  have to bundle OpenSSL, which is C too (not measured). Linking the
  host's OpenSSL would give up the static binary sb ships.
- **Find the zone without psl:** asking Cloudflare for every suffix of
  the domain in turn would change sb-go's requests and its error texts.
  psl is the list x/net's publicsuffix package embeds.
- **Hand-written pattern matching:** the schemas' `pattern` rules are
  regular expressions. Go's regexp and the regex crate share RE2's
  semantics, which the rules depend on.
- **Smaller builds without changing the stack (not measured):** the
  regex crates without their `perf-*` features, or `opt-level = "s"` or
  `"z"` for the production profile. Each trades speed for size: regex's
  for pattern matching, the profile's for every command. Fat LTO, one codegen
  unit and stripping are already on. Either can be measured and proposed
  on its own; this ADR records the stack as chosen.
- **saphyr for the schemas too:** config decision 5 chose serde-saphyr
  for strict, typed schema loading. ADR-004 already weighed its size.

## Consequences

- When this ADR is accepted, CONSTRAINTS.md's "Production binary size"
  row and its pointer move to this ADR's value, in their own commit.
- `sb --help`'s median moves from 0.76 ms toward 1.1 ms, within the
  startup ratchet's tolerance. That row moves with this ADR's acceptance
  too.
- Later commands that call web services reuse the HTTPS stack. Later
  config commands reuse the YAML and regex stacks.
- The aarch64 release build goes through the same route (ADR-008).
