import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import type { Outcome } from '../src/plan.ts';
import { finalize, quoteFor, rewriteStep } from '../src/shell.ts';
import { cleanEnv, repo, SHOP } from './helpers.ts';

const HOME = path.resolve(import.meta.dirname, '..');

function planner(outcomes: Record<string, Outcome>, setup = { skip: true, note: 'skip: all 1 marked commands in this job skip' }) {
  return {
    line: async (words: string[]) => outcomes[words.join(' ')] ?? { action: 'full' as const, note: 'command not recognized; runs in full' },
    setup: async () => setup,
  };
}

describe('rewriteStep', () => {
  it('decides every command of the step, in the shell it runs in', async () => {
    const script = 'npm ci\r\nWrite-Host "go"\r\nnpx vitest run\r\nnpx tsc\r\nnpx eslint `\r\n  .\r\n';
    const logs: string[] = [];
    const out = await rewriteStep(
      script,
      'pwsh',
      planner({
        'npx vitest run': { action: 'narrow', argv: ['npx', 'vitest', 'run', 'test/a b.test.ts'], note: 'running 1 of 3 test files' },
        'npx tsc': { action: 'skip', note: 'skip: no changes tsc checks' },
      }),
      (m) => logs.push(m),
    );
    expect(out.split('\n')).toEqual([
      '# skipped by picky',
      'Write-Host "go"',
      "npx vitest run 'test/a b.test.ts'",
      '# skipped by picky',
      'npx eslint `',
      '  .',
      '',
    ]);
    expect(logs).toEqual([
      'picky: npm ci -> skip: all 1 marked commands in this job skip',
      'picky: npx vitest run -> running 1 of 3 test files',
      "picky: $ npx vitest run 'test/a b.test.ts'",
      'picky: npx tsc -> skip: no changes tsc checks',
    ]);
  });

  it('wraps PowerShell the way GitHub does and quotes for it', () => {
    expect(finalize('npx vitest', 'pwsh')).toBe("$ErrorActionPreference = 'stop'\nnpx vitest\nif ((Test-Path -LiteralPath variable:\\LASTEXITCODE)) { exit $LASTEXITCODE }\n");
    expect(finalize('npx vitest', 'sh')).toBe('npx vitest');
    expect(quoteFor('pwsh', ['a', "it's", '$x'])).toBe("a 'it''s' '$x'");
  });
});

describe('picky as a step shell (real sh)', () => {
  const tests = {
    'test/math.test.ts': "import { test } from 'node:test';\nimport '../src/math.ts';\ntest('math', () => {});\n",
    'test/cart.test.ts': "import { test } from 'node:test';\nimport '../src/cart.ts';\ntest('cart', () => {});\n",
    'test/strings.test.ts': "import { test } from 'node:test';\nimport '../src/strings.ts';\ntest('strings', () => {});\n",
  };

  function step(root: string, script: string) {
    const file = path.join(root, '.step');
    writeFileSync(file, script);
    const res = spawnSync(path.join(HOME, 'bin/picky'), ['sh', file], { cwd: root, encoding: 'utf8', env: cleanEnv() });
    return { code: res.status, out: res.stdout, err: res.stderr };
  }

  it('narrows the tests and keeps the step failing when a command fails', () => {
    const root = repo({ ...SHOP, ...tests, '.picky.yml': 'mode: enforce\nmodel: none\n' }, { 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' });
    const ok = step(root, 'echo before\nnode --test\n');
    expect(ok.code).toBe(0);
    expect(ok.out).toContain('before');
    expect(ok.out).toMatch(/✔ cart/);
    expect(ok.out).not.toMatch(/✔ strings/);
    expect(ok.err).toContain('picky: $ node --test test/cart.test.ts test/math.test.ts');

    const failing = step(root, 'false\necho after\n');
    expect(failing.code).toBe(1);
    expect(failing.out).not.toContain('after');
  });

});
