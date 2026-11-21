# ADR-028: sb's own reader for Saltbox's schema files

## Status

Accepted (maintainer, 2026-11-21: performance round 5, infra finding 1).
Supersedes the schema half of the config spec's Tech Stack (serde-saphyr
2.0 for the schema files); config decision 12.

## Date

2026-11-21

## Context

validate-config, `sb edit` and motd's check at every login load
Saltbox's schema files (`schema/*.schema.yml`) strictly into typed rules
(config decision 5). They were read with serde-saphyr 2.0.0, which parses
with its own fork of saphyr's parser (granit-parser) and layers serde
on top. Round 5 measured that layer at about 460 instructions a byte of
motd.schema.yml (11.4 KB), against 175 for the parser under it: it
records every mapping key as events, fingerprints it with SipHash,
keeps a set per mapping and replays the key, with comment and budget
bookkeeping per event. `schema::load` was 39% of validate-config's
instructions, 30% of motd's check and 43% of `edit motd.yml`.

Every config file is already parsed by saphyr-parser 0.3.3 (ADR-022).

The library-first rule (CLAUDE.md) asks for a measurement before code
replaces a dependency's; this is it.

## Decision

The schema files are read by a serde `Deserializer` of sb's own over
saphyr-parser's events (`crates/config/src/schema/events.rs`), into the
same derived `Rule`. serde-saphyr leaves sb's dependencies (granit-parser
and annotate-snippets with it); the proto-markdown prototype keeps its
own.

- **Scalars resolve as serde-saphyr 2.0.0's defaults did**: a plain
  `~`, `null` (any case) or empty scalar is null, which reads as none,
  an empty list or an empty mapping where one belongs; YAML 1.1's
  booleans (`yes`, `on`, `y`, any case), also from a quoted scalar where
  a boolean belongs; whole numbers in decimal without a leading zero or
  `0x`/`0o`/`0b`, `_` between digits; Rust's float forms and `.inf`/
  `.nan`, which aren't taken as values. Its limits stay: 64 nested
  collections, 250,000 nodes; a duplicate key in any mapping (a plain
  whole number compared by value) and a second document are refused.
- **Refused, naming the file, line and column** (the maintainer): an
  anchor or alias, a merge key (`<<`), and a tag, which Saltbox's
  schemas don't use either; a character YAML doesn't allow, and a byte
  order mark past the file's start (cli-core's rule for config files).
- **A text saphyr-parser refuses as written** is read again through
  cli-core's pre-pass (ADR-022), which pads a flow collection's or quoted
  scalar's continuation line at or left of its block's indentation, as
  libyaml and yaml.v3 read it. Only then: a schema saphyr reads costs
  nothing more.
