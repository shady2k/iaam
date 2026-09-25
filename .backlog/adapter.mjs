#!/usr/bin/env node
// Beads -> the normalized backlog the shipped rules read (.backlog/rules/, model.md
// of shady2k-skills). The rules know no tracker; everything beads-specific is here.
//
//   adapter.mjs                 the live tracker, through `bd export`
//   adapter.mjs --jsonl <file>  a beads JSONL export, e.g. an earlier revision
//   adapter.mjs --at <rev>      .beads/issues.jsonl as committed at <rev>
//
// Exit 2, with the reason, when the tracker cannot be read: an unreadable
// backlog is never an empty one.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const TYPES = { epic: 'epic', task: 'task', bug: 'bug', chore: 'chore' };

// Beads has no `submitted` or `implemented`. They are stored as metadata on the
// issue (`shady2k_state`, with `shady2k_revision` and `shady2k_evidence`) and
// emitted here; an open issue without that metadata is plain open.
function status(r, meta) {
  if (r.status === 'closed') return 'closed';
  if (r.status === 'deferred') return 'deferred';
  if (meta.shady2k_state === 'submitted' || meta.shady2k_state === 'implemented') return meta.shady2k_state;
  if (r.status === 'in_progress') return 'active';
  // `blocked` in beads is a derived view; the edges say what blocks it.
  return 'open';
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
    .filter((r) => (r._type || 'issue') === 'issue')
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
      };
      if (s === 'submitted') out.delivery = { revision: meta.shady2k_revision || '', evidence: meta.shady2k_evidence || '' };
      if (s === 'implemented') out.integration = { revision: meta.shady2k_revision || '', evidence: meta.shady2k_evidence || '' };
      return out;
    });
  return { generatedAt: new Date().toISOString(), source, issues };
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
  const dir = mkdtempSync(join(tmpdir(), 'iaam-backlog-'));
  try {
    const out = join(dir, 'export.jsonl');
    execFileSync('bd', ['export', '-o', out], { stdio: ['ignore', 'ignore', 'pipe'] });
    return normalize(parseJsonl(readFileSync(out, 'utf8')), 'beads live export');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    process.stdout.write(JSON.stringify(read(process.argv.slice(2))));
  } catch (e) {
    console.error(`backlog adapter: cannot read the tracker: ${e.message.split('\n')[0]}`);
    console.error('Is `bd` installed and this clone connected? Run: make backlog-connect');
    process.exit(2);
  }
}
