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
import { readdirSync, readFileSync, symlinkSync } from 'node:fs';
// For the mutation pass's directory (ADR-021).
import { existsSync } from 'node:fs';
// For the processes a killed run's pass left (ADR-021).
import { spawn } from 'node:child_process';
// For the gate's QEMU image tag, its Dockerfile's hash.
import { createHash } from 'node:crypto';

const guard = join(dirname(fileURLToPath(import.meta.url)), 'floor-guard.mjs');

// The scratch repositories see no configuration and no repository of the
// caller's (a hook exports GIT_DIR and friends); HOME and XDG_CONFIG_HOME
// are each run's scratch directory.
// Nor does a FLOOR_APPROVED the caller set for its own uncommitted work
// (ADR-029): the `approved` option sets it where a case needs it.
const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_') && k !== 'FLOOR_APPROVED'));
env.GIT_CONFIG_GLOBAL = '/dev/null';
env.GIT_CONFIG_SYSTEM = '/dev/null';

// run(base, change): the guard's findings, as "rule file" strings, for
// `change` (path -> new content, null deletes) on top of the committed
// `base` (path -> content): `found`, the blocking ones; `reported`, the
// ones printed for the reviewer; `approved`, the blocking ones an approval
// covers. With `commit`, the change is committed too and the guard is
// given the base commit, as a range run is. `config` is git configuration
// set in the scratch repository (key -> value). A list of changes is a
// commit each, the last one left uncommitted unless `commit` is set; the
// guard is then given the commit before the first of them. `message` is
// the last commit's message, `baseMessage` the base commit's, and
// `approved` the FLOOR_APPROVED variable.
const run = (base, change, { commit = false, config = {}, message = 'test', baseMessage = 'test', approved } = {}) => {
  const dir = mkdtempSync(join(tmpdir(), 'floor-guard-'));
  try {
    const scratch = { ...env, HOME: dir, XDG_CONFIG_HOME: dir };
    const git = (...args) => execFileSync('git', args, { cwd: dir, env: scratch, stdio: 'pipe' });
    const save = (m = 'test') => {
      git('add', '-A');
      git('-c', 'user.name=test', '-c', 'user.email=test', 'commit', '-q', '-m', m);
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
    save(baseMessage);
    const steps = [change].flat();
    steps.forEach((step, i) => {
      write(step);
      if (i < steps.length - 1) save();
      else if (commit) save(message);
    });
    const r = spawnSync(process.execPath, [guard, ...(commit ? ['--base', 'HEAD~' + steps.length] : [])],
      { cwd: dir, env: approved === undefined ? scratch : { ...scratch, FLOOR_APPROVED: approved }, encoding: 'utf8' });
    const of = (text, re) => [...text.matchAll(re)].map((m) => m[1] + ' ' + m[2]);
    return { status: r.status, found: of(r.stderr, /^ {2}\[([\w-]+)\] (\S+):/gm),
      reported: of(r.stdout, /^ {2}\[([\w-]+)\] (\S+):/gm), approved: of(r.stdout, /^ {2}approved \[([\w-]+)\] (\S+):/gm),
      output: r.stdout + r.stderr };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

// The findings that are only reported, never blocking (ADR-029).
const REPORTED = ['assertion-removed', 'test-removed', 'test-deleted', 'gate-line-removed'];
const isReported = (finding) => REPORTED.includes(finding.split(' ')[0]);
// flags: the change gives exactly these findings, in any order, blocking
// and reported, and at least one blocks, so the run fails. reports: it
// gives only these reported ones, and the run passes. clean: it gives
// none of either.
const outcome = (base, change, expected, options) => {
  const r = run(base, change, options);
  const all = [expected].flat();
  assert.deepEqual(r.found.sort(), all.filter((e) => !isReported(e)).sort(), r.output);
  assert.deepEqual(r.reported.sort(), all.filter(isReported).sort(), r.output);
  return r;
};
const flags = (base, change, expected, options) => {
  assert.ok([expected].flat().some((e) => !isReported(e)), 'flags expects a blocking finding');
  assert.equal(outcome(base, change, expected, options).status, 1);
};
const reports = (base, change, expected, options) => {
  assert.ok([expected].flat().every(isReported), 'reports expects reported findings only');
  assert.equal(outcome(base, change, expected, options).status, 0);
};
const clean = (base, change, options) => {
  const r = run(base, change, options);
  assert.deepEqual([...r.found, ...r.reported], [], r.output);
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
  flags(checkSh, edit(checkSh, CHECK, ' -- -D warnings', ''), ['lint-weakened ' + CHECK, 'gate-line-removed ' + CHECK]);
  flags(checkSh, edit(checkSh, CHECK, '-D warnings', '-W warnings'), ['lint-weakened ' + CHECK, 'gate-line-removed ' + CHECK]);
  clean(checkSh, edit(checkSh, CHECK, '  cargo clippy', '    cargo clippy'));
});

test('check.sh: an added -A on a cargo line is flagged; a -D added, or moved to another line, is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, ' -D warnings', ' -D warnings -A clippy::pedantic'),
    ['lint-weakened ' + CHECK, 'gate-line-removed ' + CHECK]);
  // Every spelling: no space, quoted, after `=`, and the long forms.
  for (const [from, to] of [[' -D warnings', ' -Dwarnings -Aclippy::pedantic'],
    [' -D warnings', ' -D warnings --allow clippy::pedantic'], [' -D warnings', ' -D warnings --allow=clippy::pedantic'],
    [' -D warnings', ' --warn warnings'], [' -D warnings', ' --force-warn=warnings'],
    ['  cargo clippy', '  RUSTFLAGS="-A warnings" cargo clippy'], ['  cargo clippy', '  RUSTFLAGS=-Awarnings cargo clippy']]) {
    flags(checkSh, edit(checkSh, CHECK, from, to), ['lint-weakened ' + CHECK, 'gate-line-removed ' + CHECK]);
  }
  flags(checkSh, edit(checkSh, CHECK, 'fast() {', 'fast() {\n  cargo build --quiet -- --allow warnings'), 'lint-weakened ' + CHECK);
  reports(checkSh, edit(checkSh, CHECK, ' -D warnings', ' --deny warnings'), 'gate-line-removed ' + CHECK);
  reports(checkSh, edit(checkSh, CHECK, ' -D warnings', ' -D warnings # not -A clippy::all'), 'gate-line-removed ' + CHECK);
  reports(checkSh, edit(checkSh, CHECK, ' -D warnings', ' -D warnings -D clippy::pedantic'), 'gate-line-removed ' + CHECK);
  reports(checkSh, edit(checkSh, CHECK, '--quiet -- -D warnings', '--quiet \\\n    -- -D warnings'),
    'gate-line-removed ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, 'step() {', 'declare -A seen=()\nstep() {'));
});

test('check.sh: a new MUTATION_TESTERS entry is flagged, a comment beside it is clean', () => {
  flags(checkSh, edit(checkSh, CHECK, 'for c in', 'MUTATION_TESTERS[sb-exec]="sb-docker"\nfor c in'),
    'mutation-scope-narrowed ' + CHECK);
  clean(checkSh, edit(checkSh, CHECK, 'for c in', '# Add a crate when its tests reach a listed one.\nfor c in'));
});

test('check.sh: a removed step is flagged, an added step is clean', () => {
  reports(checkSh,
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

// clippy.toml holds a rule (no child process started outside sb-exec;
// CONSTRAINTS.md, Floor), so taking an entry out, or the file, loosens it.
test('clippy.toml: a removed disallowed-* entry or the deleted file is flagged; other removed settings are clean', () => {
  const config = { 'clippy.toml': '# Why.\nmsrv = "1.99"\ndisallowed-methods = [\n  { path = "std::process::Command::new", reason = "sb-exec" },\n  { path = "tokio::process::Command::new", reason = "sb-exec" },\n]\n' };
  flags(config, edit(config, 'clippy.toml', '  { path = "tokio::process::Command::new", reason = "sb-exec" },\n', ''),
    'clippy-rule-removed clippy.toml');
  flags(config, edit(config, 'clippy.toml', 'disallowed-methods = [\n  { path = "std::process::Command::new", reason = "sb-exec" },\n  { path = "tokio::process::Command::new", reason = "sb-exec" },\n]\n', ''),
    ['clippy-rule-removed clippy.toml', 'clippy-rule-removed clippy.toml', 'clippy-rule-removed clippy.toml']);
  flags(config, { 'clippy.toml': null }, 'gate-file-deleted clippy.toml');
  clean(config, edit(config, 'clippy.toml', 'msrv = "1.99"\n', ''));
  clean(config, edit(config, 'clippy.toml', '# Why.\n', ''));
});

test('nextest.toml: a changed timeout is flagged, a comment is clean', () => {
  const NEXTEST = '.config/nextest.toml';
  const config = { [NEXTEST]: '[profile.mutants]\nslow-timeout = { period = "30s", terminate-after = 2 }\n' };
  flags(config, edit(config, NEXTEST, 'terminate-after = 2', 'terminate-after = 1'), 'tool-config-changed ' + NEXTEST);
  flags(config, edit(config, NEXTEST, ' }', ', on-timeout = "pass" }'), 'tool-config-changed ' + NEXTEST);
  clean(config, edit(config, NEXTEST, '[profile', '# Why.\n[profile'));
});

// The workflow linters' suppressions (the maintainer, 2026-11-24): any
// edit to actionlint's or zizmor's settings, and an added zizmor ignore
// comment under .github/.
const ACTIONLINT = '.github/actionlint.yaml';
const ZIZMOR = '.github/zizmor.yml';
const linterConfigs = {
  [ACTIONLINT]: '# Why each entry is here.\nself-hosted-runner:\n  labels:\n    - ubuntu-26.04\n',
  [ZIZMOR]: '# Why each policy is here.\nrules:\n  unpinned-uses:\n    config:\n      policies:\n        "saltyorg/*": ref-pin\n        "*": hash-pin\n',
};

test('actionlint.yaml and zizmor.yml: an added, changed or removed setting is flagged, a comment is clean', () => {
  flags(linterConfigs, edit(linterConfigs, ACTIONLINT, '    - ubuntu-26.04\n', '    - ubuntu-26.04\n    - my-runner\n'),
    'linter-config-changed ' + ACTIONLINT);
  flags(linterConfigs, edit(linterConfigs, ACTIONLINT, '    - ubuntu-26.04\n', ''), 'linter-config-changed ' + ACTIONLINT);
  flags(linterConfigs, edit(linterConfigs, ZIZMOR, '"*": hash-pin', '"*": ref-pin'),
    ['linter-config-changed ' + ZIZMOR, 'linter-config-changed ' + ZIZMOR]);
  flags(linterConfigs, edit(linterConfigs, ZIZMOR, '        "*": hash-pin\n', ''), 'linter-config-changed ' + ZIZMOR);
  flags(linterConfigs, edit(linterConfigs, ZIZMOR, 'rules:\n', 'rules:\n  template-injection:\n    disable: true\n'),
    ['linter-config-changed ' + ZIZMOR, 'linter-config-changed ' + ZIZMOR], { commit: true });
  clean(linterConfigs, edit(linterConfigs, ZIZMOR, '# Why each policy is here.\n', '# Why each policy is here,\n# with its source.\n'));
  clean(linterConfigs, edit(linterConfigs, ACTIONLINT, '# Why each entry is here.\n', ''));
});

for (const config of [ACTIONLINT, '.github/actionlint.yml', ZIZMOR, '.github/zizmor.yaml', 'zizmor.yml', 'zizmor.yaml']) {
  test('a new or deleted workflow linter config is flagged: ' + config, () => {
    flags({}, { [config]: 'rules: {}\n' }, 'linter-config-changed ' + config);
    flags({}, { [config]: '# comments only\n' }, 'linter-config-changed ' + config, { commit: true });
    flags({ [config]: 'rules: {}\n' }, { [config]: null }, 'linter-config-changed ' + config);
  });
}

test('a workflow linter config name elsewhere is clean', () => {
  clean({}, { 'docs/zizmor.yml': 'rules: {}\n', '.github/workflows/actionlint.yaml': 'on: push\n' });
});

test('an added zizmor ignore comment under .github/ is flagged, a removed one and one elsewhere are clean', () => {
  const WORKFLOW = '.github/workflows/ci.yml';
  const ACTION = '.github/actions/update/action.yml';
  const files = {
    [WORKFLOW]: 'jobs:\n  a:\n    steps:\n      - name: Build # zizmor: ignore[artipacked]\n        run: make\n',
    [ACTION]: 'runs:\n  using: composite\n',
  };
  flags(files, edit(files, WORKFLOW, '        run: make\n', '        # zizmor: ignore[template-injection]\n        run: make\n'),
    'zizmor-ignore-added ' + WORKFLOW);
  flags(files, edit(files, WORKFLOW, 'ignore[artipacked]', 'ignore[artipacked,ref-confusion]'), 'zizmor-ignore-added ' + WORKFLOW);
  flags(files, edit(files, ACTION, 'composite\n', 'composite # zizmor :  ignore[unpinned-uses]\n'), 'zizmor-ignore-added ' + ACTION,
    { commit: true });
  flags({}, { [WORKFLOW]: files[WORKFLOW] }, 'zizmor-ignore-added ' + WORKFLOW);
  // Moved to another line, it silences another finding.
  flags(files, edit(files, WORKFLOW, ' # zizmor: ignore[artipacked]\n        run: make\n', '\n        run: make # zizmor: ignore[artipacked]\n'),
    'zizmor-ignore-added ' + WORKFLOW);
  clean(files, edit(files, WORKFLOW, ' # zizmor: ignore[artipacked]', ''));
  clean(files, edit(files, WORKFLOW, '        run: make\n', '        run: make test\n'));
  clean({}, { 'docs/notes.md': 'a `# zizmor: ignore[artipacked]` comment\n' });
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
  reports(guardMjs, edit(guardMjs, GUARD, "  if (SKIPS.test(text)) flag('test-made-easier', file, text);\n", ''),
    'gate-line-removed ' + GUARD);
  clean(guardMjs, edit(guardMjs, GUARD, '}\n', "  if (/dbg!/.test(text)) flag('unfinished-work', file, text);\n}\n"));
});

test('floor-guard.mjs: a narrowed pattern is flagged, a new pattern and a reworded comment are clean', () => {
  reports(guardMjs, edit(guardMjs, GUARD, '|\\bTODO\\b/;', '/;'), 'gate-line-removed ' + GUARD);
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
  reports(messagesSh, edit(messagesSh, MESSAGES, '[^A-Z ](.{0,98}[^.])?$', '.*$'), 'gate-line-removed ' + MESSAGES);
  clean(messagesSh, edit(messagesSh, MESSAGES, '# Fails if', '# Exits 1 if'));
});

test('check-commit-messages.sh: a removed rule is flagged, an added rule is clean', () => {
  reports(messagesSh, edit(messagesSh, MESSAGES, "  echo \"Missing 'Review:' line: $sha\"\n  status=1\n", '  :\n'),
    ['gate-line-removed ' + MESSAGES, 'gate-line-removed ' + MESSAGES]);
  clean(messagesSh, edit(messagesSh, MESSAGES, 'exit $status',
    'if ! grep -qE \'^Library-check: .+\' <<<"$body"; then status=1; fi\nexit $status'));
});

test('a deleted guard script is flagged once, not line by line', () => {
  flags(messagesSh, { [MESSAGES]: null }, 'gate-file-deleted ' + MESSAGES);
  flags(guardMjs, { [GUARD]: null }, 'gate-file-deleted ' + GUARD, { commit: true });
});

test('a committed weakening is flagged against its base, a committed clean change is not', () => {
  flags(checkSh, edit(checkSh, CHECK, ' -- -D warnings', ''), ['lint-weakened ' + CHECK, 'gate-line-removed ' + CHECK],
    { commit: true });
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
    ['tool-config-added .gitattributes', 'lint-weakened ' + CHECK, 'gate-line-removed ' + CHECK], { commit: true });
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
  clean({}, { 'tools/tui/clippy.toml': 'msrv = "1.99"\n', 'docs/rust-toolchain': 'notes\n' });
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
  reports(checkSh, edit(checkSh, CHECK, entry, 'MUTATION_TESTERS[sb-journal]="sb-host sb"'), 'gate-line-removed ' + CHECK);
  const two = edit(checkSh, CHECK, entry, 'MUTATION_TESTERS[sb-journal]="sb-host sb"');
  flags(two, edit(two, CHECK, '"sb-host sb"', '"sb-host"'), ['mutation-scope-narrowed ' + CHECK, 'gate-line-removed ' + CHECK]);
});

test('one added line does not excuse several removed copies of it', () => {
  const twice = { [MESSAGES]: messagesSh[MESSAGES].replace('exit $status', 'if bad; then\n  status=1\nfi\nexit $status') };
  reports(twice, { [MESSAGES]: messagesSh[MESSAGES].replace("  echo \"Missing 'Review:' line: $sha\"\n  status=1\n", '    status=1\n') },
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

test("the guard's tests are no gate file (ADR-029): a removed assertion, a deleted file and a new test are clean", () => {
  clean(guardTests, edit(guardTests, TESTS, "  flags(checkSh, lowered, 'threshold-loosened');\n", ''));
  clean(guardTests, { [TESTS]: null });
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

// Approved exceptions (ADR-029): a Floor-approved line in a commit message
// after the base, or in FLOOR_APPROVED for uncommitted work.

const approval = (rule, file, reason = 'the reason') => `Floor-approved: ${rule} ${file} — ${reason} (the maintainer, 2026-11-24)`;
const loosened = edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=80 ');

test('an approved blocking finding passes, and is printed with its approval', () => {
  const line = approval('threshold-loosened', CHECK, 'the floor the tree measures');
  for (const options of [{ approved: line }, { commit: true, message: 'test\n\n' + line + '\n' }]) {
    const r = run(checkSh, loosened, options);
    assert.equal(r.status, 0, r.output);
    assert.deepEqual(r.approved, ['threshold-loosened ' + CHECK], r.output);
    assert.ok(r.output.includes(line), r.output);
  }
});

test('an approval covers every finding of its rule in its file, in any commit of the range', () => {
  const twice = edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=80 ');
  twice[CHECK] = twice[CHECK].replace('=45711520', '=55711520');
  const r = run(checkSh, twice, { approved: approval('threshold-loosened', CHECK) });
  assert.equal(r.status, 0, r.output);
  assert.deepEqual(r.approved, ['threshold-loosened ' + CHECK, 'threshold-loosened ' + CHECK], r.output);
  // ADR-029's limit: one commit's approval covers another commit's finding
  // of the same rule in the same file in the range.
  const first = edit(checkSh, CHECK, 'COVERAGE_MIN=90 ', 'COVERAGE_MIN=80 ');
  const second = { [CHECK]: first[CHECK].replace('=45711520', '=55711520') };
  const range = run(checkSh, [first, second], { commit: true, message: 'test\n\n' + approval('threshold-loosened', CHECK) });
  assert.equal(range.status, 0, range.output);
  assert.equal(range.approved.length, 2, range.output);
});

test('an approval that covers no finding is printed, and fails nothing', () => {
  const r = run(checkSh, { 'README.md': 'more\n' }, { approved: approval('threshold-loosened', CHECK) });
  assert.equal(r.status, 0, r.output);
  assert.match(r.output, /an approval that covers no finding/);
});

test('the caller\'s FLOOR_APPROVED never reaches the scratch repositories', () => {
  assert.equal(env.FLOOR_APPROVED, undefined);
});

test('an approval covers its rule in its file only', () => {
  for (const other of [approval('threshold-removed', CHECK), approval('threshold-loosened', MESSAGES)]) {
    flags(checkSh, loosened, 'threshold-loosened ' + CHECK, { approved: other });
  }
  const both = { ...loosened, ...edit(rootToml, ROOT, 'todo = "deny"', 'todo = "allow"') };
  flags({ ...checkSh, ...rootToml }, both, 'lint-weakened ' + ROOT, { approved: approval('threshold-loosened', CHECK) });
});

test('an approval in the base commit approves nothing', () => {
  const old = 'test\n\n' + approval('threshold-loosened', CHECK);
  flags(checkSh, loosened, 'threshold-loosened ' + CHECK, { baseMessage: old });
  flags(checkSh, loosened, 'threshold-loosened ' + CHECK, { baseMessage: old, commit: true });
});

test('a Floor-approved line in another form, or for a reported rule, fails the run', () => {
  for (const bad of ['Floor-approved: threshold-loosened ' + CHECK,
    'Floor-approved: threshold-loosened ' + CHECK + ' — the reason',
    'Floor-approved: threshold-loosened ' + CHECK + ' — the reason (an agent, 2026-11-24)',
    'Floor-approved: threshold-loosened ' + CHECK + ' - the reason (the maintainer, 2026-11-24)',
    '  ' + approval('threshold-loosened', CHECK)]) {
    flags(checkSh, loosened, ['threshold-loosened ' + CHECK, 'approval-malformed Floor-approved'], { approved: bad });
  }
  flags(checkSh, edit(checkSh, CHECK, '  step "floor guard"\n', ''), ['approval-malformed Floor-approved',
    'gate-line-removed ' + CHECK], { approved: approval('gate-line-removed', CHECK) });
  flags(checkSh, { 'README.md': 'more\n' }, 'approval-malformed Floor-approved',
    { commit: true, message: 'test\n\nFloor-approved: anything' });
});

test('reported findings are printed for the reviewer, and pass', () => {
  const r = run(checkSh, edit(checkSh, CHECK, '  step "floor guard"\n', ''));
  assert.equal(r.status, 0, r.output);
  assert.match(r.output, /for the code reviewer to judge/);
  assert.ok(r.output.includes('[gate-line-removed] ' + CHECK + ': step "floor guard"'), r.output);
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
    reports(cliRs, edit(cliRs, CLI, step, step.endsWith(';\n') ? ';\n' : ''), 'assertion-removed ' + CLI);
  }
  reports(cliRs, edit(cliRs, CLI, '        .assert()\n        .failure();\n', '        ;\n'),
    ['assertion-removed ' + CLI, 'assertion-removed ' + CLI]);
  reports(cliRs, edit(cliRs, CLI, '.arg("--bad").assert().failure();', '.arg("--bad").ok();'), 'assertion-removed ' + CLI);
  clean(cliRs, edit(cliRs, CLI, '        .arg("version")\n', ''));
  clean(runRs, edit(runRs, RUN, '        .stdout(Stdio::piped())\n        .stderr(std::process::Stdio::null())\n', ''));
  clean(runRs, edit(runRs, RUN, '\n        .failure("Docker container start");', ';'));
  const exitCode = { [RUN]: runRs[RUN].replace('    Ok(status.success())\n', '    Ok(status\n        .code()\n        .is_some())\n') };
  clean(exitCode, edit(exitCode, RUN, '        .code()\n        .is_some())\n', '        .success())\n'));
});

test('a deleted src file that held tests is flagged, one without tests is clean', () => {
  reports(libRs, { [LIB]: null }, 'test-deleted ' + LIB);
  reports(libRs, { [LIB]: null }, 'test-deleted ' + LIB, { commit: true });
  const tokio = { [RUN]: runRs[RUN] + '\n#[tokio::test]\nasync fn runs() {}\n' };
  reports(tokio, { [RUN]: null }, 'test-deleted ' + RUN);
  reports(cliRs, { [CLI]: null }, 'test-deleted ' + CLI);
  reports({ 'crates/sb/tests/common/mod.rs': 'pub fn sb() {}\n' }, { 'crates/sb/tests/common/mod.rs': null },
    'test-deleted crates/sb/tests/common/mod.rs');
  clean({ 'docs/testing.md': 'Unit tests carry #[test].\n' }, { 'docs/testing.md': null });
  clean(runRs, { [RUN]: null });
  reports({ ...runRs, ...cliRs }, { [RUN]: null, [CLI]: null }, 'test-deleted ' + CLI);
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
  reports(split, edit(split, CLI, '        .code(\n            predicate::in_iter([0, 1]),\n        )\n', ''), 'assertion-removed ' + CLI);
});

test('a test removed from a file that stays is flagged, a renamed or re-attributed one is clean', () => {
  reports(libRs, edit(libRs, LIB, '    #[test]\n', ''), 'test-removed ' + LIB);
  reports(libRs, edit(libRs, LIB, '    #[test]\n    fn adds() {\n        assert_eq!(add(1, 2), 3);\n    }\n', ''),
    ['test-removed ' + LIB, 'assertion-removed ' + LIB]);
  reports(libRs, edit(libRs, LIB, '    #[test]', '    #[cfg_attr(any(), test)]'), 'test-removed ' + LIB);
  const outOfLine = { [RUN]: runRs[RUN] + '\n#[cfg(test)]\nmod tests;\n' };
  reports(outOfLine, edit(outOfLine, RUN, '\n#[cfg(test)]\nmod tests;\n', ''), 'test-removed ' + RUN);
  clean(libRs, edit(libRs, LIB, '    fn adds() {', '    fn adds_two() {'));
  clean(libRs, edit(libRs, LIB, '    #[test]', '    #[tokio::test]'));
  const doc = { 'docs/testing.md': 'Unit tests carry #[test].\nThey run in CI.\n' };
  clean(doc, edit(doc, 'docs/testing.md', 'Unit tests carry #[test].\n', ''));
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
  reports(plain, edit(plain, LIB, fn, fn.split('\n').map((l) => (l ? '    // ' + l.trim() : l)).join('\n')), 'test-removed ' + LIB);
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
| jq | 1.6 | minimum | system package |
| python3 | 3.10 | minimum | system package |
| actionlint | 1.7.12 | exact | go install |
| zizmor | 1.32.0 | exact | uv |
| shellcheck | 0.8 | minimum | system package |

## Exceptions
`;
// The release scripts' tests check.sh's fast stage runs (when scripts/ or
// .github/ changed, or nothing did), as empty scripts: their own content
// is tested by running them, not here.
const RELEASE_TESTS = Object.fromEntries(['scripts/release.test.sh',
  ...['commit_test.sh', 'download_test.sh', 'publish_test.sh', 'isolation_test.py']
    .map((f) => '.github/actions/update-saltbox-python/' + f)].map((f) => [f, '#!/bin/sh\n']));
const VERSIONS = {
  'cargo-llvm-cov': '0.9.1', 'diff-cover': '9.7.2', 'cargo-mutants': '27.1.0', 'cargo-nextest': '0.9.151',
  'cargo-deny': '0.20.2', hyperfine: '2.0.0', gitleaks: '8.30.1', go: '1.27.3', node: '24.20.0', script: '2.39.3', cross: '0.2.5',
  docker: '29.7.2',
  jq: '1.7.1', python3: '3.12.3',
  actionlint: '1.7.12', zizmor: '1.32.0', shellcheck: '0.8.0',
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
const SYSTEM_TOOLS = ['awk', 'basename', 'bash', 'cat', 'chmod', 'dirname', 'env', 'find', 'getconf', 'git', 'grep', 'id', 'mkdir',
  'ip', 'mktemp', 'mount', 'rm', 'sed', 'setsid', 'sha256sum', 'sleep', 'sort', 'stat', 'truncate', 'unshare', 'xargs'];
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
    // The binary of the --target asked for.
    cross: 't=x86_64-unknown-linux-gnu; p=; for a in "$@"; do [ "$p" = --target ] && t="$a"; p="$a"; done\n'
      + 'mkdir -p "$CARGO_TARGET_DIR/$t/production"\n'
      + 'printf sb >"$CARGO_TARGET_DIR/$t/production/sb"\n',
    'diff-cover': version('diff-cover ' + versions['diff-cover']),
    node: version('v' + versions.node),
    script: version('script from util-linux ' + versions.script),
    docker: version('Docker version ' + versions.docker + ', build a7dcaa6'),
    jq: version('jq-' + versions.jq),
    python3: version('Python ' + versions.python3),
    // The workflows' linters: a finding fails each, with zizmor's own
    // status for a high one (14), when WORKFLOW_FINDING names it.
    actionlint: version('v' + versions.actionlint)
      + 'if [ "${WORKFLOW_FINDING:-}" = actionlint ]; then echo ".github/workflows/ci.yml:1:1: finding [syntax-check]"; exit 1; fi\n',
    zizmor: version('zizmor ' + versions.zizmor)
      + 'if [ "${WORKFLOW_FINDING:-}" = zizmor ]; then echo "error[unpinned-uses]: unpinned action reference"; exit 14; fi\n',
    // Its first line names the tool only.
    shellcheck: 'if [ "$1" = --version ]; then printf \'ShellCheck - shell script analysis tool\\nversion: %s\\nlicense: GPLv3\\n\' "'
      + versions.shellcheck + '"; exit 0; fi\n',
    go: 'if [ "$1 $2" = "version -m" ]; then printf \'%s: go1.27.3\\n\\tpath\\tgithub.com/zricethezav/gitleaks/v8\\n'
      + '\\tmod\\tgithub.com/zricethezav/gitleaks/v8\\tv' + versions.gitleaks + '\\th1:x=\\n\' "$3"; exit 0; fi\n'
      + 'echo "go version go' + versions.go + ' linux/amd64"\n',
    gitleaks: 'cat >"$STUB_LOG.gitleaks"\n',
    uname: 'echo ' + arch + '\n',
  };
  // cross's container caps, which a scope around check.sh doesn't reach.
  out.cross += 'printf \'%s\\n\' "${CROSS_CONTAINER_OPTS-unset}" >>"$STUB_LOG.cross-opts"\n';
  // With SIZE_BYTES set, the production binary is that many bytes (ADR-025).
  out.cross += 'if [ -n "${SIZE_BYTES:-}" ] && [ "$t" = x86_64-unknown-linux-gnu ]; then truncate -s "$SIZE_BYTES" "$CARGO_TARGET_DIR/$t/production/sb"; fi\n';
  // sb's normal dependency graph (`cargo tree ... -e normal,features`):
  // rustls's prefer-post-quantum on (ADR-027's amendment), unless NO_PQ is
  // set.
  out.cargo += 'if [ "$1" = tree ] && [ -z "${NO_PQ:-}" ]; then echo \'rustls feature "prefer-post-quantum"\'; fi\n';
  // The coverage run's lcov report (--output-path): a file under crates/
  // with COV_LF lines, COV_LH of them hit, COV_FNF functions and COV_FNH
  // hit (99% of each by default), then a file outside crates/ with nothing
  // hit, which the project floors must not count (ADR-025). With COV_NONE
  // set, only the file outside crates/.
  out.cargo += 'if [ "$1" = llvm-cov ]; then\n'
    + '  o=; for a in "$@"; do [ "$o" = next ] && o="$a"; [ "$a" = --output-path ] && o=next; done\n'
    + '  if [ -n "$o" ] && [ "$o" != next ]; then\n'
    + '    : >"$o"\n'
    + '    if [ -z "${COV_NONE:-}" ]; then printf \'SF:%s/crates/foo/src/lib.rs\\nFNF:%s\\nFNH:%s\\nLF:%s\\nLH:%s\\nend_of_record\\n\' '
    + '"$PWD" "${COV_FNF:-100}" "${COV_FNH:-99}" "${COV_LF:-1000}" "${COV_LH:-990}" >>"$o"; fi\n'
    + '    printf \'SF:%s/tools/p/src/lib.rs\\nFNF:50\\nFNH:0\\nLF:500\\nLH:0\\nend_of_record\\n\' "$PWD" >>"$o"\n'
    + '  fi\n'
    + 'fi\n';
  // The NEXTEST_* and CARGO_MUTANTS_* variables cargo (cargo-mutants, so
  // nextest) is run with.
  out.cargo += 'env | grep -E \'^(NEXTEST|CARGO_MUTANTS)_\' >>"$STUB_LOG.nextest-env" || true\n';
  // With LEAK_PIDS set, cargo-mutants lists one mutant in the wide pass
  // (sb-foo's), and its run leaves a process behind as a signal-ignoring
  // mutant of sb does: in its own session (as under `script`), ignoring
  // SIGHUP, SIGINT and SIGTERM, its executable in the run's temp copy
  // (under TMPDIR, deleted at the end).
  // A decoy outside that copy is started beside it, then a helper shell's
  // stand-in. Their pids (leak, decoy, shell) go to LEAK_PIDS, the TMPDIR
  // to LEAK_PIDS.tmpdir.
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
    // A helper shell's stand-in, third: its executable outside the copy,
    // its working directory in it (exec's tests leave such shells).
    + '  ( cd "$copy" && $LEAK_SETSID /bin/sh -c \'trap "" HUP INT TERM; echo $$ >>"$1"; exec "$0" 600\' "$LEAK_SLEEP" "$LEAK_PIDS" </dev/null >/dev/null 2>&1 & )\n'
    + '  n=0; while [ "$($LEAK_WC -l <"$LEAK_PIDS")" -lt 3 ] && [ $((n += 1)) -lt 100 ]; do $LEAK_SLEEP 0.05; done\n'
    + '  $LEAK_SLEEP 0.2\n'
    + '  $LEAK_RM -rf "$copy"\n'
    + '  while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\n'
    + '  $LEAK_MKDIR -p "$out/mutants.out"\n'
    + '  echo \'{"caught":1,"missed":0,"timeout":0}\' >"$out/mutants.out/outcomes.json"\n'
    + '  exit 0\n'
    + 'fi\n';
  // With LEAK_DURING set, the same mutant's run starts four processes
  // and, while it still runs, waits up to 20 s for the orphan and the
  // shell to go: an orphan of the run's temp copy (its parent init, in its
  // own session, ignoring SIGHUP, SIGINT and SIGTERM), a decoy outside that
  // copy, orphaned too, and an orphaned shell's stand-in (its executable
  // outside the copy, its working directory in it). Their pids go to
  // LEAK_DURING in that order. A live child of the copy,
  // its parent this run, stands for a test's sb. What is still running
  // when the wait ends goes to LEAK_DURING.result.
  out.cargo += 'if [ "$1" = mutants ] && [ -n "${LEAK_DURING:-}" ]; then\n'
    + '  case " $* " in *" --list "*) case " $* " in *" -p sb-foo "*) echo "crates/foo/src/lib.rs:1:1: replace f with ()" ;; esac; exit 0 ;; esac\n'
    + '  copy="${TMPDIR:-/tmp}/cargo-mutants-repo-stub.tmp"; $LEAK_MKDIR -p "$copy/target/debug" "$LEAK_DECOY_DIR"\n'
    + '  $LEAK_CP "$LEAK_SLEEP" "$copy/target/debug/sb"; $LEAK_CP "$LEAK_SLEEP" "$LEAK_DECOY_DIR/sb"\n'
    + '  "$copy/target/debug/sb" 600 </dev/null >/dev/null 2>&1 &\n'
    + '  live=$!\n'
    + '  k=0\n'
    + '  for exe in "$copy/target/debug/sb" "$LEAK_DECOY_DIR/sb"; do\n'
    + '    k=$((k + 1))\n'
    // The subshell ends at once, so init adopts the process.
    + '    ( $LEAK_SETSID /bin/sh -c \'trap "" HUP INT TERM; echo $$ >>"$1"; exec "$0" 600\' "$exe" "$LEAK_DURING" </dev/null >/dev/null 2>&1 & )\n'
    + '    n=0; while [ "$($LEAK_WC -l <"$LEAK_DURING")" -lt "$k" ] && [ $((n += 1)) -lt 100 ]; do $LEAK_SLEEP 0.05; done\n'
    + '  done\n'
    // A helper shell's stand-in, orphaned too: its executable outside the
    // copy, its working directory in it.
    + '  ( cd "$copy" && $LEAK_SETSID /bin/sh -c \'trap "" HUP INT TERM; echo $$ >>"$1"; exec "$0" 600\' "$LEAK_SLEEP" "$LEAK_DURING" </dev/null >/dev/null 2>&1 & )\n'
    + '  n=0; while [ "$($LEAK_WC -l <"$LEAK_DURING")" -lt 3 ] && [ $((n += 1)) -lt 100 ]; do $LEAK_SLEEP 0.05; done\n'
    + '  read -r orphan <"$LEAK_DURING"\n'
    + '  { read -r _; read -r _; read -r shell; } <"$LEAK_DURING"\n'
    // A process is running while its executable link can be read; a
    // zombie's can't.
    + '  n=0; while { $LEAK_READLINK "/proc/$orphan/exe" || $LEAK_READLINK "/proc/$shell/exe"; } >/dev/null 2>&1 && [ $((n += 1)) -lt 200 ]; do $LEAK_SLEEP 0.1; done\n'
    + '  for p in "orphan $orphan" "live $live" "shell $shell"; do\n'
    + '    if $LEAK_READLINK "/proc/${p#* }/exe" >/dev/null 2>&1; then echo "${p% *}" >>"$LEAK_DURING.result"; fi\n'
    + '  done\n'
    + '  kill -KILL "$live"; wait "$live" || true\n'
    + '  $LEAK_RM -rf "$copy"\n'
    + '  while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\n'
    + '  $LEAK_MKDIR -p "$out/mutants.out"\n'
    + '  echo \'{"caught":1,"missed":0,"timeout":0}\' >"$out/mutants.out/outcomes.json"\n'
    + '  exit 0\n'
    + 'fi\n';
  // With LEAK_KILLED set, the same mutant's run leaves a helper shell's
  // stand-in (in its own session, ignoring SIGHUP, SIGINT and SIGTERM, its
  // working directory in the run's temp copy) and then SIGKILLs check.sh,
  // its parent, so that no EXIT trap runs. With LEAK_KILL_SWEEPER (pgrep)
  // set, check.sh's other children (the pass's sweeper) go first, as when
  // a gate run's whole tree is killed. Then, as cargo-mutants runs on
  // after check.sh, it waits (but for LEAK_KILL_SWEEPER, which leaves
  // nothing to wait for) up to 20 s for the stand-in and the copy to go
  // and writes what is still there (shell, copy) to LEAK_KILLED.result.
  // The stand-in's pid goes to LEAK_KILLED, the TMPDIR to
  // LEAK_KILLED.tmpdir.
  out.cargo += 'if [ "$1" = mutants ] && [ -n "${LEAK_KILLED:-}" ]; then\n'
    + '  case " $* " in *" --list "*) case " $* " in *" -p sb-foo "*) echo "crates/foo/src/lib.rs:1:1: replace f with ()" ;; esac; exit 0 ;; esac\n'
    + '  echo "${TMPDIR-unset}" >"$LEAK_KILLED.tmpdir"\n'
    + '  copy="${TMPDIR:-/tmp}/cargo-mutants-repo-stub.tmp"; $LEAK_MKDIR -p "$copy"\n'
    + '  ( cd "$copy" && $LEAK_SETSID /bin/sh -c \'trap "" HUP INT TERM; echo $$ >>"$1"; exec "$0" 600\' "$LEAK_SLEEP" "$LEAK_KILLED" </dev/null >/dev/null 2>&1 & )\n'
    + '  n=0; while [ "$($LEAK_WC -l <"$LEAK_KILLED")" -lt 1 ] && [ $((n += 1)) -lt 100 ]; do $LEAK_SLEEP 0.05; done\n'
    + '  if [ -n "${LEAK_KILL_SWEEPER:-}" ]; then for p in $($LEAK_KILL_SWEEPER -P "$PPID"); do [ "$p" = $$ ] || kill -KILL "$p"; done; fi\n'
    + '  kill -KILL "$PPID"\n'
    + '  read -r shell <"$LEAK_KILLED"\n'
    + '  n=0; [ -n "${LEAK_KILL_SWEEPER:-}" ] && n=200\n'
    + '  while { $LEAK_READLINK "/proc/$shell/exe" >/dev/null 2>&1 || [ -d "$copy" ]; } && [ $((n += 1)) -lt 200 ]; do $LEAK_SLEEP 0.1; done\n'
    + '  if $LEAK_READLINK "/proc/$shell/exe" >/dev/null 2>&1; then echo shell >>"$LEAK_KILLED.result"; fi\n'
    + '  if [ -d "$copy" ]; then echo copy >>"$LEAK_KILLED.result"; fi\n'
    + '  exit 0\n'
    + 'fi\n';
  // With LEAK_STUCK set, the same mutant's run starts a spawner outside
  // the copy that, for up to a minute, starts a process in the copy every
  // 50 ms, so that check.sh's sweep after the pass can't end them all in
  // its 10 s. The spawner's pid, then each process's, go to LEAK_STUCK,
  // the TMPDIR to LEAK_STUCK.tmpdir.
  out.cargo += 'if [ "$1" = mutants ] && [ -n "${LEAK_STUCK:-}" ]; then\n'
    + '  case " $* " in *" --list "*) case " $* " in *" -p sb-foo "*) echo "crates/foo/src/lib.rs:1:1: replace f with ()" ;; esac; exit 0 ;; esac\n'
    + '  echo "${TMPDIR-unset}" >"$LEAK_STUCK.tmpdir"\n'
    + '  copy="${TMPDIR:-/tmp}/cargo-mutants-repo-stub.tmp"; $LEAK_MKDIR -p "$copy"\n'
    + '  ( i=0; while [ $((i += 1)) -le 1200 ]; do\n'
    + '      ( cd "$copy" && exec /bin/sh -c \'echo $$ >>"$1"; exec "$0" 600\' "$LEAK_SLEEP" "$LEAK_STUCK" ) </dev/null >/dev/null 2>&1 &\n'
    + '      $LEAK_SLEEP 0.05\n'
    + '    done ) </dev/null >/dev/null 2>&1 &\n'
    + '  echo $! >>"$LEAK_STUCK"\n'
    + '  while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\n'
    + '  $LEAK_MKDIR -p "$out/mutants.out"\n'
    + '  echo \'{"caught":1,"missed":0,"timeout":0}\' >"$out/mutants.out/outcomes.json"\n'
    + '  exit 0\n'
    + 'fi\n';
  // With SANDBOX_PROBE set (a directory outside /tmp, on the host), the
  // same mutant's run writes as a mutant that bends a path would: a file
  // in SANDBOX_PROBE, then one in its TMPDIR and one in its output
  // directory, as the pass itself does. Each write's result (`host ok` or
  // `host refused`, and so on) goes to SANDBOX_RESULT; its mount table,
  // its network namespace and what /run holds go beside it.
  out.cargo += 'if [ "$1" = mutants ] && [ -n "${SANDBOX_PROBE:-}" ]; then\n'
    + '  case " $* " in *" --list "*) case " $* " in *" -p sb-foo "*) echo "crates/foo/src/lib.rs:1:1: replace f with ()" ;; esac; exit 0 ;; esac\n'
    + '  while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\n'
    + '  $SANDBOX_MKDIR -p "$out/mutants.out"\n'
    + '  for w in "host $SANDBOX_PROBE/written" "tmpdir ${TMPDIR:-/tmp}/written" "output $out/mutants.out/written"; do\n'
    + '    if (echo mutant >"${w#* }") 2>/dev/null; then echo "${w%% *} ok"; else echo "${w%% *} refused"; fi >>"$SANDBOX_RESULT"\n'
    + '  done\n'
    + '  cat /proc/self/mountinfo >"$SANDBOX_RESULT.mountinfo"\n'
    + '  $SANDBOX_READLINK /proc/self/ns/net >"$SANDBOX_RESULT.net"\n'
    + '  $SANDBOX_LS -A /run >"$SANDBOX_RESULT.run"\n'
    + '  echo \'{"caught":1,"missed":0,"timeout":0}\' >"$out/mutants.out/outcomes.json"\n'
    + '  exit 0\n'
    + 'fi\n';
  // With TEST_SANDBOX_PROBE set, each test run (the coverage run, the doc
  // tests, the release-build run) writes as a test that bends a path
  // would: a file in TEST_SANDBOX_PROBE (on the host) and TEST_SANDBOX_ETC,
  // then one in its TMPDIR and one in target/, and makes a directory in
  // /tmp (its private one: TEST_SANDBOX_TMP_CHILD names it), as bench's
  // Speedtest step does. Each result goes to TEST_SANDBOX_RESULT, as
  // `<subcommand> <place> ok|refused`; its mount table, its network and
  // PID namespaces, what /run holds and /tmp's mode and owner go beside
  // it, one file each per subcommand. With TEST_WRITES_ETC set, the
  // coverage run writes that file and fails if it can't, as a test would,
  // saying so.
  out.cargo += 'if [ -n "${TEST_SANDBOX_PROBE:-}" ]; then case "$1" in llvm-cov | test | nextest)\n'
    + '  for w in "host $TEST_SANDBOX_PROBE/written" "etc $TEST_SANDBOX_ETC" "tmpdir ${TMPDIR:-/tmp}/written" "target target/written"; do\n'
    + '    if (echo test >"${w#* }") 2>/dev/null; then echo "$1 ${w%% *} ok"; else echo "$1 ${w%% *} refused"; fi >>"$TEST_SANDBOX_RESULT"\n'
    + '  done\n'
    + '  if mkdir "/tmp/$TEST_SANDBOX_TMP_CHILD" 2>/dev/null; then echo "$1 tmp ok"; else echo "$1 tmp refused"; fi >>"$TEST_SANDBOX_RESULT"\n'
    + '  cat /proc/self/mountinfo >"$TEST_SANDBOX_RESULT.mountinfo.$1"\n'
    + '  $SANDBOX_READLINK /proc/self/ns/net >"$TEST_SANDBOX_RESULT.net.$1"\n'
    + '  $SANDBOX_READLINK /proc/self/ns/pid >"$TEST_SANDBOX_RESULT.pid.$1"\n'
    + '  $SANDBOX_LS -A /run >"$TEST_SANDBOX_RESULT.run.$1"\n'
    + '  $SANDBOX_STAT -c "%a %u" /tmp >"$TEST_SANDBOX_RESULT.tmpmode.$1" ;;\n'
    + 'esac; fi\n';
  out.cargo += 'if [ "$1" = llvm-cov ] && [ -n "${TEST_WRITES_ETC:-}" ]; then\n'
    + '  echo test >"$TEST_WRITES_ETC" || { echo "the test could not write $TEST_WRITES_ETC"; exit 101; }\n'
    + 'fi\n';
  // With SIGNALS_PROBE set, each test run writes the signals it starts
  // with ignored (/proc's SigIgn mask), as `<subcommand> <mask>`.
  out.cargo += 'if [ -n "${SIGNALS_PROBE:-}" ]; then case "$1" in llvm-cov | test | nextest)\n'
    + '  while read -r k v; do [ "$k" = SigIgn: ] && echo "$1 $v" >>"$SIGNALS_PROBE"; done </proc/$$/status ;;\n'
    + 'esac; fi\n';
  // With RUN_FAILS_AT set (`llvm-cov` or `nextest`), that test run fails
  // as a failing test makes cargo fail, with status 101.
  out.cargo += 'if [ -n "${RUN_FAILS_AT:-}" ] && [ "$1" = "$RUN_FAILS_AT" ]; then exit 101; fi\n';
  // With STOP_RUN set, the test run named by STOP_RUN_AT (`llvm-cov` or
  // `nextest`) starts a child (as cargo starts its test binaries), writes
  // STOP_RUN, the ready file checkRun's stopWhen waits for, and runs on:
  // the wrapper then stops check.sh by its pid alone, as a coverage pass
  // was stopped (2026-11-17). Both run `STOP_SLEEP 60 STOP_MARK` (sleep
  // sums its arguments), so the test finds them on the host by that mark,
  // with their output closed, so that neither holds the test's pipes.
  out.cargo += 'if [ -n "${STOP_RUN:-}" ] && [ "$1" = "$STOP_RUN_AT" ]; then\n'
    + '  $STOP_SLEEP 60 "$STOP_MARK" </dev/null >/dev/null 2>&1 &\n'
    + '  echo ready >"$STOP_RUN"\n'
    + '  exec $STOP_SLEEP 60 "$STOP_MARK" </dev/null >/dev/null 2>&1\n'
    + 'fi\n';
  // `docker run`: sb version, unless the container is of the release in
  // DOCKER_FAILS, which can't run the binary, or DOCKER_DOWN is set: docker
  // fails itself, with its own status 125.
  // With QEMU_FAILS set, the aarch64 binary's run under QEMU fails.
  out.docker += 'if [ -n "${DOCKER_DOWN:-}" ]; then echo "Cannot connect to the Docker daemon" >&2; exit 125; fi\n'
    + 'case " $* " in *" qemu-aarch64-static "*) [ -n "${QEMU_FAILS:-}" ] && { echo "qemu: exec format error" >&2; exit 1; } ;; esac\n'
    // `docker image inspect`: the gate's QEMU image is there only with
    // QEMU_IMAGE_PRESENT set.
    + 'if [ "$1 $2" = "image inspect" ]; then [ -n "${QEMU_IMAGE_PRESENT:-}" ]; exit; fi\n'
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
// logDir: the directory the scratch repository's target/test-logs (its
// $SB_LOG_DIR) links to, which check.sh's sandboxes keep writable: the
// stubs' log and any probe file a test names go there, so that a stub
// running in a sandbox can write them. A test that reads its probe files
// after the run names its own; else one in the run's scratch directory.
// stopWhen: a file a stub writes when it is ready to be stopped; check.sh
// then runs under a wrapper that waits for it, writes how many processes
// with STOP_MARK in their command line run (to stopWhen.count; STOP_PGREP
// and STOP_WC name pgrep and wc), and stops
// check.sh by its pid with SIGTERM, as an agent stops a gate (a stub can't
// do it from inside the test steps' PID namespace).
const checkRun = (stage, { files = {}, work = {}, commits = [], base = 'HEAD', versions = {}, arch = 'x86_64',
  moves = [], logDir = null, stopWhen = null,
  env: extra = {}, config = {} } = {}) => {
  const dir = mkdtempSync(join(tmpdir(), 'check-sh-'));
  try {
    const logs = logDir ?? join(dir, 'logs');
    mkdirSync(logs, { recursive: true });
    const bin = join(dir, 'stub-bin'), repo = join(dir, 'repo'), logFile = join(logs, 'stub.log');
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
    // TMPDIR is the run's own, unless a test names one: check.sh sweeps
    // the pass directories it finds there, and a stubbed pass makes its
    // own there.
    const tmp = join(dir, 'tmp');
    mkdirSync(tmp);
    const scratch = { ...env, HOME: dir, XDG_CONFIG_HOME: dir, PATH: bin + ':' + sys, STUB_LOG: logFile, TMPDIR: tmp, ...extra };
    const git = (...args) => execFileSync('git', args, { cwd: repo, env: scratch, stdio: 'pipe' });
    mkdirSync(join(repo, 'target'), { recursive: true });
    symlinkSync(logs, join(repo, 'target', 'test-logs'));
    git('init', '-q');
    write(repo, {
      'scripts/check.sh': readFileSync(join(SCRIPTS, 'check.sh'), 'utf8'),
      'build/qemu.Dockerfile': readFileSync(join(SCRIPTS, '../build/qemu.Dockerfile'), 'utf8'),
      'scripts/check-commit-messages.sh': readFileSync(join(SCRIPTS, 'check-commit-messages.sh'), 'utf8'),
      ...RELEASE_TESTS,
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
    const wrap = 'bash scripts/check.sh "$1" "$2" & p=$!; n=0\n'
      + 'while [ ! -e "$3" ] && [ $((n += 1)) -lt 1200 ]; do sleep 0.05; done\n'
      + '"$STOP_PGREP" -f -- "$STOP_MARK" | "$STOP_WC" -l >"$3.count"\n'
      + 'kill -TERM "$p"; wait "$p"\n';
    const r = stopWhen
      ? spawnSync('bash', ['-c', wrap, 'stop', stage, base, stopWhen], { cwd: repo, env: scratch, encoding: 'utf8' })
      : spawnSync('bash', ['scripts/check.sh', stage, base], { cwd: repo, env: scratch, encoding: 'utf8' });
    const calls = readFileSync(logFile, 'utf8').split('\n').filter(Boolean).map((l) => {
      // The arguments come last, so a | among them stays in them.
      const [tool, target, buildTarget, covTarget, ...args] = l.split('|');
      return { tool, target, args: args.join('|'), buildTarget, covTarget };
    });
    const read = (f) => { try { return readFileSync(f, 'utf8'); } catch { return null; } };
    return {
      status: r.status, signal: r.signal, output: r.stdout + r.stderr, calls, root: repo,
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
  for (const sub of ['fmt', 'clippy', 'test', 'llvm-cov', 'nextest', 'mutants', 'build', 'deny']) {
    assert.ok(cargo.some((c) => c.args.split(' ')[0] === sub), 'no cargo ' + sub + ' call:\n' + r.output);
  }
  for (const c of cargo) {
    assert.equal(c.target, c.args.startsWith('mutants') ? 'unset' : join(r.root, 'target'), 'cargo ' + c.args);
    assert.equal(c.buildTarget, 'unset', 'cargo ' + c.args);
    assert.equal(c.covTarget, 'unset', 'cargo ' + c.args);
    if (!c.args.startsWith('fmt')) assert.match(c.args, /(^| )(--locked|--cargo-arg=--locked)( |$)/, 'cargo ' + c.args);
  }
  // The production builds: x86_64, then aarch64 (arm64 ships too).
  const cross = r.calls.filter((c) => c.tool === 'cross');
  assert.equal(cross.length, 2, r.output);
  for (const [build, target] of cross.map((c, i) => [c, ['x86_64', 'aarch64'][i]])) {
    assert.equal(build.target, join(r.root, 'target/cross-gnu'));
    assert.match(build.args, /(^| )--locked( |$)/);
    assert.match(build.args, new RegExp('(^| )--target ' + target + '-unknown-linux-gnu( |$)'));
  }
  // A scope around check.sh doesn't reach cross's container: the caps go
  // to its `docker run`, for each build.
  assert.equal(r.crossOpts, '--memory 16g --cpus 8\n'.repeat(2), r.output);
});

// Each mutant's tests run under nextest's `mutants` profile, which ends a
// hung test (ADR-021), and no inherited NEXTEST_* or CARGO_MUTANTS_*
// variable overrides it, nor the release-build run's profile (the stub
// logs the variables of every cargo call).
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
// or working directory lies there is killed after the pass; nothing
// outside it is touched (ADR-021).
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
      work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir,
      env: { LEAK_PIDS: pids, LEAK_DECOY_DIR: join(dir, 'decoy'), LEAK_CP: tool('cp'), LEAK_MKDIR: tool('mkdir'),
        LEAK_RM: tool('rm'), LEAK_SETSID: tool('setsid'), LEAK_SLEEP: tool('sleep'), LEAK_WC: tool('wc'), TMPDIR: dir },
    });
    started = readFileSync(pids, 'utf8').split('\n').filter(Boolean).map(Number);
    passes(r, 'task');
    assert.equal(started.length, 3, r.output);
    const passTmp = readFileSync(pids + '.tmpdir', 'utf8').trim();
    assert.match(passTmp, new RegExp('^' + dir + '/sb-mutants-0\\.\\w+$'), r.output);
    assert.ok(!existsSync(passTmp), 'the pass directory is left: ' + passTmp);
    // A SIGKILLed process drops its executable link before it has fully
    // exited, so it is given up to 5 s to finish.
    const pause = new Int32Array(new SharedArrayBuffer(4));
    for (let i = 0; i < 50 && (alive(started[0]) || alive(started[2])); i++) Atomics.wait(pause, 0, 0, 100);
    assert.ok(!alive(started[0]), 'the leaked process ' + started[0] + ' is still running:\n' + r.output);
    assert.ok(!alive(started[2]), 'the leaked shell ' + started[2] + ' is still running:\n' + r.output);
    assert.ok(alive(started[1]), 'the decoy outside the pass was killed:\n' + r.output);
    assert.match(r.output, /mutants: killed 2 process/, r.output);
  } finally {
    for (const pid of started) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    rmSync(dir, { recursive: true, force: true });
  }
});

// A long pass leaked busy, signal-ignoring mutants while it ran, not just
// at its end (19 at once on 2026-11-13). A sweeper kills, every few
// seconds during the pass, the processes of its copy that init has
// adopted; a test's live sb, whose parent is not init, and anything
// outside the copy are left alone (ADR-021). It assumes init adopts an
// orphan, as it does for the gate as root or under a system scope; under a
// subreaper (a `systemd --user` session) the orphan is that process's and
// this test fails.
// Runs a stubbed mutation pass that orphans a process and a shell of its
// copy (and a decoy outside it) while it runs; AGE, if set, is SB_MUTATION_ORPHAN_AGE.
function leakDuringPass(age) {
  const dir = mkdtempSync(join(tmpdir(), 'leak-'));
  const pids = join(dir, 'pids');
  writeFileSync(pids, '');
  const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
  const alive = (pid) => {
    try { return !/^\d+ \(.*\) Z /.test(readFileSync('/proc/' + pid + '/stat', 'utf8')); } catch { return false; }
  };
  let started = [];
  try {
    const env = { LEAK_DURING: pids, LEAK_DECOY_DIR: join(dir, 'decoy'), LEAK_CP: tool('cp'), LEAK_MKDIR: tool('mkdir'),
      LEAK_READLINK: tool('readlink'), LEAK_RM: tool('rm'), LEAK_SETSID: tool('setsid'), LEAK_SLEEP: tool('sleep'),
      LEAK_WC: tool('wc'), TMPDIR: dir };
    // Empty is unset to check.sh, so the caller's value never leaks in.
    env.SB_MUTATION_ORPHAN_AGE = age ?? '';
    const r = checkRun('task', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir, env });
    started = readFileSync(pids, 'utf8').split('\n').filter(Boolean).map(Number);
    const left = existsSync(pids + '.result') ? readFileSync(pids + '.result', 'utf8') : '';
    return { r, started, left, decoyAlive: started.length > 1 && alive(started[1]) };
  } finally {
    for (const pid of started) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    rmSync(dir, { recursive: true, force: true });
  }
}

// A gate stopped with SIGTERM by its pid alone ends its coverage pass and
// its release-build test run, each with the processes it started: they
// run in a process group of their own, which the EXIT trap signals
// (in_group), where they ran on, adopted by init (2026-11-17).
for (const at of ['llvm-cov', 'nextest']) {
  test('check.sh: stopping the gate by its pid ends its ' + at + ' run and that run\'s children', () => {
    const dir = mkdtempSync(join(tmpdir(), 'stop-'));
    const file = join(dir, 'pids');
    const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
    const alive = (pid) => {
      try { return !/^\d+ \(.*\) Z /.test(readFileSync('/proc/' + pid + '/stat', 'utf8')); } catch { return false; }
    };
    // The processes with the mark in their command line, on the host.
    const mark = '0.000' + String(process.pid).padStart(7, '0') + at.length;
    const marked = () => readdirSync('/proc').filter((pid) => /^\d+$/.test(pid)).filter((pid) => {
      try { return readFileSync('/proc/' + pid + '/cmdline', 'utf8').split('\0').includes(mark) && alive(pid); } catch { return false; }
    }).map(Number);
    try {
      const r = checkRun('task', {
        work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' },
        logDir: dir, stopWhen: file,
        env: { STOP_RUN: file, STOP_RUN_AT: at, STOP_SLEEP: tool('sleep'), STOP_MARK: mark,
          STOP_PGREP: tool('pgrep'), STOP_WC: tool('wc'), TMPDIR: dir },
      });
      assert.equal(r.status, 143, 'check.sh did not end by the signal:\n' + r.output);
      assert.equal(readFileSync(file + '.count', 'utf8').trim(), '2', 'the run and its child were not running:\n' + r.output);
      // The trap's signal is sent as check.sh exits; give it a moment.
      for (let n = 0; n < 50 && marked().length; n++) execFileSync(tool('sleep'), ['0.1']);
      assert.deepEqual(marked(), [], 'the ' + at + ' run or its child outlived the gate:\n' + r.output);
    } finally {
      for (const pid of marked()) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
      rmSync(dir, { recursive: true, force: true });
    }
  });
}

// A failing test run fails the gate with its own status, through the
// run's own process group (in_group), and the steps after it don't run.
test('check.sh: a failing coverage or release-build run fails the gate with its status', () => {
  for (const [at, after] of [['llvm-cov', 'diff-cover'], ['nextest', 'mutants']]) {
    const r = checkRun('task', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, env: { RUN_FAILS_AT: at } });
    assert.equal(r.status, 101, at + ':\n' + r.output);
    assert.doesNotMatch(r.output, /check task: all passed/, r.output);
    // The runs, not the version checks before the stages.
    const ran = r.calls.filter((c) => !/(^|\s)(--version|install --list)$/.test(c.args))
      .map((c) => (c.tool === 'cargo' ? c.args.split(' ')[0] : c.tool));
    assert.ok(ran.includes(at), at + ' never ran:\n' + r.output);
    assert.ok(!ran.includes(after), after + ' ran after a failing ' + at + ':\n' + r.output);
  }
});

// Started with `&`, a run would ignore SIGINT and SIGQUIT (bash, without
// job control), and so would its tests: exec's test of a child that
// SIGINTs itself failed. And the sandbox's unshare --fork ignores SIGTERM
// around its fork: sb-ansible's test of a listing's SIGTERM trap failed
// in it (2026-11-17). Every test run starts with the three at their
// defaults.
test('check.sh: the test runs start with SIGINT, SIGQUIT and SIGTERM not ignored', () => {
  const dir = mkdtempSync(join(tmpdir(), 'signals-'));
  const file = join(dir, 'masks');
  try {
    passes(checkRun('task', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir, env: { SIGNALS_PROBE: file } }), 'task');
    const masks = readFileSync(file, 'utf8').trim().split('\n').map((l) => l.split(' '));
    assert.deepEqual(masks.map(([sub]) => sub), ['llvm-cov', 'test', 'nextest'], masks.join('\n'));
    // SIGINT is signal 2 (bit 1), SIGQUIT 3 (bit 2), SIGTERM 15 (bit 14):
    // the sandbox's unshare --fork ignores SIGTERM around its fork, which
    // a test's own SIGTERM trap must not inherit.
    for (const [sub, mask] of masks) assert.equal(BigInt('0x' + mask) & 0x4006n, 0n, sub + ' ignores ' + mask);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('check.sh: an orphan of a mutation pass is killed during it, a live child and a process outside are not', () => {
  const { r, started, left, decoyAlive } = leakDuringPass('0');
  passes(r, 'task');
  assert.equal(started.length, 3, r.output);
  assert.doesNotMatch(left, /orphan/, 'the orphan ran on until the pass ended:\n' + r.output);
  assert.doesNotMatch(left, /shell/, 'the orphaned shell ran on until the pass ended:\n' + r.output);
  assert.match(left, /^live$/m, 'the live child was killed during the pass:\n' + r.output);
  assert.ok(decoyAlive, 'the decoy outside the pass was killed:\n' + r.output);
  // The two may fall in different sweeps, each printing its own count.
  const killed = [...r.output.matchAll(/mutants: killed (\d+) orphaned process\(es\) during the pass/g)]
    .reduce((n, m) => n + Number(m[1]), 0);
  assert.equal(killed, 2, r.output);
});

// A test may orphan sb on purpose (sb's hangup tests kill its parent
// shell); such an orphan lives seconds, so by default only one older than
// any test (120 s) is killed during the pass. The sweep after the pass
// still ends it.
test('check.sh: a young orphan of a mutation pass is left running during it by default', () => {
  const { r, started, left } = leakDuringPass(undefined);
  passes(r, 'task');
  assert.equal(started.length, 3, r.output);
  assert.match(left, /^orphan$/m, 'a young orphan was killed during the pass:\n' + r.output);
  assert.match(left, /^shell$/m, 'a young orphaned shell was killed during the pass:\n' + r.output);
  assert.doesNotMatch(r.output, /during the pass/, r.output);
});

test('check.sh: an orphan age that is not a whole number of seconds is refused', () => {
  for (const age of ['2m', '08', '-1']) {
    refuses(checkRun('fast', { env: { SB_MUTATION_ORPHAN_AGE: age } }), 2, /SB_MUTATION_ORPHAN_AGE must be a whole number/);
  }
});

// A gate run killed during a mutation pass runs no EXIT trap, so nothing
// sweeps what the pass leaves (2026-11-14: 185 of exec's helper shells,
// from five passes, ran on for hours). A supervising tool stops a command
// so: SIGTERM, then SIGKILL about a second later, to every descendant,
// before check.sh's own sweep can finish; orphans already adopted by init
// are no descendants and get neither. Each pass directory names the
// check.sh that made it (its pid and start time), and the next run sweeps
// the pass directories whose check.sh has ended, then removes them
// (ADR-021).
test('check.sh: what the mutation pass of a killed run left is swept by the next run', () => {
  const dir = mkdtempSync(join(tmpdir(), 'leak-'));
  const pids = join(dir, 'pids');
  writeFileSync(pids, '');
  const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
  const alive = (pid) => {
    try { return !/^\d+ \(.*\) Z /.test(readFileSync('/proc/' + pid + '/stat', 'utf8')); } catch { return false; }
  };
  let started = [];
  try {
    const killed = checkRun('task', {
      work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir,
      env: { LEAK_KILLED: pids, LEAK_KILL_SWEEPER: tool('pgrep'), LEAK_MKDIR: tool('mkdir'), LEAK_READLINK: tool('readlink'),
        LEAK_SETSID: tool('setsid'), LEAK_SLEEP: tool('sleep'), LEAK_WC: tool('wc'), TMPDIR: dir },
    });
    started = readFileSync(pids, 'utf8').split('\n').filter(Boolean).map(Number);
    assert.equal(killed.signal, 'SIGKILL', killed.output);
    assert.equal(started.length, 1, killed.output);
    const passTmp = readFileSync(pids + '.tmpdir', 'utf8').trim();
    assert.ok(alive(started[0]) && existsSync(passTmp), 'the killed run left nothing to sweep:\n' + killed.output);
    const next = checkRun('fast', { env: { TMPDIR: dir } });
    passes(next, 'fast');
    const pause = new Int32Array(new SharedArrayBuffer(4));
    for (let i = 0; i < 50 && alive(started[0]); i++) Atomics.wait(pause, 0, 0, 100);
    assert.ok(!alive(started[0]), 'the leaked shell ' + started[0] + ' is still running:\n' + next.output);
    assert.ok(!existsSync(passTmp), 'the pass directory is left: ' + passTmp);
    assert.match(next.output, /mutants: killed 1 process\(es\) the pass left running/, next.output);
    assert.ok(next.output.includes('mutants: swept ' + passTmp + ', left by a run that ended during its pass'), next.output);
  } finally {
    for (const pid of started) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    rmSync(dir, { recursive: true, force: true });
  }
});

// check.sh SIGKILLed alone (its sweeper still running) leaves cargo-mutants
// running: init adopts it, and it goes on through the rest of the pass,
// building and testing mutants with nothing to sweep what they leave
// (seen 2026-11-15). The sweeper, finding check.sh gone, does the cleanup
// on exit itself: every process of the pass goes, and the copies with
// them (ADR-021).
test('check.sh: a run killed alone during a mutation pass is cleaned up by its sweeper', () => {
  const dir = mkdtempSync(join(tmpdir(), 'leak-'));
  const pids = join(dir, 'pids');
  writeFileSync(pids, '');
  const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
  let started = [];
  try {
    const r = checkRun('task', {
      work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir,
      env: { LEAK_KILLED: pids, LEAK_MKDIR: tool('mkdir'), LEAK_READLINK: tool('readlink'), LEAK_SETSID: tool('setsid'),
        LEAK_SLEEP: tool('sleep'), LEAK_WC: tool('wc'), TMPDIR: dir },
    });
    started = readFileSync(pids, 'utf8').split('\n').filter(Boolean).map(Number);
    assert.equal(r.signal, 'SIGKILL', r.output);
    assert.equal(started.length, 1, r.output);
    const left = existsSync(pids + '.result') ? readFileSync(pids + '.result', 'utf8') : '';
    assert.doesNotMatch(left, /shell/, 'the leaked shell ran on after check.sh was killed:\n' + r.output);
    assert.doesNotMatch(left, /copy/, 'the pass\'s copy is left after check.sh was killed:\n' + r.output);
    assert.ok(!existsSync(readFileSync(pids + '.tmpdir', 'utf8').trim()), 'the pass directory is left');
  } finally {
    for (const pid of started) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    rmSync(dir, { recursive: true, force: true });
  }
});

// The sweep at the start never touches another run's pass: one whose
// check.sh still runs, one that names no check.sh (made before pass
// directories named theirs, or another check.sh's), or a symbolic link is
// left as it is. A pid that now belongs to another process (its start
// time differs) is a check.sh that has ended.
test('check.sh: a pass directory whose check.sh still runs, that names none, or a link is left alone', () => {
  const dir = mkdtempSync(join(tmpdir(), 'leak-'));
  const startOf = (pid) => readFileSync('/proc/' + pid + '/stat', 'utf8').split(') ').pop().split(' ')[19];
  const alive = (pid) => {
    try { return !/^\d+ \(.*\) Z /.test(readFileSync('/proc/' + pid + '/stat', 'utf8')); } catch { return false; }
  };
  const ended = spawnSync('true').pid;
  const cases = {
    live: process.pid + ' ' + startOf(process.pid) + '\n',
    unmarked: null,
    reused: process.pid + ' ' + (Number(startOf(process.pid)) + 1) + '\n',
    ended: ended + ' 1\n',
    // A link to a directory that would be swept: the link is left alone.
    link: ended + ' 1\n',
  };
  const children = {};
  try {
    for (const [name, owner] of Object.entries(cases)) {
      const pass = join(dir, name === 'link' ? 'linked' : 'sb-mutants-0.' + name);
      mkdirSync(join(pass, 'copy'), { recursive: true });
      if (name === 'link') symlinkSync(pass, join(dir, 'sb-mutants-0.link'));
      if (owner !== null) writeFileSync(join(pass, 'check-sh.pid'), owner);
      // A process of that pass: its working directory in the pass's copy.
      const child = spawn('sleep', ['600'], { cwd: join(pass, 'copy'), detached: true, stdio: 'ignore' });
      child.unref();
      children[name] = child.pid;
    }
    const r = checkRun('fast', { env: { TMPDIR: dir } });
    passes(r, 'fast');
    const pause = new Int32Array(new SharedArrayBuffer(4));
    for (let i = 0; i < 50 && (alive(children.reused) || alive(children.ended)); i++) Atomics.wait(pause, 0, 0, 100);
    for (const name of ['live', 'unmarked', 'link']) {
      assert.ok(alive(children[name]), 'the ' + name + ' pass\'s process was killed:\n' + r.output);
      assert.ok(existsSync(join(dir, 'sb-mutants-0.' + name)), 'the ' + name + ' pass directory was removed');
    }
    assert.ok(existsSync(join(dir, 'linked')), 'the directory the link names was removed');
    for (const name of ['reused', 'ended']) {
      assert.ok(!alive(children[name]), 'the ' + name + ' pass\'s process is still running:\n' + r.output);
      assert.ok(!existsSync(join(dir, 'sb-mutants-0.' + name)), 'the ' + name + ' pass directory is left');
    }
  } finally {
    for (const pid of Object.values(children)) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    rmSync(dir, { recursive: true, force: true });
  }
});

// A pass directory whose leftovers the sweep after the pass could not end
// is kept for a look, and check.sh exits 2. It names no check.sh then, so
// the next run's sweep leaves it, and a shell opened in it, alone.
test('check.sh: a pass directory kept for a look names no check.sh', () => {
  const dir = mkdtempSync(join(tmpdir(), 'leak-'));
  const pids = join(dir, 'pids');
  writeFileSync(pids, '');
  const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
  const read = () => readFileSync(pids, 'utf8').split('\n').filter(Boolean).map(Number);
  try {
    const r = checkRun('task', {
      work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir,
      env: { LEAK_STUCK: pids, LEAK_MKDIR: tool('mkdir'), LEAK_SLEEP: tool('sleep'), TMPDIR: dir },
    });
    refuses(r, 2, /check: processes left under /);
    const passTmp = readFileSync(pids + '.tmpdir', 'utf8').trim();
    assert.ok(existsSync(passTmp), 'the pass directory was not kept: ' + passTmp);
    assert.ok(!existsSync(join(passTmp, 'check-sh.pid')), 'the kept pass directory still names its check.sh');
  } finally {
    // The spawner first, then everything it started.
    for (const pid of read()) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    const pause = new Int32Array(new SharedArrayBuffer(4));
    Atomics.wait(pause, 0, 0, 500);
    for (const pid of read()) { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
    rmSync(dir, { recursive: true, force: true });
  }
});

// The production binary is built on the oldest supported release, and the
// gate runs it on each release of the host spec's runtime list (ADR-020).
test('check.sh full: sb version runs in a container of each supported release, the built binary mounted', () => {
  const r = checkRun('full', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
  passes(r, 'full');
  // The runs of an ubuntu:<release> image (the QEMU run has its own).
  const runs = r.calls.filter((c) => c.tool === 'docker' && c.args.startsWith('run ') && c.args.includes(' ubuntu:'));
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

// arm64 ships: full builds its production binary in the same image and
// runs its sb version under QEMU user-mode, with no binfmt handler.
test('check.sh full: the aarch64 production build runs under QEMU', () => {
  const r = checkRun('full', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
  passes(r, 'full');
  const builds = r.calls.filter((c) => c.tool === 'cross').map((c) => c.args);
  assert.ok(builds.some((a) => / --profile production .*--target aarch64-unknown-linux-gnu/.test(a)), builds.join('\n'));
  assert.ok(builds.some((a) => /--target x86_64-unknown-linux-gnu/.test(a)), builds.join('\n'));
  const qemu = r.calls.filter((c) => c.tool === 'docker' && c.args.includes('qemu-aarch64-static'));
  assert.equal(qemu.length, 1, r.output);
  const bin = join(r.root, 'target/cross-gnu/aarch64-unknown-linux-gnu/production/sb');
  assert.ok(qemu[0].args.includes(' -v ' + bin + ':/sb:ro '), qemu[0].args);
  assert.match(qemu[0].args, /(^| )--network none( |$)/);
  // The gate's own image, named by its Dockerfile's hash, built when
  // missing and only then.
  const dockerfile = readFileSync(join(SCRIPTS, '../build/qemu.Dockerfile'));
  const image = 'sb-rust-gate/qemu-check:' + createHash('sha256').update(dockerfile).digest('hex').slice(0, 12);
  assert.ok(qemu[0].args.includes(' ' + image + ' qemu-aarch64-static -L /usr/aarch64-linux-gnu /sb version'), qemu[0].args);
  const imageBuilds = (run) => run.calls.filter((c) => c.tool === 'docker' && c.args.startsWith('build '));
  assert.deepEqual(imageBuilds(r).map((c) => c.args), ['build --quiet --tag ' + image + ' --file build/qemu.Dockerfile build'], r.output);
  assert.match(r.output, /aarch64 \(QEMU\): Saltbox CLI version/, r.output);
  const cached = checkRun('full', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, env: { QEMU_IMAGE_PRESENT: '1' } });
  passes(cached, 'full');
  assert.deepEqual(imageBuilds(cached), [], cached.output);
});

test('check.sh full: an aarch64 binary that does not run under QEMU fails the gate', () => {
  refuses(checkRun('full', { env: { QEMU_FAILS: '1' } }), 1, /aarch64 production binary doesn.t run under QEMU/);
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

// The ratchet (ADR-025): full enforces the project's coverage floors over
// crates/ from the coverage run's lcov report, and the binary's size
// ceiling, each with its tolerance; the figures are printed every run.
const checkConst = (name) => {
  const m = new RegExp('^' + name + '=(\\S+)', 'm').exec(readFileSync(join(SCRIPTS, 'check.sh'), 'utf8'));
  assert.ok(m, name + ' in check.sh');
  return Number(m[1]);
};
const crateWork = { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } };
// Hits out of 10000 at a percentage, rounded to the nearest hundredth.
const hits = (pct) => String(Math.round(pct * 100));
test('check.sh full: the project coverage over crates/ is printed, and a file outside crates/ is not counted', () => {
  const r = checkRun('full', crateWork);
  passes(r, 'full');
  assert.match(r.output, /^lines: 99\.000% \(990 of 1000\), floor [\d.]+%, failing below [\d.]+%$/m, r.output);
  assert.match(r.output, /^functions: 99\.000% \(99 of 100\), floor [\d.]+%, failing below [\d.]+%$/m, r.output);
});

test('check.sh full: coverage at a floor less its tolerance passes, below it fails; task leaves it to full', () => {
  const tol = checkConst('PROJECT_COVERAGE_TOLERANCE_MAX');
  for (const [lf, lh, fnf, fnh] of [['COV_LF', 'COV_LH', 'COV_FNF', 'COV_FNH'], ['COV_FNF', 'COV_FNH', 'COV_LF', 'COV_LH']]) {
    const floor = checkConst(lf === 'COV_LF' ? 'PROJECT_LINES_MIN' : 'PROJECT_FUNCTIONS_MIN');
    const at = { [lf]: '10000', [lh]: hits(floor - tol), [fnf]: '100', [fnh]: '100' };
    passes(checkRun('full', { ...crateWork, env: at }), 'full');
    const below = { ...at, [lh]: String(Number(hits(floor - tol)) - 1) };
    const [name, other] = lf === 'COV_LF' ? ['lines', 'functions'] : ['functions', 'lines'];
    const failed = checkRun('full', { ...crateWork, env: below });
    refuses(failed, 1, new RegExp('^' + name + ': .*: below$', 'm'));
    assert.match(failed.output, new RegExp('^' + other + ': .*%$', 'm'), failed.output);
    passes(checkRun('task', { ...crateWork, env: below }), 'task');
  }
});

test('check.sh full: a coverage report with nothing under crates/ means the check could not run', () => {
  refuses(checkRun('full', { ...crateWork, env: { COV_NONE: '1' } }), 2, /nothing under crates\//);
});

test('check.sh full: the binary at its ratchet ceiling plus the tolerance passes, a byte more fails', () => {
  const today = checkConst('SIZE_RATCHET_MAX_BYTES'), pct = checkConst('SIZE_TOLERANCE_MAX_PCT');
  const ceiling = today + Math.floor(today * pct / 100);
  const at = checkRun('full', { ...crateWork, env: { SIZE_BYTES: String(ceiling) } });
  passes(at, 'full');
  assert.match(at.output, new RegExp('sb: ' + ceiling + ' bytes'), at.output);
  assert.match(at.output, new RegExp('at most ' + ceiling + ' bytes'), at.output);
  refuses(checkRun('full', { ...crateWork, env: { SIZE_BYTES: String(ceiling + 1) } }), 1, /above the ratchet/);
});

test('check.sh full: sb\'s normal build without rustls\'s prefer-post-quantum fails', () => {
  refuses(checkRun('full', { ...crateWork, env: { NO_PQ: '1' } }), 1, /lacks rustls's prefer-post-quantum/);
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

test('check-commit-messages.sh: a Floor-approved line names a rule, a file, a reason and the maintainer\'s date', () => {
  const ok = 'Floor-approved: threshold-loosened scripts/check.sh — the floor the tree measures (the maintainer, 2026-11-24)';
  accepted(['build: lower it\n\n' + ok, 'build: lower two\n\n' + ok + '\n' + ok.replace('scripts/check.sh', 'deny.toml')]);
  for (const bad of [ok.replace(' (the maintainer, 2026-11-24)', ''), ok.replace('the maintainer', 'an agent'),
    ok.replace(' — ', ' - '), ok.replace('2026-11-24', '24.11.2026'), 'Floor-approved: threshold-loosened',
    '  ' + ok]) {
    rejected(['build: lower it\n\n' + bad], /Malformed 'Floor-approved:' line/);
  }
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
    for (const sub of ['llvm-cov', 'test', 'nextest', 'build', 'deny']) assert.ok(cargoSubs(r).includes(sub), sub + '\n' + r.output);
    // The two production builds, x86_64 and aarch64.
    assert.equal(r.calls.filter((c) => c.tool === 'cross').length, 2, r.output);
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

const workflowLints = (r) => r.calls.filter((c) => ['actionlint', 'zizmor'].includes(c.tool) && c.args !== '--version')
  .map((c) => (c.tool + ' ' + c.args).trim());

test('check.sh: the workflows\' linters run at fast when .github/ changed or nothing did, at full always, and are skipped otherwise', () => {
  const both = ['actionlint', 'zizmor --offline --persona=auditor --no-progress .github/'];
  for (const [stage, work] of [['fast', {}], ['fast', { '.github/workflows/x.yml': 'name: x\n' }],
    ['task', { '.github/actionlint.yaml': 'paths: {}\n' }], ['full', { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }],
    ['full', { 'docs/x.md': 'x\n' }]]) {
    const r = checkRun(stage, { work });
    passes(r, stage);
    assert.deepEqual(workflowLints(r), both, stage + ' ' + Object.keys(work) + '\n' + r.output);
  }
  for (const [stage, work] of [['fast', { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }], ['fast', { 'scripts/new.sh': 'true\n' }],
    ['task', { 'docs/x.md': 'x\n' }]]) {
    const r = checkRun(stage, { work });
    passes(r, stage);
    assert.deepEqual(workflowLints(r), [], r.output);
    assert.match(r.output, /skipped: nothing under \.github\/ changed/, r.output);
  }
});

test('check.sh: a finding from actionlint or zizmor fails the gate with its status', () => {
  refuses(checkRun('fast', { env: { WORKFLOW_FINDING: 'actionlint' } }), 1, /\[syntax-check\]/);
  const r = checkRun('full', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, env: { WORKFLOW_FINDING: 'zizmor' } });
  refuses(r, 14, /unpinned-uses/);
  // It fails before the stage's tests.
  assert.ok(!cargoSubs(r).includes('llvm-cov'), r.output);
});

test('check.sh: the workflows\' linters are held to the Tools table when their step runs, shellcheck to a minimum', () => {
  refuses(checkRun('fast', { versions: { actionlint: '1.7.11' } }), 2, /actionlint is 1\.7\.11/);
  refuses(checkRun('fast', { versions: { zizmor: '1.33.1' } }), 2, /zizmor is 1\.33\.1/);
  refuses(checkRun('fast', { versions: { shellcheck: '0.7.2' } }), 2, /shellcheck is 0\.7\.2/);
  // actionlint skips its shellcheck rule when shellcheck is missing.
  refuses(checkRun('fast', { versions: { shellcheck: null } }), 2, /shellcheck not installed/);
  passes(checkRun('fast', { versions: { shellcheck: '0.11.0' } }), 'fast');
  // Not asked for when the step doesn't run.
  passes(checkRun('fast', { versions: { zizmor: null, actionlint: null, shellcheck: null },
    work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } }), 'fast');
});

test('check.sh task: the crates\' tests run once under coverage, then the doc tests, then the crates\' again in a release build', () => {
  const r = checkRun('task', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
  passes(r, 'task');
  const runs = r.calls.filter((c) => c.tool === 'cargo' && /^(llvm-cov|test|nextest) /.test(c.args)).map((c) => c.args);
  assert.equal(runs.length, 3, r.output);
  assert.match(runs[0], /^llvm-cov .*-p sb -p sb-foo /);
  assert.match(runs[1], /^test .*--doc .*-p sb-foo/);
  // The release build's run: the crates the coverage run tests, under
  // the `release-tests` cargo profile (its own directory) and nextest
  // profile (its per-test timeout, every failure reported).
  const crates = runs[0].match(/(?: -p \S+)+/)[0];
  assert.match(crates, / -p sb -p sb-foo /);
  assert.equal(runs[2], 'nextest run --locked --cargo-profile release-tests --profile release-tests' + crates);
  // No cargo test run but the doc tests, and none over the workspace.
  assert.ok(!r.calls.some((c) => c.tool === 'cargo' && /^test (?!.*--doc)/.test(c.args)), r.output);
});

// insta removes a stale pending snapshot when a test runs, which fails in
// the sandbox's read-only worktree; the gate names them before any test.
test('check.sh task: a stale insta .pending-snap or .snap.new under crates/ fails before the tests, naming it', () => {
  const ignore = { '.gitignore': '/target/\n*.pending-snap\n*.snap.new\n' };
  const stale = {
    'crates/foo/src/lib.rs': 'pub fn f() {}\n',
    'crates/foo/src/.lib.rs.pending-snap': '{}\n',
    'crates/foo/src/snapshots/foo__a.snap.new': '---\n',
  };
  const r = checkRun('task', { files: ignore, work: stale });
  refuses(r, 1, /crates\/foo\/src\/\.lib\.rs\.pending-snap/);
  assert.match(r.output, /crates\/foo\/src\/snapshots\/foo__a\.snap\.new/, r.output);
  assert.match(r.output, /read-only/, r.output);
  assert.match(r.output, /find crates -type f \\\( -name '\*\.pending-snap' -o -name '\*\.snap\.new' \\\) -delete/, r.output);
  assert.ok(!r.calls.some((c) => c.tool === 'cargo' && /^(llvm-cov|test|nextest) /.test(c.args)), r.output);
  // Without them the same change passes, and fast runs no tests to check.
  passes(checkRun('task', { files: ignore, work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } }), 'task');
  passes(checkRun('fast', { files: ignore, work: stale }), 'fast');
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

// The passes run tests as root, and a mutant that bends a path or drops a
// safety flag writes wherever the bent path points: on 2026-11-15 two
// mutants of the motd.yml migration wrote /nonexistent and
// /opt/saltbox/backups on the host. Each pass runs in private mount and
// network namespaces where every mount is read-only but the pass's
// directory (its copies and the tests' temp roots, its TMPDIR), the pass's
// output, the tests' log directory and /proc, /sys and /dev, /run is empty
// and only the namespace's loopback is up (ADR-021).
// A stub mutant's write to the host fails; the pass's own writes work.
// Only the pass's directories are writable, never /tmp as a whole, so the
// probe in this repository's target/ is read-only there wherever the
// repository is, /tmp included.
// The writable mounts a sandbox's processes can reach, from its mount table
// (proc(5): field 5 the mount point, octal escapes; field 6 its options):
// a mount under a later one (the same point, or one of its directories)
// is hidden and left out, as the sandbox's own check does. Each is named
// by what it is: `pass` (a mutation pass's directory), `run` (a test
// step's temp directory), `output` (a pass's output), `target` (a
// repository's target/), `logs` (a log directory: test-logs or a test's
// own), `proc` (the namespace's /proc), `pts`, `shm`, `mqueue` (/dev's
// terminals and the private tmpfs), `tmp` (the private /tmp), or the path
// itself: /sys, /dev or
// /proc/sys among them would fail the assertion.
const writableMounts = (file) => {
  const unescape = (p) => p.replace(/\\([0-7]{3})/g, (_, o) => String.fromCharCode(parseInt(o, 8)));
  const name = (m) => /\/sb-mutants-\d+\.[^/]+$/.test(m) ? 'pass' : /\/sb-check\.[^/]+$/.test(m) ? 'run'
    : /\/target\/mutants-run-\d+$/.test(m) ? 'output' : /\/repo\/target$/.test(m) ? 'target'
      : /\/(test-logs|logs|sandbox-[^/]+|test-sandbox-[^/]+)$/.test(m) ? 'logs'
        : m === '/proc' ? 'proc' : m === '/dev/pts' ? 'pts' : m === '/dev/shm' ? 'shm' : m === '/dev/mqueue' ? 'mqueue'
          : m === '/tmp' ? 'tmp' : m;
  const mounts = readFileSync(file, 'utf8').split('\n').filter(Boolean).map((l) => l.split(' '))
    .map((f) => ({ point: unescape(f[4]), rw: !f[5].split(',').includes('ro') }));
  const hidden = (i) => mounts.slice(i + 1).some(({ point }) =>
    point === mounts[i].point || point === '/' || mounts[i].point.startsWith(point + '/'));
  return mounts.filter((m, i) => m.rw && !hidden(i)).map((m) => name(m.point)).sort();
};

test("check.sh: a mutant can't write a host file or reach a host daemon, the pass's own writes work", () => {
  const dir = mkdtempSync(join(tmpdir(), 'sandbox-'));
  // On the host, outside /tmp: under the repository's target directory.
  mkdirSync(join(SCRIPTS, '..', 'target'), { recursive: true });
  const probe = mkdtempSync(join(SCRIPTS, '..', 'target', 'sandbox-probe-'));
  const result = join(dir, 'result');
  const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
  try {
    writeFileSync(join(probe, 'before'), 'writable outside the pass');
    const r = checkRun('task', {
      work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir,
      env: { SANDBOX_PROBE: probe, SANDBOX_RESULT: result, SANDBOX_MKDIR: tool('mkdir'), SANDBOX_READLINK: tool('readlink'),
        SANDBOX_LS: tool('ls') },
    });
    passes(r, 'task');
    assert.equal(readFileSync(result, 'utf8'), 'host refused\ntmpdir ok\noutput ok\n', r.output);
    assert.ok(!existsSync(join(probe, 'written')), 'the mutant wrote on the host');
    // Every mount is read-only but the pass's directory, its output and the
    // tests' log directory, the private /tmp, and /proc's, /sys's and /dev's.
    assert.deepEqual(writableMounts(result + '.mountinfo'),
      ['logs', 'mqueue', 'output', 'pass', 'proc', 'pts', 'shm', 'tmp'], r.output);
    // No host daemon is reachable: an empty /run, a network namespace of
    // its own.
    assert.equal(readFileSync(result + '.run', 'utf8'), '', r.output);
    assert.notEqual(readFileSync(result + '.net', 'utf8').trim(), execFileSync('readlink', ['/proc/self/ns/net'], { encoding: 'utf8' }).trim());
  } finally {
    rmSync(probe, { recursive: true, force: true });
    rmSync(dir, { recursive: true, force: true });
  }
});

// Every test run of the gate is in the sandbox (the maintainer,
// 2026-11-17, after a completion test wrote the host's /etc/fish): a
// stub test run's writes to the host (this repository's target/, outside
// the run's) and to /etc fail; its TMPDIR, the run's own, and its
// target/ work; /run is empty and the network its own. Wherever the
// repository is, /tmp included: only the run's directories and its
// private /tmp are writable; a child made in that /tmp is not the host's.
test("check.sh: a test can't write a host file or /etc, or reach a host daemon; its TMPDIR, target/ and a private /tmp work", () => {
  const dir = mkdtempSync(join(tmpdir(), 'test-sandbox-'));
  mkdirSync(join(SCRIPTS, '..', 'target'), { recursive: true });
  const probe = mkdtempSync(join(SCRIPTS, '..', 'target', 'test-sandbox-probe-'));
  const etc = '/etc/sb-gate-sandbox-probe-' + process.pid;
  // A child of /tmp (sticky, root's): the test's own, in its private
  // /tmp, never the host's.
  const tmpChild = 'sb-gate-sandbox-tmp-' + process.pid;
  const result = join(dir, 'result');
  const tool = (t) => execFileSync('bash', ['-c', 'command -v ' + t], { encoding: 'utf8' }).trim();
  try {
    const r = checkRun('task', {
      work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, logDir: dir,
      env: { TEST_SANDBOX_PROBE: probe, TEST_SANDBOX_ETC: etc, TEST_SANDBOX_RESULT: result,
        TEST_SANDBOX_TMP_CHILD: tmpChild, SANDBOX_READLINK: tool('readlink'), SANDBOX_LS: tool('ls'), SANDBOX_STAT: tool('stat') },
    });
    passes(r, 'task');
    const subs = ['llvm-cov', 'test', 'nextest'];
    assert.equal(readFileSync(result, 'utf8'),
      subs.map((sub) => [sub + ' host refused', sub + ' etc refused', sub + ' tmpdir ok', sub + ' target ok', sub + ' tmp ok'].join('\n') + '\n').join(''),
      r.output);
    assert.ok(!existsSync(join(probe, 'written')), 'the test wrote on the host');
    assert.ok(!existsSync(etc), 'the test wrote ' + etc);
    assert.ok(!existsSync(join('/tmp', tmpChild)), 'the test wrote the host\'s /tmp');
    const own = (ns) => execFileSync('readlink', ['/proc/self/ns/' + ns], { encoding: 'utf8' }).trim();
    for (const sub of subs) {
      // Read-only but the run's temp directory, target/ and the log
      // directory, the namespace's /proc, /dev's terminals and its
      // private /dev/shm and /dev/mqueue.
      assert.deepEqual(writableMounts(result + '.mountinfo.' + sub),
        ['logs', 'mqueue', 'proc', 'pts', 'run', 'shm', 'target', 'tmp'], sub + '\n' + r.output);
      // The private /tmp is as the host's: 1777 and root's.
      assert.equal(readFileSync(result + '.tmpmode.' + sub, 'utf8').trim(), '1777 0', sub);
      assert.equal(readFileSync(result + '.run.' + sub, 'utf8'), '', sub);
      assert.notEqual(readFileSync(result + '.net.' + sub, 'utf8').trim(), own('net'), sub);
      assert.notEqual(readFileSync(result + '.pid.' + sub, 'utf8').trim(), own('pid'), sub);
    }
  } finally {
    rmSync(probe, { recursive: true, force: true });
    rmSync(dir, { recursive: true, force: true });
    rmSync(etc, { force: true });
    rmSync(join('/tmp', tmpChild), { recursive: true, force: true });
  }
});

// A host mount that goes while the sandbox is set up (a container ending
// on the host: docker removes its mount point, which takes the mount out
// of the sandbox's namespace too) leaves nothing to make read-only, and
// the run goes on; so does a mount table that changes while it is read.
// A churn on the host, outside /tmp, mounts and removes tmpfs mounts for
// the whole stubbed run (in the gate, inside the floor guard tests' own
// sandbox; run by hand, on the host, where a sandbox set up meanwhile by
// a gate without this change can stop as Checkpoint O's did). It ends at
// the stop file, when node is gone, or after 120 s, its mounts removed.
test("check.sh: host mounts removed while the sandbox is set up don't stop a test run", () => {
  mkdirSync(join(SCRIPTS, '..', 'target'), { recursive: true });
  const churn = mkdtempSync(join(SCRIPTS, '..', 'target', 'sandbox-churn-'));
  const stop = join(churn, 'stop');
  const script = 'cd "$1" || exit 1; k=0\n'
    + 'while [ ! -e stop ] && kill -0 "$2" 2>/dev/null && [ "$SECONDS" -lt 120 ]; do\n'
    + '  for m in $(seq 1 40); do mkdir "m$k-$m" && mount -t tmpfs -o size=1m sb-churn "m$k-$m"; done\n'
    + '  for m in $(seq 1 40); do umount "m$k-$m"; rmdir "m$k-$m"; done\n'
    + '  k=$((k + 1))\n'
    + 'done\n'
    + ': >done\n';
  const loop = spawn('bash', ['-c', script, 'churn', churn, String(process.pid)], { stdio: 'ignore' });
  try {
    const r = checkRun('task', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' } });
    passes(r, 'task');
    assert.doesNotMatch(r.output, /check: the sandbox/, r.output);
  } finally {
    writeFileSync(stop, '');
    // Waited for by the file it leaves as it ends, its own mounts removed
    // (not its pid: it stays a zombie until node's event loop runs).
    const ended = spawnSync('bash', ['-c', 'for _ in $(seq 1 600); do [ -e "$1/done" ] && exit 0; sleep 0.1; done; exit 1', 'wait', churn]);
    if (ended.status !== 0) loop.kill('SIGKILL');
    // Whatever is left, removed without hiding the test's own failure; a
    // mount still there keeps its directory (nothing is deleted in it).
    spawnSync('bash', ['-c', 'for m in "$1"/m*; do [ -d "$m" ] || continue; umount -R "$m" 2>/dev/null; rmdir "$m" 2>/dev/null; done; rm -f "$1/stop" "$1/done"; rmdir "$1"', 'clean', churn]);
  }
});

// A test that writes /etc fails in the gate: the write is refused, the
// test fails, and so does the run.
test('check.sh: a test that writes /etc fails the run', () => {
  const etc = '/etc/sb-gate-sandbox-probe-fail-' + process.pid;
  try {
    const r = checkRun('task', { work: { 'crates/foo/src/lib.rs': 'pub fn f() {}\n' }, env: { TEST_WRITES_ETC: etc } });
    assert.equal(r.status, 101, r.output);
    assert.match(r.output, new RegExp('the test could not write ' + etc), r.output);
    assert.ok(!existsSync(etc), 'the test wrote ' + etc);
  } finally {
    rmSync(etc, { force: true });
  }
});
