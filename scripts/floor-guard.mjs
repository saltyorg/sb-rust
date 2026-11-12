#!/usr/bin/env node
// floor-guard.mjs: diff-scoped enforcement of the CONSTRAINTS.md floor.
// Adapted from the constraint-driven-development skill's reference guard
// (agent-skills 0.6.13, skills/constraint-driven-development/references/
// floor-guard.md): same contract, plumbing and CONSTRAINTS.md checks, with
// Rust's patterns (rules 1 to 4) and this repository's gate files (rule 5).
//
// Usage: node scripts/floor-guard.mjs [--base <ref>]
//   default base: HEAD (uncommitted and untracked work). Before handing back
//   committed work, pass the commit the task started from.
// Exit: 0 clean (every finding acknowledged in
// scripts/floor-acknowledged.txt counts as clean, and is printed), 1 floor
// violation, 2 the guard could not run.
// Reports the rule, the file and the flagged line (its first 120
// characters), so a line that holds a secret is printed: gitleaks, not this
// guard, keeps secrets out of the diff.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const base = (() => {
  const i = process.argv.indexOf('--base');
  return i > -1 ? process.argv[i + 1] : 'HEAD';
})();

// `git diff --no-index` exits 1 whenever the two sides differ, which is the
// normal case for a new file, so that output is kept. Any other failure is
// null, and null never reads as clean.
const git = (args, { diffExit = false } = {}) => {
  try { return execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }); }
  catch (e) { return diffExit && e.status === 1 && typeof e.stdout === 'string' ? e.stdout : null; }
};
const bail = (msg) => { console.error('floor-guard: ' + msg); process.exit(2); };

const top = git(['rev-parse', '--show-toplevel'])?.trim();
if (!top) bail('not inside a git work tree');
process.chdir(top);

const mergeBase = git(['merge-base', base, 'HEAD'])?.trim();
if (!mergeBase) bail('no merge base against ' + base);

// Every diff is taken the same way, whatever the user's git configuration
// or an added .gitattributes says: a rename is a deletion and an addition,
// no external diff or textconv, no colour, every file is text, and the
// prefixes are a/ and b/.
const DIFF = ['diff', '--no-renames', '--no-ext-diff', '--no-textconv', '--no-color', '--text',
  '--src-prefix=a/', '--dst-prefix=b/'];
const tracked = git([...DIFF, '--unified=0', mergeBase, '--']);
if (tracked === null) bail('could not diff against ' + mergeBase);
const untrackedFiles = git(['ls-files', '--others', '--exclude-standard']);
if (untrackedFiles === null) bail('could not list untracked files');
const untracked = untrackedFiles.split('\n').filter(Boolean).map((f) => {
  const d = git([...DIFF, '--no-index', '--unified=0', '/dev/null', f], { diffExit: true });
  if (d === null) bail('could not diff untracked file ' + f);
  return d;
}).join('\n');
const diff = tracked + '\n' + untracked;

