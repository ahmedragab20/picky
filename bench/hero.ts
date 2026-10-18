// Records which test files picky's graph runs, and why, for a few real commits of one repo.
// The docs landing page draws it as a grid with one cell per test file.
//
//   node bench/hero.ts <repo-dir> <sha>...
//
// Like selection.ts: graph only (model: none), each commit's first parent is the base, and the
// repo is checked out at every commit, so use a throwaway clone. Writes bench/hero.json.
import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { decide } from '../src/decide.ts';

const [dir, ...shas] = process.argv.slice(2);
if (!dir || shas.length === 0) {
  console.error('usage: node bench/hero.ts <repo-dir> <sha>...');
  process.exit(2);
}
const root = path.resolve(dir);
const git = (...args: string[]) => execFileSync('git', ['-C', root, ...args], { encoding: 'utf8', maxBuffer: 1 << 28 }).trim();

const branch = git('rev-parse', '--abbrev-ref', 'HEAD');
const commits = [];
try {
  for (const sha of shas) {
    git('checkout', '-q', '--detach', '--force', sha);
    const d = await decide({ root, env: { PATH: process.env.PATH, PICKY_MODEL: 'none', PICKY_MODE: 'enforce', PICKY_BASE: `${sha}^1` } });
    if (d.reason.startsWith('internal error')) throw new Error(`${sha}: ${d.reason}`);
    commits.push({
      sha: git('rev-parse', '--short=7', sha),
      subject: git('log', '-1', '--format=%s', sha),
      changed: d.changed.map((c) => c.path),
      full: d.full,
      // Why everything (or one ecosystem) runs, when a gate decided it.
      gate: d.full ? d.reason : (Object.values(d.ecosystems ?? {})[0] ?? null),
      reason: d.reason,
      tests: d.tests,
    });
  }
} finally {
  git('checkout', '-q', '--force', branch);
}

// One cell per test file seen in any of the commits, in path order.
const files = [...new Set(commits.flatMap((c) => Object.keys(c.tests)))].sort();
const out = {
  repo: git('remote', 'get-url', 'origin').replace(/\.git$/, ''),
  files,
  commits: commits.map(({ tests, full, reason, ...c }) => ({
    ...c,
    // Index into `files` -> why it runs. A file missing here was skipped.
    run: Object.fromEntries(files.flatMap((f, i) => (full || tests[f]?.run ? [[i, full ? reason : tests[f].reason]] : []))),
  })),
};
writeFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), 'hero.json'), `${JSON.stringify(out)}\n`);
for (const c of out.commits) console.log(`${c.sha} ${Object.keys(c.run).length}/${files.length} ${c.subject}`);