- The error texts config's tests pin keep their wording (`invalid
  boolean at line 3, column 13`; `duplicate key `type`` at line 3; `more
  than one YAML document`, now at the `---` that starts the second).
  Others are worded for the reader (`invalid whole number`
  for serde-saphyr's `invalid u64`, `null where a string belongs` for
  `cannot deserialize null into string; use Option<String>`).

## Evidence

**Identical results.** A differential harness ran serde-saphyr 2.0.0
and the new reader over the same texts, comparing the loaded rules
(their debug form, compiled patterns and examples included) or that
both refused:

- Saltbox's real schemas: the seven pinned fixtures, the V4 motd schema
  sb keeps, public Saltbox's six at 60ff6f0 and sb's draft providers
  schema: identical rules, every one.
- The YAML corpus of the YAML work (`crates/config/tests/yaml-corpus`,
  100 cases), and 1,000,524 texts from four seeds of its
  generator and mutator, with schema-shaped tokens added and Saltbox's
  schemas among the texts mutated, and a fifth seed of 250,131 on the
  final code (the same classes, no value difference). Both read 82,599
  of the first million; the rules were identical for 82,598. The one other: a `|+` block scalar ending the
  file on a blank line with no final line break, one line break more
  (saphyr-parser's reading; listed in the config spec's differences).
- Read by serde-saphyr and refused now: 2,201 with an anchor or alias,
  2,913 with a tag, 243 with a merge key (all by decision); 758 with a
  byte order mark inside the text (in a comment or a scalar, where
  granit read it); 81 with content after a document's end marker
  (`...`), which granit ignored; 19 with a tab after `?`, which libyaml
  refuses too; 14 with a flow collection closed at column 0 in a file
  the pre-pass can't take (a NEL, LS or tab in it).
- Refused by serde-saphyr and read now: 228, a quoted scalar continued
  at its key's indentation, which libyaml and sb-go's yaml.v3 read.
- 911,471 texts were refused by both. The error's position differs in
  211,361 of them, mostly because the two parsers stop at different
  first faults.

**Measurements** (production glibc build, ubuntu:22.04; one container per
run, `--network none`, four pinned CPUs; `perf stat`, 20 runs, user
instructions with children; hyperfine 30 runs; peak RSS of 5 runs):

Against main a52ed64, the commit before; user instructions in M (the
spread between runs was 0.01 to 0.03%, 0.2% for the TLS case); wall
from 6 interleaved rounds of 25 runs each:

| Command | Before | After | Wall (ms) |
|---|---:|---:|---:|
| `validate-config` | 21.24 | 18.29 (-13.9%) | 8.96 to 7.86 |
| `validate-config`, old-format motd.yml | 21.82 | 17.71 (-18.8%) | 7.79 to 6.88 |
| `validate-config`, Cloudflare token and Docker Hub | 40.36 | 37.39 (-7.4%) | 20.58 to 20.14 |
| `validate-config`, a 286 KB inventory | 385.75 | 382.67 (-0.8%) | 56.4 to 57.4, within the rounds' spread (task clock 55.3 to 56.1; kernel -0.2 M; the three commits together 54.7) |
| `edit motd.yml` | 12.33 | 10.55 (-14.5%) | 6.98 to 6.25 |
| `edit settings.yml` | 5.51 | 5.20 (-5.7%) | 5.16 to 5.03 |
| motd's config check (`motd --cpu-info --apt`) | 17.25 | 15.51 (-10.1%) | 8.36 to 7.85 |
| `version`, `list`, `docker ps` | equal | equal | equal |

Kernel instructions fell 0.1 to 0.3 M on every command that loads a
schema and were level elsewhere; peak RSS was equal or 0.5 to 1 MiB
lower (it reads in 0.5 MiB steps). One motd.schema.yml load, alone in a
loop: 6.43 M instructions with serde-saphyr, 3.68 M now; 0.21 M of that
is the duplicate-key check. The stripped binary: 21,211,888 to
20,269,712 bytes (-942,176, -4.4%).

## Alternatives Considered

### Keep serde-saphyr
The cost above at every login and every validate-config. Its 2.1.1,
the release Renovate offers, measured slower again (validate-config
+1.1%, motd's check +0.8%).

### serde-saphyr with other options
Its budget and comment bookkeeping can be turned down, but the key
fingerprints and replays are how it serves `deny_unknown_fields` and
duplicate keys; they stay.

### saphyr's `MarkedYaml` tree, then the rules from the tree
A second representation per schema, built and walked, where the
derived `Rule` reads the events once; and the positions of a typed
error would have to be carried by hand.

### Another serde YAML deserializer
`serde_yaml` is archived and deprecated, `serde_yml` archived, and
`serde_yaml_ng` and `serde_norway` had their last releases in 2024
(capability map, "Rejected"); each also brings its own parser, where
this keeps one YAML parser in the binary.

### Support anchors, aliases, tags and merge keys
More code for features Saltbox's schemas don't use; the maintainer
chose refusal with a clear error.

## Consequences

- sb owns about 750 lines with their docs (200 more of tests) that
  track serde-saphyr's scalar rules; the unit tests pin them, and the
  differential harness checked them.
- A schema file is read by the parser config files are read with, one
  YAML parser in the binary.
- Saltbox adding an anchor, a tag or a merge key to a schema fails
  loading with an error naming the line; sb would need a change then.
