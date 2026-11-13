# YAML acceptance: libyaml, saphyr-parser and the candidates

Verified 2026-11-12 for decision 3 (sb accepts the YAML Ansible accepts;
hardening plan Task 42, findings config-H1 and config-L1).

**Software:** PyYAML 6.0.3 with its bundled libyaml 0.2.5 (the PyPI
wheel; `yaml.__with_libyaml__` true), Python 3.12; ansible-core 2.21.3
(its loader source, read); saphyr-parser 0.3.3 and 0.3.4, saphyr 0.3.3;
libyaml-safer 0.3.0; unsafe-libyaml-norway 0.2.15; yaml-rust2 0.13.0;
rust-yaml 1.1.0; the yaml-test-suite data vendored in saphyr's repository
at tag v0.3.3 (read as data only).

**Method:**
- **The corpus:** `crates/config/tests/yaml-corpus/cases/*.yml`, one
  layout per file. It holds the review's table (config-H1), the tab layouts
  (config-L1 and a tab after a dash under a key), and the layouts Saltbox's
  config files use: the pinned schemas' shapes, the valid fixtures',
  motd's two instance forms, the announcements format documented in
  sb-go, plain dates and a second document. Six cases both sides refuse
  are included too: the four `error-*` cases, a tab-indented nested key,
  and a tab before spaces in an indent.
- **libyaml's results:** generated once by `scripts/yaml-corpus.py`
  through PyYAML's `CSafeLoader` (`get_single_node`, then the safe
  constructor on each scalar). They are written to `expected.json`: the
  verdict, the error with its 1-based line and column, and each accepted
  case's scalar values. Each value has its key path, style, text as
  parsed, and resolved value as `[type, text]`; a date or time keeps its
  source text, and an empty collection is a `seq` or `map` entry. The
  generator records the PyYAML and libyaml versions in its output. Run
  it again only when a case changes. Its mypy check needs types-PyYAML.
- **sb's side:** `crates/config/tests/yaml_corpus.rs` runs
  `sb_config::parse::parse` and `tree::build` on each case. It asserts
  the exact list of differences in `mismatches.txt`. It doesn't run
  cli-core's own loader (the inventory read, `first_document` in
  `crates/cli-core/src/config.rs`), which uses saphyr-parser the same
  way; a fix has to cover both.
- **The candidates:** a scratch harness ran each parser over the corpus
  (acceptance, plus scalar path, style and text for the event parsers).
  The decoded yaml-test-suite and the panic reproducers from
  libyaml-safer's issue tracker went through the same harness. The
  harness wasn't kept, so the table below can't be re-run from the
  repository. Only the saphyr-parser 0.3.3 row is backed by the
  checked-in test. Upstream code was not run: the scanner patches were
  applied to saphyr-parser 0.3.3's crates.io source and built into the
  harness. Binary sizes are stripped `x86_64-unknown-linux-musl` builds
  with sb's production profile (fat LTO, one codegen unit).

## Which parser Ansible uses

ansible-core 2.21.3's loader (`ansible/_internal/_yaml/_loader.py`)
builds on `yaml.cyaml.CParser` when PyYAML has libyaml (`HAS_LIBYAML`).
Otherwise it uses PyYAML's pure-Python parser, which is stricter about
tabs. On the corpus it refuses five cases libyaml accepts: a tab after
a colon (twice), a tab before a comment, a tab in a flow sequence, and
a tab inside a plain value. PyPI's PyYAML wheels bundle libyaml, so
libyaml is the reference.

## libyaml against saphyr-parser 0.3.3 (sb today)

- **libyaml accepts and sb refuses** (config-H1):
  - a flow sequence closed at its key's column after a quoted item
    (`list: ["a",\n  "b"\n]`), or left of its key's column;
  - an empty flow collection split over lines (`[\n]`, `{\n}`);
  - flow items one per line, closed at the key's column;
  - a double-quoted scalar continued at column 0;
  - a tab after a mapping colon (`key:\tvalue`).
- **sb accepts and libyaml refuses** (config-L1): a tab-only line between
  entries, a tab before a top-level key, and a tab after a sequence dash
  (`-\tvalue`).
