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
// For the stubbed runs of check.sh (hardening Task 4).
import { readFileSync, symlinkSync } from 'node:fs';
// For the mutation pass's directory (ADR-021).
import { existsSync } from 'node:fs';

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
SIZE_MAX_BYTES=45711520 # production glibc binary
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
targets = ["x86_64-unknown-linux-gnu"]
`,
};

test('rust-toolchain.toml: a changed channel is flagged, an added target is clean', () => {
  flags(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"1.99.1"', '"stable"'), 'toolchain-changed ' + TOOLCHAIN);
  clean(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"x86_64-unknown-linux-gnu"',
    '"x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"'));
});

test('rust-toolchain.toml: a removed component or file is flagged, an added component is clean', () => {
  flags(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"clippy", ', ''), 'toolchain-changed ' + TOOLCHAIN);
  flags(toolchainToml, { [TOOLCHAIN]: null }, 'gate-file-deleted ' + TOOLCHAIN);
  clean(toolchainToml, edit(toolchainToml, TOOLCHAIN, '"llvm-tools"', '"llvm-tools", "rust-src"'));
});

for (const config of ['.cargo/mutants.toml', 'clippy.toml', '.gitleaksignore', '.cargo/config.toml', '.config/nextest.toml']) {
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

test('nextest.toml: a changed timeout is flagged, a comment is clean', () => {
  const NEXTEST = '.config/nextest.toml';
  const config = { [NEXTEST]: '[profile.mutants]\nslow-timeout = { period = "30s", terminate-after = 2 }\n' };
  flags(config, edit(config, NEXTEST, 'terminate-after = 2', 'terminate-after = 1'), 'tool-config-changed ' + NEXTEST);
  flags(config, edit(config, NEXTEST, ' }', ', on-timeout = "pass" }'), 'tool-config-changed ' + NEXTEST);
  clean(config, edit(config, NEXTEST, '[profile', '# Why.\n[profile'));
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

// docs-M2: false negatives in Rust files and CONSTRAINTS.md.

const LIB = 'crates/exec/src/lib.rs';
const libRs = {
  [LIB]: `pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        assert_eq!(add(1, 2), 3);
    }
}
`,
};
const silenced = 'silenced-checker ' + LIB;
const easier = 'test-made-easier ' + LIB;

test('a Rust file: cfg_attr(.., mutants::skip) and cfg_attr(.., coverage(off)) are flagged, another cfg_attr is clean', () => {
  for (const attr of ['#[cfg_attr(test, mutants::skip)]', '#[cfg_attr(coverage_nightly, coverage(off))]',
    '#[cfg_attr(\n    coverage_nightly,\n    coverage(off)\n)]', '#[cfg_attr(\n    test,\n    mutants::skip\n)]']) {
    flags(libRs, addLine(libRs, LIB, 'pub fn add', attr), silenced);
  }
  clean(libRs, addLine(libRs, LIB, 'pub fn add', '#[cfg_attr(test, derive(Debug))]\n#[cfg_attr(coverage_nightly, inline)]'));
  clean(libRs, edit(libRs, LIB, '    a + b\n', '    coverage(offset(a)) + b\n'));
});

test('a Rust file: a cfg_attr expect split over lines is flagged, a method named expect is clean', () => {
  flags(libRs, addLine(libRs, LIB, 'pub fn add', '#[cfg_attr(\n    test, expect(dead_code, reason = "x")\n)]'), silenced);
  flags(libRs, addLine(libRs, LIB, 'pub fn add', '#[cfg_attr(all(test,\n    unix), allow(dead_code))]'), silenced);
  clean(libRs, edit(libRs, LIB, '    a + b\n', '    Some(a + b)\n        .filter(|n| *n > 0)\n        .expect("positive")\n'));
  clean(libRs, edit(libRs, LIB, '    a + b\n', '    let n = Some(a).expect("set"); n.max(b.min(a))\n'));
});

test('a Rust file: #[cfg(any())] on a test and a nested cfg_attr ignore are flagged, a real cfg is clean', () => {
  flags(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg(any())]'), easier);
  flags(libRs, addLine(libRs, LIB, '#[cfg(test)]', '#![cfg(all(unix, any()))]'), easier);
  flags(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(not(miri), cfg_attr(test, ignore))]'), easier);
  flags(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(all(unix, test), ignore = "slow")]'), easier);
  flags(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(unix, /* x */ ignore)]'), easier);
  clean(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(test, doc = "flaky tests get ignored")]\n    #[cfg_attr(test, ignore_x)]'));
  flags(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(\n        not(miri),\n        ignore = "a long reason"\n    )]'), easier);
  flags(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(\n        not(miri), ignore = "slow"\n    )]'), easier);
  flags(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(\n        not(miri),\n        ignore\n    )]'), easier);
  clean(libRs, edit(libRs, LIB, '    a + b\n', '    let ignore = [a];\n    stop(ignore);\n    run(\n        ignore,\n        b,\n    )\n'));
  clean(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg(any(unix, windows))]'));
  clean(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg_attr(not(miri), cfg_attr(test, inline))]'));
});

const CLI = 'crates/sb/tests/version.rs';
const cliRs = {
  [CLI]: `use assert_cmd::Command;

#[test]
fn version_runs() {
    Command::cargo_bin("sb")
        .unwrap()
        .arg("version")
        .assert()
        .success()
        .code(0)
        .stdout(predicates::str::contains("sb"))
        .stderr("");
}

#[test]
fn bad_flag_fails() {
    Command::cargo_bin("sb").unwrap().arg("--bad").assert().failure();
    Command::cargo_bin("sb")
        .unwrap()
        .arg("--worse")
        .assert()
        .failure();
}
`,
};
const RUN = 'crates/exec/src/run.rs';
const runRs = {
  [RUN]: `use std::process::{Command, Stdio};

pub fn run(r: &Runner) -> std::io::Result<bool> {
    r.step("start")
        .failure("Docker container start");
    let status = Command::new("true")
        .stdout(Stdio::piped())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(status.success())
}
`,
};

test('a removed assert_cmd chain step is flagged, a removed builder step is clean', () => {
  for (const step of ['        .assert()\n', '        .success()\n', '        .code(0)\n',
    '        .stdout(predicates::str::contains("sb"))\n', '        .stderr("");\n']) {
    flags(cliRs, edit(cliRs, CLI, step, step.endsWith(';\n') ? ';\n' : ''), 'assertion-removed ' + CLI);
  }
  flags(cliRs, edit(cliRs, CLI, '        .assert()\n        .failure();\n', '        ;\n'),
    ['assertion-removed ' + CLI, 'assertion-removed ' + CLI]);
  flags(cliRs, edit(cliRs, CLI, '.arg("--bad").assert().failure();', '.arg("--bad").ok();'), 'assertion-removed ' + CLI);
  clean(cliRs, edit(cliRs, CLI, '        .arg("version")\n', ''));
  clean(runRs, edit(runRs, RUN, '        .stdout(Stdio::piped())\n        .stderr(std::process::Stdio::null())\n', ''));
  clean(runRs, edit(runRs, RUN, '\n        .failure("Docker container start");', ';'));
  const exitCode = { [RUN]: runRs[RUN].replace('    Ok(status.success())\n', '    Ok(status\n        .code()\n        .is_some())\n') };
  clean(exitCode, edit(exitCode, RUN, '        .code()\n        .is_some())\n', '        .success())\n'));
});

test('a deleted src file that held tests is flagged, one without tests is clean', () => {
  flags(libRs, { [LIB]: null }, 'test-deleted ' + LIB);
  flags(libRs, { [LIB]: null }, 'test-deleted ' + LIB, { commit: true });
  const tokio = { [RUN]: runRs[RUN] + '\n#[tokio::test]\nasync fn runs() {}\n' };
  flags(tokio, { [RUN]: null }, 'test-deleted ' + RUN);
  flags(cliRs, { [CLI]: null }, 'test-deleted ' + CLI);
  flags({ 'crates/sb/tests/common/mod.rs': 'pub fn sb() {}\n' }, { 'crates/sb/tests/common/mod.rs': null },
    'test-deleted crates/sb/tests/common/mod.rs');
  clean({ 'docs/testing.md': 'Unit tests carry #[test].\n' }, { 'docs/testing.md': null });
  clean(runRs, { [RUN]: null });
  flags({ ...runRs, ...cliRs }, { [RUN]: null, [CLI]: null }, 'test-deleted ' + CLI);
  const outOfLine = { [RUN]: runRs[RUN] + '\n#[cfg(test)]\nmod tests;\n' };
  clean(outOfLine, { [RUN]: null });
});

const CONS = 'CONSTRAINTS.md';
const constraintsMd = {
  [CONS]: `# Constraints

