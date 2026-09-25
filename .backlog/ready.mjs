#!/usr/bin/env node
// What a worker may take now, in one stage and one checkout. `bd ready` alone is
// not this: it offers submitted and implemented leaves again (beads keeps them
// `open`), and it cannot tell whether an implemented prerequisite is merged here.
//
//   ready.mjs [--stage <id>] [--checkout <rev>]   (checkout defaults to HEAD)
//
// Ready = an open, unheld leaf whose every prerequisite is closed, or is
// implemented in the SAME stage with its recorded revision already contained in
// the checkout. Deferred work and ideas are never ready.
import { execFileSync } from 'node:child_process';
import { read } from './adapter.mjs';

const argv = process.argv.slice(2);
const opt = (k) => (argv.indexOf(k) >= 0 ? argv[argv.indexOf(k) + 1] : null);
const stage = opt('--stage');
const checkout = opt('--checkout') || 'HEAD';

const { issues } = read([]);
const by = new Map(issues.map((i) => [i.id, i]));
const hasLiveChild = new Set(issues.filter((i) => i.parent && !['closed', 'deferred'].includes(i.status)).map((i) => i.parent));

const merged = (rev) => {
  // A recorded revision may be written `branch@sha`; the commit is what counts.
  const sha = String(rev || '').split('@').pop();
  if (!sha) return false;
  try {
    execFileSync('git', ['merge-base', '--is-ancestor', sha, checkout], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
};

const satisfied = (leaf, dep) => {
  const d = by.get(dep);
  if (!d) return false;
  if (d.status === 'closed') return true;
  return d.status === 'implemented' && d.parent === leaf.parent && merged(d.integration?.revision);
};

const ready = issues.filter(
  (i) =>
    i.status === 'open' &&
    !i.holder &&
    i.type !== 'epic' &&
    !hasLiveChild.has(i.id) &&
    !(i.labels || []).includes('idea') &&
    (!stage || i.parent === stage) &&
    i.blockedBy.every((d) => satisfied(i, d)),
);
for (const i of ready) console.log(`${i.id}\t${i.title}`);
