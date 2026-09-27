#!/usr/bin/env node
// br (beads_rust) -> the normalized backlog the shipped rules read (.backlog/rules/,
// model.md of shady2k-skills). The rules know no tracker; everything br-specific is here.
//
//   adapter.mjs                 the live tracker: br's own JSONL export, flushed first
//   adapter.mjs --jsonl <file>  a beads-format JSONL export, e.g. an earlier revision
//   adapter.mjs --at <rev>      .beads/issues.jsonl as committed at <rev>
//
// The JSONL format is the one bd wrote before 2026-09-27 (iaam-r1v7), so --at reads
// revisions from either tracker; only the way a state is stored differs, below.
//
// Exit 2, with the reason, when the tracker cannot be read: an unreadable
// backlog is never an empty one.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const TYPES = { epic: 'epic', task: 'task', bug: 'bug', chore: 'chore' };

// `submitted` and `implemented` are br statuses of this project (.beads/policy.yaml),
// and the transition that sets one carries its revision and evidence as a comment:
// `submitted: <rev> -- <evidence>`. Before the move to br, bd stored the same thing
// as metadata on an open issue; that is still read, for revisions committed then.
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

export function normalize(rows, source) {
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
  return { generatedAt: new Date().toISOString(), source, issues };
}

// The set's work records (claims, receipts, stops) are comments whose text
// starts with `[shady2k-time`. They go out raw and whole, damaged or not, with
// beads' own comment id: the gate judges them and the run script reads them, so
// a record dropped here is time lost with nothing to say so.
function records(r) {
  return (r.comments || [])
    .filter((c) => (c.text || '').startsWith('[shady2k-time'))
    .map((c) => ({ id: String(c.id), at: c.created_at, author: c.author, body: c.text }));
}

function parseJsonl(text) {
  return text.split('\n').filter((l) => l.trim()).map((l) => JSON.parse(l));
}

export function read(argv) {
  const at = argv.indexOf('--at');
  const file = argv.indexOf('--jsonl');
  if (at >= 0) {
    const rev = argv[at + 1];
    const text = execFileSync('git', ['show', `${rev}:.beads/issues.jsonl`], { encoding: 'utf8', maxBuffer: 1 << 30 });
    return normalize(parseJsonl(text), `beads .beads/issues.jsonl @ ${rev}`);
  }
  if (file >= 0) return normalize(parseJsonl(readFileSync(argv[file + 1], 'utf8')), `beads jsonl ${argv[file + 1]}`);
  // br keeps its export current after every write (sync.auto_flush); the flush makes
  // sure of it. `br where` names the main checkout's .beads even from a worktree.
  execFileSync('br', ['sync', '--flush-only', '--quiet'], { stdio: ['ignore', 'ignore', 'pipe'] });
  const where = execFileSync('br', ['where'], { encoding: 'utf8' }).split('\n')[0].trim();
  return normalize(parseJsonl(readFileSync(join(where, 'issues.jsonl'), 'utf8')), 'br live export');
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
