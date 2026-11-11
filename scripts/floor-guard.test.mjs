// Tests for floor-guard.mjs: `node --test scripts/floor-guard.test.mjs`
// (scripts/check.sh fast runs it). Each case commits a small base tree in a
// scratch repository, applies one change and runs the guard there. Every
// weakening has a clean case beside it.
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const guard = join(dirname(fileURLToPath(import.meta.url)), 'floor-guard.mjs');

// The scratch repositories see no configuration and no repository of the
// caller's (a hook exports GIT_DIR and friends); HOME and XDG_CONFIG_HOME
// are each run's scratch directory.
const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
env.GIT_CONFIG_GLOBAL = '/dev/null';
env.GIT_CONFIG_SYSTEM = '/dev/null';

// run(base, change): the guard's findings, as "rule file" strings, for
// `change` (path -> new content, null deletes) on top of the committed
// `base` (path -> content). With `commit`, the change is committed too and
// the guard is given the base commit, as a range run is. `config` is git
// configuration set in the scratch repository (key -> value). A list of
// changes is a commit each, the last one left uncommitted unless `commit`
// is set; the guard is then given the commit before the first of them.
const run = (base, change, { commit = false, config = {} } = {}) => {
  const dir = mkdtempSync(join(tmpdir(), 'floor-guard-'));
  try {
    const scratch = { ...env, HOME: dir, XDG_CONFIG_HOME: dir };
    const git = (...args) => execFileSync('git', args, { cwd: dir, env: scratch, stdio: 'pipe' });
    const save = () => {
      git('add', '-A');
      git('-c', 'user.name=test', '-c', 'user.email=test', 'commit', '-q', '-m', 'test');
    };
    const write = (files) => {
      for (const [f, text] of Object.entries(files)) {
        if (text === null) { rmSync(join(dir, f)); continue; }
        mkdirSync(dirname(join(dir, f)), { recursive: true });
        writeFileSync(join(dir, f), text);
      }
    };
    git('init', '-q');
    for (const [key, value] of Object.entries(config)) git('config', key, value);
    write({ 'README.md': 'base\n', ...base });
    save();
    const steps = [change].flat();
    steps.forEach((step, i) => {
      write(step);
      if (commit || i < steps.length - 1) save();
    });
    const r = spawnSync(process.execPath, [guard, ...(commit ? ['--base', 'HEAD~' + steps.length] : [])],
      { cwd: dir, env: scratch, encoding: 'utf8' });
    const found = [...r.stderr.matchAll(/^ {2}\[([\w-]+)\] (\S+):/gm)].map((m) => m[1] + ' ' + m[2]);
    return { status: r.status, found, output: r.stdout + r.stderr };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

// flags: the change gives exactly these findings, in any order. clean: it
// gives none.
const flags = (base, change, expected, options) => {
  const r = run(base, change, options);
  assert.deepEqual(r.found.sort(), [expected].flat().sort(), r.output);
  assert.equal(r.status, 1, r.output);
};
const clean = (base, change, options) => {
  const r = run(base, change, options);
  assert.deepEqual(r.found, [], r.output);
  assert.equal(r.status, 0, r.output);
};
// edit(files, path, from, to): `files` with one text replaced in one file.
const edit = (files, path, from, to) => {
  assert.ok(files[path].includes(from), 'the fixture has no: ' + from);
  return { [path]: files[path].replace(from, to) };
};

const CHECK = 'scripts/check.sh';
const checkSh = {
  [CHECK]: `#!/usr/bin/env bash
set -euo pipefail
# Thresholds: keep in step with CONSTRAINTS.md.
COVERAGE_MIN=90       # % of changed lines covered
SIZE_MAX_BYTES=45711520 # production musl binary
step() { printf '== %s\\n' "$1"; }
fast() {
  step "clippy (warnings are errors)"
  cargo clippy --workspace --all-targets --quiet -- -D warnings
  step "floor guard"
  node scripts/floor-guard.mjs --base "$base"
}
declare -A MUTATION_TESTERS=()
# sb-journal: judged by its own tests and sb-host's.
MUTATION_TESTERS[sb-journal]="sb-host"
for c in "\${!MUTATION_TESTERS[@]}"; do echo "\${MUTATION_TESTERS[$c]}"; done
fast
`,
};

test('check.sh: a lowered minimum is flagged, a raised one is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=80 '), 'threshold-loosened ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=95 '));
});

test('check.sh: a raised maximum is flagged, a lowered one is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, '=45711520', '=55711520'), 'threshold-loosened ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, '=45711520', '=35711520'));
});

test('check.sh: a threshold that is removed, or no longer a number, is flagged', () => {
  flags(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90       # % of changed lines covered\n', ''),
    'threshold-removed ' + CHECK);
  flags(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=${COVERAGE:-0} '),
    'threshold-changed ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, '# % of changed lines covered', '# percent of changed lines'));
});

test('check.sh: a dropped -D warnings is flagged, a reindented line is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, ' -- -D warnings', ''), 'gate-line-removed ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, '  cargo clippy', '    cargo clippy'));
});

test('check.sh: a new MUTATION_TESTERS entry is flagged, a comment beside it is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, 'for c in', 'MUTATION_TESTERS[sb-exec]="sb-docker"\nfor c in'),
    'mutation-scope-narrowed ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, 'for c in', '# Add a crate when its tests reach a listed one.\nfor c in'));
});

test('check.sh: a removed step is flagged, an added step is clean', () => {
  flags(checkSh,
    edit(checkSh, CHECK, '  step "floor guard"\n  node scripts/floor-guard.mjs --base "$base"\n', ''),
    ['gate-line-removed ' + CHECK, 'gate-line-removed ' + CHECK]);
  clean(checkSh, edit(checkSh, CHECK, '  step "floor guard"', '  step "format"\n  cargo fmt --check\n  step "floor guard"'));
});

test('check.sh: a moved line and a removed comment are clean', () => {
  clean(checkSh, edit(checkSh, CHECK, 'set -euo pipefail\n# Thresholds: keep in step with CONSTRAINTS.md.\n',
    '\nset -euo pipefail\n'));
});

