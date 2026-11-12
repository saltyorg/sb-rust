// Tests for check-plan-ids.mjs: `node --test scripts/check-plan-ids.test.mjs`
// (scripts/check.sh fast runs it). Each case writes a findings file, a plan
// and a task list to a scratch directory and runs the script on them.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const script = join(dirname(fileURLToPath(import.meta.url)), 'check-plan-ids.mjs');

// run(files, args): the script's exit status and output. `files` maps a
// name to its content; `args` defaults to the three standard names.
const run = (files, args = ['findings.md', 'plan.md', 'todo.md']) => {
  const dir = mkdtempSync(join(tmpdir(), 'check-plan-ids-'));
  try {
    for (const [f, text] of Object.entries(files)) writeFileSync(join(dir, f), text);
    const r = spawnSync(process.execPath, [script, ...args], { cwd: dir, encoding: 'utf8' });
    return { status: r.status, stdout: r.stdout, stderr: r.stderr };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

const findings = '# Findings\n\n## docs\n- docs-H1: the guard is blind (same as sb-L9).\n- docs-L2 reasoned: a stale line.\n';
const plan = '# Plan\n\n## Traceability\n\n### High\n\n| Ids | Task |\n|---|---|\n| docs-H1 | 1 |\n\n'
  + '### Low\n\n| Ids | Task |\n|---|---|\n| docs-L2 (its half) | 2 |\n\n## Dependency graph\n';
const todo = '# Tasks\n\n## Task 1: the guard\n\nFixes docs-H1.\n\n## Task 2: a sweep\n\n- docs-L2 **fix**: the line.\n\n'
  + '## Checkpoint A: gates\n- [ ] done\n';

test('consistent files pass with a one-line summary', () => {
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': todo });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout.trim().split('\n').length, 1);
  assert.match(r.stdout, /2 ids/);
  assert.equal(r.stderr, '');
});

test('an id missing from the traceability tables fails, named', () => {
  const p = plan.replace('| docs-L2 (its half) | 2 |\n', '');
  const r = run({ 'findings.md': findings, 'plan.md': p, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-L2: not in the plan's traceability tables/);
  assert.doesNotMatch(r.stderr, /docs-H1/);
});

test('an id in plan prose outside the traceability tables does not count', () => {
  const p = plan.replace('| docs-L2 (its half) | 2 |\n', '')
    .replace('## Traceability\n', '## Overview\n\nTask 2 fixes docs-L2.\n\n## Traceability\n\nNot docs-L2 either.\n')
    .replace('## Dependency graph\n', '## Dependency graph\n\nnor docs-L2\n');
  const r = run({ 'findings.md': findings, 'plan.md': p, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-L2: not in the plan's traceability tables/);
});

test('a table after the traceability section does not count', () => {
  const p = plan.replace('| docs-L2 (its half) | 2 |\n', '') + '\n| Risk | docs-L2 |\n|---|---|\n';
  const r = run({ 'findings.md': findings, 'plan.md': p, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-L2: not in the plan's traceability tables/);
});

test('an id in no task section fails, named', () => {
  const t = todo.replace('- docs-L2 **fix**: the line.\n', '- nothing here\n');
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-L2: not cited in any task/);
});

test('an id only outside the task sections does not count', () => {
  const t = todo.replace('- docs-L2 **fix**: the line.\n', '- nothing here\n')
    .replace('# Tasks\n', '# Tasks\n\nThe sweeps list docs-L2.\n')
    + '- [ ] docs-L2 checked\n\n## Proposed declines\n\n- docs-L2: declined\n';
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-L2: not cited in any task/);
});

test('a task section ends at the next level-two heading, not at a level-three one', () => {
  const t = todo.replace('- docs-L2 **fix**: the line.\n', '### Notes\n\ndocs-L2 here.\n');
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 0, r.stderr);
});

test('a task citing an id the findings file does not define fails', () => {
  const t = todo.replace('Fixes docs-H1.', 'Fixes docs-H1 and docs-M9.');
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-M9: cited in Task 1 but not defined/);
});

test('an id mentioned in findings prose is not defined by it', () => {
  // sb-L9 appears in docs-H1's text ("same as sb-L9") but has no line of
  // its own.
  const t = todo.replace('Fixes docs-H1.', 'Fixes docs-H1 and sb-L9.');
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /sb-L9: cited in Task 1 but not defined/);
});

test('an id first on a prose or continuation line is not defined by it', () => {
  const f = findings + '  sb-L9 is named on a continuation line.\nSee sb-L8 for more.\n';
  const t = todo.replace('Fixes docs-H1.', 'Fixes docs-H1, sb-L9 and sb-L8.');
  const r = run({ 'findings.md': f, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /sb-L9: cited in Task 1 but not defined/);
  assert.match(r.stderr, /sb-L8: cited in Task 1 but not defined/);
});

test('an id inside a longer word is no citation', () => {
  // None of x_docs-H1, cli-core-M1 (no core-M1), docs-H1a cites an id.
  const f = findings + '- core-M1 x\n';
  const p = plan.replace('| docs-H1 | 1 |', '| docs-H1, core-M1 | 1 |');
  const t = todo.replace('Fixes docs-H1.', 'Fixes x_docs-H1, cli-core-M1 and docs-H1a.');
  const r = run({ 'findings.md': f, 'plan.md': p, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-H1: not cited in any task/);
  assert.match(r.stderr, /core-M1: not cited in any task/);
  assert.doesNotMatch(r.stderr, /not defined/);
});

test('every rating letter defines an id', () => {
  const f = '- a-H1 x\n- a-M1 x\n- a-L1 x\n- a-I1 x\n- a-T1 x\n- a-N1 x\n';
  const ids = ['a-H1', 'a-M1', 'a-L1', 'a-I1', 'a-T1', 'a-N1'];
  const p = '## Traceability\n\n| Ids | Task |\n|---|---|\n| ' + ids.join(', ') + ' | 1 |\n';
  const t = '## Task 1: all\n\n' + ids.join(', ') + '\n';
  const r = run({ 'findings.md': f, 'plan.md': p, 'todo.md': t });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /6 ids/);
});

// A findings bullet that reads as an id but doesn't define one is reported,
// so a mistyped definition can't drop out of the check unseen.
for (const line of ['- a-X1 another letter', '- docs-C1: no such rating', '- term-ui-M2: a dash in the area',
  '- docs-H1a: a letter after the number', '  - docs-M7: indented', '* docs-M8: a star bullet',
  '- **docs-M9**: in bold']) {
  test('a findings bullet that looks like an id but defines none is reported: ' + line.trim(), () => {
    const r = run({ 'findings.md': findings + line + '\n', 'plan.md': plan, 'todo.md': todo });
    assert.equal(r.status, 1);
    assert.ok(r.stderr.includes('findings.md:6: "' + line.trim() + '" looks like an id but defines none'), r.stderr);
  });
}

test('an indented bullet does not define its id', () => {
  const r = run({ 'findings.md': findings + '  - sb-L9 x\n', 'plan.md': plan,
    'todo.md': todo.replace('Fixes docs-H1.', 'Fixes docs-H1 and sb-L9.') });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /sb-L9: cited in Task 1 but not defined/);
});

test('an id defined twice is reported with both lines', () => {
  const r = run({ 'findings.md': findings + '- docs-H1 again\n', 'plan.md': plan, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-H1: defined more than once in findings\.md \(lines 4, 6\)/);
});

test('the real findings bullets define what they name', () => {
  // The shapes tasks/findings.md uses: an id, then a status or a colon.
  const f = '## docker (done)\n- docker-M1 CONFIRMED (same as san-H1): x\n- docker-L1: y\n- file-L5 weak tests: z\n';
  const ids = ['docker-M1', 'docker-L1', 'file-L5'];
  const p = '## Traceability\n\n| Ids | Task |\n|---|---|\n| ' + ids.join(', ') + ' | 1 |\n';
  const r = run({ 'findings.md': f, 'plan.md': p, 'todo.md': '## Task 1: all\n\n' + ids.join(', ') + '\n' });
  assert.equal(r.status, 0, r.stderr);
});

test('a prose line holding a pipe inside the traceability section does not count', () => {
  const p = plan.replace('| docs-L2 (its half) | 2 |\n', 'Not docs-L2 | nor this.\n');
  const r = run({ 'findings.md': findings, 'plan.md': p, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-L2: not in the plan's traceability tables/);
});

test('a section headed "## Traceability notes" is not the traceability section', () => {
  const p = plan.replace('| docs-L2 (its half) | 2 |\n', '')
    + '\n## Traceability notes\n\n| Ids | Task |\n|---|---|\n| docs-L2 | 2 |\n';
  const r = run({ 'findings.md': findings, 'plan.md': p, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-L2: not in the plan's traceability tables/);
});

test('an unknown id in a traceability row is reported', () => {
  const p = plan.replace('| docs-H1 | 1 |', '| docs-H1, docs-M9 | 1 |');
  const r = run({ 'findings.md': findings, 'plan.md': p, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-M9: in the plan's traceability tables but not defined in findings\.md/);
});

test('an unknown id anywhere in the task list is reported, with its section', () => {
  const t = todo.replace('# Tasks\n', '# Tasks\n\nSee docs-M7.\n').replace('- [ ] done\n', '- [ ] docs-M8 done\n')
    + '\n## Proposed declines\n\n- docs-M9: declined\n';
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-M7: cited in the preamble but not defined/);
  assert.match(r.stderr, /docs-M8: cited in "## Checkpoint A: gates" but not defined/);
  assert.match(r.stderr, /docs-M9: cited in "## Proposed declines" but not defined/);
});

test('an id in a task heading is cited by that task', () => {
  const t = todo.replace('## Task 2: a sweep', '## Task 2: a sweep (docs-L2)').replace('- docs-L2 **fix**: the line.\n', '');
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 0, r.stderr);
});

test('an unknown id in a task heading is reported', () => {
  const t = todo.replace('## Task 1: the guard', '## Task 1: the guard (docs-M9)');
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-M9: cited in Task 1 but not defined/);
});

// A heading that names a task in another form would leave its ids out of
// the task sections; it is reported, never skipped.
for (const heading of ['## Task 2 - a sweep', '## Task 2a: a sweep', '### Task 2: a sweep', '#Task 2: a sweep',
  '## Task: a sweep']) {
  test('a task heading in another form is reported: ' + heading, () => {
    const t = todo.replace('## Task 2: a sweep\n\n- docs-L2 **fix**: the line.\n',
      '## Task 2: a sweep\n\n- docs-L2 **fix**: the line.\n\n' + heading + '\n');
    const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
    assert.equal(r.status, 1);
    assert.ok(r.stderr.includes('todo.md:11: "' + heading + '" is not a task heading of the form "## Task N:"'), r.stderr);
  });
}

test('other headings that mention tasks are no task heading', () => {
  const t = todo + '\n## Tasks done\n\n### Taskbar notes\n';
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': t });
  assert.equal(r.status, 0, r.stderr);
});

test('every problem is listed, not only the first', () => {
  const p = plan.replace('| docs-H1 | 1 |\n', '').replace('| docs-L2 (its half) | 2 |\n', '');
  const t = todo.replace('Fixes docs-H1.', 'Fixes docs-M9.');
  const r = run({ 'findings.md': findings, 'plan.md': p, 'todo.md': t });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /docs-H1: not in the plan's traceability tables/);
  assert.match(r.stderr, /docs-L2: not in the plan's traceability tables/);
  assert.match(r.stderr, /docs-H1: not cited in any task/);
  assert.match(r.stderr, /docs-M9: cited in Task 1 but not defined/);
});

test('a findings file that defines no id fails rather than passing vacuously', () => {
  const r = run({ 'findings.md': '# Findings\n\nnone yet\n', 'plan.md': plan, 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /defines no id/);
});

test('a plan without a traceability section is reported as such', () => {
  const r = run({ 'findings.md': findings, 'plan.md': '# Plan\n', 'todo.md': todo });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /no "## Traceability" section/);
});

test('a task list without a task section is reported as such', () => {
  const r = run({ 'findings.md': findings, 'plan.md': plan, 'todo.md': '# Tasks\n\ndocs-H1 docs-L2\n' });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /no "## Task N:" section/);
});

test('a wrong number of arguments exits 2 with the usage', () => {
  const files = { 'findings.md': findings, 'plan.md': plan, 'todo.md': todo };
  for (const args of [[], ['findings.md', 'plan.md'], ['findings.md', 'plan.md', 'todo.md', 'extra']]) {
    const r = run(files, args);
    assert.equal(r.status, 2, args.join(' '));
    assert.match(r.stderr, /usage: /);
  }
});

test('an unreadable file exits 2, naming it', () => {
  const r = run({ 'findings.md': findings, 'plan.md': plan }, ['findings.md', 'plan.md', 'missing.md']);
  assert.equal(r.status, 2);
  assert.match(r.stderr, /missing\.md/);
});