## Floor (always enforced)

- Formatting is rustfmt's; clippy and rustc warnings are errors.
- No test made easier: no new \`#[ignore]\`, no deleted test, no assertion
  removed from code that stays, without a reason in the commit message.

## Enforced with numbers

| Dimension | Rule | Checked by |
|---|---|---|
| Coverage | Changed lines ≥ 90% covered | diff-cover 9 |
| Binary size | Production binary ≤ 45 MB | stat |

## Notes

- Last reviewed by the maintainer, after
  an interview.
`,
};
const continuation = '  removed from code that stays, without a reason in the commit message.';

test('CONSTRAINTS.md: a changed continuation line of a Floor bullet is flagged, a rewrapped bullet is clean', () => {
  flags(constraintsMd, edit(constraintsMd, CONS, continuation, '  removed from code that stays, unless it was slow.'),
    'rule-changed ' + CONS);
  flags(constraintsMd, edit(constraintsMd, CONS, continuation, continuation + '\n  Tests of the prototypes are exempt.'),
    'rule-changed ' + CONS);
  flags(constraintsMd, edit(constraintsMd, CONS, continuation, '  removed from code that stays, unless it was slow.'),
    'rule-changed ' + CONS, { commit: true });
  flags(constraintsMd, edit(constraintsMd, CONS, 'warnings are errors.', 'warnings are errors.\n  Prototypes are exempt.'),
    'rule-changed ' + CONS);
  clean(constraintsMd, edit(constraintsMd, CONS, 'no deleted test, no assertion\n' + continuation,
    'no deleted test,\n  no assertion removed from code that stays, without a reason in the\n  commit message.'));
  clean(constraintsMd, edit(constraintsMd, CONS, continuation + '\n', continuation + '\n- No secrets in source, tests\n  or logs.\n'));
  clean(constraintsMd, edit(constraintsMd, CONS, '  an interview.', '  a second interview.'));
  clean(constraintsMd, edit(constraintsMd, CONS, '- No test made easier:', '- No test made easier: no new skip.\n- No test made easier:'));
});

test('CONSTRAINTS.md: a changed Floor bullet is reported with both sides of the change', () => {
  const r = run(constraintsMd, edit(constraintsMd, CONS, 'no deleted test,', 'no deleted unit test,'));
  assert.equal(r.status, 1, r.output);
  assert.ok(r.output.includes('[rule-changed] CONSTRAINTS.md: No test made easier: ...test, no assertion removed from  ->  ...unit test, no assertion removed\n'),
    r.output);
});

test('CONSTRAINTS.md: a looser alternative threshold appended to a row is flagged, a stricter one is clean', () => {
  flags(constraintsMd, edit(constraintsMd, CONS, '≥ 90% covered', '≥ 90% covered, or ≥ 50% for tests'),
    'threshold-loosened ' + CONS);
  flags(constraintsMd, edit(constraintsMd, CONS, '≤ 45 MB', '≤ 45 MB, or ≤ 60 MB with debug info'),
    'threshold-loosened ' + CONS);
  clean(constraintsMd, edit(constraintsMd, CONS, '≥ 90% covered', '≥ 90% covered, and ≥ 95% for new crates'));
  clean(constraintsMd, edit(constraintsMd, CONS, '≤ 45 MB', '≤ 45 MB, or ≤ 40 MB stripped'));
  clean(constraintsMd, edit(constraintsMd, CONS, '≥ 90% covered', '≥ 90% covered, in at most 5 minutes'));
  clean(constraintsMd, edit(constraintsMd, CONS, '≥ 90% covered', '≥ 90% covered, or ≥ 90% for tests'));
  const two = edit(constraintsMd, CONS, '≥ 90% covered', '≥ 90% of lines and ≥ 80% of functions covered');
  clean(two, edit(two, CONS, '80% of functions covered', '80% of functions covered, or ≥ 85% for tests'));
  flags(two, edit(two, CONS, '80% of functions covered', '80% of functions covered, or ≥ 70% for tests'),
    'threshold-loosened ' + CONS);
  clean(constraintsMd, edit(constraintsMd, CONS, 'diff-cover 9 |', 'diff-cover 9, since 2026 |'));
});

test('a Rust file: #[cfg(false)] and #[cfg(not(all()))] are flagged, #[cfg(not(any()))] is clean', () => {
  for (const attr of ['    #[cfg(false)]', '    #[cfg(all(unix, false))]', '    #[cfg(not(all()))]', '    #[cfg(not( all( ) ))]']) {
    flags(libRs, addLine(libRs, LIB, '    #[test]', attr), easier);
  }
  clean(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg(not(any()))]'));
  clean(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg(not( any( ) ))]'));
  clean(libRs, addLine(libRs, LIB, '    #[test]', '    #[cfg(feature = "false")]'));
});

test('a removed assert_cmd .code( with its argument on the next line is flagged', () => {
  const split = { [CLI]: cliRs[CLI].replace('        .code(0)\n', '        .code(\n            predicate::in_iter([0, 1]),\n        )\n') };
  flags(split, edit(split, CLI, '        .code(\n            predicate::in_iter([0, 1]),\n        )\n', ''), 'assertion-removed ' + CLI);
});

test('a test removed from a file that stays is flagged, a renamed or re-attributed one is clean', () => {
  flags(libRs, edit(libRs, LIB, '    #[test]\n', ''), 'test-removed ' + LIB);
  flags(libRs, edit(libRs, LIB, '    #[test]\n    fn adds() {\n        assert_eq!(add(1, 2), 3);\n    }\n', ''),
    ['test-removed ' + LIB, 'assertion-removed ' + LIB]);
  flags(libRs, edit(libRs, LIB, '    #[test]', '    #[cfg_attr(any(), test)]'), 'test-removed ' + LIB);
  const outOfLine = { [RUN]: runRs[RUN] + '\n#[cfg(test)]\nmod tests;\n' };
  flags(outOfLine, edit(outOfLine, RUN, '\n#[cfg(test)]\nmod tests;\n', ''), 'test-removed ' + RUN);
  clean(libRs, edit(libRs, LIB, '    fn adds() {', '    fn adds_two() {'));
  clean(libRs, edit(libRs, LIB, '    #[test]', '    #[tokio::test]'));
  const doc = { 'docs/testing.md': 'Unit tests carry #[test].\nThey run in CI.\n' };
  clean(doc, edit(doc, 'docs/testing.md', 'Unit tests carry #[test].\n', ''));
  const proto = { 'prototypes/tui/src/lib.rs': libRs[LIB] };
  clean(proto, edit(proto, 'prototypes/tui/src/lib.rs', '    #[test]\n', ''));
  clean(libRs, edit(libRs, LIB, '    #[test]\n    fn adds() {\n', '    #[test]\n    fn adds_more() {\n        let _ = add(0, 0);\n    }\n\n    #[test]\n    fn adds() {\n'));
});

const floorMd = {
  [CONS]: `# Constraints

## Floor (always enforced)

- Formatting is rustfmt's; clippy and rustc warnings are errors.
- Strict: tests always run.
- No test made easier: no new \`#[ignore]\`, no deleted test, no assertion
  removed from code that stays, without a reason in the commit message.

## Enforced with numbers

| Dimension | Rule | Checked by |
|---|---|---|
| Tests | All pass | cargo test |
`,
};

