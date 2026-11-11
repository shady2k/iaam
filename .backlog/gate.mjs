#!/usr/bin/env node
// The backlog gate: the live tracker judged by .backlog/rules/check.mjs, with the
// tracker as the store recorded it at <base> (default HEAD) as the baseline, so
// under `block-new` only what this change introduced fails and older debt is printed.
//
//   gate.mjs [--base <rev>] [--json] [--offline] [--no-recover]
//
// The baseline config is the one committed at <base>, so a budget lowered in this
// change counts against it. The age snapshots of the 2026-09-25 bulk deferral,
// when this clone has them (.git/shady2k/), correct the ages that edit rewrote.
//
// BEFORE IT JUDGES, it recovers: every work span that has ended without its
// receipt and that this machine's transcripts can measure is posted to the
// tracker exactly as the run script (`runs.mjs gaps`) prints it, so a receipt
// one session left unwritten stops nobody else's commit and the time is still
// recorded. A span whose session is still live is the run script's to leave
// alone. A recovery that cannot run or fails changes nothing: the gate still
// judges, and the failure is never a pass.
//
// Exit 0 clean, 1 new violations, 2 cannot check (never a pass).
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { read } from './adapter.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const git = (...a) => execFileSync('git', a, { encoding: 'utf8', maxBuffer: 1 << 30, stdio: ['ignore', 'pipe', 'ignore'] });

// A rules file the check imports and cannot find crashes node with exit 1, which
// would read as "this change adds a problem". It is a missing input, said as one.
const RULES = ['check.mjs', 'time-format.mjs', 'regular-file.mjs', 'argv.mjs'];

// The run script ships with the person's plugin, not with the repository.
function runsScript() {
  if (process.env.SHADY2K_RUNS && existsSync(process.env.SHADY2K_RUNS)) return process.env.SHADY2K_RUNS;
  const cache = join(homedir(), '.claude', 'plugins', 'cache', 'shady2k', 'shady2k-skills');
  if (!existsSync(cache)) return null;
  const num = (v) => v.split('.').map(Number);
  const newest = readdirSync(cache).filter((v) => /^\d+\.\d+\.\d+$/.test(v))
    .sort((a, b) => { const [x, y] = [num(a), num(b)]; return x[0] - y[0] || x[1] - y[1] || x[2] - y[2]; })
    .reverse().find((v) => existsSync(join(cache, v, 'skills', 'engineering', 'take-task', 'runs.mjs')));
  return newest ? join(cache, newest, 'skills', 'engineering', 'take-task', 'runs.mjs') : null;
}

function recover(now, put) {
  const runs = runsScript();
  if (!runs) { console.error('gate: no run script on this machine (shady2k-skills plugin), so no ended span is recovered; the gate still judges'); return; }
  try {
    const top = git('rev-parse', '--show-toplevel').trim();
    const g = spawnSync(process.execPath, [runs, 'gaps', '--json', '--backlog', put('recover.json', now), '--project', 'iaam', '--repo', top],
      { encoding: 'utf8', maxBuffer: 1 << 28 });
    if (g.status !== 0) throw new Error(`runs.mjs gaps exited ${g.status}: ${(g.stderr || '').split('\n')[0]}`);
    let n = 0;
    for (const rec of JSON.parse(g.stdout).recover || []) {
      const it = now.issues.find((i) => i.id === rec.item);
      if (!it) throw new Error(`no item ${rec.item} in the export`);
      const p = put(`record-${++n}.txt`, rec.body);
      const r = spawnSync('br', ['comments', 'add', rec.item, '-f', p], { encoding: 'utf8' });
      if (r.status !== 0) throw new Error(`br comments add ${rec.item} failed: ${(r.stderr || '').split('\n')[0]}`);
      (it.comments ||= []).push({ id: `recovered-${n}`, at: new Date().toISOString(), author: 'gate-recovery', body: rec.body });
      console.error(`gate: recovered the receipt of an ended span on ${rec.item}`);
    }
  } catch (e) {
    console.error(`gate: the recovery step failed (${e.message}); the gate still judges`);
  }
}

function main(argv) {
  const gone = RULES.filter((f) => !existsSync(join(HERE, 'rules', f)));
  if (gone.length) throw new Error(`missing .backlog/rules/${gone.join(', .backlog/rules/')}`);
  const b = argv.indexOf('--base');
  const base = b >= 0 ? argv[b + 1] : 'HEAD';
  const dir = mkdtempSync(join(tmpdir(), 'iaam-gate-'));
  try {
    const put = (name, value) => {
      const p = join(dir, name);
      writeFileSync(p, typeof value === 'string' ? value : JSON.stringify(value));
      return p;
    };
    const now = read(argv.includes('--offline') ? ['--offline'] : ['--or-offline']);
    if (!argv.includes('--no-recover')) recover(now, put);
    const head = /@\s*(\S+)\s*$/.exec(now.source)[1];
    const args = [join(HERE, 'rules', 'check.mjs'), '--config', join(HERE, 'config.json'), '--at', head, put('now.json', now)];
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
  console.error('This refuses the commit rather than passing it unchecked. Connect the clone: make backlog-connect');
  process.exitCode = 2;
}
