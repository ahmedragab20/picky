import { describe, expect, it } from 'vitest';
import { parse } from 'yaml';
import { editWorkflow, planInit } from '../src/init.ts';
import { SHOP, SHOP_PHP, tempDir, writeFiles } from './helpers.ts';

const WORKFLOW = `name: CI
on:
  pull_request:
  push:
    branches: [main]

jobs:
  js:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npm ci
      - run: npx vitest run
      - run: npx tsc # types

  php:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: composer install
      - run: |
          echo "unit tests"
          vendor/bin/phpunit
`;

function project(files: Record<string, string> = {}): string {
  const root = tempDir();
  writeFiles(root, { ...SHOP, ...SHOP_PHP, ...files });
  return root;
}

describe('editWorkflow', () => {
  it('marks recognized commands, adds the env lines and shares one decision across jobs', () => {
    const { text, notes } = editWorkflow(WORKFLOW, project());
    const wf = parse(text);

    expect(text).toContain('      - run: npx vitest run # picky\n');
    expect(text).toContain('          vendor/bin/phpunit # picky\n');
    expect(text).toContain('          echo "unit tests"\n');
    expect(text).toContain('      - run: npx tsc # types\n'); // an existing comment is left alone
    expect(text).toContain('      - run: npm ci\n'); // setup commands are skipped by the job-level rule instead
    expect(wf.env).toEqual({
      BASH_ENV: '${{ github.workspace }}/.github/picky/hook.sh',
      PICKY_API_KEY: '${{ secrets.PICKY_API_KEY }}',
    });
    expect(Object.keys(wf.jobs)).toEqual(['picky', 'js', 'php']);
    expect(wf.jobs.picky.steps.map((s: any) => s.uses ?? s.run)).toEqual([
      'actions/checkout@v4',
      '.github/picky/picky decide',
      'actions/upload-artifact@v4',
    ]);
    expect(wf.jobs.picky.steps.every((s: any) => s['continue-on-error'] === true)).toBe(true);
    for (const job of ['js', 'php']) {
      expect(wf.jobs[job].needs).toBe('picky');
      expect(wf.jobs[job].steps[0]).toMatchObject({
        uses: 'actions/download-artifact@v4',
        'continue-on-error': true,
        with: { name: 'picky-decision', path: '${{ runner.temp }}/picky' },
      });
    }
    expect(notes).toEqual(['marked 2 commands in 2 jobs']);
  });

  it('is idempotent, and wires in a job marked after the first run', () => {
    const root = project();
    const once = editWorkflow(WORKFLOW, root).text;
    expect(editWorkflow(once, root).text).toBe(once);

    const later = parse(editWorkflow(`${once}\n  e2e:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npx vitest run\n`, root).text);
    expect(later.jobs.e2e.needs).toBe('picky');
    expect(later.jobs.e2e.steps[0].uses).toBe('actions/download-artifact@v4');
    expect(Object.keys(later.jobs).filter((j) => j === 'picky')).toHaveLength(1);
  });

  it('keeps existing env keys and needs, and adds no decide job for a single job', () => {
    const src = `on: [pull_request]
env:
  CI: true
  BASH_ENV: custom.sh
jobs:
  test:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - run: npx vitest run
`;
    const out = parse(editWorkflow(src, project()).text);
    expect(out.env).toEqual({ CI: true, BASH_ENV: 'custom.sh', PICKY_API_KEY: '${{ secrets.PICKY_API_KEY }}' });
    expect(Object.keys(out.jobs)).toEqual(['test']);
    expect(out.jobs.test.needs).toBe('build');
    expect(out.jobs.test.steps[0].run).toBe('npx vitest run');
  });

  it('shares the decision for a matrix job, appending picky to its needs', () => {
    const src = `on: pull_request
jobs:
  test:
    needs: [lint, build]
    strategy:
      matrix:
        node: [20, 22]
    runs-on: ubuntu-latest
    steps:
      - run: npx vitest run
`;
    const out = parse(editWorkflow(src, project()).text);
    expect(out.jobs.test.needs).toEqual(['lint', 'build', 'picky']);
    expect(Object.keys(out.jobs)).toEqual(['picky', 'test']);
  });

  it('leaves workflows alone when picky has nothing to do there', () => {
    const push = 'on: push\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npx vitest run\n';
    expect(editWorkflow(push, project())).toEqual({ text: push, notes: ['not changed: it does not run on pull_request'] });

    const windows = WORKFLOW.replaceAll('ubuntu-latest', 'windows-latest');
    const r = editWorkflow(windows, project());
    expect(r.text).toBe(windows);
    expect(r.notes).toContain('job "js" skipped: Windows runner (the hook needs bash)');

    const unknown = 'on: pull_request\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - run: make all\n';
    expect(editWorkflow(unknown, project()).text).toBe(unknown);
  });
});

describe('planInit', () => {
  it('lists the vendored files and the workflow edit, and nothing once applied', () => {
    const root = project({ '.github/workflows/ci.yml': WORKFLOW });
    const assets = { hook: '# hook\n', bundle: Buffer.from('bundle') };
    const { files } = planInit(root, assets);
    expect(files.map((f) => f.path)).toEqual([
      '.picky.yml',
      '.github/picky/hook.sh',
      '.github/picky/picky',
      '.github/picky/picky.mjs',
      '.github/workflows/ci.yml',
    ]);
    expect(files.find((f) => f.path === '.github/picky/picky')?.mode).toBe(0o755);

    writeFiles(root, Object.fromEntries(files.map((f) => [f.path, f.after.toString()])));
    expect(planInit(root, assets).files).toEqual([]);
  });
});
