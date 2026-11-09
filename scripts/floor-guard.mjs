#!/usr/bin/env node
// floor-guard.mjs: diff-scoped enforcement of the CONSTRAINTS.md floor.
// Adapted from the constraint-driven-development skill's reference guard
// (agent-skills 0.6.13, skills/constraint-driven-development/references/
// floor-guard.md): same contract, plumbing and CONSTRAINTS.md checks; only
// the patterns are Rust's.
//
// Usage: node scripts/floor-guard.mjs [--base <ref>]
//   default base: HEAD (uncommitted and untracked work). Before handing back
//   committed work, pass the commit the task started from.
// Exit: 0 clean, 1 floor violation, 2 the guard could not run.
// Reports the rule and the location, never a matched value.
import { execFileSync } from 'node:child_process';

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

const tracked = git(['diff', '--unified=0', mergeBase, '--']);
if (tracked === null) bail('could not diff against ' + mergeBase);
const untrackedFiles = git(['ls-files', '--others', '--exclude-standard']);
if (untrackedFiles === null) bail('could not list untracked files');
const untracked = untrackedFiles.split('\n').filter(Boolean).map((f) => {
  const d = git(['diff', '--no-index', '--unified=0', '/dev/null', f], { diffExit: true });
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
// line, or as rustfmt splits it, with `expect(` opening a line of its own
// (no method call starts a line without its dot).
const SUPPRESSIONS = /#!?\[\s*(allow|expect)\s*\(|#!?\[\s*cfg_attr\s*\(.*[(,]\s*(allow|expect)\s*\(|^\s*(allow|expect)\s*\(|#\[\s*coverage\s*\(\s*off|#\[\s*mutants::skip|gitleaks:allow|deny\.toml.*ignore|clippy::all\s*=\s*"allow"/;
// 4. Unfinished work.
const STUBS = /\btodo!\s*\(|\bunimplemented!\s*\(|\bTODO\b|\bFIXME\b|\bXXX\b/;
// 2. A test made easier.
const SKIPS = /#\[\s*ignore\b|#\[\s*cfg_attr\s*\([^)]*ignore/;
const ASSERTS = /\b(assert|assert_eq|assert_ne|debug_assert|prop_assert|insta::assert_\w+)!/;

for (const { file, text } of added) {
  if (!guarded(file)) continue;
  if (isRust(file) && SUPPRESSIONS.test(text)) flag('silenced-checker', file, text);
  if (isRust(file) && STUBS.test(text)) flag('unfinished-work', file, text);
  if (isRust(file) && SKIPS.test(text)) flag('test-made-easier', file, text);
  if (isConstraints(file) && /^\| *(W|E)\d+ *\|/.test(text)) flag('new-exception', file, text);
}

// 2b. A test file deleted, or an assertion removed from Rust code that still
// exists (unit tests sit in the same file as the code).
for (const f of deleted) if (guarded(f) && isTest(f)) flag('test-deleted', f, 'file deleted');
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
  }
  if (verdict) flag(verdict, r.file, r.text + '  ->  ' + a.text);
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
