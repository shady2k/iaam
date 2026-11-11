#!/usr/bin/env node
// Commit messages -> the input of .backlog/rules/check-commits.mjs, then the check.
//
// THE CONVENTION: a commit names its task(s) in parentheses, as this repository
// has always done: `Carry the blocked quantity (iaam-1u7b)` or `(iaam-a, iaam-b.2)`.
// Only ids inside parentheses count, so a crate named in prose (`iaam-core`) is
// never read as a task. The task must exist in the tracker's store (closed ones
// included) and be a leaf: an epic, or an issue with children, is refused. The
// same parser (adapter.mjs `taskIds`) feeds the backlog gate's `landed` list.
//
// A MERGE that names no task carries the links of the commits it brings in; a
// merge of commits that name none is refused like any other unlinked commit.
// A revert keeps the reverted subject, and with it the link.
//
//   commits.mjs --message-file <file>   the pending message (commit-msg hook)
//   commits.mjs --range <base>..<head>  every commit the range introduces
//   commits.mjs --push <remote>         pre-push: ref lines on stdin, as git gives them
//
// WHAT A PUSH INTRODUCES, per pushed ref: the commits of its tip (a tag peeled to
// its commit) that no ref the remote already has reaches. A remote ref whose
// object this clone does not hold bounds nothing here and is skipped. A ref that
// introduces nothing (a tag or a new branch on a commit the remote holds) passes
// with "introduces no commits: nothing to check". A hook given no ref lines, or a
// range git cannot compute, is refused.
//
// Each commit carries the paths it changed, and the config's `codePaths` is
// passed through; an empty list counts nothing as code (iaam-ima7z: off until
// the document gate, iaam-46x4, can admit a change record).
//
// Exit 0 linked, 1 a commit without a valid link, 2 cannot check (never a pass).
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { read, taskIds } from './adapter.mjs';

export { taskIds };
const HERE = dirname(fileURLToPath(import.meta.url));
const git = (...a) => execFileSync('git', a, { encoding: 'utf8', maxBuffer: 1 << 28, stdio: ['ignore', 'pipe', 'pipe'] }).trim();
const ZERO = /^0+$/;

function linksOfRange(range) {
  const out = git('log', '--format=%B%x00', range);
  return [...new Set(out.split('\0').flatMap(taskIds))];
}

function pathsOf(sha) {
  // --root lists a root commit's files; -m with --first-parent gives a merge's
  // change against its first parent.
  return git('diff-tree', '--no-commit-id', '--name-only', '-r', '--root', '-m', '--first-parent', sha).split('\n').filter(Boolean);
}

function pending(file) {
  const message = readFileSync(file, 'utf8');
  let ids = taskIds(message);
  const mergeHead = join(git('rev-parse', '--git-dir'), 'MERGE_HEAD');
  if (!ids.length && existsSync(mergeHead)) ids = linksOfRange(`HEAD..${readFileSync(mergeHead, 'utf8').trim()}`);
  const paths = git('diff', '--cached', '--name-only').split('\n').filter(Boolean);
  return [{ id: 'pending-message', taskIds: ids, paths }];
}

function commitsOf(shas) {
  return shas.map((sha) => {
    let ids = taskIds(git('log', '-1', '--format=%B', sha));
    const parents = git('log', '-1', '--format=%P', sha).split(' ');
    if (!ids.length && parents.length > 1) ids = linksOfRange(`${parents[0]}..${parents[1]}`);
    return { id: sha, taskIds: ids, paths: pathsOf(sha) };
  });
}

function range(r) {
  const shas = git('rev-list', '--reverse', r).split('\n').filter(Boolean);
  if (!shas.length) throw new Error(`the range ${r} introduces no commits; refusing to report an empty success`);
  return commitsOf(shas);
}

function push(remote) {
  const lines = readFileSync(0, 'utf8').split('\n').map((l) => l.trim()).filter(Boolean);
  if (!lines.length) throw new Error('the pre-push hook was given no ref lines; refusing to report an empty success');
  // The remote's refs, as the remote has them now; a ref whose object is not here
  // cannot reach a local commit, so it bounds nothing and is skipped.
  const have = git('ls-remote', remote).split('\n').filter(Boolean).map((l) => l.split(/\s+/)[0])
    .filter((oid) => spawnSync('git', ['cat-file', '-e', `${oid}^{commit}`]).status === 0);
  const shas = [];
  for (const line of lines) {
    const [localRef, localOid, remoteRef] = line.split(/\s+/);
    if (!localOid || ZERO.test(localOid)) continue; // a deletion introduces nothing
    const tip = git('rev-parse', '--verify', `${localOid}^{commit}`);
    const introduced = git('rev-list', '--reverse', tip, '--not', ...have).split('\n').filter(Boolean);
    if (!introduced.length) console.error(`${remoteRef || localRef}: introduces no commits: nothing to check`);
    for (const s of introduced) if (!shas.includes(s)) shas.push(s);
  }
  return commitsOf(shas);
}

function main(argv) {
  const mf = argv.indexOf('--message-file');
  const rg = argv.indexOf('--range');
  const ps = argv.indexOf('--push');
  if ([mf, rg, ps].filter((i) => i >= 0).length !== 1)
    throw new Error('give exactly one of --message-file <file>, --range <base>..<head> or --push <remote>');
  const commits = mf >= 0 ? pending(argv[mf + 1]) : rg >= 0 ? range(argv[rg + 1]) : push(argv[ps + 1]);
  if (!commits.length) return 0; // every pushed ref introduced nothing, and each said so
  const config = JSON.parse(readFileSync(join(HERE, 'config.json'), 'utf8'));
  const issues = read(['--or-offline']).issues.map(({ id, type, parent, body }) => ({ id, type, parent, body }));
  const res = spawnSync(process.execPath, [join(HERE, 'rules', 'check-commits.mjs'), '-'], {
    input: JSON.stringify({ codePaths: config.codePaths || [], issues, commits }),
    encoding: 'utf8',
    maxBuffer: 1 << 28,
  });
  if (res.status === 0) return 0;
  if (res.status !== 1) {
    console.error(`commit-link check could not run: ${res.stderr || res.stdout}`);
    return 2;
  }
  const { violations } = JSON.parse(res.stdout);
  console.error(`${ps >= 0 ? 'PUSH' : 'COMMIT'} REFUSED: every commit names the tracked task it belongs to.`);
  for (const v of violations) {
    const what = { 'no task link': 'names no task', 'task does not exist': `names ${v.task}, which is not in the tracker`,
      'link must name a leaf task, not a container': `names ${v.task}, which is an epic or has children; name the leaf task instead` }[v.reason] || `${v.task ? `${v.task}: ` : ''}${v.reason}`;
    console.error(`  ${v.id.slice(0, 12)}: ${what}`);
  }
  console.error('Fix: put the task id in parentheses in the message, e.g. "Fix the thing (iaam-abcd)".');
  if (ps >= 0) console.error('For a commit already made: git commit --amend, or git rebase to reword it, then push again.');
  console.error('No task yet? File one first through the to-backlog skill.');
  return 1;
}

try {
  process.exitCode = main(process.argv.slice(2));
} catch (e) {
  console.error(`commit-link check could not run: ${e.message.split('\n')[0]}`);
  console.error('This refuses rather than passing unchecked. Connect the clone: make backlog-connect');
  process.exitCode = 2;
}
