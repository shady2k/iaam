#!/usr/bin/env node
// br (beads_rust) -> the normalized backlog the shipped rules read (.backlog/rules/,
// model.md of shady2k-skills). The rules know no tracker; everything br-specific is here.
//
//   adapter.mjs [--offline]       the live tracker: the machine's store on refs/tracker/state,
//                                 synced with the remote first (--offline: this store only)
//   adapter.mjs --at <rev>        the tracker as the store recorded it when <rev> was cut or
//                                 landed; a revision from before the move reads the export
//                                 that revision committed, which was that layout's record
//   adapter.mjs --jsonl <file>    a beads-format JSONL export, e.g. a snapshot
//
// THE LAYOUT (iaam-ima7z): the tracker lives off the branches, one store per machine,
// published to the ref refs/tracker/state by the background sync
// (.backlog/rules/tracker-sync.mjs with the recipe .backlog/rules/trackers/br.mjs).
// No code branch carries a tracker record. The tracker at a code revision is the
// recording the store made under that revision's own name, never a code tree and
// never a later snapshot. Until 2026-11-11 the export was committed on main
// (.beads/issues.jsonl), so a revision from then is read from its own tree: that
// file was what that layout recorded for it.
//
// The export also carries `landed`: the commits on the main line at the revision
// judged, parsed by the commit-link convention (commits.mjs), so the gate reports
// an open leaf whose change already landed.
//
// Exit 2, with the reason, when the tracker cannot be read: an unreadable
// backlog is never an empty one.
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
export const MECHANISM = join(HERE, 'rules', 'tracker-sync.mjs');
export const RECIPE = join(HERE, 'rules', 'trackers', 'br.mjs');
const ISSUES = '.beads/issues.jsonl';
const git = (...a) => execFileSync('git', a, { encoding: 'utf8', maxBuffer: 1 << 30, stdio: ['ignore', 'pipe', 'pipe'] });

const TYPES = { epic: 'epic', task: 'task', bug: 'bug', chore: 'chore' };

// `submitted` and `implemented` are br statuses of this project (policy.yaml in the
// store), and the transition that sets one carries its revision and evidence as a
// comment: `submitted: <rev> -- <evidence>`. Before the move to br, bd stored the
// same thing as metadata on an open issue; that is still read, for old revisions.
const STATES = ['submitted', 'implemented'];
const KNOWN = new Set(['open', 'in_progress', 'blocked', 'deferred', 'closed', 'tombstone', ...STATES]);

function status(r, meta) {
  if (!KNOWN.has(r.status)) throw new Error(`${r.id} has the status "${r.status}", which this project does not use`);
  if (r.status === 'closed') return 'closed';
  if (r.status === 'deferred') return 'deferred';
  if (STATES.includes(r.status)) return r.status;
  if (STATES.includes(meta.shady2k_state)) return meta.shady2k_state;
  if (r.status === 'in_progress') return 'active';
  // `blocked` is a derived view; the edges say what blocks it.
  return 'open';
}

// The latest `<state>: <rev> -- <evidence>` comment, else bd's metadata.
function evidence(r, state, meta) {
  const marked = (r.comments || [])
    .map((c) => (c.text || '').match(new RegExp(`^${state}:\\s*(\\S+)(?:\\s+--\\s+([\\s\\S]*))?`)))
    .filter(Boolean)
    .pop();
  if (marked) return { revision: marked[1], evidence: (marked[2] || '').trim() };
  return { revision: meta.shady2k_revision || '', evidence: meta.shady2k_evidence || '' };
}

function metadata(r) {
  if (!r.metadata) return {};
  if (typeof r.metadata === 'object') return r.metadata;
  try { return JSON.parse(r.metadata); } catch { return {}; }
}

function body(r) {
  const parts = [r.description || ''];
  // Beads keeps acceptance criteria in their own field; the rules look for the heading.
  if (r.acceptance_criteria) parts.push(`## Acceptance Criteria\n${r.acceptance_criteria}`);
  return parts.join('\n\n');
}