test('CONSTRAINTS.md: a reworded one-line Floor bullet with a key is flagged', () => {
  flags(floorMd, edit(floorMd, CONS, '- Strict: tests always run.', '- Strict: tests usually run.'), 'rule-changed ' + CONS);
  clean(floorMd, edit(floorMd, CONS, '- Strict: tests always run.', '- Strict:  tests always run.'));
});

test('CONSTRAINTS.md: a bullet that leaves the Floor section is flagged, a new section after it is clean', () => {
  flags(floorMd, edit(floorMd, CONS, '## Floor (always enforced)', '## The floor'),
    ['rule-removed ' + CONS, 'rule-removed ' + CONS, 'rule-removed ' + CONS]);
  flags(floorMd, edit(floorMd, CONS, '- No test made easier', '## Guidance\n\n- No test made easier'), 'rule-removed ' + CONS);
  flags(floorMd, { [CONS]: floorMd[CONS].replace('- Strict: tests always run.\n', '').replace('| Tests |', '- Strict: tests always run.\n\n| Tests |') },
    'rule-removed ' + CONS);
  flags(floorMd, edit(floorMd, CONS, '- Strict: tests always run.', '- Rigour: tests always run.'), 'rule-removed ' + CONS);
  clean(floorMd, edit(floorMd, CONS, '## Enforced with numbers', '## Notes\n\nSee the ADRs.\n\n## Enforced with numbers'));
});

test('CONSTRAINTS.md: the lines after a Floor bullet are part of it until a bullet, heading or table', () => {
  const cont = '  removed from code that stays, without a reason in the commit message.';
  flags(floorMd, edit(floorMd, CONS, cont, cont + '\nPrototype tests are exempt.'), 'rule-changed ' + CONS);
  flags(floorMd, edit(floorMd, CONS, cont, cont + '\n\n  Slow tests are exempt.'), 'rule-changed ' + CONS);
  flags(floorMd, edit(floorMd, CONS, cont, cont + '\n\nSlow tests are exempt.'), 'rule-changed ' + CONS);
  const tight = { [CONS]: floorMd[CONS].replace(cont + '\n\n## Enforced', cont + '\n## Enforced') };
  clean(tight, edit(tight, CONS, '## Enforced with numbers', '## Enforced, with numbers'));
  const lead = { [CONS]: tight[CONS].replace('## Enforced with numbers\n', '## Enforced with numbers\nThe numbers below gate a merge.\n') };
  clean(lead, edit(lead, CONS, 'gate a merge.', 'gate every merge.'));
  const table = { [CONS]: floorMd[CONS].replace(cont + '\n', cont + '\n| Floor | Each bullet | review |\n') };
  clean(table, edit(table, CONS, '| Floor | Each bullet | review |', '| Floor | Each bullet | the review |'));
  const after = { [CONS]: table[CONS].replace('| review |\n', '| review |\nThe table above is read at review.\n') };
  clean(after, edit(after, CONS, 'read at review.', 'read at each review.'));
});

test('a test commented out line by line is flagged, a doc comment that mentions #[test] is clean', () => {
  const plain = { [LIB]: libRs[LIB].replace('        assert_eq!(add(1, 2), 3);\n', '        let _ = add(1, 2);\n') };
  const fn = '    #[test]\n    fn adds() {\n        let _ = add(1, 2);\n    }\n';
  flags(plain, edit(plain, LIB, fn, fn.split('\n').map((l) => (l ? '    // ' + l.trim() : l)).join('\n')), 'test-removed ' + LIB);
  clean(plain, edit(plain, LIB, '#[cfg(test)]\nmod tests {', '/// Each `#[test]` below runs in CI.\n#[cfg(test)]\nmod tests {'));
});

// Stubbed runs of check.sh and check-commit-messages.sh (hardening Task 4).
// A scratch repository holds a copy of both scripts, a CONSTRAINTS.md with
// a Tools table, two crates and a deny.toml. Every tool check.sh runs is a
// stub first on PATH, git and the shell's own tools aside; each stub
// appends `name|CARGO_TARGET_DIR|arguments` to a log, gitleaks keeps its
// input, and cross leaves a small binary where check.sh measures one.
const SCRIPTS = dirname(fileURLToPath(import.meta.url));
const TOOLS_MD = `# Constraints

## Tools

| Tool | Version | Rule | Install |
|---|---|---|---|
| cargo-llvm-cov | 0.9.1 | exact | cargo |
| diff-cover | 9.7.2 | exact | uv |
| cargo-mutants | 27.1.0 | exact | cargo |
| cargo-nextest | 0.9.151 | exact | cargo |
| cargo-deny | 0.20.2 | exact | cargo |
| hyperfine | 2.0.0 | exact | cargo |
| gitleaks | 8.30.1 | exact | go install |
| Go | 1.27 | minimum | system package |
| node | 24 | major | system package |
| script (util-linux) | 2.39 | minimum | system package |
| cross | 0.2.5 | exact | cargo |
| Docker | 29 | minimum | system package |

## Exceptions
`;
const VERSIONS = {
  'cargo-llvm-cov': '0.9.1', 'diff-cover': '9.7.2', 'cargo-mutants': '27.1.0', 'cargo-nextest': '0.9.151',
  'cargo-deny': '0.20.2', hyperfine: '2.0.0', gitleaks: '8.30.1', go: '1.27.3', node: '24.20.0', script: '2.39.3', cross: '0.2.5',
  docker: '29.7.2',
};
// check.sh's MUTATION_TESTERS names sb-journal, sb-host and sb-tui, so
// they are here beside sb-foo and the binary.
const manifest = (name) => '[package]\nname = "' + name + '"\n\n[lints]\nworkspace = true\n';
const CRATES = {
  'crates/foo/Cargo.toml': manifest('sb-foo'),
  'crates/host/Cargo.toml': manifest('sb-host'),
  'crates/journal/Cargo.toml': manifest('sb-journal'),
  'crates/sb/Cargo.toml': manifest('sb'),
  'crates/tui/Cargo.toml': manifest('sb-tui'),
  'deny.toml': '[bans]\ndeny = [\n    # foo: used by the binary.\n    { crate = "sb-foo", wrappers = ["sb"] },\n'
    + '    { crate = "sb-host", wrappers = ["sb"] },\n    { crate = "sb-journal", wrappers = ["sb-host", "sb-journal"] },\n'
    + '    { crate = "sb-tui", wrappers = ["sb"] },\n]\n',
};
// The system tools check.sh, check-commit-messages.sh and the stubs run.
const SYSTEM_TOOLS = ['awk', 'basename', 'bash', 'cat', 'chmod', 'dirname', 'env', 'find', 'git', 'grep', 'id', 'mkdir',
  'mktemp', 'rm', 'sed', 'sleep', 'sort', 'stat', 'xargs'];
