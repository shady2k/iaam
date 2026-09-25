#!/usr/bin/env node
// Commit messages -> the input of .backlog/rules/check-commits.mjs, then the check.
//
// THE CONVENTION: a commit names its task(s) in parentheses, as this repository
// has always done: `Carry the blocked quantity (iaam-1u7b)` or `(iaam-a, iaam-b.2)`.
// Only ids inside parentheses count, so a crate named in prose (`iaam-core`) is
// never read as a task. The task must exist (closed ones included) and be a leaf:
// an epic, or an issue with children, is refused.
//
// A MERGE that names no task carries the links of the commits it brings in; a
// merge of commits that name none is refused like any other unlinked commit.
// A revert keeps the reverted subject, and with it the link.
//
//   commits.mjs --message-file <file>   the pending message (commit-msg hook)
//   commits.mjs --range <base>..<head>  every commit the range introduces
//
// Exit 0 linked, 1 a commit without a valid link, 2 cannot check (never a pass).
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { read } from './adapter.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const git = (...a) => execFileSync('git', a, { encoding: 'utf8', maxBuffer: 1 << 28 }).trim();

export function taskIds(message) {
  const text = message.split('\n').filter((l) => !l.startsWith('#')).join('\n');
  const ids = [];
  for (const group of text.matchAll(/\(([^()]*)\)/g))
    for (const m of group[1].matchAll(/\biaam-[a-z0-9]+(?:\.[0-9]+)*\b/g)) ids.push(m[0]);
  return [...new Set(ids)];
}

function linksOfRange(range) {
  const out = git('log', '--format=%B%x00', range);
  return [...new Set(out.split('\0').flatMap(taskIds))];
}

function pending(file) {
  const message = readFileSync(file, 'utf8');
  let ids = taskIds(message);
  const mergeHead = join(git('rev-parse', '--git-dir'), 'MERGE_HEAD');
  if (!ids.length && existsSync(mergeHead)) ids = linksOfRange(`HEAD..${readFileSync(mergeHead, 'utf8').trim()}`);
  return [{ id: 'pending-message', taskIds: ids }];
}

function range(r) {
  const shas = git('rev-list', '--reverse', r).split('\n').filter(Boolean);
  if (!shas.length) throw new Error(`the range ${r} introduces no commits; refusing to report an empty success`);
  return shas.map((sha) => {
    let ids = taskIds(git('log', '-1', '--format=%B', sha));
    const parents = git('log', '-1', '--format=%P', sha).split(' ');
    if (!ids.length && parents.length > 1) ids = linksOfRange(`${parents[0]}..${parents[1]}`);
    return { id: sha, taskIds: ids };
  });
}

function main(argv) {
  const mf = argv.indexOf('--message-file');
  const rg = argv.indexOf('--range');
  if ((mf < 0) === (rg < 0)) throw new Error('give exactly one of --message-file <file> or --range <base>..<head>');
  const commits = mf >= 0 ? pending(argv[mf + 1]) : range(argv[rg + 1]);
  const issues = read([]).issues.map(({ id, type, parent }) => ({ id, type, parent }));
  const res = spawnSync(process.execPath, [join(HERE, 'rules', 'check-commits.mjs'), '-'], {
    input: JSON.stringify({ issues, commits }),
    encoding: 'utf8',
  });
  if (res.status === 0) return 0;
  if (res.status !== 1) {
    console.error(`commit-link check could not run: ${res.stderr || res.stdout}`);
    return 2;
  }
  const { violations } = JSON.parse(res.stdout);
  console.error('COMMIT REFUSED: every commit names the tracked task it belongs to.');
  for (const v of violations) {
    const what = { 'no task link': 'names no task', 'task does not exist': `names ${v.task}, which is not in the tracker`,
      'link must name a leaf task, not a container': `names ${v.task}, which is an epic or has children; name the leaf task instead` }[v.reason] || v.reason;
    console.error(`  ${v.id.slice(0, 12)}: ${what}`);
  }
  console.error('Fix: put the task id in parentheses in the message, e.g. "Fix the thing (iaam-abcd)".');
  console.error('No task yet? File one first (bd create ..., or the to-backlog skill).');
  return 1;
}

try {
  process.exitCode = main(process.argv.slice(2));
} catch (e) {
  console.error(`commit-link check could not run: ${e.message.split('\n')[0]}`);
  console.error('This refuses the commit rather than passing it unchecked. Connect the clone: make backlog-connect');
  process.exitCode = 2;
}
