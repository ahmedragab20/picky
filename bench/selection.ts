// Replays a repo's recent history through picky's decision and records how many test files
// each change selects. Graph only (model: none): no API key, and the numbers are the floor
// picky always runs; with Jev, tests outside the graph can be added on top.
//
//   bun run bench <repo-dir> <name> [commits=100]
//
// That type checks this script, then runs it on Node, so the latencies are the ones users get.
//
// Each first-parent commit on the checked-out branch is treated as one PR: its first parent is
// the base. The repo is checked out at every commit, so use a throwaway clone. Results are merged
// into bench/results.json under <name>.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import picomatch from 'picomatch';
import { decide, TEST_GLOBS } from '../src/decide.ts';
import { isVendored } from '../src/graph.ts';

interface Run {
  sha: string;
  subject: string;
  changed: number;
  tests: number;
  run: number;
  kind: 'none' | 'narrowed' | 'all';
  reason: string;
  ms: number;
}

const [dir, name, count = '100'] = process.argv.slice(2);
if (!dir || !name) {
  console.error('usage: bun run bench <repo-dir> <name> [commits=100]');
  process.exit(2);
}
const root = path.resolve(dir);
const git = (...args: string[]) => execFileSync('git', ['-C', root, ...args], { encoding: 'utf8', maxBuffer: 1 << 28 }).trim();
const isTest = picomatch(TEST_GLOBS, { dot: true });

const branch = git('rev-parse', '--abbrev-ref', 'HEAD');
const shas = git('rev-list', '--first-parent', '-n', String(Number(count) + 1), 'HEAD').split('\n').slice(0, Number(count));
const runs: Run[] = [];
const errors: { sha: string; reason: string }[] = [];
try {
  for (const [i, sha] of shas.entries()) {
    git('checkout', '-q', '--detach', '--force', sha);
    const tests = git('ls-files').split('\n').filter((f) => isTest(f) && !isVendored(f)).length;
    const d = await decide({ root, env: { PATH: process.env.PATH, PICKY_MODEL: 'none', PICKY_MODE: 'enforce', PICKY_BASE: `${sha}^1` } });
    // A failed git call (e.g. a lazy blob fetch over a flaky network) is the harness failing, not picky deciding.
    if (d.reason.startsWith('internal error')) {
      errors.push({ sha: sha.slice(0, 10), reason: d.reason.split('\n')[0] });
      process.stderr.write(`${name} ${i + 1}/${shas.length} ${sha.slice(0, 10)}: excluded (${d.reason.split('\n')[0]})\n`);
      continue;
    }
    const run = d.full ? tests : Object.values(d.tests).filter((t) => t.run).length;
    const kind = run === 0 ? 'none' : run === tests ? 'all' : 'narrowed';
    runs.push({ sha: sha.slice(0, 10), subject: git('log', '-1', '--format=%s', sha), changed: d.changed.length, tests, run, kind, reason: d.reason, ms: d.ms });
    process.stderr.write(`${name} ${i + 1}/${shas.length} ${sha.slice(0, 10)}: ${run}/${tests} (${d.ms} ms) ${d.reason}\n`);
  }
} finally {
  git('checkout', '-q', '--force', branch);
}

const sorted = <T>(xs: T[], key: (x: T) => number) => xs.map(key).sort((a, b) => a - b);
const pct = (xs: number[], p: number) => xs[Math.min(xs.length - 1, Math.floor((p / 100) * xs.length))];
const share = (k: Run['kind']) => runs.filter((r) => r.kind === k).length / runs.length;
const fractions = sorted(runs, (r) => (r.tests ? r.run / r.tests : 0));
const ms = sorted(runs, (r) => r.ms);
const summary = {
  repo: git('remote', 'get-url', 'origin').replace(/\.git$/, ''),
  head: git('rev-parse', '--short=10', 'HEAD'),
  commits: runs.length,
  excluded: errors,
  testFiles: runs[0]?.tests ?? 0,
  medianSelected: pct(fractions, 50),
  meanSelected: fractions.reduce((a, b) => a + b, 0) / fractions.length,
  shareNone: share('none'),
  shareNarrowed: share('narrowed'),
  shareAll: share('all'),
  decisionMsP50: pct(ms, 50),
  decisionMsP95: pct(ms, 95),
  measured: new Date().toISOString().slice(0, 10),
  runs,
};

const out = path.join(path.dirname(fileURLToPath(import.meta.url)), 'results.json');
const all = existsSync(out) ? JSON.parse(readFileSync(out, 'utf8')) : {};
all[name] = summary;
writeFileSync(out, `${JSON.stringify(all, null, 2)}\n`);
const { runs: _, ...brief } = summary;
console.log(JSON.stringify(brief, null, 2));
