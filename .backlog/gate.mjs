#!/usr/bin/env node
// The backlog gate: the live tracker judged by .backlog/rules/check.mjs, with the
// tracker as committed at <base> (default HEAD) as the baseline, so under
// `block-new` only what this change introduced fails and older debt is printed.
//
//   gate.mjs [--base <rev>] [--json]
//
// The baseline config is the one committed at <base>, so a budget lowered in this
// change counts against it. The age snapshots of the 2026-09-25 bulk deferral,
// when this clone has them (.git/shady2k/), correct the ages that edit rewrote.
//
// Exit 0 clean, 1 new violations, 2 cannot check (never a pass).
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { read } from './adapter.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const git = (...a) => execFileSync('git', a, { encoding: 'utf8', maxBuffer: 1 << 30, stdio: ['ignore', 'pipe', 'ignore'] });

function main(argv) {
  const b = argv.indexOf('--base');
  const base = b >= 0 ? argv[b + 1] : 'HEAD';
  const dir = mkdtempSync(join(tmpdir(), 'iaam-gate-'));
  try {
    const put = (name, value) => {
      const p = join(dir, name);
      writeFileSync(p, typeof value === 'string' ? value : JSON.stringify(value));
      return p;
    };
    const args = [join(HERE, 'rules', 'check.mjs'), '--config', join(HERE, 'config.json'), put('now.json', read([]))];
    args.push('--baseline', put('base.json', read(['--at', base])));
    let baseConfig = null;
    try { baseConfig = git('show', `${base}:.backlog/config.json`); } catch { /* the base predates the installation */ }
    if (baseConfig) args.push('--baseline-config', put('base-config.json', baseConfig));
    const snaps = join(git('rev-parse', '--git-common-dir').trim(), 'shady2k');
    const before = join(snaps, 'snapshot-2026-09-25-before.jsonl');
    const after = join(snaps, 'snapshot-2026-09-25-after.jsonl');
    if (existsSync(before) && existsSync(after))
      args.push('--ages-from', put('before.json', read(['--jsonl', before])), '--ages-through', put('after.json', read(['--jsonl', after])));
    if (argv.includes('--json')) args.push('--json');
    const res = spawnSync(process.execPath, args, { stdio: 'inherit' });
    if (res.status === 1) {
      console.error('\nBACKLOG GATE: this change adds a problem to the tracker (FAIL lines above, with their fix).');
      console.error('Older problems are listed but do not block. Fix the new ones, then commit again.');
    }
    return res.status ?? 2;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

try {
  process.exitCode = main(process.argv.slice(2));
} catch (e) {
  console.error(`backlog gate could not run: ${e.message.split('\n')[0]}`);
  console.error('This refuses the commit rather than passing it unchecked. Connect the clone: make hooks');
  process.exitCode = 2;
}
