#!/usr/bin/env node
// check-plan-ids.mjs: every finding id is planned, and the plan and the
// task list cite only real ids (hardening plan, Task 5; run at each
// checkpoint).
//
// Usage: node scripts/check-plan-ids.mjs <findings> <plan> <todo>
//   for example tasks/findings.md tasks/plan.md tasks/todo.md.
// Checks:
// - each id the findings file defines is in a table row of the plan's
//   "## Traceability" section, and is cited in a task section of the todo;
// - every id in those table rows, and anywhere in the todo, is defined;
// - the findings file defines each id once, and has no bullet that reads as
//   an id without defining one (`- docs-C1:`, `  - docs-M7:`,
//   `- **docs-M9**:`);
// - every todo heading that starts with the word Task is a task heading.
// Definitions:
// - an id is `<area>-<rating><n>`: a lower-case area, a rating of H, M, L,
//   I, T or N, and a number (docs-M3, san-T1);
// - the findings file defines an id on a line `- <id>`, at the start of
//   the line; an id named later on a line ("same as san-H1") defines
//   nothing;
// - a section runs from its `## ` heading, which it includes, to the next
//   `## ` heading, so it takes in its `### ` sub-sections; a task section
//   is one headed `## Task <n>:`. Fenced code blocks aren't treated
//   specially: a `## ` line inside one would end the section.
// Exit: 0 consistent (one line on stdout), 1 a problem (each one listed on
// stderr), 2 bad arguments or a file that can't be read.
import { readFileSync } from 'node:fs';

const fail = (msg) => { console.error('check-plan-ids: ' + msg); process.exit(2); };

const args = process.argv.slice(2);
if (args.length !== 3) fail('usage: node scripts/check-plan-ids.mjs <findings> <plan> <todo>');
const [findingsPath, planPath, todoPath] = args;
const read = (f) => {
  try { return readFileSync(f, 'utf8'); }
  catch (e) { return fail('could not read ' + f + ': ' + e.code); }
};
const [findingsText, planText, todoText] = [findingsPath, planPath, todoPath].map(read);

const ID = /(?<![\w-])[a-z]+-[HMLITN]\d+(?!\w)/g;
const DEFINED = /^- ([a-z]+-[HMLITN]\d+)(?!\w)/;
// A bullet whose first word reads as an id: a word, a dash, a capital and
// a digit, after any bold or code marks.
const LOOKS_DEFINED = /^\s*[-*+]\s+[*_`]*[a-z][\w-]*-[A-Z]\d/;
const TASK = /^## (Task \d+):/;
const TASK_LIKE = /^#{1,6}\s*Task\b/;
const ids = (text) => new Set(text.match(ID) ?? []);
const quote = (line) => '"' + line.trim().slice(0, 80) + '"';

// The file's `## ` sections, each as its name and its lines (the heading
// first). The lines before the first heading are the preamble.
const sections = (text) => {
  const out = [{ heading: null, lines: [] }];
  for (const line of text.split('\n')) {
    if (/^## /.test(line)) out.push({ heading: line, lines: [] });
    out[out.length - 1].lines.push(line);
  }
  return out;
};

const problems = [];

const defined = new Map();
findingsText.split('\n').forEach((line, i) => {
  const m = DEFINED.exec(line);
  if (m) defined.set(m[1], [...(defined.get(m[1]) ?? []), i + 1]);
  else if (LOOKS_DEFINED.test(line)) {
    problems.push(findingsPath + ':' + (i + 1) + ': ' + quote(line) + ' looks like an id but defines none'
      + ' (a definition is "- <area>-<rating><n>")');
  }
});
if (defined.size === 0) problems.push(findingsPath + ': defines no id (no line "- <area>-<rating><n>")');
for (const [id, lines] of defined) {
  if (lines.length > 1) problems.push(id + ': defined more than once in ' + findingsPath + ' (lines ' + lines.join(', ') + ')');
}

const traceability = sections(planText).filter((s) => /^## Traceability\s*$/.test(s.heading ?? ''));
if (traceability.length === 0) problems.push(planPath + ': no "## Traceability" section');
const traced = ids(traceability.flatMap((s) => s.lines.filter((l) => /^\s*\|/.test(l))).join('\n'));
for (const id of traced) {
  if (!defined.has(id)) problems.push(id + ": in the plan's traceability tables but not defined in " + findingsPath);
}

todoText.split('\n').forEach((line, i) => {
  if (TASK_LIKE.test(line) && !TASK.test(line)) {
    problems.push(todoPath + ':' + (i + 1) + ': ' + quote(line) + ' is not a task heading of the form "## Task N:"');
  }
});
const cited = new Set();
let tasks = 0;
for (const s of sections(todoText)) {
  const task = TASK.exec(s.heading ?? '')?.[1];
  if (task) tasks++;
  const name = task ?? (s.heading === null ? 'the preamble' : '"' + s.heading.trim() + '"');
  for (const id of ids(s.lines.join('\n'))) {
    if (task) cited.add(id);
    if (!defined.has(id)) problems.push(id + ': cited in ' + name + ' but not defined in ' + findingsPath);
  }
}
if (tasks === 0) problems.push(todoPath + ': no "## Task N:" section');

for (const id of defined.keys()) {
  if (!traced.has(id)) problems.push(id + ": not in the plan's traceability tables (" + planPath + ')');
  if (!cited.has(id)) problems.push(id + ': not cited in any task (' + todoPath + ')');
}

if (problems.length) {
  console.error('check-plan-ids: ' + problems.length + ' problem(s):');
  for (const p of problems) console.error('  ' + p);
  process.exit(1);
}
console.log('check-plan-ids: ' + defined.size + ' ids, each in the traceability tables and in a task; '
  + 'the plan and the ' + tasks + ' tasks cite no unknown id');