const stubs = (versions, arch) => {
  const log = 'printf \'%s|%s|%s|%s|%s\\n\' "$(basename "$0")" "${CARGO_TARGET_DIR-unset}"'
    + ' "${CARGO_BUILD_TARGET_DIR-unset}" "${CARGO_LLVM_COV_TARGET_DIR-unset}" "$*" >>"$STUB_LOG"\n';
  const installed = ['cargo-deny', 'cargo-llvm-cov', 'cargo-mutants', 'cargo-nextest', 'cross', 'hyperfine']
    .filter((t) => versions[t] && versions[t] !== 'not-from-cargo').map((t) => t + ' v' + versions[t] + ':\n    ' + t + '\n').join('');
  // A warning with a number in it goes to stderr first: the version is
  // read from stdout only.
  const version = (text) => 'if [ "$1" = --version ]; then echo "warning: 3 settings ignored" >&2; echo "' + text + '"; exit 0; fi\n';
  const out = {
    cargo: 'if [ "$1 $2" = "install --list" ]; then printf \'%s\' "' + installed + '"; exit 0; fi\n',
    'cargo-llvm-cov': '', 'cargo-mutants': '', 'cargo-nextest': '', 'cargo-deny': '', hyperfine: '',
    cross: 'mkdir -p "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/production"\n'
      + 'printf sb >"$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/production/sb"\n',
    'diff-cover': version('diff-cover ' + versions['diff-cover']),
    node: version('v' + versions.node),
    script: version('script from util-linux ' + versions.script),
    docker: version('Docker version ' + versions.docker + ', build a7dcaa6'),
    go: 'if [ "$1 $2" = "version -m" ]; then printf \'%s: go1.27.3\\n\\tpath\\tgithub.com/zricethezav/gitleaks/v8\\n'
      + '\\tmod\\tgithub.com/zricethezav/gitleaks/v8\\tv' + versions.gitleaks + '\\th1:x=\\n\' "$3"; exit 0; fi\n'
      + 'echo "go version go' + versions.go + ' linux/amd64"\n',
    gitleaks: 'cat >"$STUB_LOG.gitleaks"\n',
    uname: 'echo ' + arch + '\n',
  };
  // cross's container caps, which a scope around check.sh doesn't reach.
  out.cross += 'printf \'%s\\n\' "${CROSS_CONTAINER_OPTS-unset}" >>"$STUB_LOG.cross-opts"\n';
  // The NEXTEST_* and CARGO_MUTANTS_* variables cargo (cargo-mutants, so
  // nextest) is run with.
  out.cargo += 'env | grep -E \'^(NEXTEST|CARGO_MUTANTS)_\' >>"$STUB_LOG.nextest-env" || true\n';
  // With LEAK_PIDS set, cargo-mutants lists one mutant in the wide pass
  // (sb-foo's), and its run leaves a process behind as a signal-ignoring
  // mutant of sb does: in its own session (as under `script`), ignoring
  // SIGHUP, SIGINT and SIGTERM, its executable in the run's temp copy
  // (under TMPDIR, deleted at the end).
  // A decoy outside that copy is started beside it. Their pids go to
  // LEAK_PIDS, the TMPDIR to LEAK_PIDS.tmpdir.
  out.cargo += 'if [ "$1" = mutants ] && [ -n "${LEAK_PIDS:-}" ]; then\n'
    + '  case " $* " in *" --list "*) case " $* " in *" -p sb-foo "*) echo "crates/foo/src/lib.rs:1:1: replace f with ()" ;; esac; exit 0 ;; esac\n'
    + '  echo "${TMPDIR-unset}" >"$LEAK_PIDS.tmpdir"\n'
    + '  copy="${TMPDIR:-/tmp}/cargo-mutants-repo-stub.tmp"; $LEAK_MKDIR -p "$copy/target/debug" "$LEAK_DECOY_DIR"\n'
    + '  $LEAK_CP "$LEAK_SLEEP" "$copy/target/debug/sb"; $LEAK_CP "$LEAK_SLEEP" "$LEAK_DECOY_DIR/sb"\n'
    // One after the other, so the leak's pid is the first line.
    + '  k=0\n'
    + '  for exe in "$copy/target/debug/sb" "$LEAK_DECOY_DIR/sb"; do\n'
    + '    k=$((k + 1))\n'
    + '    $LEAK_SETSID /bin/sh -c \'trap "" HUP INT TERM; echo $$ >>"$1"; exec "$0" 600\' "$exe" "$LEAK_PIDS" </dev/null >/dev/null 2>&1 &\n'
    + '    n=0; while [ "$($LEAK_WC -l <"$LEAK_PIDS")" -lt "$k" ] && [ $((n += 1)) -lt 100 ]; do $LEAK_SLEEP 0.05; done\n'
    + '  done\n'
    + '  $LEAK_SLEEP 0.2\n'
    + '  $LEAK_RM -rf "$copy"\n'
    + '  while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\n'
    + '  $LEAK_MKDIR -p "$out/mutants.out"\n'
    + '  echo \'{"caught":1,"missed":0,"timeout":0}\' >"$out/mutants.out/outcomes.json"\n'
    + '  exit 0\n'
    + 'fi\n';
  // `docker run`: sb version, unless the container is of the release in
  // DOCKER_FAILS, which can't run the binary, or DOCKER_DOWN is set: docker
  // fails itself, with its own status 125.
  out.docker += 'if [ -n "${DOCKER_DOWN:-}" ]; then echo "Cannot connect to the Docker daemon" >&2; exit 125; fi\n'
    + 'case " $* " in *" ubuntu:${DOCKER_FAILS:-none} "*) echo "sb: GLIBC_2.39 not found" >&2; exit 1 ;; esac\n'
    + 'echo "Saltbox CLI version: 0.1.0"\n';
  return Object.fromEntries(Object.entries(out).filter(([t]) => versions[t] !== null)
    .map(([t, body]) => [t, '#!/bin/sh\n' + log + body]));
};

