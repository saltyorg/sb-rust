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
  the exact list of differences in `mismatches.txt`. Since Task 43 it
  also checks that cli-core's inventory read (`inventory_document` in
  `crates/cli-core/src/config.rs`) accepts exactly the cases
  validate-config accepts, and that the config-L1 tabs are refused
  where libyaml says.
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
- **Documents:** sb read the first document of a file with two;
  libyaml's single-document loader, like Ansible's, refuses it. Since
  Task 59 sb refuses it too.
- saphyr-parser 0.3.4 (2026-11-11) gives the same results on the
  corpus.

## sb's pre-pass (Task 43)

The maintainer chose the pre-pass (decision 28; ADR-022). With it, the
corpus test's mismatch list holds only the first-document line and the
eight resolution lines: every parser difference is closed, and the
three config-L1 tab layouts are refused at the line and column libyaml
gives. Every case accepted before resolves to the same values. Task 43
added three cases from its security review (JSON-style flow pairs,
`{"k":"a #b"}`), which libyaml and sb read alike.

The rewritten text is used only when saphyr refuses the file as
written, and padding stops at 1 MiB per file.

Differential fuzzing (2026-11-13, scratch harnesses that weren't kept,
by the author and Task 43's two reviewers) compared the pre-pass plus
saphyr-parser 0.3.3 with libyaml 0.2.5 through PyYAML 6.0.3 on several
million generated inputs: Saltbox-like
layouts with multi-line flow collections, quoted and block scalars and
tabs, and random strings of YAML indicators, tabs and breaks. No input
that saphyr alone accepted was refused by sb, and the pre-pass added no
acceptance libyaml refuses beyond what saphyr already accepts on the
same text with spaces. What remained was saphyr's own behaviour; the
leniencies (saphyr accepts, libyaml refuses) are refused since Task 59,
below:

- saphyr accepts junk after an indented first mapping that is dedented
  (`  a: 1\nb: 2`): its first document ends at the dedent and sb read
  no further; libyaml refuses the file;
- saphyr accepts a `:` with no key at the start of a line (`: x`);
- a clipped block scalar at the end of a file without a final line
  break keeps its line break in saphyr, not in libyaml (a value
  difference; since Task 61 sb takes saphyr's break off, below);
- saphyr refuses a few layouts libyaml accepts, none in Saltbox's
  files: a block scalar header at the key's column (`a:\n|`), a tab
  after `?` in a flow collection, a comment glued to a token
  (`t: |# c`, `[#\n]`), a block scalar whose leading blank line is
  deeper than a following comment line, a lone `-` in a flow sequence;
- libyaml also breaks lines at NEL, LS and PS (U+0085, U+2028, U+2029);
  saphyr and the pre-pass don't, so a file using them reads differently
  (the config spec lists it);
- a multi-line quoted key in a flow collection (`{"k\n": 1}`) is
  accepted once padded; libyaml refuses it.

## What Ansible refuses, sb refuses (Task 59)

Decision 38 asks for exactly Ansible's acceptance. sb's pre-pass now
also refuses, each at a 1-based line and column, in validate-config and
in the inventory read alike:

- a second document, explicit (`---`) or after the first one's `...`,
  and junk after an indented first mapping (saphyr-parser's own error,
  in libyaml's words, now that sb reads the whole stream);
- a document end marker with no document (`...` alone), and a tab in a
  line's white space after `...`;
- a `:` with no key before it on its line, in block or flow context
  (`: x`, `a: 1\n: b`, `{a: 1, : 2}`, a key split over lines), except
  the one `:` an explicit `?` key takes;
- NEL, LS and PS (U+0085, U+2028, U+2029) anywhere: libyaml reads them as
  line breaks, saphyr as text, so a file holding one reads differently.
  Where libyaml happens to read it as saphyr does (`a: "x<LS>y"`) sb
  refuses what libyaml accepts: the corpus's `ls-line-break` line;
- and, found by a differential fuzz of about 1.7 million generated
  inputs over the same harness (scratch, not kept), extended with `?`, `:`, `...`, `---` and anchors: an
  anchor defined twice (PyYAML's composer refuses it), an anchor or
  alias name that libyaml ends at `.`, `:` or another character saphyr
  takes into it, a `:` right before `,`, `?` or a bracket in a flow
  collection's plain value (`{key:}`), and `|` or `>` starting a value
  in a flow collection;
- and, found by Task 59's code and security reviews: a control
  character libyaml's reader refuses (anything below U+0020 but tab and
  the line breaks, U+007F to U+009F, U+FFFE, U+FFFF; an ESC pasted into
  a password is the likely one), a `%YAML` directive other than 1.1 or
  1.2 and any directive but `%YAML` and `%TAG`, a tag followed by
  anything but white space (or `,` in a flow collection), a `?` in a
  flow collection with no space after it, a top-level block scalar's
  content at column 0, a flow key split over lines in an outer flow
  level, and a collection as a mapping key (PyYAML's constructor:
  "found unhashable key"; nested, saphyr's loader hashed it at every
  level, about 12 s for 46 KB of `? ? ? …`).

After the last change a fuzz of 720,000 inputs, with libyaml's side
also refusing collection keys, held none that sb accepts and libyaml
refuses. It still held block scalars at the end of a file without a
final line break (the value difference above, closed by Task 61) and saphyr's refusals of
a block scalar header at an explicit key's column. sb refuses a few
layouts libyaml reads differently from saphyr: `[?x]` (libyaml a key
`x`, saphyr a scalar `?x`), an alias glued to its key's colon (`*x: 1`,
which saphyr alone refused before too), and a tab-only line between a
directive and `---`; a byte order mark inside a document is saphyr's
own refusal. A mapping with the keys `~` and an empty one is two keys
to validate-config's duplicate rule (yaml.v3 compares text) and one key
twice to the inventory read's loader, which refuses it (Ansible's
loader takes it).

The corpus has a case for each refusal (`multi-document`,
`mapping-indented-then-dedented`, `colon-no-key-*`,
`explicit-key-two-values`, `flow-map-multi-line-quoted-key`,
`content-after-end-marker`, `end-marker-alone`, `nel-line-break`,
`anchor-defined-twice`, `anchor-name-then-dot`,
`flow-plain-colon-before-brace`, `flow-block-indicator`,
`tab-after-end-marker`, `control-character`, `directive-unknown`,
`directive-yaml-1-3`, `tag-before-bracket`,
`flow-question-without-space`, `top-level-block-scalar-at-column-0`,
`flow-key-across-lines`, `collection-as-key`), its libyaml verdict from
`scripts/yaml-corpus.py`, which since Task 59 also records the reader's
errors and a collection as a mapping key, and the test checks sb's
position against libyaml's. sb names an earlier point for three: the
NEL itself (libyaml the line after it), an unknown directive's `%`
(libyaml its name) and a flow `?` (libyaml the token after it).

Not covered by Task 59: a tag Ansible's constructor has none for
(Task 61 refuses it, below), and a value its constructor can't build.

## Nesting depth (Task 59)

**Method:** each depth was loaded in a fresh Python process (a deep
enough one crashes the interpreter), with `yaml.load(text,
Loader=yaml.CSafeLoader)` (PyYAML 6.0.3, libyaml 0.2.5, Python 3.12) and
with ansible-core 2.21.2's own `AnsibleLoader`, on the default 8 MiB
stack (`ulimit -s`, Ubuntu 22.04). The deepest depth that loaded was
found by doubling, then bisection, for four layouts: block sequences
(`- - - … x`), flow sequences (`[[[…x]]]`), block mappings (`a:` each
line two spaces deeper) and flow mappings (`{a: {a: …x}}`). Then each
loader loaded 23,000, 23,500, 23,700 and 23,780 levels of both mapping
layouts ten times.

**Numbers:**

| Layout | CSafeLoader | AnsibleLoader |
|---|---|---|
| Block sequences | 26,174 | 26,176 |
| Flow sequences | 26,178 | 26,178 |
| Block mappings | 23,805 | 23,798 |
| Flow mappings | 23,799 | 23,791 |

Neither loader has a limit of its own: one level past these the
process dies with SIGSEGV (libyaml's composer, through PyYAML's binding,
recurses in C), and the bound moves by a few levels between runs (the
lowest crash seen was 23,792 flow mappings; every repeated run at
23,780 loaded). The constructors don't recurse: both build collections
from generators. The numbers follow the stack size.

**sb:** `yaml::MAX_DEPTH` is 23,000 levels (an alias counted as the
collections it copies, as saphyr's loader copies them): under every
crash seen, so sb never takes a depth Ansible's loader may crash on.
Deeper is refused, with the line and column where it goes too deep, in
validate-config and in the inventory read. The limit is on depth only,
never on length: the inventory (localhost.yml) is as long as its owner
makes it. Flow collections stop at 255 levels, saphyr-parser's own
limit (its flow level is a `u8`), with its "recursion limit exceeded"
error: there sb refuses depths Ansible takes.

A tree deeper than 256 levels is built, walked and dropped on a thread
with a 64 MiB stack (384 MiB in a debug build), reserved, not committed: at 23,800 levels
validate-config's parse and tree took between 64 and 128 MiB of stack
in a debug build and between 16 and 24 MiB in a release one, and a
40 KB `- - - …` inventory overflowed the main thread's stack before.
Key paths are kept so that a deep file costs memory in its depth, not
its depth squared (1.3 GB for a 46 KB file before that change).

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
| sb's pre-pass (Task 43) + saphyr-parser 0.3.3 | 59 | not run | config-L1 refused by the pre-pass |
| granit-parser 2.0.0, default options | 55 (acceptance only) | not run | measured in Task 43's library review; refuses the column-0 quoted case, accepts the three config-L1 tab layouts; already linked as serde-saphyr's parser |
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

## Task 61: tags, the end of a block scalar, the inventory's length

Verified 2026-11-13 for decision 41.

**Software:** ansible-core 2.21.3 (its loader run, and its source read),
PyYAML 6.0.3 with libyaml 0.2.5, Python 3.12; go.yaml.in/yaml/v3 v3.0.5
(sb-go's YAML library, run with Go 1.27.4); saphyr-parser 0.3.3.

**Tags.** Method: each tagged layout was loaded with ansible-core's
`AnsibleLoader` (`ansible/_internal/_yaml/_loader.py`: libyaml's
`CParser`, `AnsibleConstructor`, PyYAML's `Resolver`) and with
`CSafeLoader`, through `get_single_data`, in a scratch probe; the
constructor's source names the same table (`_register_constructors`).
Ansible's loader constructs a node whose tag, as libyaml resolves it, is
one of:

- the non-specific `!`, resolved as if untagged;
- PyYAML's `SafeConstructor` tags: `!!null`, `!!bool`, `!!int`,
  `!!float`, `!!binary`, `!!timestamp`, `!!omap`, `!!pairs`, `!!set`,
  `!!str`, `!!seq`, `!!map`;
- ansible-core's `!!python/dict` and `!!python/unicode` (CSafeLoader
  refuses both);
- ansible-core's own `!unsafe`, `!vault` and `!vault-encrypted` (the last
  deprecated, as are `!!omap` and `!!pairs`), which resolve the node as
  if untagged first.

Any other tag is refused ("could not determine a constructor for the
tag"), at the node's start, on a scalar, a collection, a key or the
root: `!custom`, `!!merge` and `!!value` as values, `!!python/str`, a
`%TAG` handle naming another prefix (`%TAG ! tag:yaml.org,2002:` makes
`!unsafe` the unknown `tag:yaml.org,2002:unsafe`). Verbatim and escaped
forms count as what they resolve to (`!<!unsafe>`, `!%75nsafe`,
`!<tag:yaml.org,2002:%73tr>`). saphyr-parser 0.3.3 resolves each of
these layouts' tags to libyaml's (its handle replaced by the prefix,
escapes decoded), and sb refuses the others at the same line and column,
in validate-config and the inventory read alike. With two such tags in
different collections PyYAML names the first it constructs, nested
collections later; sb names the first in the file. sb reads `!unsafe`
and `!vault` values as their text, as yaml.v3 does.

Still not covered: a value Ansible's constructor can't build (`!!int abc`,
`!!str [a]`, `!vault {}`): validate-config reports yaml.v3's decode
error for a scalar one; the inventory read takes them. `!!merge` and
`!!value` on a mapping key, which PyYAML's `flatten_mapping` reads, are
refused.

The corpus has `tag-custom` (refused), `tag-ansible` (`!unsafe` and
`!vault` as Saltbox would write them) and `tag-ansible-constructed`
(every constructed tag but `!!binary`, whose value sb keeps as text);
`scripts/yaml-corpus.py` takes the tags Ansible constructs from
ansible-core's constructor table, so its regeneration needs ansible-core
(2.19 or later, where the loader moved to `ansible._internal`), records
its version, and flags a tag a later ansible-core drops. Task 61's code
review ran a differential fuzz of 60,000 tag-heavy inputs (handles,
`%TAG` redefinitions, verbatim and escaped forms, keys, roots) against
`AnsibleLoader` (scratch, not kept): none that Ansible refuses for a
missing constructor did sb accept.

**The end of a block scalar.** Method: the same layouts through
yaml.v3 3.0.5's `Unmarshal` (a scratch Go program), libyaml through
`CSafeLoader`, and saphyr-parser 0.3.3's events. At the end of a file
with no final line break, yaml.v3 gives libyaml's value in every layout:
`a: |\n  x` is `x`, as are `|+`, `|-` and `>`; a last line of spaces
only is a blank line when it reaches the indentation (`a: |\n  x\n  ` is
`x\n`) and content past it. yaml.v3's `yaml_parser_scan_block_scalar`
(`scannerc.go`, "Chomp the tail") adds only the line break it read.
saphyr-parser adds a line break there when the last line reaches the
indentation and the scalar isn't stripped, and gives a `|+` scalar with
one blank line and no content `\n` (libyaml and yaml.v3: empty). So no
Difference is listed: sb takes saphyr's added break off, and the
corpus's `block-scalar-clip-at-end-without-newline` lines are gone, with
two cases added (`|+`, and a last line of spaces). The pre-pass decides
from the text whether saphyr added one (the scalar's chomping, its
content indentation, explicit or found, whether a content line came, and
the last line's length), not from the scalar's span, which starts at a
tag or an anchor when it has one (`a: !vault |`). The unit tests hold
the layouts run against libyaml: tagged and anchored headers,
indentation indicators with and without content, keep and strip, and
mid-file scalars, which keep saphyr's value. A differential fuzz against
libyaml's events (the code review's harness, scratch, not kept; 350,000
generated block-scalar layouts with and without properties) left only
one class of difference, which Task 61 didn't touch: a top-level block
scalar followed by a comment at column 0 (`--- >\n\n# c`), which saphyr
read as content and libyaml as the scalar's end. The module build's
follow-up closed it in the pre-pass: under a top-level block scalar with
no content yet, a comment at column 0 ends the scalar as libyaml ends it
(its value is the blank lines above, kept only by `+`), the comment
lines after it are emptied so saphyr doesn't read them on, content after
it is refused where libyaml refuses it, and a `---` there, which saphyr
also read as content, is refused as the second document libyaml finds.
Checked against libyaml 0.2.5 through `PyYAML` 6.0.3, case by case and
in the corpus and by the code review's differential run (about 620,000
generated inputs: no value differs). The differences left are refusals
the pass already made before: spaces-only lines in a block scalar with
no content yet, deeper than a later end marker or indented comment
(`--- |\n   \n...`, `k: |\n    \n  # c`), and a comment at column 0
under a header with an indentation indicator (`|1\n# c`), which saphyr
refuses and libyaml reads as empty. No Saltbox file has a top-level
block scalar.

**The inventory's length.** localhost.yml is read whole at any length
(`read_inventory`), for validate-config and the inventory read; the
other files keep cli-core's 1 MiB cap. A 3.5 MB inventory of 200,000
keys is read and checked in the tests. The cost grows with it: Task 61's
reviews measured sb's start-up inventory read in a release build at
0.75 s and 105 MB peak for 3.5 MB, 1.3 s and 211 MB for 7 MB, and 10 to
12 s and 1.55 GB for 56 MB of `key: value` lines (about 30 times the
file at the peak; 64 MB of bare line breaks took 3.1 GB). The time is
linear in the file over the crafted shapes they tried.

The module build's follow-up cut that cost, keeping no length cap: the
pre-pass builds its rewritten text only when saphyr can't read the
original and walks the lines one at a time, and the inventory read takes
`server_appdata_path` from the parser's events, deciding duplicate keys
and the alias budget as saphyr 0.3.3's loader does without building its
tree (a test compares the two on the YAML corpus and on crafted cases).
Measured the same way afterwards (release build, peak resident size):
3.5 MB of `key: value` lines, 0.35 s and 16 MB; 56 MB of them, 5.9 s
and 227 MB (887 MB before the change on the same file, about 4 times
the file now); 64 MB of bare line breaks, 3.1 s and 126 MB. One shape
costs as much as before: each anchored collection is copied whole when
it ends, as the loader copies it for its aliases, so anchors nested
inside each other grow with the square of their depth (2,500 nested in
3.1 MB peaked at 1.48 GB with either read, the security review's
measurement). No Saltbox inventory nests anchors.
