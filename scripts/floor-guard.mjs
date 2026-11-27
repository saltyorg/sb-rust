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
// Two kinds of finding (ADR-029):
// - blocking: the gate's configuration, its thresholds and the suppressions,
//   a test's child output discarded and a test's read of the host, each
//   without its reason (every rule but those reported, rule 6). One passes
//   only with the maintainer's approval, a `Floor-approved:` line (rule 7).
// - reported: a removed assertion, a removed or deleted test, a removed
//   or changed line of a gate script, and a hand-rolled root or euid skip
//   in test code. They are printed for the code
//   reviewer, who judges each against the commit message, and never fail.
// Exit: 0 no blocking finding without an approval (the reported ones and
// the approved ones are printed), 1 a blocking finding, 2 the guard could
// not run.
// Reports the rule, the file and the flagged line (its first 120
// characters), so a line that holds a secret is printed: gitleaks, not this
// guard, keeps secrets out of the diff.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const base = (() => {
  const i = process.argv.indexOf('--base');
  return i > -1 ? process.argv[i + 1] : 'HEAD';
})();

// Node ends a child whose output passes `maxBuffer`, 1 MiB by default
// (https://nodejs.org/api/child_process.html#child_processexecfilesyncfile-args-options),
// which a pinned upstream data file's diff passes; 256 MiB holds any diff
// of this repository, and a larger one still fails as null.
const MAX_DIFF_BYTES = 256 * 1024 * 1024;
// `git diff --no-index` exits 1 whenever the two sides differ, which is the
// normal case for a new file, so that output is kept. Any other failure is
// null, and null never reads as clean.
const git = (args, { diffExit = false } = {}) => {
  try {
    return execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: MAX_DIFF_BYTES });
  }
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

