import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { markedLines } from '../src/hook.ts';
import { cleanEnv, repo, SHOP } from './helpers.ts';

const HOME = path.resolve(import.meta.dirname, '..');
const GRAPH_ONLY = { '.picky.yml': 'mode: enforce\nmodel: none\n' };
const MATH_CHANGE = { 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' };

describe('markedLines', () => {
  it('marks commands with a trailing marker, or every command after a standalone marker', () => {
    expect(markedLines('npm ci\nnpm test # picky\necho "# pickyish"')).toEqual([2]);
    expect(markedLines('npm ci\n# picky\nnpm test\n\n# note\nnpx vitest')).toEqual([3, 6]);
  });

  it('never touches heredocs or continued lines', () => {
    const script = ['cat <<EOF', 'vitest # picky', 'EOF', 'vitest \\', '  --run # picky', 'jest # picky'].join('\n');
    expect(markedLines(script)).toEqual([6]);
  });
});

/** Runs a step the way GitHub Actions does: bash --noprofile --norc -eo pipefail <script>, with BASH_ENV set. */
function runStep(root: string, script: string, env: Record<string, string> = {}) {
  const file = path.join(root, '.step.sh');
  writeFileSync(file, script);
  const res = spawnSync('bash', ['--noprofile', '--norc', '-eo', 'pipefail', file], {
    cwd: root,
    encoding: 'utf8',
    env: { ...cleanEnv(), BASH_ENV: path.join(HOME, 'hook/hook.sh'), PICKY_BIN: path.join(HOME, 'bin/picky'), ...env },
  });
  return { code: res.status, out: res.stdout, err: res.stderr };
}

const gh = (root: string) => ({
  GITHUB_WORKFLOW_REF: 'acme/shop/.github/workflows/ci.yml@refs/pull/7/merge',
  GITHUB_JOB: 'test',
  GITHUB_WORKSPACE: root,
});

describe('hook (real bash)', () => {
  const tests = {
    'test/math.test.ts': "import { test } from 'node:test';\nimport '../src/math.ts';\ntest('math', () => {});\n",
    'test/cart.test.ts': "import { test } from 'node:test';\nimport '../src/cart.ts';\ntest('cart', () => {});\n",
    'test/strings.test.ts': "import { test } from 'node:test';\nimport '../src/strings.ts';\ntest('strings', () => {});\n",
  };

  it('narrows a marked test command and leaves unmarked lines alone', () => {
    const root = repo({ ...SHOP, ...tests, ...GRAPH_ONLY }, { 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' });
    const r = runStep(root, 'echo before\nnode --test # picky\necho after\n');
    expect(r.code).toBe(0);
    expect(r.out).toContain('before');
    expect(r.out).toContain('after');
    expect(r.out).toMatch(/✔ math/);
    expect(r.out).toMatch(/✔ cart/);
    expect(r.out).not.toMatch(/✔ strings/);
    expect(r.err).toContain('picky: $ node --test test/cart.test.ts test/math.test.ts');
  });

  it('keeps -e semantics: a failing selected test fails the step', () => {
    const failing = { 'test/math.test.ts': "import { test } from 'node:test';\nimport '../src/math.ts';\ntest('math', () => { throw new Error('boom'); });\n" };
    const root = repo({ ...SHOP, ...tests, ...GRAPH_ONLY }, { ...failing, ...MATH_CHANGE });
    const r = runStep(root, 'node --test # picky\necho unreachable\n');
    expect(r.code).not.toBe(0);
    expect(r.out).not.toContain('unreachable');
  });

  it('runs the original command when picky itself fails', () => {
    const root = repo({ ...SHOP, ...tests, ...GRAPH_ONLY }, MATH_CHANGE);
    // A picky that prepares the step fine but crashes when asked about a line.
    const broken = path.join(root, 'broken-picky');
    writeFileSync(broken, `#!/bin/sh\n[ "$1" = hook-exec ] && exec "${path.join(HOME, 'bin/picky')}" "$@"\nexit 3\n`, { mode: 0o755 });
    const r = runStep(root, 'node --test # picky\n', { PICKY_BIN: broken });
    expect(r.code).toBe(0);
    expect(r.out).toMatch(/✔ math/);
    expect(r.out).toMatch(/✔ strings/); // the unchanged command ran every test
  });

  it('honors a marker written as a YAML comment on a one-line run step', () => {
    const wf = { '.github/workflows/ci.yml': 'on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: node --test   # picky\n' };
    const root = repo({ ...SHOP, ...tests, ...GRAPH_ONLY, ...wf }, MATH_CHANGE);
    // GitHub hands the step script over without the comment: YAML already dropped it.
    const r = runStep(root, 'node --test\n', { ...gh(root) });
    expect(r.code).toBe(0);
    expect(r.err).toContain('picky: $ node --test test/cart.test.ts test/math.test.ts');
  });

  it('runs unmarked steps of a marked workflow as they are, keeping $0', () => {
    const wf = { '.github/workflows/ci.yml': 'on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo "$0"\n      - run: node --test # picky\n' };
    const root = repo({ ...SHOP, ...tests, ...GRAPH_ONLY, ...wf }, MATH_CHANGE);
    const r = runStep(root, 'echo "$0"\n', gh(root));
    expect(r.code).toBe(0);
    expect(r.out.trim()).toBe(path.join(root, '.step.sh'));
  });

  it('skips the job setup only when every marked command in the job skips', () => {
    const wf = {
      '.github/workflows/ci.yml':
        'on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: npm ci\n      - run: node --test # picky\n',
      'fake-bin/npm': '#!/bin/sh\necho "npm-ran $*"\n',
    };
    const fake = (root: string) => ({ ...gh(root), PATH: `${path.join(root, 'fake-bin')}:${process.env.PATH}` });

    const docs = repo({ ...SHOP, ...tests, ...GRAPH_ONLY, ...wf }, { 'README.md': '# docs\n' });
    chmodSync(path.join(docs, 'fake-bin/npm'), 0o755);
    const skipped = runStep(docs, 'npm ci\n', fake(docs));
    expect(skipped.code).toBe(0);
    expect(skipped.out).not.toContain('npm-ran');
    expect(skipped.err).toContain('picky: npm ci -> skip: all 1 marked commands in this job skip');

    const code = repo({ ...SHOP, ...tests, ...GRAPH_ONLY, ...wf }, MATH_CHANGE);
    chmodSync(path.join(code, 'fake-bin/npm'), 0o755);
    const ran = runStep(code, 'npm ci\n', fake(code));
    expect(ran.out).toContain('npm-ran ci');
  });

  it('finds the picky that init vendored next to the hook when PICKY_BIN is unset', () => {
    const root = repo({ ...SHOP, ...tests, ...GRAPH_ONLY }, MATH_CHANGE);
    mkdirSync(path.join(root, '.github/picky'), { recursive: true });
    copyFileSync(path.join(HOME, 'hook/hook.sh'), path.join(root, '.github/picky/hook.sh'));
    writeFileSync(path.join(root, '.github/picky/picky'), `#!/bin/sh\nexec ${path.join(HOME, 'bin/picky')} "$@"\n`, { mode: 0o755 });
    const r = runStep(root, 'node --test # picky\n', { BASH_ENV: path.join(root, '.github/picky/hook.sh'), PICKY_BIN: '' });
    expect(r.code).toBe(0);
    expect(r.err).toContain('picky: $ node --test test/cart.test.ts test/math.test.ts');
  });

  it('runs unmarked steps without picky', () => {
    const root = repo({ ...SHOP, ...tests, ...GRAPH_ONLY }, MATH_CHANGE);
    const r = runStep(root, 'node --test\n');
    expect(r.code).toBe(0);
    expect(r.err).not.toContain('picky');
    expect(r.out).toMatch(/✔ strings/);
  });
});