// checkRun(stage, options): runs scripts/check.sh in a scratch repository.
// - files: added to the committed tree (path -> content, null deletes);
// - work: changed in the work tree afterwards, uncommitted;
// - commits: messages committed (empty) after the base commit;
// - base: check.sh's base argument (default HEAD);
// - versions: a stub's version (null: no stub at all);
// - arch, env, config: uname -m, extra environment, git configuration.
const checkRun = (stage, { files = {}, work = {}, commits = [], base = 'HEAD', versions = {}, arch = 'x86_64',
  moves = [],
  env: extra = {}, config = {} } = {}) => {
  const dir = mkdtempSync(join(tmpdir(), 'check-sh-'));
  try {
    const bin = join(dir, 'stub-bin'), repo = join(dir, 'repo'), logFile = join(dir, 'stub.log');
    const write = (root, entries, mode) => {
      for (const [f, text] of Object.entries(entries)) {
        if (text === null) { rmSync(join(root, f)); continue; }
        mkdirSync(dirname(join(root, f)), { recursive: true });
        writeFileSync(join(root, f), text, { mode });
      }
    };
    write(bin, stubs({ ...VERSIONS, ...versions }, arch), 0o755);
    // PATH holds only scratch directories: the stubs, and links to the
    // system tools the scripts run besides them, so no tool the host has
    // installed is found in place of a missing stub.
    const sys = join(dir, 'sys-bin');
    mkdirSync(sys);
    for (const t of SYSTEM_TOOLS) symlinkSync(execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim(), join(sys, t));
    writeFileSync(logFile, '');
    const scratch = { ...env, HOME: dir, XDG_CONFIG_HOME: dir, PATH: bin + ':' + sys, STUB_LOG: logFile, ...extra };
    const git = (...args) => execFileSync('git', args, { cwd: repo, env: scratch, stdio: 'pipe' });
    mkdirSync(join(repo, 'target'), { recursive: true });
    git('init', '-q');
    write(repo, {
      'scripts/check.sh': readFileSync(join(SCRIPTS, 'check.sh'), 'utf8'),
      'scripts/check-commit-messages.sh': readFileSync(join(SCRIPTS, 'check-commit-messages.sh'), 'utf8'),
      'CONSTRAINTS.md': TOOLS_MD, '.gitignore': '/target/\n', ...CRATES, ...files,
    }, 0o755);
    const commit = (message) => git('-c', 'user.name=test', '-c', 'user.email=test', 'commit', '-q', '--allow-empty', '-m', message);
    git('add', '-A');
    commit('chore: base\n\nReview: none, a test fixture');
    for (const message of commits) commit(message);
    for (const [key, value] of Object.entries(config)) git('config', key, value);
    write(repo, work);
    // moves: [from, to] pairs, renamed with `git mv` (staged).
    for (const [from, to] of moves) { mkdirSync(dirname(join(repo, to)), { recursive: true }); git('mv', from, to); }
    const r = spawnSync('bash', ['scripts/check.sh', stage, base], { cwd: repo, env: scratch, encoding: 'utf8' });
    const calls = readFileSync(logFile, 'utf8').split('\n').filter(Boolean).map((l) => {
      // The arguments come last, so a | among them stays in them.
      const [tool, target, buildTarget, covTarget, ...args] = l.split('|');
      return { tool, target, args: args.join('|'), buildTarget, covTarget };
    });
    const read = (f) => { try { return readFileSync(f, 'utf8'); } catch { return null; } };
    return {
      status: r.status, output: r.stdout + r.stderr, calls, root: repo,
      gitleaksInput: read(logFile + '.gitleaks'), changesDiff: read(join(repo, 'target/changes.diff')),
      crossOpts: read(logFile + '.cross-opts'), nextestEnv: read(logFile + '.nextest-env'),
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};
const passes = (r, stage) => {
  assert.equal(r.status, 0, r.output);
  assert.match(r.output, new RegExp('check ' + stage + ': all passed'), r.output);
};
const refuses = (r, status, pattern) => {
  assert.equal(r.status, status, r.output);
  assert.match(r.output, pattern, r.output);
};

test('check.sh: every stage passes when the stubbed tools match the Tools table', () => {
  passes(checkRun('fast'), 'fast');
  passes(checkRun('task'), 'task');
  passes(checkRun('full'), 'full');
});

test('check.sh fast: a crate without a deny.toml bans.deny entry fails', () => {
  const bar = { 'crates/bar/Cargo.toml': '[package]\nname = "sb-bar"\n\n[lints]\nworkspace = true\n' };
  refuses(checkRun('fast', { files: bar }), 1, /sb-bar.*deny\.toml|deny\.toml.*sb-bar/);
  // An entry that is only a comment, or has no wrappers, is no entry.
  const commented = { ...bar, 'deny.toml': CRATES['deny.toml'].replace('\n]\n', '\n    # { crate = "sb-bar", wrappers = ["sb"] },\n]\n') };
  refuses(checkRun('fast', { files: commented }), 1, /sb-bar/);
  const unwrapped = { ...bar, 'deny.toml': CRATES['deny.toml'].replace('\n]\n', '\n    { crate = "sb-bar" },\n]\n') };
  refuses(checkRun('fast', { files: unwrapped }), 1, /sb-bar/);
  passes(checkRun('fast', {
    files: { ...bar, 'deny.toml': CRATES['deny.toml'].replace('\n]\n', '\n    { crate = "sb-bar", wrappers = ["sb"] },\n]\n') },
  }), 'fast');
});

test('check.sh fast: a crate that does not inherit the workspace lints fails', () => {
  refuses(checkRun('fast', { files: { 'crates/foo/Cargo.toml': '[package]\nname = "sb-foo"\n' } }), 1, /crates\/foo.*lints/);
  refuses(checkRun('fast', { files: { 'crates/foo/Cargo.toml': '[package]\nname = "sb-foo"\n\n[lints]\nworkspace = false\n' } }),
    1, /crates\/foo.*lints/);
  // `workspace = true` under another table is not the lints table's.
  refuses(checkRun('fast', { files: { 'crates/foo/Cargo.toml': '[package]\nname = "sb-foo"\nworkspace = true\n' } }),
    1, /crates\/foo.*lints/);
  refuses(checkRun('fast', { files: { 'crates/sb/Cargo.toml': '[package]\nname = "sb"\n' } }), 1, /crates\/sb.*lints/);
});

test('check.sh: a tool version that differs from the Tools table exits 2 naming the tool, before any check', () => {
  const r = checkRun('task', { versions: { 'cargo-mutants': '27.0.0' } });
  refuses(r, 2, /cargo-mutants.*27\.0\.0.*27\.1\.0/);
  assert.ok(r.calls.filter((c) => c.tool === 'cargo').every((c) => c.args === 'install --list'), r.output);
  refuses(checkRun('fast', { versions: { gitleaks: '8.30.0' } }), 2, /gitleaks/);
  refuses(checkRun('task', { versions: { 'diff-cover': '9.7.3' } }), 2, /diff-cover/);
  refuses(checkRun('full', { versions: { cross: '0.2.4' } }), 2, /cross/);
  refuses(checkRun('full', { versions: { 'cargo-deny': '0.20.1' } }), 2, /cargo-deny/);
});

test('check.sh: a tool cargo did not install, or one missing, exits 2 naming it', () => {
  refuses(checkRun('task', { versions: { hyperfine: null } }), 2, /hyperfine not installed/);
  refuses(checkRun('fast', { versions: { go: null } }), 2, /go not installed/);
  refuses(checkRun('fast', { versions: { node: null } }), 2, /node not installed/);
  refuses(checkRun('full', { versions: { docker: null } }), 2, /docker not installed/);
  // On PATH, but not in `cargo install --list` (a copy, or a distro package).
  refuses(checkRun('task', { versions: { hyperfine: 'not-from-cargo' } }), 2, /could not read the version of hyperfine/);
  // A git or path build is no registry release: only `NAME vX.Y.Z:` counts.
  refuses(checkRun('full', { versions: { cross: '0.2.5 (https://github.com/cross-rs/cross#4090beca)' } }), 2,
    /could not read the version of cross/);
  refuses(checkRun('task', { versions: { hyperfine: '2.0.0 (/src/hyperfine)' } }), 2, /could not read the version of hyperfine/);
});

test('check.sh: node is held to its major version, script, Docker and Go to a minimum', () => {
  refuses(checkRun('fast', { versions: { node: '22.11.0' } }), 2, /\bnode\b/);
  refuses(checkRun('fast', { versions: { node: '25.0.0' } }), 2, /\bnode\b/);
  passes(checkRun('fast', { versions: { node: '24.99.1' } }), 'fast');
  refuses(checkRun('task', { versions: { script: '2.38.1' } }), 2, /\bscript\b/);
  passes(checkRun('task', { versions: { script: '2.41' } }), 'task');
  refuses(checkRun('full', { versions: { docker: '28.5.2' } }), 2, /\bdocker\b/);
  passes(checkRun('full', { versions: { docker: '30.0.1' } }), 'full');
  refuses(checkRun('fast', { versions: { go: '1.26.9' } }), 2, /\bgo\b/);
  // A pre-release isn't the release the row names, nor later than it.
  refuses(checkRun('fast', { versions: { go: '1.27rc1' } }), 2, /\bgo is 1\.27rc1/);
  passes(checkRun('fast', { versions: { go: '1.30.0' } }), 'fast');
});

test('check.sh: a Tools row without a known rule, or a tool without a row, exits 2', () => {
  refuses(checkRun('fast', { files: { 'CONSTRAINTS.md': TOOLS_MD.replace('| 24 | major |', '| 24 | latest |') } }), 2, /\bnode\b/);
  refuses(checkRun('fast', { files: { 'CONSTRAINTS.md': TOOLS_MD.replace(/^\| gitleaks .*\n/m, '') } }), 2, /gitleaks/);
});

test('check.sh: a tool row outside the Tools section is not read', () => {
  const decoy = '# Constraints\n\n## Notes\n\n| Tool | Version | Rule |\n|---|---|---|\n| node | 1 | exact |\n\n'
    + TOOLS_MD.replace('# Constraints\n\n', '');
  passes(checkRun('fast', { files: { 'CONSTRAINTS.md': decoy } }), 'fast');
  const after = TOOLS_MD.replace('## Exceptions\n', '## Exceptions\n\n| node | 1 | exact |\n');
  passes(checkRun('fast', { files: { 'CONSTRAINTS.md': after } }), 'fast');
});

test('check.sh full: the commit messages after the base are checked, and a base at HEAD says so', () => {
  const good = 'fix: correct the reader\n\nReview: none, a test fixture';
  passes(checkRun('full', { commits: [good], base: 'HEAD~1' }), 'full');
  assert.match(checkRun('full', { commits: [good], base: 'HEAD~1' }).output, /check-commit-messages: OK/);
  const bad = checkRun('full', { commits: ['Fix the reader\n\nReview: none, a test fixture'], base: 'HEAD~1' });
  refuses(bad, 1, /Not a Conventional Commit.*Fix the reader/);
  // It runs first, so a bad subject fails before any build.
  assert.ok(bad.calls.every((c) => c.tool !== 'cargo' || c.args === 'install --list'), bad.output);
  const atHead = checkRun('full', { commits: ['Fix the reader'] });
  passes(atHead, 'full');
  assert.match(atHead.output, /skipped: .*HEAD/);
  refuses(checkRun('fast', { base: 'no-such-ref' }), 2, /no-such-ref/);
});

// CARGO_TARGET_DIR is set to the repository's target/: cargo reads it before
// a configuration file's build.target-dir
// (https://doc.rust-lang.org/cargo/reference/config.html#buildtarget-dir).
// cargo-mutants gets none, so each mutant builds in its own copy.
test('check.sh: cargo runs locked, with the target directory its own', () => {
  const r = checkRun('full', {
    work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' },
    env: { CARGO_TARGET_DIR: '/nonexistent/elsewhere', CARGO_BUILD_TARGET_DIR: '/nonexistent/elsewhere',
      CARGO_LLVM_COV_TARGET_DIR: '/nonexistent/elsewhere' },
  });
  passes(r, 'full');
  const cargo = r.calls.filter((c) => c.tool === 'cargo' && c.args !== 'install --list');
  for (const sub of ['fmt', 'clippy', 'test', 'llvm-cov', 'mutants', 'build', 'deny']) {
    assert.ok(cargo.some((c) => c.args.split(' ')[0] === sub), 'no cargo ' + sub + ' call:\n' + r.output);
  }
  for (const c of cargo) {
    assert.equal(c.target, c.args.startsWith('mutants') ? 'unset' : join(r.root, 'target'), 'cargo ' + c.args);
    assert.equal(c.buildTarget, 'unset', 'cargo ' + c.args);
    assert.equal(c.covTarget, 'unset', 'cargo ' + c.args);
    if (!c.args.startsWith('fmt')) assert.match(c.args, /(^| )(--locked|--cargo-arg=--locked)( |$)/, 'cargo ' + c.args);
  }
  const cross = r.calls.filter((c) => c.tool === 'cross');
  assert.equal(cross.length, 1, r.output);
  assert.equal(cross[0].target, join(r.root, 'target/cross-gnu'));
  assert.match(cross[0].args, /(^| )--locked( |$)/);
  assert.match(cross[0].args, /(^| )--target x86_64-unknown-linux-gnu( |$)/);
  // A scope around check.sh doesn't reach cross's container: the caps go
  // to its `docker run`.
  assert.equal(r.crossOpts, '--memory 16g --cpus 8\n', r.output);
});

// Each mutant's tests run under nextest's `mutants` profile, which ends a
// hung test (ADR-021), and no inherited NEXTEST_* or CARGO_MUTANTS_*
// variable overrides it.
test('check.sh: the mutation passes run nextest with the mutants profile, and no NEXTEST_ or CARGO_MUTANTS_ variable', () => {
  const r = checkRun('task', {
    work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' },
    env: { NEXTEST_PROFILE: 'default', NEXTEST_RETRIES: '3', NEXTEST_TEST_THREADS: '1', CARGO_MUTANTS_JOBS: '8',
      CARGO_MUTANTS_MINIMUM_TEST_TIMEOUT: '1' },
  });
  passes(r, 'task');
  const mutants = r.calls.filter((c) => c.tool === 'cargo' && c.args.startsWith('mutants '));
  assert.ok(mutants.length > 0, r.output);
  for (const c of mutants) {
    assert.match(c.args, /(^| )--test-tool=nextest( |$)/, c.args);
    assert.match(c.args, /(^| )--cargo-test-arg=--profile=mutants( |$)/, c.args);
    assert.match(c.args, /(^| )--timeout 300( |$)|--list/, c.args);
  }
  assert.equal(r.nextestEnv ?? '', '', r.nextestEnv);
  refuses(checkRun('task', { versions: { 'cargo-nextest': '0.9.150' } }), 2, /cargo-nextest.*0\.9\.150.*0\.9\.151/);
  refuses(checkRun('task', { versions: { 'cargo-nextest': null } }), 2, /cargo-nextest not installed/);
});

// A mutant that ignores signals outlives its test when nextest or
// cargo-mutants ends the test's process group: under `script` it runs in
// its own session. Each pass's temp copies go under a new directory of
// its own, outside the repository, and every process whose executable
// lies there is killed after the pass; nothing outside it is touched
// (ADR-021).
test('check.sh: a signal-ignoring process left by a mutation pass is gone after it, a process outside its copy is not', () => {
  const dir = mkdtempSync(join(tmpdir(), 'leak-'));
  const pids = join(dir, 'pids');
  writeFileSync(pids, '');
  const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
  // Gone: no process, or a zombie (its parent hasn't reaped it yet).
  const alive = (pid) => {
    try { return !/^\d+ \(.*\) Z /.test(readFileSync('/proc/' + pid + '/stat', 'utf8')); } catch { return false; }
  };
  let started = [];
  try {
    const r = checkRun('task', {
      work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' },
      env: { LEAK_PIDS: pids, LEAK_DECOY_DIR: join(dir, 'decoy'), LEAK_CP: tool('cp'), LEAK_MKDIR: tool('mkdir'),
        LEAK_RM: tool('rm'), LEAK_SETSID: tool('setsid'), LEAK_SLEEP: tool('sleep'), LEAK_WC: tool('wc'), TMPDIR: dir },
    });
    started = readFileSync(pids, 'utf8').split('\n').filter(Boolean).map(Number);
    passes(r, 'task');
    assert.equal(started.length, 2, r.output);
    const passTmp = readFileSync(pids + '.tmpdir', 'utf8').trim();
    assert.match(passTmp, new RegExp('^' + dir + '/sb-mutants-0\\.\\w+$'), r.output);
    assert.ok(!existsSync(passTmp), 'the pass directory is left: ' + passTmp);
    // A SIGKILLed process drops its executable link before it has fully
    // exited, so it is given up to 5 s to finish.
    const pause = new Int32Array(new SharedArrayBuffer(4));
    for (let i = 0; i < 50 && alive(started[0]); i++) Atomics.wait(pause, 0, 0, 100);
    assert.ok(!alive(started[0]), 'the leaked process ' + started[0] + ' is still running:\n' + r.output);
    assert.ok(alive(started[1]), 'the decoy outside the pass was killed:\n' + r.output);
    assert.match(r.output, /mutants: killed 1 process/, r.output);
  } finally {
    for (const pid of started) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    rmSync(dir, { recursive: true, force: true });
  }
});

// The production binary is built on the oldest supported release, and the
// gate runs it on each release of the host spec's runtime list (ADR-020).
test('check.sh full: sb version runs in a container of each supported release, the built binary mounted', () => {
  const r = checkRun('full', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
  passes(r, 'full');
  const runs = r.calls.filter((c) => c.tool === 'docker' && c.args.startsWith('run '));
  assert.deepEqual(runs.map((c) => c.args.match(/ ubuntu:(\S+) /)[1]), ['22.04', '24.04', '26.04'], r.output);
  const bin = join(r.root, 'target/cross-gnu/x86_64-unknown-linux-gnu/production/sb');
  for (const c of runs) {
    assert.ok(c.args.includes(' -v ' + bin + ':/sb:ro '), c.args);
    assert.match(c.args, /(^| )--rm( |$)/);
    assert.match(c.args, /(^| )--network none( |$)/);
    assert.match(c.args, /(^| )-e SB_LOG_DIR=\S+( |$)/);
    assert.match(c.args, / \/sb version$/);
  }
  assert.match(r.output, /ubuntu:26\.04: Saltbox CLI version/, r.output);
});

test('check.sh full: a release that cannot run the binary fails the gate, naming it', () => {
  for (const release of ['22.04', '26.04']) {
    refuses(checkRun('full', { env: { DOCKER_FAILS: release } }), 1, new RegExp('ubuntu:' + release.replace('.', '\\.')
      + '.*can.t run'));
  }
});

test('check.sh full: docker failing itself (status 125) means the check could not run, not that sb failed', () => {
  refuses(checkRun('full', { env: { DOCKER_DOWN: '1' } }), 2, /docker could not run a ubuntu:22\.04 container/);
});

test('check.sh: diffs are plain whatever git is configured to do', () => {
  const external = { 'external-diff.sh': '#!/bin/sh\necho EXTERNAL\n' };
  const r = checkRun('task', {
    files: { ...external, 'README.md': 'base\n', '.gitattributes': '*.md diff=hide\n*.rs diff=hide\n',
      'crates/foo/src/old.rs': 'pub fn old() {}\n' },
    work: { 'README.md': 'changed SECRET\n', 'crates/foo/src/old.rs': 'pub fn old() -> u8 { 1 }\n', 'crates/foo/src/lib.rs': 'pub fn f() {}\n' },
    config: { 'diff.noprefix': 'true', 'diff.mnemonicPrefix': 'true', 'color.ui': 'always', 'color.diff': 'always',
      'diff.external': './external-diff.sh', 'diff.hide.textconv': 'sed s/SECRET/xxxx/; s/pub/xxx/' },
  });
  passes(r, 'task');
  assert.ok(r.gitleaksInput.includes('+++ b/README.md'), r.gitleaksInput);
  assert.ok(r.gitleaksInput.includes('+changed SECRET'), r.gitleaksInput);
  assert.ok(r.changesDiff.includes('+pub fn f() {}'), r.changesDiff);
  assert.ok(r.changesDiff.includes('+++ b/crates/foo/src/old.rs'), r.changesDiff);
  assert.ok(r.changesDiff.includes('+pub fn old() -> u8 { 1 }'), r.changesDiff);
  assert.ok(r.changesDiff.includes('+++ b/crates/foo/src/lib.rs'), r.changesDiff);
  for (const text of [r.gitleaksInput, r.changesDiff]) {
    assert.ok(!text.includes('EXTERNAL') && !text.includes('\x1b['), text);
  }
});

test('check.sh full: another architecture says it skips the production build, and needs no cross or Docker', () => {
  const r = checkRun('full', { arch: 'aarch64', versions: { cross: null, docker: null } });
  passes(r, 'full');
  assert.match(r.output, /skipped: .*aarch64/);
  assert.equal(r.calls.filter((c) => c.tool === 'cross' || c.tool === 'docker').length, 0, r.output);
});

// msgRun(commits): check-commit-messages.sh over the commits made after a
// base commit, each a message, or { merge: message } for a merge of a side
// branch with one commit.
const msgRun = (commits, extra = {}) => {
  const dir = mkdtempSync(join(tmpdir(), 'commit-messages-'));
  try {
    const scratch = { ...env, HOME: dir, XDG_CONFIG_HOME: dir, ...extra };
    const git = (...args) => execFileSync('git', args, { cwd: dir, env: scratch, stdio: 'pipe' }).toString();
    const id = ['-c', 'user.name=test', '-c', 'user.email=test'];
    git('init', '-q', '-b', 'main');
    git(...id, 'commit', '-q', '--allow-empty', '-m', 'chore: base');
    const base = git('rev-parse', 'HEAD').trim();
    for (const c of commits) {
      if (typeof c === 'string') { git(...id, 'commit', '-q', '--allow-empty', '-m', c); continue; }
      git('checkout', '-q', '-b', 'side');
      git(...id, 'commit', '-q', '--allow-empty', '-m', 'feat: a side change');
      git('checkout', '-q', 'main');
      git(...id, 'commit', '-q', '--allow-empty', '-m', 'feat: a main change');
      git(...id, 'merge', '-q', '--no-ff', '-m', c.merge, 'side');
      git('branch', '-q', '-D', 'side');
    }
    const r = spawnSync('bash', [join(SCRIPTS, 'check-commit-messages.sh'), base, 'HEAD'], { cwd: dir, env: scratch, encoding: 'utf8' });
    return { status: r.status, output: r.stdout + r.stderr };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};
const accepted = (commits) => { const r = msgRun(commits); assert.equal(r.status, 0, r.output); };
const rejected = (commits, pattern) => { const r = msgRun(commits); assert.equal(r.status, 1, r.output); assert.match(r.output, pattern, r.output); };

test('check-commit-messages.sh: a "Merge " subject that is no merge is checked, a true merge is skipped', () => {
  rejected(['Merge the two readers'], /Merge the two readers/);
  accepted([{ merge: "Merge branch 'side'" }]);
});

test('check-commit-messages.sh: the description\'s first letter is lowercase, after leading non-letters', () => {
  rejected(['fix: `Foo` reads the journal'], /`Foo` reads the journal/);
  rejected(['fix: "Fix" the quoting'], /"Fix" the quoting/);
  rejected(['fix: 2 Fix the readers'], /2 Fix the readers/);
  rejected(['fix: Fix the reader'], /Fix the reader/);
  accepted(['fix: fix the `Foo` reader', 'docs: close ADR-001', 'build: add `--locked`', 'fix: `foo` reads Rust files']);
});

test('check-commit-messages.sh: the scope is lower-case ASCII letters, digits and hyphens', () => {
  rejected(['fix(B): an upper-case scope'], /upper-case scope/);
  accepted(['fix(term-ui2): a scope with a digit']);
});

test('check-commit-messages.sh: the scope and the length are read the same way in any locale', (t) => {
  const collates = spawnSync('bash', ['-c', '[[ é =~ ^[a-z]$ ]]'], { env: { ...env, LC_ALL: 'en_US.UTF-8' } }).status === 0;
  if (!collates) { t.skip('en_US.UTF-8 is not installed, or does not collate é inside a-z'); return; }
  for (const LC_ALL of ['C', 'en_US.UTF-8']) {
    // é is no ASCII letter; and an a with 50 of them is 101 bytes, over
    // the limit of 100 (lengths are bytes), though 51 characters. One byte
    // fewer passes.
    let r = msgRun(['fix(é): an accented scope'], { LC_ALL });
    assert.equal(r.status, 1, LC_ALL + ': ' + r.output);
    r = msgRun(['fix: a' + 'é'.repeat(50)], { LC_ALL });
    assert.equal(r.status, 1, LC_ALL + ': ' + r.output);
    r = msgRun(['fix: ' + 'é'.repeat(49) + 'a'], { LC_ALL });
    assert.equal(r.status, 0, LC_ALL + ': ' + r.output);
  }
});

test('check-commit-messages.sh: the first-letter rule reads ASCII letters in any locale', (t) => {
  // en_US collates Ü inside A-Z; the rule's letters are ASCII, so Ü is
  // skipped as a non-letter in every locale, and the n after it decides.
  // Without that locale bash falls back to C, and the case would prove
  // nothing: it is skipped, saying so.
  const collates = spawnSync('bash', ['-c', '[[ Ü =~ ^[A-Z]$ ]]'], { env: { ...env, LC_ALL: 'en_US.UTF-8' } }).status === 0;
  if (!collates) { t.skip('en_US.UTF-8 is not installed, or does not collate Ü inside A-Z'); return; }
  for (const LC_ALL of ['C', 'C.UTF-8', 'en_US.UTF-8']) {
    const r = msgRun(['fix: Ünicode names'], { LC_ALL });
    assert.equal(r.status, 0, LC_ALL + ': ' + r.output);
  }
});

test('check-commit-messages.sh: the length and the trailing period are checked as before', () => {
  rejected(['fix: end with a period.'], /end with a period/);
  // At any length, and a description that is only punctuation.
  rejected(['fix: .'], /fix: \./);
  rejected(['fix: ,;'], /fix: ,;/);
  accepted(['build: 1.2.3', 'build: 42']);
  rejected(['fix:  two spaces'], /two spaces/);
  rejected(['fix: ' + 'a'.repeat(101)], /Not a Conventional Commit/);
  accepted(['fix: ' + 'a'.repeat(100), 'fix: a']);
});

// Path-aware stages and the tiered Review line (ADR-016).
const cargoSubs = (r) => r.calls.filter((c) => c.tool === 'cargo' && c.args !== 'install --list').map((c) => c.args.split(' ')[0]);
const guardTestRuns = (r) => r.calls.filter((c) => c.tool === 'node' && c.args.includes('.test.mjs')).length;

test('check.sh task and full: a change under docs/ and tasks/ only ends after the fast checks, and says so', () => {
  for (const stage of ['task', 'full']) {
    const r = checkRun(stage, { work: { 'docs/specs/x.md': 'x\n', 'tasks/todo.md': 'x\n' } });
    passes(r, stage);
    assert.match(r.output, /skipped: only docs\/ and tasks\/ changed/, r.output);
    assert.deepEqual(cargoSubs(r), ['fmt', 'clippy', 'clippy'], r.output);
    // check.sh asks each tool for its version before the stage starts.
    for (const tool of ['hyperfine', 'diff-cover', 'cross']) assert.ok(!r.calls.some((c) => c.tool === tool && c.args !== '--version'), tool + '\n' + r.output);
    assert.ok(r.calls.some((c) => c.tool === 'gitleaks'), r.output);
  }
});

test('check.sh full: a document beside any other file, a clean tree, and a document outside docs/ and tasks/ run every step', () => {
  for (const work of [{ 'docs/x.md': 'x\n', 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, {},
    { 'docs/y.md': 'y\n', 'CONSTRAINTS.md': TOOLS_MD + '\n' }, { 'docs/y.md': 'y\n', 'crates/foo/README.md': 'x\n' }]) {
    const r = checkRun('full', { work });
    passes(r, 'full');
    assert.doesNotMatch(r.output, /only docs\/ and tasks\/ changed/, r.output);
    for (const sub of ['llvm-cov', 'test', 'build', 'deny']) assert.ok(cargoSubs(r).includes(sub), sub + '\n' + r.output);
    assert.equal(r.calls.filter((c) => c.tool === 'cross').length, 1, r.output);
  }
});

test('check.sh full: committed documents after the base are documents only, with their messages checked', () => {
  // The harness commits are empty, so the document arrives in the work tree
  // and the commit after the base is what the message check reads.
  const r = checkRun('full', { commits: ['docs: note it\n\nReview: none, a test fixture'], base: 'HEAD~1', work: { 'docs/x.md': 'x\n' } });
  passes(r, 'full');
  assert.match(r.output, /check-commit-messages: OK/, r.output);
  assert.match(r.output, /only docs\/ and tasks\/ changed/, r.output);
});

test('check.sh fast: the gate scripts\' own tests run when scripts/ changed or nothing did, and are skipped otherwise', () => {
  assert.equal(guardTestRuns(checkRun('fast')), 1);
  assert.equal(guardTestRuns(checkRun('fast', { work: { 'scripts/new.sh': 'true\n' } })), 1);
  for (const work of [{ 'docs/x.md': 'x\n' }, { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }]) {
    const r = checkRun('fast', { work });
    passes(r, 'fast');
    assert.equal(guardTestRuns(r), 0, r.output);
    assert.match(r.output, /skipped: nothing under scripts\/ changed/, r.output);
    // The guard itself always runs.
    assert.ok(r.calls.some((c) => c.tool === 'node' && c.args.startsWith('scripts/floor-guard.mjs')), r.output);
  }
});

test('check.sh task: the crates\' tests run once, under coverage, then the doc tests and the prototypes', () => {
  const proto = { 'prototypes/proto-a/Cargo.toml': '[package]\nname = "proto-a"\n\n[dependencies]\nname = "1"\n' };
  const r = checkRun('task', { files: proto, work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
  passes(r, 'task');
  const runs = r.calls.filter((c) => c.tool === 'cargo' && /^(llvm-cov|test) /.test(c.args)).map((c) => c.args);
  assert.equal(runs.length, 3, r.output);
  assert.match(runs[0], /^llvm-cov .*-p sb -p sb-foo /);
  assert.match(runs[1], /^test .*--doc .*-p sb-foo/);
  assert.equal(runs[2], 'test --locked --quiet -p proto-a');
  // Without a prototype there is no third run, and none over the workspace.
  const none = checkRun('task', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
  passes(none, 'task');
  assert.ok(!none.calls.some((c) => c.tool === 'cargo' && /^test (?!.*--doc)/.test(c.args)), none.output);
});

// reviewRun(files, message): check-commit-messages.sh over one commit that
// adds the files, after a base that holds the script (the Review rule
// applies from the commit after the one that introduced it).
const reviewRun = (files, message) => {
  const dir = mkdtempSync(join(tmpdir(), 'commit-review-'));
  try {
    const scratch = { ...env, HOME: dir, XDG_CONFIG_HOME: dir };
    const git = (...args) => execFileSync('git', args, { cwd: dir, env: scratch, stdio: 'pipe' }).toString();
    const id = ['-c', 'user.name=test', '-c', 'user.email=test'];
    const add = (f, text) => { mkdirSync(dirname(join(dir, f)), { recursive: true }); writeFileSync(join(dir, f), text); };
    git('init', '-q', '-b', 'main');
    add('scripts/check-commit-messages.sh', readFileSync(join(SCRIPTS, 'check-commit-messages.sh'), 'utf8'));
    git('add', '-A');
    git(...id, 'commit', '-q', '-m', 'chore: base');
    for (const f of files) add(f, 'x\n');
    git('add', '-A');
    git(...id, 'commit', '-q', '--allow-empty', '-m', message);
    const r = spawnSync('bash', [join(SCRIPTS, 'check-commit-messages.sh'), 'HEAD~1', 'HEAD'], { cwd: dir, env: scratch, encoding: 'utf8' });
    return { status: r.status, output: r.stdout + r.stderr };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

test('check-commit-messages.sh: a commit of docs/ and tasks/ files only needs no Review line, any other does', () => {
  const ok = (files, message) => { const r = reviewRun(files, message); assert.equal(r.status, 0, r.output); };
  const missing = (files, message) => { const r = reviewRun(files, message); assert.equal(r.status, 1, r.output); assert.match(r.output, /Missing 'Review:' line/, r.output); };
  ok(['docs/specs/x.md', 'tasks/todo.md'], 'docs: note it');
  ok(['docs/x.md', 'CONSTRAINTS.md'], 'docs: note it\n\nReview: none, a test fixture');
  missing(['docs/x.md', 'CONSTRAINTS.md'], 'docs: note it');
  missing(['scripts/x.sh'], 'build: add it');
  missing(['CLAUDE.md'], 'docs: change the rules');
  // A path that only starts like one of the two directories is no document.
  missing(['docs-extra/x.md'], 'docs: note it');
  missing(['crates/foo/docs/x.md'], 'docs: note it');
  // A commit with no files says nothing about what it is: it needs the line.
  missing([], 'chore: nothing');
});

test('check.sh: a file moved into docs/ is no document, and a script moved out of scripts/ runs the gate tests', () => {
  const fixture = { 'crates/foo/tests/snap.txt': 'a fixture a test reads\n', 'scripts/x.sh': 'true\n' };
  const r = checkRun('full', { files: fixture, moves: [['crates/foo/tests/snap.txt', 'docs/snap.txt']] });
  passes(r, 'full');
  assert.doesNotMatch(r.output, /only docs\/ and tasks\/ changed/, r.output);
  assert.ok(cargoSubs(r).includes('llvm-cov'), r.output);
  const moved = checkRun('fast', { files: fixture, moves: [['scripts/x.sh', 'docs/x.sh']] });
  passes(moved, 'fast');
  assert.equal(guardTestRuns(moved), 1, moved.output);
});

test('check.sh task: each prototype is named, wherever its [package] section sits', () => {
  const files = {
    'prototypes/proto-a/Cargo.toml': '[dependencies]\nname = "1"\n\n[package]\nname = "proto-a"\n',
    'prototypes/proto-b/Cargo.toml': '[package]\nname = "proto-b"\n',
  };
  const r = checkRun('task', { files, work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
  passes(r, 'task');
  assert.ok(r.calls.some((c) => c.tool === 'cargo' && c.args === 'test --locked --quiet -p proto-a -p proto-b'), r.output);
});