- **sb accepts these, as libyaml does:**
  - a flow sequence closed at the key's column after a plain or nested
    item (config-H1 lists the nested case as refused; it isn't);
  - an indented multi-line quoted or plain scalar;
  - a tab before a comment, inside a plain value, or in a flow sequence.
- **Against YAML 1.2** (yaml-test-suite): the flow layouts and the
  column-0 quoted continuation are invalid YAML 1.2. The suite's QB6E is
  the quoted case, and saphyr's maintainers say so for the flow layouts
  (saphyr issues 55 and 73). The tab after a colon is valid YAML 1.2 (the
  suite's J3BT and 6BCT have tabs as separation). saphyr refuses it only
  when a letter or `-` follows the tab, a guard against the invalid
  Y79Y-09 (`:\tkey:`). The config-L1 tab layouts are valid YAML 1.2
  (DK95-03, DK95-04, 6BCT). libyaml, a YAML 1.1 parser, refuses them.
- **Scalar resolution:** sb resolves plain scalars as yaml.v3 does
  (docs/specs/config.md). PyYAML's resolver is YAML 1.1. The corpus
  shows `yes`/`no`/`on`/`off` (bool there), `12:30` (sexagesimal 750
  there), `1e3` (a string there) and an integer past 64 bits (an int
  there, a float in yaml.v3). The schema rules read the Ansible booleans
  from the string either way.
- **Documents:** sb reads the first document of a file with two;
  libyaml's single-document loader, like Ansible's, refuses it.
- saphyr-parser 0.3.4 (2026-11-11) gives the same results on the
  corpus.

## The candidates on the corpus

Acceptance against libyaml's verdict over 59 cases (the multi-document
case left out: that is the loader's rule, not the parser's). For the
event parsers, an accepted case also needs the same scalar paths,
styles and texts.

| Parser | Matches libyaml | yaml-test-suite (405) disagreements | Notes |
|---|---|---|---|
| saphyr-parser 0.3.3 | 47 | 1 | config-H1 and config-L1 |
| saphyr-parser 0.3.3, tab patch | 49 | 2 (Y79Y-09 accepted) | |
| saphyr-parser 0.3.3, flow and quoted patch | 54 | 2 (QB6E accepted) | the flow guard alone costs none |
| saphyr-parser 0.3.3, both patches | 56 | 3 | config-L1 remains |
| pre-pass prototype + saphyr-parser 0.3.3 | 56 | not run | config-L1 remains |
| yaml-rust2 0.13.0 | 47 (acceptance only) | not run | same scanner lineage as saphyr |
| rust-yaml 1.1.0 | 56 (acceptance only) | not run | refuses the column-0 quoted case; accepts tab-after-dash and tab-only-line |
| unsafe-libyaml-norway 0.2.15 | 59 (acceptance only) | not run | libyaml translated by c2rust; unsafe throughout |
| libyaml-safer 0.3.0 | 59 | 72 (libyaml's YAML 1.1 gaps) | panics on 2 suite inputs and both open issue reproducers |

- **libyaml-safer's panics** (0.3.0, at the event level; issues 6 and 7,
  open since 2024-08):
  - a block scalar whose last line is only spaces, with no final newline
    (`foo: |\n  x\n   `, the suite's L24T-01; libyaml accepts it);
  - a tag followed by `,` in a flow collection (`[!,` refused by libyaml;
    WZ62 accepted by libyaml);
  - `a: ''|\n 0`, refused by libyaml.

  unsafe-libyaml-norway gives libyaml's result on all four. A
  differential fuzz of 300,000 random inputs, built from YAML indicators,
  tabs, breaks and words, compared libyaml-safer with
  unsafe-libyaml-norway. Acceptance never differed. libyaml-safer
  panicked on 1,261 inputs, at three sites in `scanner.rs`:
  - a line break read past the end of input (908);
  - a backslash at the end of input inside a double-quoted scalar
    (256);
  - the tag-then-comma `TODO` (97).
- **libyaml-safer through saphyr's loader:** its events, translated to
  saphyr-parser's `Event` and `Span` and fed to saphyr's `YamlLoader`
  (sb's settings), give sb's tree. On the corpus that leaves only the
  eight resolution lines and the multi-document line. Tags and the duplicate-key pass weren't
  translated in the prototype.
- **Binary size** (sb's production profile, musl, stripped):
  saphyr-parser alone 508,664 bytes. With libyaml-safer beside it,
  586,488 (+77,824). With unsafe-libyaml-norway, 570,104 (+61,440).