// The set's records go out raw and whole, damaged or not, with br's own comment
// id: work records (`[shady2k-time`) and the command runner's journal of
// durations (`<!-- shady2k-exec v1 -->`). The gate judges them and the run
// scripts read them, so a record dropped here is time lost with nothing to say so.
const RECORD = /^(\[shady2k-time|<!-- shady2k-exec v1 -->)/;
function records(r) {
  return (r.comments || [])
    .filter((c) => RECORD.test(c.text || ''))
    .map((c) => ({ id: String(c.id), at: c.created_at, author: c.author, body: c.text }));
}

export function normalize(rows, source, landed) {
  const issues = rows
    // A tombstone is br's deleted issue: gone from the tracker, not a state of work.
    .filter((r) => (r._type || 'issue') === 'issue' && r.status !== 'tombstone')
    .map((r) => {
      const meta = metadata(r);
      const deps = r.dependencies || [];
      const parent = deps.find((d) => d.type === 'parent-child')?.depends_on_id ?? null;
      const s = status(r, meta);
      const out = {
        id: r.id,
        title: r.title,
        type: TYPES[r.issue_type] || 'other',
        status: s,
        labels: r.labels || [],
        parent,
        // Only `blocks` gates work. `discovered-from`, `related` and the rest are
        // provenance and never reach the rules as a dependency.
        blockedBy: deps.filter((d) => d.type === 'blocks').map((d) => d.depends_on_id),
        body: body(r),
        updatedAt: r.updated_at,
        createdAt: r.created_at,
        holder: r.assignee || null,
        comments: records(r),
      };
      if (s === 'submitted') out.delivery = evidence(r, s, meta);
      if (s === 'implemented') out.integration = evidence(r, s, meta);
      return out;
    });
  const out = { generatedAt: new Date().toISOString(), source, issues };
  if (landed) out.landed = landed;
  return out;
}

function parseJsonl(text) {
  return text.split('\n').filter((l) => l.trim()).map((l) => JSON.parse(l));
}

// The mechanism prints the store's state files as `=== <path>` sections.
function issuesSection(text, what) {
  const head = `=== ${ISSUES}\n`;
  const at = text.indexOf(head);
  if (at < 0) throw new Error(`${what} carries no ${ISSUES}`);
  const rest = text.slice(at + head.length);
  const next = rest.search(/^=== \.beads\//m);
  return next < 0 ? rest : rest.slice(0, next);
}

function mechanism(args) {
  const r = spawnSync(process.execPath, [MECHANISM, ...args, '--recipe', RECIPE], { encoding: 'utf8', maxBuffer: 1 << 30 });
  if (r.error) throw r.error;
  return r;
}

// The commit-link convention, shared with commits.mjs: ids inside parentheses.
export function taskIds(message) {
  const text = message.split('\n').filter((l) => !l.startsWith('#')).join('\n');
  const ids = [];
  for (const group of text.matchAll(/\(([^()]*)\)/g))
    for (const m of group[1].matchAll(/\biaam-[a-z0-9]+(?:\.[0-9]+)*\b/g)) ids.push(m[0]);
  return [...new Set(ids)];
}

// A commit of the layout before the move that changed nothing but the tracker's
// export (and, for a finding budget, the config beside it) recorded what
// happened to the items it names; it delivered none of them, so it is not a
// landing of their work. Since the move no branch carries such a commit.
const RECORD_PATH = (p) => p.startsWith('.beads/') || p === '.backlog/config.json';
export function recordsOnly(paths) {
  return paths.length > 0 && paths.some((p) => p.startsWith('.beads/')) && paths.every(RECORD_PATH);
}

// The main line at a code revision: every commit both on `main` and in that
// revision's history, less the records-only commits above. Local history only,
// never the network.
export function landedAt(rev) {
  let main = null;
  for (const ref of ['refs/heads/main', 'refs/remotes/origin/main']) {
    try { git('rev-parse', '--verify', '--quiet', ref); main = ref; break; } catch { /* try the next */ }
  }
  if (!main) throw new Error('no main line here: neither main nor origin/main resolves');
  const tip = git('merge-base', main, rev).trim();
  return git('log', '--diff-merges=first-parent', '--name-only', '--format=%x02%H%x01%B%x03', tip).split('\x02').filter(Boolean).flatMap((s) => {
    const [id, rest] = s.split('\x01');
    const [msg, files] = (rest || '').split('\x03');
    if (recordsOnly((files || '').split('\n').map((f) => f.trim()).filter(Boolean))) return [];
    return [{ id: id.trim(), taskIds: taskIds(msg || '') }];
  });
}

function revisionOf(rev) {
  return git('rev-parse', '--verify', `${rev}^{commit}`).trim();
}

export function read(argv) {
  const at = argv.indexOf('--at');
  const file = argv.indexOf('--jsonl');
  if (file >= 0) return normalize(parseJsonl(readFileSync(argv[file + 1], 'utf8')), `beads jsonl ${argv[file + 1]}`);
  if (at >= 0) {
    const rev = revisionOf(argv[at + 1]);
    const r = mechanism(['export', '--at', rev]);
    if (r.status === 0)
      return normalize(parseJsonl(issuesSection(r.stdout, `the recording of ${rev}`)), `br refs/tracker/state recording @ ${rev}`, landedAt(rev));
    // Before the move the layout's record of a revision was the export it committed.
    let text = null;
    try { text = git('show', `${rev}:${ISSUES}`); } catch { /* not a pre-move revision */ }
    if (text !== null) return normalize(parseJsonl(text), `beads ${ISSUES} committed (layout before 2026-11-11) @ ${rev}`, landedAt(rev));
    throw new Error((r.stderr || r.stdout || `tracker-sync export --at ${rev} failed`).trim().replace(/^tracker-sync: /, ''));
  }
  let where = 'live store';
  let r = mechanism(argv.includes('--offline') ? ['export', '--offline'] : ['export']);
  // A hook asks with --or-offline: an unreachable remote (exit 4) does not stop a
  // commit, but the read says it is this machine's store only, never the whole state.
  if (r.status === 4 && argv.includes('--or-offline')) {
    console.error('backlog adapter: the remote is unreachable, so this reads this machine\'s store only; other machines\' changes are not in it');
    r = mechanism(['export', '--offline']);
    where = 'this machine\'s store only (remote unreachable)';
  }
  if (r.status !== 0) throw new Error((r.stderr || r.stdout || 'tracker-sync export failed').trim().replace(/^tracker-sync: /, ''));
  const head = revisionOf('HEAD');
  return normalize(parseJsonl(issuesSection(r.stdout, 'the store')), `br refs/tracker/state ${where} @ ${head}`, landedAt(head));
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    process.stdout.write(JSON.stringify(read(process.argv.slice(2))));
  } catch (e) {
    console.error(`backlog adapter: cannot read the tracker: ${e.message.split('\n')[0]}`);
    console.error('Is `br` installed and this clone connected? Run: make backlog-connect');
    process.exit(2);
  }
}
