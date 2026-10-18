import { describe, expect, it } from 'bun:test';
import { decide } from '../src/decide.ts';
import { planCommand } from '../src/plan.ts';
import { cleanEnv, repo, SHOP, SHOP_PHP } from './helpers.ts';

// A mixed repo: TypeScript (SHOP) and PHP (SHOP_PHP), graph-only and enforcing.
const MIXED = {
  ...SHOP,
  ...SHOP_PHP,
  'eslint.config.js': 'export default [];',
  '.picky.yml': 'mode: enforce\nmodel: none\n',
};

async function plans(change: Record<string, string | null>, base: Record<string, string> = MIXED) {
  const root = repo(base, change);
  const decision = await decide({ root, env: cleanEnv() });
  const plan = (cmd: string) => planCommand(cmd.split(' '), root, root, decision);
  return { decision, plan };
}

describe('planCommand: tests across ecosystems', () => {
  it('narrows PHPUnit to the tests reaching a changed class and skips the JS side', async () => {
    const { plan } = await plans({ 'app/Money.php': '<?php\nnamespace App;\n\nfinal class Money { public int $cents = 0; }\n' });
    expect(plan('vendor/bin/phpunit')).toMatchObject({ action: 'narrow', argv: ['vendor/bin/phpunit', 'tests/Unit/CartTest.php'] });
    expect(plan('node --test')).toMatchObject({ action: 'skip' });
    expect(plan('npx tsc --noEmit')).toMatchObject({ action: 'skip' });
    expect(plan('vendor/bin/phpstan analyse')).toMatchObject({ action: 'full' });
    expect(plan('npx eslint .')).toMatchObject({ action: 'skip' }); // ESLint doesn't check PHP files
  });

  it('selects the PHP tests that used a deleted class', async () => {
    const { plan } = await plans({ 'app/Models/User.php': null });
    expect(plan('vendor/bin/pest')).toMatchObject({ action: 'narrow', argv: ['vendor/bin/pest', 'tests/Unit/UserTest.php'] });
  });

  it('runs one ecosystem in full when its lockfile changes, and keeps narrowing the other', async () => {
    const { plan, decision } = await plans({ 'composer.lock': '{}', 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' });
    expect(decision.full).toBe(false);
    expect(plan('vendor/bin/phpunit')).toEqual({ action: 'full', note: 'full run for php commands: composer.lock changed' });
    expect(plan('node --test')).toMatchObject({ action: 'narrow', argv: ['node', '--test', 'test/cart.test.ts', 'test/math.test.ts'] });
  });
});

describe('planCommand: lint and type checks', () => {
  it('lints changed files plus their dependents, and type-checks in full', async () => {
    const { plan } = await plans({ 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' });
    expect(plan('npx eslint .')).toMatchObject({
      action: 'narrow',
      argv: ['npx', 'eslint', '--no-warn-ignored', 'src/cart.ts', 'src/math.ts', 'test/cart.test.ts', 'test/math.test.ts'],
    });
    expect(plan('npx prettier --check .')).toMatchObject({ action: 'narrow', argv: ['npx', 'prettier', '--check', '--ignore-unknown', 'src/math.ts'] });
    expect(plan('npx tsc --noEmit')).toMatchObject({ action: 'full' });
    expect(plan('vendor/bin/phpstan analyse')).toMatchObject({ action: 'skip' });
    expect(plan('vendor/bin/pint --test')).toMatchObject({ action: 'skip' });
  });

  it('formats docs-only changes and skips everything else', async () => {
    const { plan } = await plans({ 'README.md': '# shop, documented\n' });
    expect(plan('npx prettier --check .')).toMatchObject({ action: 'narrow', argv: ['npx', 'prettier', '--check', '--ignore-unknown', 'README.md'] });
    // ESLint gets the file too: markdown plugins lint it, and --no-warn-ignored skips it otherwise.
    expect(plan('npx eslint .')).toMatchObject({ action: 'narrow', argv: ['npx', 'eslint', '--no-warn-ignored', 'README.md'] });
    for (const cmd of ['npx tsc', 'vendor/bin/phpstan', 'vendor/bin/phpunit', 'node --test']) {
      expect(plan(cmd)).toMatchObject({ action: 'skip' });
    }
  });

  it('lints everything when the linter config, or a file it imports, changes', async () => {
    const withRule = { ...MIXED, 'eslint.config.js': "import './rules/local.js';\nexport default [];", 'rules/local.js': 'export {};' };
    const { plan } = await plans({ 'rules/local.js': 'export const x = 1;' }, withRule);
    expect(plan('npx eslint .')).toEqual({ action: 'full', note: 'eslint config eslint.config.js affected; runs in full' });
  });

  it('only lints files under the command targets', async () => {
    const { plan } = await plans({ 'src/strings.ts': 'export const shout = (s: string) => s + "!";\n' });
    expect(plan('npx eslint src')).toMatchObject({ action: 'narrow', argv: ['npx', 'eslint', '--no-warn-ignored', 'src/strings.ts'] });
    expect(plan('npx eslint lib')).toMatchObject({ action: 'skip' });
  });

  it('turns every skip or narrow into a full run in shadow mode', async () => {
    const { plan } = await plans({ 'README.md': 'x' }, { ...MIXED, '.picky.yml': 'mode: shadow\nmodel: none\n' });
    expect(plan('npx tsc')).toEqual({
      action: 'full',
      note: 'shadow mode: would skip: no changes tsc checks; running in full',
      enforce: { action: 'skip', note: 'skip: no changes tsc checks' },
    });
  });
});