const added = [], removed = [], deleted = [];
const pathOf = (s) => s.replace(/^[ab]\//, '');
let file = '', oldFile = '', inHeader = false;
for (const line of diff.split('\n')) {
  if (line.startsWith('diff ')) inHeader = true;
  else if (line.startsWith('@@')) inHeader = false;
  else if (inHeader) {
    if (line.startsWith('--- ')) oldFile = pathOf(line.slice(4));
    else if (line.startsWith('+++ ')) {
      const newFile = pathOf(line.slice(4));
      file = newFile === '/dev/null' ? oldFile : newFile;
      if (newFile === '/dev/null') deleted.push(file);
    }
  }
  else if (line.startsWith('+')) added.push({ file, text: line.slice(1) });
  else if (line.startsWith('-')) removed.push({ file, text: line.slice(1) });
}

const findings = [];
const flag = (rule, f, text) => findings.push({ rule, file: f, text: text.trim().slice(0, 120) });
const isRust = (f) => f.endsWith('.rs');
// Rust tests live beside the code (#[cfg(test)]) and in tests/.
const isTest = (f) => isRust(f) && (/(^|\/)tests\//.test(f) || /_test\.rs$/.test(f));
const isConstraints = (f) => /(^|\/)CONSTRAINTS\.md$/.test(f);
// Prototypes are throwaway evidence (CLAUDE.md); the floor guards crates/
// and the repository's own tooling.
const guarded = (f) => !f.startsWith('prototypes/');

// 1. Silenced checker. #[allow] is also denied by the workspace lints; a new
// #[expect] needs a CONSTRAINTS.md Exceptions row, so it is reported here.
// So does the conditional form, `#![cfg_attr(cond, expect(...))]`: on one
// line, or split over lines, with `expect(` opening a line of its own (no
// method call starts a line without its dot) or after the condition's `,`
// or `(` (a method call has its dot there too). `coverage(off)` and
// `mutants::skip` are reported wherever they are, so inside a `cfg_attr`
// too. deny.toml and the lint tables are rules 5g and 5h.
const SUPPRESSIONS = /#!?\[\s*(allow|expect)\s*\(|[(,]\s*(allow|expect)\s*\(|^\s*(allow|expect)\s*\(|\bcoverage\s*\(\s*off\b|\bmutants::skip|gitleaks:allow/;
// 4. Unfinished work.
const STUBS = /\btodo!\s*\(|\bunimplemented!\s*\(|\bTODO\b|\bFIXME\b|\bXXX\b/;
// 2. A test made easier.
// - An ignore: `#[ignore]`; the word `ignore` anywhere on a `cfg_attr` line,
//   so nested or after a comment too (a string such as `feature = "ignore"`
//   is reported as well; `ignored` and `ignore_x` are not); and a
//   `cfg_attr` split over lines, with `ignore` or `ignore = "..."` opening
//   a line, or `ignore = "` after a `,` or `(` (a variable named ignore
//   passed to a call is neither).
// - A cfg that is never true: `any()`, `false` (the spelling since Rust
//   1.88) or `not(all())`, but not `not(any())`, which is always true. A
//   line pattern can't know that `cfg(not(test))` hides a test, or that a
//   feature or cfg name is never set (`cfg(feature = "never")`,
//   `cfg(FALSE)`): those stay unseen.
const SKIPS = /#\[\s*ignore\b|#\[\s*cfg_attr\s*\(.*\bignore\b|^\s*ignore\s*(=\s*"|\)|$)|[(,]\s*ignore\s*=\s*"|#!?\[\s*cfg\s*(?=\().*(\b(?<!not\s*\(\s*)any\s*\(\s*\)|[(,]\s*false\s*[),]|\bnot\s*\(\s*all\s*\(\s*\)\s*\))/;
// An assertion is a macro or a step of an assert_cmd chain: `.assert()`,
// `.success()`, `.failure()`, `.code(..)`, `.stdout(..)` and `.stderr(..)`.
// A chain step opens its line, as rustfmt puts it (`.code(` may end it,
// with its argument below); `.assert()` counts anywhere. Not assertions: a
// `Stdio` argument (std::process::Command's builder), `.code()` with no
// argument (ExitStatus) and `.failure(` with a message (sb's own step
// API). Any other argument to `.stdout(` or `.stderr(` (a `File`, an `if`)
// is reported, and is acknowledged where it is std's builder. Unseen: a
// step on a bound value (`let a = cmd.assert(); a.success();`).
const ASSERTS = /\b(assert|assert_eq|assert_ne|debug_assert|prop_assert|insta::assert_\w+)!|\.assert\s*\(\s*\)|^\s*\.(success|failure)\s*\(\s*\)|^\s*\.code\s*\(\s*([^\s)]|$)|^\s*\.(stdout|stderr)\s*\((?!\s*(?:\w+::)*Stdio::)/;

for (const { file, text } of added) {
  if (!guarded(file)) continue;
  if (isRust(file) && SUPPRESSIONS.test(text)) flag('silenced-checker', file, text);
  if (isRust(file) && STUBS.test(text)) flag('unfinished-work', file, text);
  if (isRust(file) && SKIPS.test(text)) flag('test-made-easier', file, text);
  if (isConstraints(file) && /^\| *(W|E)\d+ *\|/.test(text)) flag('new-exception', file, text);
}

// 2b. A test file deleted (under tests/, or any Rust file with a test
// attribute: `#[test]`, `#[tokio::test]`), or an assertion removed from
// Rust code that still exists (unit tests sit in the same file as the code).
// A rename is a deletion here, so moving a src file that holds tests is
// reported too, and is acknowledged.
const TEST_ATTR = /#\[\s*(?:\w+::)*test\b/;
const heldTests = (f) => removed.some((l) => l.file === f && TEST_ATTR.test(l.text));
for (const f of deleted) if (guarded(f) && (isTest(f) || (isRust(f) && heldTests(f)))) flag('test-deleted', f, 'file deleted');
// A test removed from a file that stays: more test-attribute lines (a test
// attribute, or the `#[cfg(test)]` of a test module) removed than added.
// A test renamed or moved within its file, or given another test attribute,
// is clean; a test moved to another file is reported, and is acknowledged.
// An added line that is a `//` comment adds no test, so a test commented
// out line by line is reported.
const TEST_LINE = /#\[\s*(?:\w+::)*test\b|#\[\s*cfg\s*\(\s*test\s*\)\s*\]/;
const testLines = (lines, f) => lines.filter((l) => l.file === f && !(lines === added && l.text.trim().startsWith('//'))
  && TEST_LINE.test(l.text)).length;
for (const f of new Set(removed.map((l) => l.file))) {
  if (!guarded(f) || !isRust(f) || deleted.includes(f)) continue;
  const [gone, come] = [testLines(removed, f), testLines(added, f)];
  if (gone > come) flag('test-removed', f, gone + ' test attribute line(s) removed, ' + come + ' added');
}
for (const { file, text } of removed) {
  if (guarded(file) && isRust(file) && !deleted.includes(file) && ASSERTS.test(text)) {
    flag('assertion-removed', file, text);
  }
}

// 1b/2c. A rule in CONSTRAINTS.md weakened or removed (unchanged from the
// reference: direction read from the words around each number).
const ruleKey = (t) => {
  const s = t.trim();
  if (s.startsWith('|')) return s.split('|').map((c) => c.trim()).filter(Boolean)[0] ?? '';
  if (/^[-*] /.test(s)) return s.slice(2).split(':')[0].trim();
  return null;
};
const isException = (t) => /^\| *(W|E)\d+ *\|/.test(t.trim());
const MIN_BEFORE = /(>=|>|≥|at least|minimum|\bmin\b|no less than|not fall|not drop)\s*$/;
const MAX_BEFORE = /(<=|<|≤|at most|maximum|\bmax\b|no more than|under|below|not grow|not exceed)\s*$/;
const MIN_AFTER = /^\s*\S*\s*(or more|or higher|must not fall|must not drop)/;
const MAX_AFTER = /^\s*\S*\s*(or less|or lower|must not grow|must not exceed)/;
const thresholds = (t) => {
  const out = [], re = /\d+(?:\.\d+)?/g;
  let m;
  while ((m = re.exec(t))) {
    const before = t.slice(Math.max(0, m.index - 24), m.index).toLowerCase();
    const after = t.slice(m.index + m[0].length, m.index + m[0].length + 40).toLowerCase();
    const dir = MIN_BEFORE.test(before) || MIN_AFTER.test(after) ? 'min'
      : MAX_BEFORE.test(before) || MAX_AFTER.test(after) ? 'max' : null;
    out.push({ n: Number(m[0]), dir });
  }
  return out;
};
const removedRules = removed.filter((l) => isConstraints(l.file) && ruleKey(l.text) !== null);
const addedRules = added.filter((l) => isConstraints(l.file) && ruleKey(l.text) !== null);
for (const r of removedRules) {
  const a = addedRules.find((x) => ruleKey(x.text) === ruleKey(r.text));
  if (!a) {
    if (!isException(r.text)) flag('rule-removed', r.file, r.text);
    continue;
  }
  const before = thresholds(r.text), after = thresholds(a.text);
  let verdict = null;
  for (const dir of ['min', 'max', null]) {
    const was = before.filter((x) => x.dir === dir), now = after.filter((x) => x.dir === dir);
    was.forEach((b, i) => {
      const n = now[i];
      if (verdict) return;
      if (!n) verdict = 'threshold-removed';
      else if (n.n === b.n) return;
      else if (dir === 'min' ? n.n < b.n : dir === 'max' ? n.n > b.n : true) {
        verdict = dir ? 'threshold-loosened' : 'threshold-changed';
      }
    });
    // A number appended in the same direction is read as an alternative
    // ("≥ 90%, or ≥ 50% for tests"): looser than every number before, it
    // lowers the bar. An added requirement on something else ("and ≥ 80%
    // of branches") reads the same way, and is acknowledged. An appended
    // number with no direction word of its own isn't seen.
    const loosest = dir === 'min' ? Math.min(...was.map((b) => b.n)) : Math.max(...was.map((b) => b.n));
    const looser = (n) => (dir === 'min' ? n.n < loosest : n.n > loosest);
    if (!verdict && dir && was.length && now.slice(was.length).some(looser)) verdict = 'threshold-loosened';
  }
  if (verdict) flag(verdict, r.file, r.text + '  ->  ' + a.text);
}

// 5. The gate files: the files outside Rust and CONSTRAINTS.md that hold
// the bar, this guard's own scripts and tests included (docs-H1). Each is
// compared with its copy at the merge base.
// The plan id check (hardening Task 5) and its tests are watched the same way.
const GATE_SCRIPTS = ['scripts/check.sh', 'scripts/floor-guard.mjs', 'scripts/check-commit-messages.sh',
  'scripts/check-plan-ids.mjs'];
const GUARD_TESTS = ['scripts/floor-guard.test.mjs', 'scripts/check-plan-ids.test.mjs'];
const isGuardTest = (f) => GUARD_TESTS.includes(f);
// Files that configure a tool the gate runs, at the root or in a crate.
// - clippy reads `clippy.toml` or `.clippy.toml` from the crate's directory
//   (CARGO_MANIFEST_DIR), then from each parent:
//   https://doc.rust-lang.org/clippy/configuration.html
//   UNVERIFIED which of the two names wins in one directory (the page
//   doesn't say), so both are watched.
// - rustup reads `rust-toolchain` before `rust-toolchain.toml` when a
//   directory holds both: https://rust-lang.github.io/rustup/overrides.html
// - .gitattributes can hide a file's changes from a diff.
const TOOL_CONFIGS = ['.cargo/mutants.toml', '.cargo/config.toml', '.cargo/config', 'clippy.toml', '.clippy.toml',
  '.gitleaksignore', '.gitleaks.toml', 'rust-toolchain', '.gitattributes'];
const isGateScript = (f) => GATE_SCRIPTS.includes(f);
const isTestScript = (f) => /^crates\/[^/]+\/tests\/[^/]+\.sh$/.test(f);
const isManifest = (f) => f === 'Cargo.toml' || /^crates\/[^/]+\/Cargo\.toml$/.test(f);
const isToolConfig = (f) => TOOL_CONFIGS.some((n) => f === n || (f.startsWith('crates/') && f.endsWith('/' + n)));
const isCode = (f, t) => t.trim() !== '' && !t.trim().startsWith(f.endsWith('.mjs') ? '//' : '#');

const changedFiles = git([...DIFF, '--name-only', mergeBase, '--']);
if (changedFiles === null) bail('could not list the changed files');
const changed = (changedFiles + '\n' + untrackedFiles).split('\n').filter(Boolean);
// null: the file isn't there (at the merge base, or in the work tree). A
// file that is there and can't be read stops the guard.
const atBase = new Map();
const oldText = (f) => {
  if (atBase.has(f)) return atBase.get(f);
  const listed = git(['ls-tree', '--name-only', mergeBase, '--', f]);
  if (listed === null) bail('could not look for ' + f + ' at ' + mergeBase);
  const text = listed.trim() === '' ? null : git(['show', mergeBase + ':' + f]);
  if (listed.trim() !== '' && text === null) bail('could not read ' + f + ' at ' + mergeBase);
  atBase.set(f, text);
  return text;
};
const newText = (f) => {
  try { return readFileSync(f, 'utf8'); }
  catch (e) { return e.code === 'ENOENT' ? null : bail('could not read ' + f); }
};
// A line the diff both removes and adds (moved, reindented) is still there.
// `taken` pairs each added line with one removed line at most.
const sameLine = (a, b) => a.file === b.file && a.text.trim() === b.text.trim();

// 1c. A CONSTRAINTS.md Floor bullet, changed or gone from the Floor
// section. A continuation line has no key of its own, and a bullet whose
// lines stay can leave the section (a heading renamed or put above it), so
// rule 1b sees neither.
// - A bullet runs on over every line after it in the section, blank lines
//   aside, until the next bullet, a heading or a table row: so over a
//   continuation, indented or not, and over a paragraph after a blank line.
// - It is compared whole with the bullet of the same key (the one with the
//   same words, if two share the key), word by word, so a rewrapped bullet
//   is clean. The finding shows five words of each side from the first that
//   differs.
// - A bullet whose key is no longer in the section is rule-removed, once:
//   not again when 1b reports its first line.
// - Unseen: prose under a sub-heading inside the Floor, and an exemption
//   written as a table row there. Added prose that weakens a rule has no
//   bound, like an added bullet that contradicts one. Likewise in a .rs
//   file, a test or an assertion wrapped in `/* ... */` is unseen.
const floorBullets = (text) => {
  const out = [];
  let inFloor = false, bullet = null;
  for (const line of (text ?? '').split('\n')) {
    if (line.trim() === '') continue;
    if (/^#{1,6}(\s|$)/.test(line)) {
      if (/^## /.test(line)) inFloor = /^## Floor\b/.test(line);
      bullet = null;
    } else if (inFloor && /^[-*] /.test(line)) out.push(bullet = { key: ruleKey(line), lines: [line] });
    else if (bullet && !/^\s*\|/.test(line)) bullet.lines.push(line);
    else bullet = null;
  }
  return out;
};
const words = (b) => b.lines.join(' ').trim().split(/\s+/);
for (const f of changed.filter(isConstraints)) {
  const now = floorBullets(newText(f));
  for (const was of floorBullets(oldText(f))) {
    const w = words(was);
    const same = now.filter((b) => b.key === was.key);
    const is = same.find((b) => words(b).join(' ') === w.join(' ')) ?? same[0];
    if (!is) {
      const first = was.lines[0].trim().slice(0, 120);
      if (!findings.some((x) => x.rule === 'rule-removed' && x.file === f && x.text === first)) flag('rule-removed', f, first);
      continue;
    }
    const n = words(is);
    if (w.join(' ') === n.join(' ')) continue;
    let at = 0;
    while (w[at] === n[at]) at++;
    flag('rule-changed', f, was.key + ': ...' + w.slice(at, at + 5).join(' ') + '  ->  ...' + n.slice(at, at + 5).join(' '));
  }
}

// A TOML file's lines without their comments, each with its table, its key
// and its value; a line inside a multi-line array has the array's key.
// Quotes around a key or a table name are dropped. Not a TOML parser: it
// reads the tables, keys and array members the rules below compare.
const QUOTED = /"[^"]*"|'[^']*'/g;
const strings = (text) => (text.match(QUOTED) ?? []).map((q) => q.slice(1, -1));
const tomlCode = (raw) => {
  let quote = null;
  for (let i = 0; i < raw.length; i++) {
    if (quote) { if (raw[i] === quote) quote = null; }
    else if (raw[i] === '"' || raw[i] === "'") quote = raw[i];
    else if (raw[i] === '#') return raw.slice(0, i).trim();
  }
  return raw.trim();
};
const TOML_KEY = /^((?:[\w-]+|"[^"]*"|'[^']*')(?:\s*\.\s*(?:[\w-]+|"[^"]*"|'[^']*'))*)\s*=\s*(.*)$/;
const tomlItems = (text) => {
  const out = [];
  let table = '', array = null, depth = 0;
  for (const raw of (text ?? '').split('\n')) {
    const line = tomlCode(raw);
    if (line === '') continue;
    const header = depth === 0 ? /^\[\[?([^[\]]+)\]\]?$/.exec(line) : null;
    if (header) { table = header[1].replace(/["'\s]/g, ''); continue; }
    const pair = depth === 0 ? TOML_KEY.exec(line) : null;
    const key = pair ? pair[1].replace(/["'\s]/g, '') : array ?? '';
    const value = pair ? pair[2] : line;
    out.push({ table, key, value, line });
    const bare = value.replace(QUOTED, '');
    depth = Math.max(0, depth + (bare.match(/[[{]/g) ?? []).length - (bare.match(/[\]}]/g) ?? []).length);
    array = depth > 0 ? key : null;
  }
  return out;
};

// 5a. A gate file deleted or renamed (a crate's manifest goes with its
// crate).
for (const f of deleted) {
  if (isGateScript(f) || isGuardTest(f) || isTestScript(f) || f === 'deny.toml' || f === 'rust-toolchain.toml') {
    flag('gate-file-deleted', f, 'file deleted');
  }
}

// 5b. Shell thresholds: an upper-case constant with MIN or MAX in its name
// (check.sh's thresholds, a test script's PREADS_*_MAX). `rest` is what
// follows the value on its line, a comment aside: a second command there
// can assign the constant again.
const DECLARED = '^\\s*(?:(?:readonly|export|local|declare(?:\\s+-\\w+)*)\\s+)?';
const LIMIT = new RegExp(DECLARED + '([A-Z0-9_]+)=(\\S*)(.*)$');
const limit = (t) => {
  const m = LIMIT.exec(t);
  const dir = m && /(?:^|_)(MIN|MAX)(?:_|$)/.exec(m[1])?.[1];
  return dir ? { name: m[1], dir, value: m[2], rest: !/^\s*(#.*)?$/.test(m[3]) } : null;
};
const isShellGate = (f) => f.endsWith('.sh') && (isGateScript(f) || isTestScript(f));
for (const f of changed.filter(isShellGate)) {
  if (newText(f) === null) continue;
  const limits = (lines) => lines.filter((l) => l.file === f).map((l) => ({ text: l.text, ...limit(l.text) })).filter((l) => l.name);
  const now = limits(added);
  for (const was of limits(removed)) {
    const i = now.findIndex((l) => l.name === was.name);
    if (i < 0) { flag('threshold-removed', f, was.text); continue; }
    const [is] = now.splice(i, 1);
    if (is.rest || ![was.value, is.value].every((v) => /^\d+(\.\d+)?$/.test(v))) {
      if (is.rest || is.value !== was.value) flag('threshold-changed', f, was.text + '  ->  ' + is.text);
    } else if (was.dir === 'MIN' ? Number(is.value) < Number(was.value) : Number(is.value) > Number(was.value)) {
      flag('threshold-loosened', f, was.text + '  ->  ' + is.text);
    }
  }
  // An added assignment that replaces no removed one: the constant, set at
  // the merge base, is now set again (to anything). So is an added line
  // that sets it another way: in arithmetic, or with let, read or
  // printf -v. Not caught: a constant set through eval, a name reference
  // or a sourced file.
  const names = (oldText(f) ?? '').split('\n').map((l) => limit(l)?.name).filter(Boolean);
  for (const is of now) if (names.includes(is.name)) flag('threshold-reassigned', f, is.text);
  for (const a of added) {
    if (a.file !== f || !isCode(f, a.text) || limit(a.text)) continue;
    const set = (n) => new RegExp('\\(\\(\\s*' + n + '\\s*[-+*/%]?=(?!=)|\\b(let|read|printf\\s+-v)\\b.*\\b' + n + '\\b').test(a.text);
    if (names.some(set)) flag('threshold-reassigned', f, a.text);
  }
}

// 5c. The guard's own scripts and tests: a line of code that is gone or
// changed (a dropped `-D warnings`, a removed step, rule, pattern or
// assertion). A threshold is 5b's.
const taken = new Set();
for (const r of removed) {
  const { file, text } = r;
  if (!(isGateScript(file) || isGuardTest(file)) || deleted.includes(file) || !isCode(file, text) || limit(text)) continue;
  const twin = added.find((a) => !taken.has(a) && sameLine(a, r));
  if (twin) taken.add(twin);
  else flag('gate-line-removed', file, text);
}

// 5d. Added lines in the gate scripts and the test scripts.
// - A named expected failure (tasks/plan.md). Its shape is
//   `expected_failure <id> <task> <always|racy> <name...>`, where id is a
//   finding's (follow-H1, config-T2) and task is the bare number of the
//   task that clears it. Any added line with the word `expected_failure`
//   is reported, whatever its shape (after a condition, quoted, without
//   its mark), and so is a function or variable whose name starts with
//   it, so the list can't move into another form unseen.
// - A line that can switch a check off. This is a heuristic over single
//   added lines. It does not catch a function defined again, a variable
//   assigned again (5b's thresholds aside), or a step moved into a
//   function nothing calls. A line that only moved is reported too: a
//   `return` moved to the top of a stage switches the stage off.
//   - Everywhere: exit 0; if false; set +e; a here-document fed to `:`,
//     which swallows the lines after it; process.exit(0); an opened block
//     comment; --cap-lints.
//   - In the three gate scripts only: a bare return or return 0, a bare
//     exit, `|| true` and `|| :`. A test script's helpers return early
//     and its cleanup lines end in `|| true` as a matter of course, so
//     there they would be noise.
//   - The guard's tests are left out altogether: their fixtures quote
//     these patterns.
const EXPECTED_FAILURE = new RegExp('\\bexpected_failure\\b|^\\s*(?:function\\s+)?expected_failure\\w*\\s*\\(\\)|'
  + DECLARED + 'expected_failure\\w*\\+?=', 'i');
const DISABLERS = [
  /(^|[\s;{&|(])exit\s+0\b/,
  /\bif\s*\(?\s*false\b/,
  /\bset\s+\+\w*e/,
  /(^|[\s;]):\s*<</,
  /process\.exit\(\s*0?\s*\)/,
  /^\s*\/\*/,
  /--cap-lints/,
];
const GATE_SCRIPT_DISABLERS = [
  /(^|[\s;{&|])return(\s+0)?\s*(;|\}|#|$)/,
  /(^|[\s;{&|(])exit\s*(;|\}|#|$)/,
  /\|\|\s*(true\b|:(\s|;|$))/,
];
for (const { file, text } of added) {
  if (!(isGateScript(file) || isTestScript(file)) || !isCode(file, text)) continue;
  if (isTestScript(file) && EXPECTED_FAILURE.test(text)) {
    flag('expected-failure-added', file, text);
  }
  const disablers = isGateScript(file) ? [...DISABLERS, ...GATE_SCRIPT_DISABLERS] : DISABLERS;
  if (disablers.some((d) => d.test(text))) flag('check-disabled', file, text);
}

// 5e. The mutation scope (ADR-012): a crate that gains an entry in
// MUTATION_TESTERS leaves the pass every crate's tests judge, and an entry
// that loses a tester is judged by fewer tests. More testers is wider.
const testers = (text) => {
  const code = (text ?? '').split('\n').filter((l) => isCode('scripts/check.sh', l)).join('\n');
  const map = new Map();
  const entries = (list) => {
    for (const m of list.matchAll(/\[([^\]]+)\]\+?=\s*["']?([^"'\n)]*)/g)) {
      map.set(m[1], [...(map.get(m[1]) ?? []), ...m[2].split(/\s+/).filter(Boolean)]);
    }
  };
  for (const m of code.matchAll(/MUTATION_TESTERS\+?=\(([^)]*)\)/g)) entries(m[1]);
  for (const m of code.matchAll(/MUTATION_TESTERS(\[[^\]]+\]\+?=.*)/g)) entries(m[1]);
  return map;
};
if (changed.includes('scripts/check.sh') && newText('scripts/check.sh') !== null) {
  const before = testers(oldText('scripts/check.sh'));
  for (const [crate, now] of testers(newText('scripts/check.sh'))) {
    const gone = before.get(crate)?.filter((t) => !now.includes(t));
    if (!gone) flag('mutation-scope-narrowed', 'scripts/check.sh', crate + ': a new entry');
    else if (gone.length) flag('mutation-scope-narrowed', 'scripts/check.sh', crate + ': ' + gone.join(', ') + ' removed');
  }
}

for (const f of changed) {
  const now = newText(f);
  if (now === null || !guarded(f)) continue;
  // 5f. A tool configuration file that is new (one finding for the file),
  // or has new lines (one finding for each): each can switch a check off
  // (mutants to skip, lint settings, ignored secrets, build flags, another
  // toolchain, hidden diffs).
  if (isToolConfig(f)) {
    if (oldText(f) === null) flag('tool-config-added', f, 'file added');
    else for (const a of added) if (a.file === f && isCode(f, a.text)) flag('tool-config-changed', f, a.text);
  }
  // 5g. deny.toml.
  if (f === 'deny.toml') {
    const before = tomlItems(oldText(f)), after = tomlItems(now);
    const same = (a, b) => a.table === b.table && a.key === b.key && a.value === b.value;
    const has = (i) => /\w/.test(i.value);
    // Lists where a new member loosens and a removed one tightens.
    const listRule = (i) => i.table === 'advisories' && i.key === 'ignore' ? 'advisory-ignored'
      : i.table === 'licenses' && ['allow', 'exceptions'].includes(i.key) ? 'licence-added'
      : i.table === 'licenses.exceptions' ? (i.key === 'allow' ? 'licence-added' : 'none')
      : i.table === 'licenses.private' && i.key === 'ignore' ? 'licence-ignored'
      : i.table === 'bans' && ['skip', 'skip-tree'].includes(i.key) ? 'ban-skipped' : null;
    for (const a of after) {
      const rule = listRule(a);
      if (rule && rule !== 'none' && has(a) && !before.some((b) => same(a, b))) flag(rule, f, a.line);
    }
    // Every other line of the four checks' tables, the architecture
    // entries aside: changed or removed (`unmaintained`, `yanked`, the
    // confidence threshold, a feature ban, a source rule).
    for (const b of before) {
      const setting = /^(advisories|sources|licenses|bans)(\.|$)/.test(b.table) && !listRule(b)
        && !(b.table === 'bans' && b.key === 'deny') && has(b);
      if (setting && !after.some((a) => same(a, b))) flag('deny-setting-changed', f, b.line);
    }
    // The architecture entries: a crate's `wrappers` may not gain a member,
    // and its entry may not go. Entries are read across lines.
    const wrappers = (items) => {
      const map = new Map();
      const bans = items.filter((i) => i.table === 'bans').map((i) => i.line).join(' ');
      for (const m of bans.matchAll(/crate\s*=\s*("[^"]*"|'[^']*')[^{}]*?wrappers\s*=\s*\[([^\]]*)\]/g)) {
        const crate = m[1].slice(1, -1);
        map.set(crate, [...(map.get(crate) ?? []), ...strings(m[2])]);
      }
      return map;
    };
    const allowed = wrappers(after);
    for (const [crate, was] of wrappers(before)) {
      const extra = allowed.get(crate)?.filter((w) => !was.includes(w));
      if (!extra) flag('wrappers-widened', f, crate + ': entry removed');
      else if (extra.length) flag('wrappers-widened', f, crate + ': ' + [...new Set(extra)].join(', ') + ' added');
    }
  }
  // 5h. The lint tables of the workspace and of each crate: a lint removed,
  // lowered or set to allow (`all = "allow"` under clippy included), or a
  // crate leaving the workspace table.
  if (isManifest(f)) {
    const lints = (text) => new Map(tomlItems(text)
      .map((i) => [[i.table, i.key].filter(Boolean).join('.'), i.line])
      .filter(([path]) => /^(workspace\.)?lints(\.|$)/.test(path)));
    const level = (line) => ['allow', 'warn', 'deny', 'forbid'].indexOf(/["'](allow|warn|deny|forbid)["']/.exec(line)?.[1]);
    const before = lints(oldText(f)), after = lints(now);
    for (const [path, was] of before) {
      const line = after.get(path);
      if (line === undefined) flag('lint-removed', f, was);
      else if (level(line) < level(was) || (level(was) < 0 && line !== was)) flag('lint-weakened', f, was + '  ->  ' + line);
    }
    for (const [path, line] of after) if (!before.has(path) && level(line) === 0) flag('lint-weakened', f, line);
  }
  // 5i. The toolchain pin: another channel, or a component gone.
  if (f === 'rust-toolchain.toml') {
    const values = (text, key) => tomlItems(text)
      .filter((i) => i.table === 'toolchain' && i.key === key).flatMap((i) => strings(i.line));
    const old = oldText(f);
    if (old !== null && values(old, 'channel').join() !== values(now, 'channel').join()) {
      flag('toolchain-changed', f, 'channel ' + values(old, 'channel').join() + '  ->  ' + values(now, 'channel').join());
    }
    const gone = values(old, 'components').filter((c) => !values(now, 'components').includes(c));
    if (gone.length) flag('toolchain-changed', f, 'components removed: ' + gone.join(', '));
  }
}

// 6. Acknowledged findings (ADR-015). scripts/floor-acknowledged.txt holds
// one entry a line, four fields with a tab between them: the rule, the
// file, the flagged line's text as this guard prints it (runs of white
// space count as one space), and a reason.
// - An entry counts only when the checked diff adds it, so it is committed
//   with the change it acknowledges; an entry from an earlier commit
//   acknowledges nothing.
// - An entry covers one finding. Two findings need two entries, alike or
//   not, and no entry covers a file.
// - Each acknowledged finding is printed with its reason. An added entry
//   that matches no finding, or has no reason, fails the run; so does a
//   removed or edited entry: the file is a record, and lines are only
//   added. None of the three can itself be acknowledged: a wrong entry is
//   put right by adding the right one in the diff that needs it.
const ACKNOWLEDGED = 'scripts/floor-acknowledged.txt';
const squash = (t) => t.trim().replace(/\s+/g, ' ');
const acknowledged = [];
const refused = [];
if (deleted.includes(ACKNOWLEDGED)) refused.push(['acknowledgement-removed', 'file deleted']);
for (const r of removed) {
  if (r.file === ACKNOWLEDGED && !deleted.includes(ACKNOWLEDGED) && isCode(r.file, r.text)
    && !added.some((a) => a.file === r.file && a.text === r.text)) {
    refused.push(['acknowledgement-removed', r.text]);
  }
}
for (const { file, text: line } of added) {
  if (file !== ACKNOWLEDGED || !isCode(file, line)) continue;
  // An entry that only moved is an old entry: not removed, and not new.
  if (removed.some((r) => r.file === file && r.text === line)) continue;
  const [rule, path, text, ...reason] = line.split('\t').map(squash);
  if (!rule || !path || !text || reason.join('') === '') { refused.push(['acknowledgement-malformed', line]); continue; }
  const i = findings.findIndex((f) => f.rule === rule && f.file === path && squash(f.text) === text);
  if (i < 0) refused.push(['acknowledgement-unmatched', line]);
  else acknowledged.push({ ...findings.splice(i, 1)[0], reason: reason.join(' ') });
}
// After the matching, so that neither can be acknowledged.
for (const [rule, line] of refused) flag(rule, ACKNOWLEDGED, line);
if (acknowledged.length) {
  console.log('floor-guard: ' + acknowledged.length + ' acknowledged finding(s), by entries this diff adds to ' + ACKNOWLEDGED + ':');
  for (const f of acknowledged) console.log(`  [${f.rule}] ${f.file}: ${f.text}\n      reason: ${f.reason}`);
}

if (findings.length === 0) { console.log('floor-guard: clean'); process.exit(0); }
console.error('floor-guard: ' + findings.length + ' floor violation(s):');
for (const f of findings) console.error(`  [${f.rule}] ${f.file}: ${f.text}`);
if (findings.some((f) => f.rule === 'rule-removed')) {
  console.error('\nA rule-removed finding can also mean the rule\'s label changed: rename a rule in one commit and change its thresholds in another.');
}
if (findings.some((f) => f.rule === 'threshold-removed')) {
  console.error('\nA threshold-removed finding can also mean a number gained or lost its direction words: compare the two lines before assuming a threshold was deleted.');
}
console.error('\nEach is a move that lowers the bar. Fix the code, or route it through a tracked exception.');
process.exit(1);