const DENY = 'deny.toml';
const denyToml = {
  [DENY]: `[advisories]
# An ignore needs a CONSTRAINTS.md Exceptions row.
unmaintained = "all"
yanked = "deny"
ignore = [
    { id = "RUSTSEC-2025-0141", reason = "E1: bincode via syntect" },
]

[licenses]
allow = [
    "Apache-2.0",
    "MIT",
]
confidence-threshold = 0.9

[bans]
wildcards = "deny"
features = [
    { crate = "lzma-rust2", deny = ["optimization"] },
]
deny = [
    # cli-core: every module builds on it.
    { crate = "sb-cli-core", wrappers = ["sb", "sb-term-ui"] },
    { crate = "sb-docker", wrappers = ["sb"] },
]

[sources]
unknown-git = "deny"
`,
};
const ignoreLine = '    { id = "RUSTSEC-2025-0141", reason = "E1: bincode via syntect" },\n';

test('deny.toml: a new advisory ignore is flagged (docs-H1: the alternative that could not fire)', () => {
  flags(denyToml, edit(denyToml, DENY, ignoreLine, ignoreLine + '    { id = "RUSTSEC-2024-0320", reason = "yaml-rust" },\n'),
    'advisory-ignored ' + DENY);
  flags(denyToml, edit(denyToml, DENY, ignoreLine, ignoreLine + '    "RUSTSEC-2024-0320",\n'),
    'advisory-ignored ' + DENY, { commit: true });
});

test('deny.toml: a removed ignore and a comment in the list are clean', () => {
  clean(denyToml, edit(denyToml, DENY, ignoreLine, ''));
  clean(denyToml, edit(denyToml, DENY, ignoreLine, '    # E1 expires in January.\n' + ignoreLine));
});

test('deny.toml: a widened wrappers list is flagged, a narrowed one is clean', () => {
  flags(denyToml, edit(denyToml, DENY, 'wrappers = ["sb"] }', 'wrappers = ["sb", "sb-exec"] }'),
    'wrappers-widened ' + DENY);
  clean(denyToml, edit(denyToml, DENY, 'wrappers = ["sb", "sb-term-ui"] }', 'wrappers = ["sb"] }'));
});

test('deny.toml: a removed architecture entry is flagged, a new one is clean', () => {
  flags(denyToml, edit(denyToml, DENY, '    { crate = "sb-docker", wrappers = ["sb"] },\n', ''),
    'wrappers-widened ' + DENY);
  clean(denyToml, edit(denyToml, DENY, '    { crate = "sb-docker"',
    '    { crate = "sb-host", wrappers = ["sb"] },\n    { crate = "sb-docker"'));
});

test('deny.toml: an added licence is flagged, a removed one is clean', () => {
  flags(denyToml, edit(denyToml, DENY, '    "MIT",\n', '    "MIT",\n    "MPL-2.0",\n'), 'licence-added ' + DENY);
  clean(denyToml, edit(denyToml, DENY, '    "MIT",\n', ''));
});

test('deny.toml: a deleted file is flagged', () => {
  flags(denyToml, { [DENY]: null }, 'gate-file-deleted ' + DENY);
});

const ROOT = 'Cargo.toml';
const rootToml = {
  [ROOT]: `[workspace]
members = ["crates/*"]

[workspace.dependencies]
anyhow = "1"

[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
allow_attributes = "deny"
todo = "deny"

[profile.production]
inherits = "release"
`,
};

test('Cargo.toml: clippy::all set to allow is flagged (docs-H1: the alternative that could not fire)', () => {
  flags(rootToml, edit(rootToml, ROOT, 'todo = "deny"\n', 'todo = "deny"\nall = "allow"\n'), 'lint-weakened ' + ROOT);
});

test('Cargo.toml: a lowered lint level is flagged, a raised one is clean', () => {
  flags(rootToml, edit(rootToml, ROOT, 'todo = "deny"', 'todo = "warn"'), 'lint-weakened ' + ROOT);
  flags(rootToml, edit(rootToml, ROOT, 'unsafe_code = "forbid"', 'unsafe_code = "deny"'), 'lint-weakened ' + ROOT);
  flags(rootToml, edit(rootToml, ROOT, 'level = "warn"', 'level = "allow"'), 'lint-weakened ' + ROOT);
  clean(rootToml, edit(rootToml, ROOT, 'missing_debug_implementations = "warn"', 'missing_debug_implementations = "deny"'));
});

test('Cargo.toml: a removed lint is flagged, a new lint and a new dependency are clean', () => {
  flags(rootToml, edit(rootToml, ROOT, 'allow_attributes = "deny"\n', ''), 'lint-removed ' + ROOT);
  clean(rootToml, edit(rootToml, ROOT, 'todo = "deny"\n', 'todo = "deny"\ndbg_macro = "deny"\n'));
  clean(rootToml, edit(rootToml, ROOT, 'anyhow = "1"\n', 'anyhow = "1"\nserde = "1"\n'));
});

const CRATE = 'crates/exec/Cargo.toml';
const crateToml = {
  [CRATE]: `[package]
name = "sb-exec"

[dependencies]
anyhow.workspace = true

[lints]
workspace = true
`,
};

test('a crate Cargo.toml: leaving the workspace lints is flagged, a new dependency is clean', () => {
  flags(crateToml, edit(crateToml, CRATE, '\n[lints]\nworkspace = true\n', ''), 'lint-removed ' + CRATE);
  flags(crateToml, edit(crateToml, CRATE, '[lints]\nworkspace = true\n', '[lints]\nworkspace = false\n'), 'lint-weakened ' + CRATE);
  clean(crateToml, edit(crateToml, CRATE, 'anyhow.workspace = true\n', 'anyhow.workspace = true\nserde.workspace = true\n'));
});

test('a crate Cargo.toml: a lint of its own set to allow is flagged, a deleted crate is clean', () => {
  flags(crateToml, edit(crateToml, CRATE, '[lints]\nworkspace = true\n', '[lints.clippy]\ntoo_many_lines = "allow"\n'),
    ['lint-removed ' + CRATE, 'lint-weakened ' + CRATE]);
  clean(crateToml, { [CRATE]: null });
});

test('a prototype Cargo.toml is not guarded', () => {
  const proto = { 'prototypes/tui/Cargo.toml': crateToml[CRATE] };
  clean(proto, edit(proto, 'prototypes/tui/Cargo.toml', '\n[lints]\nworkspace = true\n', ''));
});