// Each added line keeps its line number in the work tree's file, from
// its hunk's header (`@@ -a,b +c,d @@`, the new side's first line c).
const added = [], removed = [], deleted = [];
const pathOf = (s) => s.replace(/^[ab]\//, '');
let file = '', oldFile = '', inHeader = false, newLine = 0;
for (const line of diff.split('\n')) {
  if (line.startsWith('diff ')) inHeader = true;
  else if (line.startsWith('@@')) {
    inHeader = false;
    newLine = Number(/^@@ -\d+(?:,\d+)? \+(\d+)/.exec(line)?.[1] ?? 0);
  }
  else if (inHeader) {
    if (line.startsWith('--- ')) oldFile = pathOf(line.slice(4));
    else if (line.startsWith('+++ ')) {
      const newFile = pathOf(line.slice(4));
      file = newFile === '/dev/null' ? oldFile : newFile;
      if (newFile === '/dev/null') deleted.push(file);
    }
  }
  else if (line.startsWith('+')) added.push({ file, text: line.slice(1), line: newLine++ });
  else if (line.startsWith('-')) removed.push({ file, text: line.slice(1) });
}

const findings = [];
const flag = (rule, f, text) => findings.push({ rule, file: f, text: text.trim().slice(0, 120) });
const isRust = (f) => f.endsWith('.rs');
// Rust tests live beside the code (#[cfg(test)]) and in tests/.
const isTest = (f) => isRust(f) && (/(^|\/)tests\//.test(f) || /_test\.rs$/.test(f));
const isConstraints = (f) => /(^|\/)CONSTRAINTS\.md$/.test(f);

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
// is reported, and the reviewer passes it where it is std's builder. Unseen: a
// step on a bound value (`let a = cmd.assert(); a.success();`).
const ASSERTS = /\b(assert|assert_eq|assert_ne|debug_assert|prop_assert|insta::assert_\w+)!|\.assert\s*\(\s*\)|^\s*\.(success|failure)\s*\(\s*\)|^\s*\.code\s*\(\s*([^\s)]|$)|^\s*\.(stdout|stderr)\s*\((?!\s*(?:\w+::)*Stdio::)/;

for (const { file, text } of added) {
  if (isRust(file) && SUPPRESSIONS.test(text)) flag('silenced-checker', file, text);
  if (isRust(file) && STUBS.test(text)) flag('unfinished-work', file, text);
  if (isRust(file) && SKIPS.test(text)) flag('test-made-easier', file, text);
  if (isConstraints(file) && /^\| *(W|E)\d+ *\|/.test(text)) flag('new-exception', file, text);
}

// 2b. A test file deleted (under tests/, or any Rust file with a test
// attribute: `#[test]`, `#[tokio::test]`), or an assertion removed from
// Rust code that still exists (unit tests sit in the same file as the code).
// A rename is a deletion here, so moving a src file that holds tests is
// reported too, for the reviewer.
const TEST_ATTR = /#\[\s*(?:\w+::)*test\b/;
const heldTests = (f) => removed.some((l) => l.file === f && TEST_ATTR.test(l.text));
for (const f of deleted) if (isTest(f) || (isRust(f) && heldTests(f))) flag('test-deleted', f, 'file deleted');
// A test removed from a file that stays: more test-attribute lines (a test
// attribute, or the `#[cfg(test)]` of a test module) removed than added.
// A test renamed or moved within its file, or given another test attribute,
// is clean; a test moved to another file is reported, for the reviewer.
// An added line that is a `//` comment adds no test, so a test commented
// out line by line is reported.
const TEST_LINE = /#\[\s*(?:\w+::)*test\b|#\[\s*cfg\s*\(\s*test\s*\)\s*\]/;
const testLines = (lines, f) => lines.filter((l) => l.file === f && !(lines === added && l.text.trim().startsWith('//'))
  && TEST_LINE.test(l.text)).length;
for (const f of new Set(removed.map((l) => l.file))) {
  if (!isRust(f) || deleted.includes(f)) continue;
  const [gone, come] = [testLines(removed, f), testLines(added, f)];
  if (gone > come) flag('test-removed', f, gone + ' test attribute line(s) removed, ' + come + ' added');
}
for (const { file, text } of removed) {
  if (isRust(file) && !deleted.includes(file) && ASSERTS.test(text)) {
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
    // of branches") reads the same way, and needs the maintainer's
    // approval like a loosening (ADR-029). An appended
    // number with no direction word of its own isn't seen.
    const loosest = dir === 'min' ? Math.min(...was.map((b) => b.n)) : Math.max(...was.map((b) => b.n));
    const looser = (n) => (dir === 'min' ? n.n < loosest : n.n > loosest);
    if (!verdict && dir && was.length && now.slice(was.length).some(looser)) verdict = 'threshold-loosened';
  }
  if (verdict) flag(verdict, r.file, r.text + '  ->  ' + a.text);
}

// 2d. A test that discards a child's stdout or stderr (the maintainer,
// 2026-11-25): an added `.stdout(Stdio::null())` or `.stderr(..)` in test
// code, unless a comment says why, on its line after the call or on the
// comment line just above it, or above the child's other stdio steps
// (`.stdin(..)`, `.stdout(..)`, `.stderr(..)` with a `Stdio`) next to it;
// a comment needs a word, and any word passes: the reviewer judges it.
// Test code: a file under tests/, a `_test.rs` or a `tests.rs`, or a line
// inside a `#[cfg(test)] mod name { .. }` (to its closing brace at the
// `mod` line's indentation). Unseen: a discard through a variable (`let
// null = Stdio::null()`) or split over lines (rustfmt keeps it on one), an
// out-of-line test module's own file (`#[cfg(test)] mod x;`) and the
// `testing` features' support files, and a reason removed from beside a
// discard that stays.
const DISCARDS = /\.(stdout|stderr)\s*\(\s*(?:\w+::)*Stdio::null\s*\(\s*\)\s*\)/;
const STDIO_STEP = /^\s*\.(stdin|stdout|stderr)\s*\(\s*(?:\w+::)*Stdio::\w+\s*\(\s*\)\s*\)\s*;?\s*$/;
const REASON_LINE = /^\s*\/\/+\s*\S/;
const REASON_AFTER = /\)\s*;?\s*\/\/+\s*\S/;
const TEST_MODULE = /^\s*#\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*$/;
const INLINE_MODULE = /^(\s*)(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{\s*$/;
const isTestFile = (f) => isTest(f) || /(^|\/)tests\.rs$/.test(f);
// The line-number ranges (1-based, exclusive) of `f`'s inline test
// modules: from a `#[cfg(test)]` whose next code line opens `mod name {`
// to the `}` at that line's indentation.
const testModules = new Map();
const inTestModule = (f, lines, n) => {
  if (!testModules.has(f)) {
    const ranges = [];
    for (let i = 0; i < lines.length; i++) {
      if (!TEST_MODULE.test(lines[i])) continue;
      let j = i + 1;
      while (j < lines.length && (lines[j].trim() === '' || /^\s*#\[/.test(lines[j]))) j++;
      const m = INLINE_MODULE.exec(lines[j] ?? '');
      if (!m) continue;
      let k = j + 1;
      while (k < lines.length && lines[k].trimEnd() !== m[1] + '}') k++;
      ranges.push([j + 1, k + 1]);
    }
    testModules.set(f, ranges);
  }
  return testModules.get(f).some(([from, to]) => n > from && n < to);
};
// The work tree's lines of `f` (none when it is gone), read once.
const workLines = new Map();
const workText = (f) => {
  if (!workLines.has(f)) {
    let content = null;
    try { content = readFileSync(f, 'utf8'); } catch (e) { if (e.code !== 'ENOENT') bail('could not read ' + f); }
    workLines.set(f, content === null ? [] : content.split('\n'));
  }
  return workLines.get(f);
};
for (const { file, text, line } of added) {
  if (!isRust(file) || !DISCARDS.test(text) || REASON_AFTER.test(text)) continue;
  const lines = workText(file);
  if (!isTestFile(file) && !inTestModule(file, lines, line)) continue;
  let above = line - 2;
  while (above >= 0 && STDIO_STEP.test(lines[above])) above--;
  if (above >= 0 && REASON_LINE.test(lines[above])) continue;
  flag('output-discarded', file, text);
}

// 2e. A skip by hand: an added early return in test code (as 2d reads it)
// whose condition reads the root ask or the euid (`SB_TEST_ROOT`,
// `effective_uid`, `geteuid`, `is_root`), outside sb-cli-core's testing.rs.
// The tests skip through that module (`testing::root_tests`,
// `testing::skip`), which lists each skip for the gate; a hand-rolled one
// passes unseen. The condition's line holds `if`, `else` (a `let ... else`
// too) or `match`; the return is on that line or in the next few, up to
// the block's closing brace, or the line alone when its braces close there.
// Reported for the reviewer, not blocking: a test that only branches on the
// euid can still be right. Not seen: a skip whose condition reads something
// else (a variable set from the euid), a return added to a block that was
// there, one past an earlier arm's block body or in an `else` block, and
// a name rustfmt moved off the condition's line.
const SKIP_CONDITION = /^(?=.*\b(if|else|match)\b)(?=.*\b(SB_TEST_ROOT|effective_uid|geteuid|is_root)\b)/;
const SKIP_HELPER = 'crates/cli-core/src/testing.rs';
const uncommented = (l) => l.replace(/\/\/.*$/, '');
for (const { file, text, line } of added) {
  const code = uncommented(text);
  if (!isRust(file) || file === SKIP_HELPER || !SKIP_CONDITION.test(code)) continue;
  const lines = workText(file);
  if (!isTestFile(file) && !inTestModule(file, lines, line)) continue;
  // A block opened and closed on the condition's line is all of it; else
  // the lines after it, up to the block's closing brace (`line` is
  // 1-based, so lines[line] is the one after it).
  const block = [code];
  const opens = (code.match(/\{/g) ?? []).length;
  if (opens === 0 || opens !== (code.match(/\}/g) ?? []).length) {
    for (let n = line; n < Math.min(lines.length, line + 5); n++) {
      block.push(uncommented(lines[n]));
      if (/^\s*\}/.test(lines[n])) break;
    }
  }
  if (block.some((l) => /\breturn\b/.test(l))) flag('skip-added', file, text);
}

// 2f. A test that reads the host (the maintainer, 2026-11-26: no test
// depends on the host's data files, configuration or environment): an
// added line in test code (as 2d reads it) that names a host path in a
// string (`"/usr/share/.."`, `"/etc/.."`, `"/proc/.."`, `"/sys/.."`,
// `"/var/.."`, `"/run/.."`, each directory alone too), the home directory
// (`"HOME"`, `home_dir(`) or an environment variable it reads
// (`env::var(`, `env::var_os(`, `env::vars(`), unless a comment says why:
// after the code on its line, or on the comment line above it or above
// the statement it continues (the lines before it whose code ends in no
// `;`, `,`, `{` or `}`); a comment needs a word, and any word passes: the reviewer
// judges it. sb-cli-core's testing.rs, which reads the gate's own
// switches (SB_TEST_ROOT, SB_TEST_SKIPS) for every test, is outside it.
// Not seen: a path built from parts or held in a constant
// named elsewhere, a read through a helper, the shell and node tests, and
// a reason removed from beside a read that stays; `std::env::temp_dir()`
// (TMPDIR: the run's own in the gate); a read in a shell or node test.
const HOST_READ = /"\/(?:usr\/share|etc|proc|sys|var|run)(?:\/[^"]*)?"|"HOME"|\bhome_dir\s*\(|\benv::vars?(?:_os)?\s*\(/;
const TRAILING_REASON = /\/\/+\s*\S[^"]*$/;
const STATEMENT_END = /[;,{}]\s*$/;
for (const { file, text, line } of added) {
  if (!isRust(file) || file === SKIP_HELPER || !HOST_READ.test(uncommented(text)) || TRAILING_REASON.test(text)) continue;
  const lines = workText(file);
  if (!isTestFile(file) && !inTestModule(file, lines, line)) continue;
  let above = line - 2;
  while (above >= 0 && !REASON_LINE.test(lines[above]) && lines[above].trim() !== ''
    && !STATEMENT_END.test(uncommented(lines[above]))) above--;
  if (above >= 0 && REASON_LINE.test(lines[above])) continue;
  flag('host-read', file, text);
}

// 5. The gate files: the files outside Rust and CONSTRAINTS.md that hold
// the bar, this guard's own script included. Each is compared with its
// copy at the merge base. The guard's tests are not among them (ADR-029):
// check.sh runs them, and a review reads their changes.
const GATE_SCRIPTS = ['scripts/check.sh', 'scripts/floor-guard.mjs', 'scripts/check-commit-messages.sh'];
// Files that configure a tool the gate runs, at the root or in a crate.
// - clippy reads `clippy.toml` or `.clippy.toml` from the crate's directory
//   (CARGO_MANIFEST_DIR), then from each parent:
//   https://doc.rust-lang.org/clippy/configuration.html
//   UNVERIFIED which of the two names wins in one directory (the page
//   doesn't say), so both are watched.
// - rustup reads `rust-toolchain` before `rust-toolchain.toml` when a
//   directory holds both: https://rust-lang.github.io/rustup/overrides.html
// - .gitattributes can hide a file's changes from a diff.
// - nextest reads `.config/nextest.toml` at the workspace root; its
//   `mutants` profile sets the mutation passes' per-test timeout (ADR-021):
//   https://nexte.st/docs/configuration/
const TOOL_CONFIGS = ['.cargo/mutants.toml', '.cargo/config.toml', '.cargo/config', 'clippy.toml', '.clippy.toml',
  '.gitleaksignore', '.gitleaks.toml', 'rust-toolchain', '.gitattributes', '.config/nextest.toml'];
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
  if (isGateScript(f) || isTestScript(f) || f === 'deny.toml' || f === 'rust-toolchain.toml'
    || f === 'clippy.toml') {
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

// 5c. The gate scripts: a line of code that is gone or changed (a removed
// step, rule or pattern), reported for the reviewer. A threshold is 5b's
// alone; a line that loses a lint level is reported here and is 5j's
// blocking finding as well.
const taken = new Set();
for (const r of removed) {
  const { file, text } = r;
  if (!isGateScript(file) || deleted.includes(file) || !isCode(file, text) || limit(text)) continue;
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
//   - The guard's tests are not a gate script: their fixtures quote these
//     patterns.
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

// 5j. A lint level on a gate script's cargo, clippy or rustc command line
// (RUSTFLAGS included): fewer of a `-D <lint>` (`-D warnings` among them)
// than at the merge base, or more of an `-A <lint>`. The short forms with
// or without a space (`-Dwarnings`, `"-A warnings"`, `=-Awarnings`) and the
// long ones (`--deny`, `--allow`, `--warn`, `--force-warn`, with a space
// or `=`) count; a `#` comment after the command doesn't. Counted over the
// file's lines of code, a line continued with a backslash read as one, so
// a command rewrapped or moved to another line is clean. Not seen: flags
// kept in a variable or an array.
const LINT_FLAG = /(?:^|[\s"'=])(?:-([DAW])\s*|--(deny|allow|warn|force-warn)(?:\s+|=))([a-z][\w:-]*)/g;
const LONG = { deny: 'D', allow: 'A', warn: 'W', 'force-warn': 'W' };
const lintFlags = (text, f) => {
  const count = new Map();
  for (const raw of (text ?? '').replace(/\\\n/g, ' ').split('\n')) {
    const line = raw.replace(/(^|\s)#.*$/, '');
    if (!isCode(f, raw) || !/\b(cargo|clippy|rustc|RUSTFLAGS)\b/.test(line)) continue;
    for (const [, short, long, lint] of line.matchAll(LINT_FLAG)) {
      const k = '-' + (short ?? LONG[long]) + ' ' + lint;
      count.set(k, (count.get(k) ?? 0) + 1);
    }
  }
  return count;
};
for (const f of changed.filter(isGateScript)) {
  if (newText(f) === null) continue;
  const was = lintFlags(oldText(f), f), now = lintFlags(newText(f), f);
  for (const k of new Set([...was.keys(), ...now.keys()])) {
    const [a, b] = [was.get(k) ?? 0, now.get(k) ?? 0];
    if ((k.startsWith('-D') && b < a) || (k.startsWith('-A') && b > a)) flag('lint-weakened', f, k + ': ' + a + ' -> ' + b);
  }
}

for (const f of changed) {
  const now = newText(f);
  if (now === null) continue;
  // 5f. A tool configuration file that is new (one finding for the file),
  // or has new lines (one finding for each): each can switch a check off
  // (mutants to skip, lint settings, ignored secrets, build flags, another
  // toolchain, hidden diffs).
  if (isToolConfig(f)) {
    if (oldText(f) === null) flag('tool-config-added', f, 'file added');
    else for (const a of added) if (a.file === f && isCode(f, a.text)) flag('tool-config-changed', f, a.text);
  }
  // 5f'. A clippy.toml's disallowed-* lists hold a rule (CONSTRAINTS.md,
  // Floor: no child process started outside sb-exec), so a removed list or
  // entry loosens it; other removed settings stay clean, as in any tool
  // config.
  if (/(^|\/)\.?clippy\.toml$/.test(f)) {
    for (const r of removed) {
      if (r.file === f && isCode(f, r.text) && /disallowed-|\bpath\s*=/.test(r.text)) flag('clippy-rule-removed', f, r.text);
    }
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

// 5k. The workflow linters' suppressions (the maintainer, 2026-11-24):
// zizmor's settings, read from .github/zizmor.yml or .yaml, or zizmor.yml
// or .yaml at the root (https://docs.zizmor.sh/configuration/). They hold
// ignores and pin policies, so any edit to one blocks: a line of code
// added or removed (a policy taken out loosens as well), the file added or
// deleted. And a `# zizmor: ignore[...]` comment added anywhere under
// .github/, a moved one included, since it then silences another line
// (https://docs.zizmor.sh/usage/#ignoring-results). A removed ignore
// comment tightens, and is clean. check-jsonschema, the other workflow
// linter, reads no settings file and has no suppression comment: its
// schemas are built in, and its options are check.sh's lines.
const LINTER_CONFIGS = ['.github/zizmor.yml', '.github/zizmor.yaml', 'zizmor.yml', 'zizmor.yaml'];
const ZIZMOR_IGNORE = /\bzizmor\s*:\s*ignore\b/;
for (const f of changed.filter((x) => LINTER_CONFIGS.includes(x))) {
  if (oldText(f) === null) flag('linter-config-changed', f, 'file added');
  else if (deleted.includes(f)) flag('linter-config-changed', f, 'file deleted');
  else for (const l of [...added, ...removed]) if (l.file === f && isCode(f, l.text)) flag('linter-config-changed', f, l.text);
}
for (const { file, text } of added) {
  if (file.startsWith('.github/') && ZIZMOR_IGNORE.test(text)) flag('zizmor-ignore-added', file, text);
}

// 6. The reported findings (ADR-029): printed for the code reviewer, who
// judges each against the commit message; they never fail the run.
const REPORTED = ['assertion-removed', 'test-removed', 'test-deleted', 'gate-line-removed', 'skip-added'];
const reported = findings.filter((f) => REPORTED.includes(f.rule));
findings.splice(0, findings.length, ...findings.filter((f) => !REPORTED.includes(f.rule)));
if (reported.length) {
  console.log('floor-guard: ' + reported.length + ' finding(s) for the code reviewer to judge (not blocking):');
  for (const f of reported) console.log(`  [${f.rule}] ${f.file}: ${f.text}`);
}

// 7. Approved exceptions (ADR-029). A blocking finding passes when the
// maintainer approved it: a line
//   Floor-approved: <rule> <path> — <reason> (the maintainer, <date>)
// in the message of a commit after the merge base or, for work not yet
// committed, in the FLOOR_APPROVED environment variable (one such line a
// line), the line the commit will carry. It covers every finding of that
// rule in that file in the checked diff. A line with `Floor-approved:` at
// its start, indented or not, in any other form fails the run (the form
// is check-commit-messages.sh's), and so does an approval of a rule that
// is only reported. An approval that covers nothing is printed, so a
// stale FLOOR_APPROVED shows.
const APPROVAL = /^Floor-approved: ([^ ]+) ([^ ]+) — ([^ ].*) \(the maintainer, \d{4}-\d{2}-\d{2}\)$/;
const messages = git(['log', '--format=%B', mergeBase + '..HEAD']);
if (messages === null) bail('could not read the commit messages after ' + mergeBase);
const approvals = [...messages.split('\n'), ...(process.env.FLOOR_APPROVED ?? '').split('\n')]
  .map((l) => l.trimEnd()).filter((l) => /^\s*Floor-approved:/.test(l));
const approved = [], malformed = [];
for (const line of approvals) {
  const m = APPROVAL.exec(line);
  if (!m || REPORTED.includes(m[1])) { malformed.push(line); continue; }
  const before = approved.length;
  for (let i = findings.length - 1; i >= 0; i--) {
    if (findings[i].rule === m[1] && findings[i].file === m[2]) approved.unshift({ ...findings.splice(i, 1)[0], line });
  }
  if (approved.length === before) console.log('floor-guard: an approval that covers no finding: ' + line);
}
// After the matching, so that no approval covers one.
for (const line of malformed) flag('approval-malformed', 'Floor-approved', line);
if (approved.length) {
  console.log('floor-guard: ' + approved.length + ' blocking finding(s) the maintainer approved:');
  for (const f of approved) console.log(`  approved [${f.rule}] ${f.file}: ${f.text}\n      ${f.line}`);
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
console.error('\nEach is a move that lowers the bar. Fix the code. An exception needs the maintainer\'s approval: ask first,'
  + ' then carry "Floor-approved: <rule> <path> — <reason> (the maintainer, <date>)" in the commit message,'
  + ' and in FLOOR_APPROVED until it is committed (ADR-029).');
process.exit(1);
