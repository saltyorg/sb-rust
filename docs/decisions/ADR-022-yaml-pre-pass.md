# ADR-022: Saltbox YAML goes through a pre-pass before saphyr-parser

## Status

Accepted (the maintainer, 2026-11-12: the hardening plan's decision 28,
from Task 42's options)

## Date

2026-11-13

## Context

Decision 3 asks that sb accept the YAML Ansible accepts. Ansible parses
with libyaml. Task 42 measured saphyr-parser 0.3.3, sb's parser, against
libyaml on a checked-in corpus (`crates/config/tests/yaml-corpus`):
of 59 layouts, 47 agreed. saphyr refused nine layouts libyaml accepts
(config-H1: flow collections and quoted scalars continued at or left of
their key's column, a tab after a mapping colon), and accepted three tab
layouts libyaml refuses (config-L1: a tab-only line, a tab before a
top-level key, a tab after a sequence dash). validate-config and
cli-core's inventory read both parse with saphyr-parser, so a file
Ansible reads could fail validation, and `appdata_path` could fall back
to `/opt` for it.

The options, with their measurements, are in
[yaml-acceptance.md](../platform-notes/yaml-acceptance.md): patches
upstream, a libyaml-compatible event source (libyaml-safer, or
unsafe-libyaml-norway) feeding saphyr's loader, or a pre-pass in sb. libyaml-safer
was recommended; the maintainer chose the pre-pass.
granit-parser 2.0.0, already in sb as serde-saphyr's parser, wasn't among
the options; Task 43's library review measured it afterwards at 55 of
the corpus's 59 layouts by acceptance (it refuses the column-0 quoted
continuation and accepts the three config-L1 tab layouts), and its
events would need an adapter to saphyr's loader.

## Decision

Before saphyr-parser reads a Saltbox YAML file, sb rewrites its text
(`sb_cli_core::config::yaml::prepare`), in the first document only:

- a continuation line inside a flow collection or a quoted scalar is
  padded with spaces past the column the construct opened at, and its
  leading tabs become spaces (that white space is no part of the value);
- a tab after a simple key's `:` becomes a space;
- a tab libyaml refuses is refused, naming its line and column: in a
  line's indentation in block context, after `-`, `?` or an explicit
  `:`, in a block scalar's indentation, and in a plain scalar's
  continuation left of its indentation.

The pass tracks quotes, comments, flow depth, block scalars (left
alone) and the block indentation, following libyaml 0.2.5's scanner.
The rewritten text is used only when saphyr refuses the file as written
(the tab checks apply either way), so a gap in the pass's tracking can't
change a value saphyr reads on its own. Padding is capped at 1 MiB per
file, so a crafted file can't make it quadratic. Positions saphyr
reports on the padded text are mapped back to the file's, through a
`SpannedEventReceiver` in front of saphyr's loader. validate-config
(`sb_config::parse::parse`) and the inventory read
(`inventory_document`) both use it. No dependency is added.

Saltbox's schema files, which validate-config reads with serde-saphyr,
are sb's own input rather than files Ansible loads, and stay as they
were.

## Consequences

- The corpus's parser differences are gone: its mismatch list keeps only
  the first-document rule and yaml.v3's scalar resolution, both in the
  config spec's Differences list.
- The binary doesn't take a second parser (Task 42 measured 61 to 78 KB
  for one); sb owns a few hundred lines of scanner rules instead, which
  a saphyr-parser release could make partly redundant. The corpus test
  catches a parser change that moves any case.
- Layouts outside the corpus follow saphyr where the pass doesn't
  reach. Differential fuzzing against libyaml (the author's and the two
  reviewers', several million generated inputs, comparing acceptance and
  scalar values) found no input saphyr alone accepted that sb now
  refuses, and no value the pass changes, once the review's findings
  were fixed. The remaining differences are saphyr's own; the platform
  note lists them.