const TOOLCHAIN = 'rust-toolchain.toml';
const toolchainToml = {
  [TOOLCHAIN]: `[toolchain]
channel = "1.99.1"
components = ["clippy", "rustfmt", "llvm-tools"]
targets = ["x86_64-unknown-linux-musl"]
`,
};

test('rust-toolchain.toml: a changed channel is flagged, an added target is clean', () => {
  flags(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"1.99.1"', '"stable"'), 'toolchain-changed ' + TOOLCHAIN);
  clean(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"x86_64-unknown-linux-musl"',
    '"x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"'));
});

test('rust-toolchain.toml: a removed component or file is flagged, an added component is clean', () => {
  flags(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"clippy", ', ''), 'toolchain-changed ' + TOOLCHAIN);
  flags(toolchainToml, { [TOOLCHAIN]: null }, 'gate-file-deleted ' + TOOLCHAIN);
  clean(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"llvm-tools"', '"llvm-tools", "rust-src"'));
});

for (const config of ['.cargo/mutants.toml', 'clippy.toml', '.gitleaksignore', '.cargo/config.toml']) {
  test('a new tool config is flagged: ' + config, () => {
    flags({}, { [config]: 'exclude = "everything"\n' }, 'tool-config-added ' + config);
    flags({}, { [config]: '' }, 'tool-config-added ' + config, { commit: true });
  });
}

test('a tool config: added lines are flagged; removed lines and other new files are clean', () => {
  const config = { 'clippy.toml': '# clippy configuration\nmsrv = "1.99"\n' };
  flags(config, edit(config, 'clippy.toml', 'msrv', 'too-many-lines-threshold = 500\nmsrv'),
    'tool-config-changed clippy.toml');
  flags(config, edit(config, 'clippy.toml', 'msrv', 'too-many-lines-threshold = 500\n# why\ntoo-many-arguments-threshold = 20\nmsrv'),
    ['tool-config-changed clippy.toml', 'tool-config-changed clippy.toml']);
  clean(config, edit(config, 'clippy.toml', 'msrv = "1.99"\n', ''));
  clean(config, edit(config, 'clippy.toml', '# clippy', '# Why each setting is here.\n# clippy'));
  clean({}, { '.cargo/notes.md': 'notes\n', 'docs/clippy.toml.md': 'notes\n' });
});

const COMPARE = 'crates/journal/tests/compare.sh';
const testScripts = {
  [COMPARE]: `#!/usr/bin/env bash
set -euo pipefail
# The default variant's pread counts may not exceed these.
PREADS_TAIL_MAX=1728
PREADS_BACK_MAX=2417
max_tail=$PREADS_TAIL_MAX
bad() { echo "FAIL $1"; status=1; }
[ "$preads" -le "$max_tail" ] || bad "tail: $preads preads"
`,
  'crates/journal/tests/seeds.sh': '#!/usr/bin/env bash\necho seeds\n',
};

test('tests/*.sh: a raised bound is flagged, a lowered one is clean', () => {
  flags(testScripts, edit(testScripts, COMPARE, 'PREADS_TAIL_MAX=1728', 'PREADS_TAIL_MAX=1900'),
    'threshold-loosened ' + COMPARE);
  clean(testScripts, edit(testScripts, COMPARE, 'PREADS_BACK_MAX=2417', 'PREADS_BACK_MAX=2400'));
});

test('tests/*.sh: a deleted script is flagged, a new script and an edited line are clean', () => {
  flags(testScripts, { 'crates/journal/tests/seeds.sh': null }, 'gate-file-deleted crates/journal/tests/seeds.sh');
  clean(testScripts, { 'crates/journal/tests/paging.sh': '#!/usr/bin/env bash\necho paging\n' });
  clean(testScripts, edit(testScripts, COMPARE, 'bad "tail: $preads preads"', 'bad "tail: $preads preads, at most $max_tail"'));
});

test('tests/*.sh: an added expected_failure entry is flagged, whatever its rating letter', () => {
  for (const entry of ['expected_failure follow-H1 32 racy the follow count', '  expected_failure follow-N1 32 racy count',
    'expected_failure config-T2 41 always anchors']) {
    flags(testScripts, edit(testScripts, COMPARE, 'bad() {', entry + '\nbad() {'), 'expected-failure-added ' + COMPARE);
  }
});

test('tests/*.sh: an expected_failure entry in a new, untracked script is flagged', () => {
  flags(testScripts, { 'crates/journal/tests/paging.sh': '#!/usr/bin/env bash\nexpected_failure page-H1 32 always the tail\n' },
    'expected-failure-added crates/journal/tests/paging.sh');
});

test('tests/*.sh: a definition named expected_failure is flagged', () => {
  flags(testScripts, edit(testScripts, COMPARE, 'bad() {', 'expected_failure() { echo "expected: $*"; }\nbad() {'),
    'expected-failure-added ' + COMPARE);
  flags(testScripts, edit(testScripts, COMPARE, 'bad() {', 'EXPECTED_FAILURES=(\nbad() {'),
    'expected-failure-added ' + COMPARE);
  flags(testScripts, edit(testScripts, COMPARE, 'bad() {', 'expected_failures() { cat "$list"; }\nbad() {'),
    'expected-failure-added ' + COMPARE);
});

test('tests/*.sh: any line with the word expected_failure is flagged, whatever its shape', () => {
  for (const line of ['[ -n "$CI" ] && expected_failure follow-H1 32 racy count', 'command expected_failure follow-H1 32 racy count',
    'if ! expected_failure "follow-H1" "Task 32" racy count; then', 'expected_failure follow-H1 32', 'xf=expected_failure']) {
    flags(testScripts, edit(testScripts, COMPARE, 'bad() {', line + '\nbad() {'), 'expected-failure-added ' + COMPARE);
  }
});

test('tests/*.sh: lines that only talk about expected failures are clean', () => {
  for (const line of ['# Named expected failures (tasks/plan.md) go below.', 'echo "expected failures: $n"',
    'bad "sd-h264 always decodes"', 'known follow-H1 racy', 'note "$(expected_failure_count) entries"']) {
    clean(testScripts, edit(testScripts, COMPARE, 'bad() {', line + '\nbad() {'));
  }
});

const GUARD = 'scripts/floor-guard.mjs';
const guardMjs = {
  [GUARD]: `// floor-guard.mjs: diff-scoped enforcement of the CONSTRAINTS.md floor.
const STUBS = /\\btodo!\\s*\\(|\\bTODO\\b/;
const SKIPS = /#\\[\\s*ignore\\b/;
for (const { file, text } of added) {
  if (STUBS.test(text)) flag('unfinished-work', file, text);
  if (SKIPS.test(text)) flag('test-made-easier', file, text);
}
`,
};

test('floor-guard.mjs: a removed rule is flagged, an added rule is clean', () => {
  flags(guardMjs, edit(guardMjs, GUARD, "  if (SKIPS.test(text)) flag('test-made-easier', file, text);\n", ''),
    'gate-line-removed ' + GUARD);
  clean(guardMjs, edit(guardMjs, GUARD, '}\n', "  if (/dbg!/.test(text)) flag('unfinished-work', file, text);\n}\n"));
});

test('floor-guard.mjs: a narrowed pattern is flagged, a new pattern and a reworded comment are clean', () => {
  flags(guardMjs, edit(guardMjs, GUARD, '|\\bTODO\\b/;', '/;'), 'gate-line-removed ' + GUARD);
  clean(guardMjs, edit(guardMjs, GUARD, 'const SKIPS', 'const EMPTY = /\\{\\s*\\}/;\nconst SKIPS'));
  clean(guardMjs, edit(guardMjs, GUARD, 'diff-scoped enforcement', 'enforcement, by diff,'));
});

const MESSAGES = 'scripts/check-commit-messages.sh';
const messagesSh = {
  [MESSAGES]: `#!/usr/bin/env bash
# Fails if any commit breaks the commit message rules.
set -euo pipefail
pattern='^(feat|fix|docs)(\\([a-z0-9-]+\\))?!?: [^A-Z ](.{0,98}[^.])?$'
status=0
if ! [[ "$subject" =~ $pattern ]]; then status=1; fi
if ! grep -qE '^Review: .+' <<<"$body"; then
  echo "Missing 'Review:' line: $sha"
  status=1
fi
exit $status
`,
};

test('check-commit-messages.sh: a loosened pattern is flagged, a reworded comment is clean', () => {
  flags(messagesSh, edit(messagesSh, MESSAGES, '[^A-Z ](.{0,98}[^.])?$', '.*$'), 'gate-line-removed ' + MESSAGES);
  clean(messagesSh, edit(messagesSh, MESSAGES, '# Fails if', '# Exits 1 if'));
});

test('check-commit-messages.sh: a removed rule is flagged, an added rule is clean', () => {
  flags(messagesSh, edit(messagesSh, MESSAGES, "  echo \"Missing 'Review:' line: $sha\"\n  status=1\n", '  :\n'),
    ['gate-line-removed ' + MESSAGES, 'gate-line-removed ' + MESSAGES]);
  clean(messagesSh, edit(messagesSh, MESSAGES, 'exit $status',
    'if ! grep -qE \'^Library-check: .+\' <<<"$body"; then status=1; fi\nexit $status'));
});

test('a deleted guard script is flagged once, not line by line', () => {
  flags(messagesSh, { [MESSAGES]: null }, 'gate-file-deleted ' + MESSAGES);
  flags(guardMjs, { [GUARD]: null }, 'gate-file-deleted ' + GUARD, { commit: true });
});

test('a committed weakening is flagged against its base, a committed clean change is not', () => {
  flags(checkSh, edit(checkSh, CHECK, ' -- -D warnings', ''), 'gate-line-removed ' + CHECK, { commit: true });
  flags(rootToml, edit(rootToml, ROOT, 'todo = "deny"', 'todo = "allow"'), 'lint-weakened ' + ROOT, { commit: true });
  clean(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=95 '), { commit: true });
});

// R1: nothing in git's configuration or attributes hides a change.

test('a renamed gate file is flagged as deleted', () => {
  const move = (files, from, to) => ({ [from]: null, [to]: files[from] });
  flags(checkSh, move(checkSh, CHECK, 'scripts/check2.sh'), 'gate-file-deleted ' + CHECK, { commit: true });
  flags(denyToml, move(denyToml, DENY, 'deny2.toml'), 'gate-file-deleted ' + DENY, { commit: true });
  flags(toolchainToml, move(toolchainToml, TOOLCHAIN, 'rust-toolchain.toml.off'), 'gate-file-deleted ' + TOOLCHAIN,
    { commit: true });
  flags(testScripts, move(testScripts, COMPARE, COMPARE + '.off'), 'gate-file-deleted ' + COMPARE, { commit: true });
  flags(checkSh, move(checkSh, CHECK, 'scripts/check2.sh'), 'gate-file-deleted ' + CHECK,
    { commit: true, config: { 'diff.renames': 'copies' } });
});

test('a renamed crate directory: its test scripts are flagged, a manifest that keeps its lints is clean', () => {
  flags(testScripts, { [COMPARE]: null, 'crates/journal2/tests/compare.sh': testScripts[COMPARE] },
    'gate-file-deleted ' + COMPARE, { commit: true });
  clean(crateToml, { [CRATE]: null, 'crates/exec2/Cargo.toml': crateToml[CRATE] }, { commit: true });
});

const lowered = edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=10 ');
for (const [key, value] of [['diff.mnemonicPrefix', 'true'], ['diff.noprefix', 'true'], ['color.diff', 'always'],
  ['color.ui', 'always'], ['diff.external', 'true']]) {
  test('a lowered threshold is flagged with git configured: ' + key + '=' + value, () => {
    flags(checkSh, lowered, 'threshold-loosened ' + CHECK, { config: { [key]: value } });
    flags(checkSh, lowered, 'threshold-loosened ' + CHECK, { commit: true, config: { [key]: value } });
  });
}

test('an added .gitattributes is flagged, and hides nothing', () => {
  flags(checkSh, { '.gitattributes': 'scripts/*.sh -diff\n', ...lowered },
    ['tool-config-added .gitattributes', 'threshold-loosened ' + CHECK]);
  flags(checkSh, { '.gitattributes': '* binary\n', ...edit(checkSh, CHECK, ' -- -D warnings', '') },
    ['tool-config-added .gitattributes', 'gate-line-removed ' + CHECK], { commit: true });
});

// R2: a threshold weakened by an added assignment.

test('check.sh: a threshold assigned again by an added line is flagged, a new constant is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, 'step() {', 'COVERAGE_MIN=0\nstep() {'), 'threshold-reassigned ' + CHECK);
  flags(checkSh, edit(checkSh, CHECK, 'step() {', 'COVERAGE_MIN=$((COVERAGE_MIN - 80))\nstep() {'),
    'threshold-reassigned ' + CHECK);
  flags(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=95\nCOVERAGE_MIN=1 '), 'threshold-reassigned ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, 'step() {', 'STARTUP_MAX_MS=10\nstep() {'));
});

test('check.sh: a threshold line with more than a comment after the value is flagged', () => {
  flags(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=95 COVERAGE_MIN=1 '), 'threshold-changed ' + CHECK);
  flags(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=95 && COVERAGE_MIN=1 '), 'threshold-changed ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90       # % of changed', 'COVERAGE_MIN=95 # percent of changed'));
});

test('tests/*.sh: a bound assigned again by an added line is flagged', () => {
  flags(testScripts, edit(testScripts, COMPARE, 'max_tail=', 'PREADS_TAIL_MAX=999999\nmax_tail='),
    'threshold-reassigned ' + COMPARE);
});

for (const prefix of ['readonly ', 'declare -r ', 'export ']) {
  test('check.sh: a threshold declared with `' + prefix.trim() + '` is read too', () => {
    const declared = edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', prefix + 'COVERAGE_MIN=90 ');
    flags(declared, edit(declared, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=80 '), 'threshold-loosened ' + CHECK);
    clean(declared, edit(declared, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=95 '));
  });
}

// R3: a step disabled by an added line.

const disabled = 'check-disabled ';
const addLine = (files, path, anchor, line) => edit(files, path, anchor, line + '\n' + anchor);
for (const [name, bad, good] of [
  ['exit 0', 'exit 0', 'exit 2'],
  ['return 0', '  return 0', '  return "$rc"'],
  ['a bare return', '  [[ -n $base ]] || return', '  [[ -n $base ]] || return 1'],
  ['if false', '  if false; then echo; fi', '  if [[ -n $base ]]; then echo; fi'],
  ['set +e', 'set +e', 'set -o errtrace'],
  ['|| true', '  cargo fmt --check || true', '  cargo fmt --check'],
  ['|| :', '  cargo fmt --check || :', '  cargo fmt --check || exit 1'],
  ['a here-document that swallows lines', "  : <<'OFF'", '  grep -q main <<<"$base"'],
  ['--cap-lints', '  export RUSTFLAGS=--cap-lints=allow', '  export RUSTFLAGS=-Dwarnings'],
]) {
  test('check.sh: an added line with ' + name + ' is flagged, one without is clean', () => {
    flags(checkSh, addLine(checkSh, CHECK, '  step "floor guard"', bad), disabled + CHECK);
    clean(checkSh, addLine(checkSh, CHECK, '  step "floor guard"', good));
  });
}

test('floor-guard.mjs: an added process.exit(0) is flagged, another exit status is clean', () => {
  flags(guardMjs, addLine(guardMjs, GUARD, 'for (const', "console.log('floor-guard: clean'); process.exit(0);"), disabled + GUARD);
  clean(guardMjs, addLine(guardMjs, GUARD, 'for (const', 'if (!added) process.exit(2);'));
});

test('floor-guard.mjs: an opened block comment is flagged, a glob in a string is clean', () => {
  flags(guardMjs, { [GUARD]: guardMjs[GUARD].replace('for (const', '/*\nfor (const') + '*/\n' }, disabled + GUARD);
  clean(guardMjs, addLine(guardMjs, GUARD, 'for (const', "const SCRIPTS = 'crates/*/tests';"));
});

test('tests/*.sh and check-commit-messages.sh: an added exit 0 is flagged', () => {
  flags(testScripts, addLine(testScripts, COMPARE, 'bad() {', 'exit 0'), disabled + COMPARE);
  flags(messagesSh, addLine(messagesSh, MESSAGES, 'exit $status', 'status=0; exit 0'), disabled + MESSAGES);
});

test('check.sh: a moved or reindented disabling line is flagged again', () => {
  const cleanup = { [CHECK]: checkSh[CHECK] + 'rm -f "$tmp" || true\necho done\n' };
  flags(cleanup, { [CHECK]: checkSh[CHECK] + 'echo done\n  rm -f "$tmp" || true\n' }, disabled + CHECK);
});

test('check.sh: a return moved to the top of a stage is flagged, each copy of it once', () => {
  const skipping = edit(checkSh, CHECK, 'declare -A', 'task() {\n  if [[ -z $base ]]; then\n    return\n  fi\n  cargo test\n}\ndeclare -A');
  const moved = (files) => ({ [CHECK]: files[CHECK].replace('    return\n', '').replace('task() {\n', 'task() {\n  return\n') });
  flags(skipping, moved(skipping), disabled + CHECK);
  flags(skipping, { [CHECK]: moved(skipping)[CHECK].replace('fast() {\n', 'fast() {\n  return\n') }, [disabled + CHECK, disabled + CHECK]);
});

test('check.sh: a bare exit is flagged, an exit with a failing status is clean', () => {
  flags(checkSh, addLine(checkSh, CHECK, '  step "floor guard"', '  true; exit'), disabled + CHECK);
  flags(checkSh, addLine(checkSh, CHECK, '  step "floor guard"', '  exit # done'), disabled + CHECK);
  clean(checkSh, addLine(checkSh, CHECK, '  step "floor guard"', '  [[ -n $base ]] || exit "$rc"'));
  clean(testScripts, addLine(testScripts, COMPARE, 'bad() {', 'trap cleanup EXIT; [ -n "$1" ] || exit'));
});

test('check.sh: a threshold set through arithmetic, let, read or printf -v is flagged', () => {
  for (const line of ['(( COVERAGE_MIN = 0 ))', '((COVERAGE_MIN-=80))', 'let "COVERAGE_MIN = 0"', 'read -r COVERAGE_MIN <<<0',
    'printf -v COVERAGE_MIN 0']) {
    flags(checkSh, addLine(checkSh, CHECK, 'step() {', line), 'threshold-reassigned ' + CHECK);
  }
  clean(checkSh, addLine(checkSh, CHECK, 'step() {', '((size <= SIZE_MAX_BYTES))'));
  clean(checkSh, addLine(checkSh, CHECK, 'step() {', '(( COVERAGE_MIN == 90 )) || echo "changed"'));
  clean(checkSh, addLine(checkSh, CHECK, 'step() {', '(( STARTUP_MAX_MS = 10 ))'));
});

test('tests/*.sh: cleanup with || true and an early return are ordinary there, the other patterns are flagged', () => {
  for (const line of ['docker rm -f "$c" >/dev/null 2>&1 || true', 'rm -f "$tmp" || :', '  [ -n "$1" ] || return',
    '  return 0']) {
    clean(testScripts, addLine(testScripts, COMPARE, 'bad() {', line));
  }
  for (const line of ['if false; then', 'set +e', ": <<'OFF'", 'export RUSTFLAGS=--cap-lints=allow']) {
    flags(testScripts, addLine(testScripts, COMPARE, 'bad() {', line), disabled + COMPARE);
  }
  flags(messagesSh, addLine(messagesSh, MESSAGES, 'exit $status', 'git log -1 >/dev/null || true'), disabled + MESSAGES);
});

// R4 and the deny.toml settings.

test('Cargo.toml: a literal-string allow is flagged like a basic string', () => {
  flags(rootToml, edit(rootToml, ROOT, 'todo = "deny"\n', "todo = \"deny\"\nall = 'allow'\n"), 'lint-weakened ' + ROOT);
  flags(rootToml, edit(rootToml, ROOT, 'todo = "deny"', "todo = 'allow'"), 'lint-weakened ' + ROOT);
  clean(rootToml, edit(rootToml, ROOT, 'todo = "deny"', "todo = 'deny' # no stubs"));
});

test('deny.toml: a changed or removed setting is flagged', () => {
  for (const [from, to] of [['unmaintained = "all"', 'unmaintained = "none"'], ['yanked = "deny"', 'yanked = "allow"'],
    ['confidence-threshold = 0.9', 'confidence-threshold = 0.1'], ['wildcards = "deny"', 'wildcards = "allow"'],
    ['unknown-git = "deny"', 'unknown-git = "allow"'], ['yanked = "deny"\n', ''],
    ['    { crate = "lzma-rust2", deny = ["optimization"] },\n', '']]) {
    flags(denyToml, edit(denyToml, DENY, from, to), 'deny-setting-changed ' + DENY);
  }
});

test('deny.toml: a new setting, a new feature ban and a comment on a setting are clean', () => {
  clean(denyToml, edit(denyToml, DENY, 'wildcards = "deny"\n', 'wildcards = "deny"\nallow-wildcard-paths = true\n'));
  clean(denyToml, edit(denyToml, DENY, '    { crate = "lzma-rust2"', '    { crate = "ring", deny = ["std"] },\n    { crate = "lzma-rust2"'));
  clean(denyToml, edit(denyToml, DENY, 'yanked = "deny"', 'yanked = "deny" # always'));
});

test('deny.toml: a skipped ban, an ignored private licence and a licence exception are flagged', () => {
  flags(denyToml, edit(denyToml, DENY, 'wildcards = "deny"\n', 'wildcards = "deny"\nskip = [{ crate = "x" }]\n'),
    'ban-skipped ' + DENY);
  flags(denyToml, edit(denyToml, DENY, '[bans]\n', '[licenses.private]\nignore = true\n\n[bans]\n'), 'licence-ignored ' + DENY);
  flags(denyToml, edit(denyToml, DENY, '[bans]\n', '[[licenses.exceptions]]\nallow = ["MPL-2.0"]\ncrate = "x"\n\n[bans]\n'),
    'licence-added ' + DENY);
  flags(denyToml, edit(denyToml, DENY, 'confidence-threshold', 'exceptions = [\n    { allow = ["MPL-2.0"], crate = "x" },\n]\nconfidence-threshold'),
    'licence-added ' + DENY);
});

test('deny.toml: quoted keys and table names are read like bare ones', () => {
  flags(denyToml, edit(denyToml, DENY, 'ignore = [\n', '"ignore" = [\n    "RUSTSEC-2099-0001",\n'), 'advisory-ignored ' + DENY);
  flags(denyToml, { [DENY]: denyToml[DENY].replace('[advisories]', '["advisories"]').replace(ignoreLine, ignoreLine + "    'RUSTSEC-2099-0001',\n") },
    'advisory-ignored ' + DENY);
  clean(denyToml, edit(denyToml, DENY, '[advisories]', "['advisories']"));
});

test('deny.toml: a comment after a licence and re-sorted licences are clean', () => {
  clean(denyToml, edit(denyToml, DENY, '    "MIT",\n', '    "MIT", # permissive\n'));
  clean(denyToml, edit(denyToml, DENY, '    "Apache-2.0",\n    "MIT",\n', '    "MIT",\n    "Apache-2.0",\n'));
});

test('deny.toml: wrappers over several lines: the same members are clean, a new one is flagged', () => {
  const entry = '{ crate = "sb-docker", wrappers = ["sb"] },';
  clean(denyToml, edit(denyToml, DENY, entry, "{ crate = \"sb-docker\", wrappers = [\n        'sb',\n    ] },"));
  flags(denyToml, edit(denyToml, DENY, entry, '{ crate = "sb-docker", wrappers = [\n        "sb",\n        "sb-exec",\n    ] },'),
    'wrappers-widened ' + DENY);
  flags(denyToml, edit(denyToml, DENY, '    ' + entry, '    { crate = "sb-docker", wrappers = ["sb", "sb-exec"] },\n    ' + entry),
    'wrappers-widened ' + DENY);
});

// Config locations, the mutation scope, the guard's own files.

for (const config of ['.cargo/config', '.clippy.toml', '.gitleaks.toml', 'rust-toolchain', 'crates/exec/clippy.toml',
  'crates/exec/.clippy.toml', 'crates/exec/.gitattributes']) {
  test('a new tool config is flagged: ' + config, () => {
    flags({}, { [config]: 'stable\n' }, 'tool-config-added ' + config);
  });
}

test('a tool config name elsewhere is clean', () => {
  clean({}, { 'prototypes/tui/clippy.toml': 'msrv = "1.99"\n', 'docs/rust-toolchain': 'notes\n' });
});

test('check.sh: MUTATION_TESTERS as a compound assignment is flagged, a comment about one is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, 'for c in', 'MUTATION_TESTERS+=([sb-exec]="sb-docker")\nfor c in'),
    'mutation-scope-narrowed ' + CHECK);
  flags(checkSh, edit(checkSh, CHECK, 'declare -A MUTATION_TESTERS=()', 'declare -A MUTATION_TESTERS=( [sb-exec]="sb-docker" )'),
    ['mutation-scope-narrowed ' + CHECK, 'gate-line-removed ' + CHECK]);
  clean(checkSh, edit(checkSh, CHECK, 'for c in', '# e.g. MUTATION_TESTERS[sb-foo]="sb-bar"\nfor c in'));
});

test('check.sh: a tester added to an entry is not a narrower scope, a tester removed is', () => {
  const entry = 'MUTATION_TESTERS[sb-journal]="sb-host"';
  flags(checkSh, edit(checkSh, CHECK, entry, 'MUTATION_TESTERS[sb-journal]="sb-host sb"'), 'gate-line-removed ' + CHECK);
  const two = edit(checkSh, CHECK, entry, 'MUTATION_TESTERS[sb-journal]="sb-host sb"');
  flags(two, edit(two, CHECK, '"sb-host sb"', '"sb-host"'), ['mutation-scope-narrowed ' + CHECK, 'gate-line-removed ' + CHECK]);
});

test('one added line does not excuse several removed copies of it', () => {
  const twice = { [MESSAGES]: messagesSh[MESSAGES].replace('exit $status', 'if bad; then\n  status=1\nfi\nexit $status') };
  flags(twice, { [MESSAGES]: messagesSh[MESSAGES].replace("  echo \"Missing 'Review:' line: $sha\"\n  status=1\n", '    status=1\n') },
    ['gate-line-removed ' + MESSAGES, 'gate-line-removed ' + MESSAGES, 'gate-line-removed ' + MESSAGES,
      'gate-line-removed ' + MESSAGES]);
});

const TESTS = 'scripts/floor-guard.test.mjs';
const guardTests = {
  [TESTS]: `import { test } from 'node:test';
// Each weakening has a clean case beside it.
test('a lowered minimum is flagged', () => {
  flags(checkSh, lowered, 'threshold-loosened');
  clean(checkSh, raised);
});
`,
};

test("the guard's tests: a removed assertion or a deleted file is flagged, a new test is clean", () => {
  flags(guardTests, edit(guardTests, TESTS, "  flags(checkSh, lowered, 'threshold-loosened');\n", ''), 'gate-line-removed ' + TESTS);
  flags(guardTests, { [TESTS]: null }, 'gate-file-deleted ' + TESTS);
  clean(guardTests, { [TESTS]: guardTests[TESTS] + "test('exit 0 is flagged', () => {\n  flags(checkSh, 'exit 0 || true');\n});\n" });
});

// Cases a mutation probe of the guard found untested.

test('check.sh: a second assignment after an unchanged value is flagged', () => {
  flags(checkSh, edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=90 COVERAGE_MIN=1 '), 'threshold-changed ' + CHECK);
});

test('a comment that quotes a disabling line or an expected_failure entry is clean', () => {
  clean(checkSh, addLine(checkSh, CHECK, '  step "floor guard"', '  # exit 0 here would hide every later step || true'));
  clean(testScripts, addLine(testScripts, COMPARE, 'bad() {', '# expected_failure follow-H1 32 racy the follow count'));
});

test('deny.toml: a skipped dependency tree is flagged', () => {
  flags(denyToml, edit(denyToml, DENY, 'wildcards = "deny"\n', 'wildcards = "deny"\nskip-tree = [{ crate = "x" }]\n'),
    'ban-skipped ' + DENY);
});

test('a deleted gate file is flagged with mnemonic prefixes configured', () => {
  flags(checkSh, { [CHECK]: null }, 'gate-file-deleted ' + CHECK, { commit: true, config: { 'diff.mnemonicPrefix': 'true' } });
});

test('a textconv filter named by an added .gitattributes hides nothing', () => {
  flags(checkSh, { '.gitattributes': 'scripts/*.sh diff=hide\n', ...lowered },
    ['tool-config-added .gitattributes', 'threshold-loosened ' + CHECK], { config: { 'diff.hide.textconv': 'true' } });
});

// Acknowledged findings (ADR-015): scripts/floor-acknowledged.txt.

const ACK = 'scripts/floor-acknowledged.txt';
const entry = (rule, file, text, reason = 'the reason') => [rule, file, text, reason].join('\t') + '\n';
const clippyLine = 'cargo clippy --workspace --all-targets --quiet -- -D warnings';
const dropped = edit(checkSh, CHECK, ' -- -D warnings', '');
const droppedEntry = entry('gate-line-removed', CHECK, clippyLine, 'clippy moved to its own script');

test('an acknowledged finding passes, and is printed with its reason', () => {
  const r = run(checkSh, { ...dropped, [ACK]: '# rule, file, line, reason\n' + droppedEntry });
  assert.equal(r.status, 0, r.output);
  assert.match(r.output, /acknowledged/);
  assert.ok(r.output.includes('[gate-line-removed] ' + CHECK + ': ' + clippyLine), r.output);
  assert.ok(r.output.includes('clippy moved to its own script'), r.output);
});

test('an acknowledgement is read whatever the spacing inside its line', () => {
  clean(checkSh, { ...edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=80 '),
    [ACK]: entry('threshold-loosened', CHECK,
      'COVERAGE_MIN=90 # % of changed lines covered -> COVERAGE_MIN=80 # % of changed lines covered') });
});

test('an entry covers one finding: a second identical finding needs a second entry', () => {
  const twice = { [MESSAGES]: messagesSh[MESSAGES].replace('exit $status', 'if bad; then\n  status=1\nfi\nexit $status') };
  const both = { [MESSAGES]: twice[MESSAGES].replaceAll('  status=1\n', '  :\n') };
  const one = entry('gate-line-removed', MESSAGES, 'status=1');
  flags(twice, { ...both, [ACK]: one }, 'gate-line-removed ' + MESSAGES);
  clean(twice, { ...both, [ACK]: one + one });
});

test('gate-line-removed: two removed lines with one entry fail, with an entry each they pass', () => {
  const two = edit(checkSh, CHECK, '  step "floor guard"\n  node scripts/floor-guard.mjs --base "$base"\n', '');
  const step = entry('gate-line-removed', CHECK, 'step "floor guard"');
  flags(checkSh, { ...two, [ACK]: step }, 'gate-line-removed ' + CHECK);
  clean(checkSh, { ...two, [ACK]: step + entry('gate-line-removed', CHECK, 'node scripts/floor-guard.mjs --base "$base"') });
});

test('an entry for another rule, file or line acknowledges nothing, and is itself reported', () => {
  for (const other of [entry('threshold-loosened', CHECK, clippyLine), entry('gate-line-removed', MESSAGES, clippyLine),
    entry('gate-line-removed', CHECK, 'cargo clippy')]) {
    flags(checkSh, { ...dropped, [ACK]: other }, ['gate-line-removed ' + CHECK, 'acknowledgement-unmatched ' + ACK]);
  }
});

test('an added entry that matches no finding fails', () => {
  flags(checkSh, { [ACK]: droppedEntry }, 'acknowledgement-unmatched ' + ACK);
  flags(checkSh, { ...dropped, [ACK]: droppedEntry + droppedEntry }, 'acknowledgement-unmatched ' + ACK);
});

test('an entry without a reason fails', () => {
  flags(checkSh, { ...dropped, [ACK]: ['gate-line-removed', CHECK, clippyLine].join('\t') + '\n' },
    ['gate-line-removed ' + CHECK, 'acknowledgement-malformed ' + ACK]);
  flags(checkSh, { ...dropped, [ACK]: entry('gate-line-removed', CHECK, clippyLine, ' ') },
    ['gate-line-removed ' + CHECK, 'acknowledgement-malformed ' + ACK]);
});

test('an entry from an earlier commit acknowledges nothing', () => {
  flags({ ...checkSh, [ACK]: droppedEntry }, dropped, 'gate-line-removed ' + CHECK);
  flags({ ...checkSh, [ACK]: droppedEntry }, dropped, 'gate-line-removed ' + CHECK, { commit: true });
});

test('a later weakening in the same file still fails after an acknowledged one', () => {
  const first = { ...dropped, [ACK]: droppedEntry };
  const second = { [CHECK]: dropped[CHECK].replace('  step "floor guard"\n', '') };
  flags(checkSh, [first, second], 'gate-line-removed ' + CHECK);
  flags(checkSh, [first, second], 'gate-line-removed ' + CHECK, { commit: true });
});

test('a range of commits passes when each finding and its entry are both in it', () => {
  clean(checkSh, [{ ...dropped, [ACK]: droppedEntry }, { 'README.md': 'more\n' }], { commit: true });
  clean(checkSh, [dropped, { 'README.md': 'more\n' }, { [ACK]: droppedEntry }], { commit: true });
  const second = { [CHECK]: dropped[CHECK].replace('  step "floor guard"\n', ''),
    [ACK]: droppedEntry + entry('gate-line-removed', CHECK, 'step "floor guard"') };
  clean(checkSh, [{ ...dropped, [ACK]: droppedEntry }, second], { commit: true });
});

test('the acknowledgement file: a removed or edited entry is flagged, a comment and a moved entry are clean', () => {
  const other = entry('lint-removed', ROOT, 'todo = "deny"');
  const kept = { ...checkSh, [ACK]: '# The record of acknowledged findings.\n' + droppedEntry + other };
  flags(kept, { [ACK]: '# The record of acknowledged findings.\n' + other }, 'acknowledgement-removed ' + ACK);
  flags(kept, { [ACK]: null }, 'acknowledgement-removed ' + ACK);
  flags(kept, { [ACK]: kept[ACK].replace('clippy moved to its own script', 'another reason') },
    ['acknowledgement-removed ' + ACK, 'acknowledgement-unmatched ' + ACK]);
  clean(kept, { [ACK]: '# The record of acknowledged findings.\n# One line each.\n' + other + droppedEntry });
});

// The second review: what an entry can't cover.

test('a tool config: each added line needs its own entry', () => {
  const config = { 'clippy.toml': 'msrv = "1.99"\n' };
  const two = { 'clippy.toml': 'too-many-lines-threshold = 500\ntoo-many-arguments-threshold = 20\nmsrv = "1.99"\n' };
  flags(config, { ...two, [ACK]: entry('tool-config-changed', 'clippy.toml', 'too-many-lines-threshold = 500') },
    'tool-config-changed clippy.toml');
  flags(config, { ...two, [ACK]: entry('tool-config-changed', 'clippy.toml', 'lines added') },
    ['tool-config-changed clippy.toml', 'tool-config-changed clippy.toml', 'acknowledgement-unmatched ' + ACK]);
  clean(config, { ...two, [ACK]: entry('tool-config-changed', 'clippy.toml', 'too-many-lines-threshold = 500')
    + entry('tool-config-changed', 'clippy.toml', 'too-many-arguments-threshold = 20') });
});

test('a removed, an unmatched and a malformed entry cannot be acknowledged', () => {
  const kept = { ...checkSh, [ACK]: droppedEntry };
  flags(kept, { [ACK]: entry('acknowledgement-removed', ACK, droppedEntry.trim().slice(0, 120).replace(/\t/g, ' ')) },
    ['acknowledgement-removed ' + ACK, 'acknowledgement-unmatched ' + ACK]);
  const stray = entry('lint-removed', ROOT, 'todo = "deny"');
  flags(checkSh, { [ACK]: stray + entry('acknowledgement-unmatched', ACK, stray.trim().slice(0, 120).replace(/\t/g, ' ')) },
    ['acknowledgement-unmatched ' + ACK, 'acknowledgement-unmatched ' + ACK]);
  const short = 'lint-removed\t' + ROOT + '\n';
  flags(checkSh, { [ACK]: short + entry('acknowledgement-malformed', ACK, short.trim().slice(0, 120).replace(/\t/g, ' ')) },
    ['acknowledgement-malformed ' + ACK, 'acknowledgement-unmatched ' + ACK]);
});
