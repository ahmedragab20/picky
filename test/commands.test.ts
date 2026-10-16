import { describe, expect, it } from 'vitest';
import { recognize } from '../src/commands.ts';
import { tempDir, writeFiles } from './helpers.ts';

const files = ['test/a.test.ts', 'test/b.test.ts'];

function narrowed(cmd: string[], cwd = tempDir()) {
  const c = recognize(cmd, cwd);
  return c && c.kind !== 'check' ? c.narrow(files) : null;
}

describe('recognize', () => {
  it('narrows vitest, keeping launchers, sub-commands and flags', () => {
    expect(narrowed(['npx', '-y', 'vitest', 'run', '--coverage'])).toEqual(['npx', '-y', 'vitest', 'run', '--coverage', ...files]);
    expect(narrowed(['./node_modules/.bin/vitest'])).toEqual(['./node_modules/.bin/vitest', ...files]);
    expect(narrowed(['pnpm', 'exec', 'vitest', 'run'])).toEqual(['pnpm', 'exec', 'vitest', 'run', ...files]);
  });

  it('narrows jest by exact path and node --test by file', () => {
    expect(narrowed(['jest', '--ci'])).toEqual(['jest', '--ci', '--runTestsByPath', ...files]);
    expect(narrowed(['node', '--test'])).toEqual(['node', '--test', ...files]);
  });

  it('keeps environment assignments in front', () => {
    expect(narrowed(['CI=1', 'vitest', 'run'])).toEqual(['CI=1', 'vitest', 'run', ...files]);
  });

  it('leaves commands alone when they already target tests or are unknown', () => {
    expect(narrowed(['vitest', 'run', 'src/only'])).toBeNull();
    expect(narrowed(['vitest', '--reporter', 'dot'])).toBeNull(); // value could be a filter
    expect(narrowed(['node', 'script.js'])).toBeNull();
    expect(narrowed(['make', 'test'])).toBeNull();
  });

  it('resolves package scripts that run a known runner', () => {
    const cwd = tempDir();
    writeFiles(cwd, {
      'package.json': JSON.stringify({ scripts: { test: 'vitest run', unit: 'npm test', lint: 'eslint .', e2e: 'vitest run && echo' } }),
    });
    expect(narrowed(['npm', 'test', '--silent'], cwd)).toEqual(['npm', '--silent', 'run', 'test', '--', ...files]);
    expect(narrowed(['npm', 'run', 'unit'], cwd)).toEqual(['npm', 'run', 'unit', '--', ...files]);
    expect(narrowed(['pnpm', 'run', 'test'], cwd)).toEqual(['pnpm', 'run', 'test', ...files]);
    expect(narrowed(['yarn', 'test'], cwd)).toEqual(['yarn', 'run', 'test', ...files]);
    expect(narrowed(['npm', 'run', 'e2e'], cwd)).toBeNull();
    expect(narrowed(['npm', 'run', 'missing'], cwd)).toBeNull();
  });

  it('runs PHPUnit and Pest on several paths only from version 10', () => {
    const lock = (v: string) => ({ 'composer.lock': JSON.stringify({ packages: [], 'packages-dev': [{ name: 'phpunit/phpunit', version: v }] }) });
    const modern = tempDir();
    writeFiles(modern, lock('11.5.3'));
    expect(narrowed(['vendor/bin/phpunit', '--colors=never'], modern)).toEqual(['vendor/bin/phpunit', '--colors=never', ...files]);
    expect(narrowed(['php', '-d', 'memory_limit=-1', 'vendor/bin/pest'], modern)).toEqual(['php', '-d', 'memory_limit=-1', 'vendor/bin/pest', ...files]);
    expect(recognize(['phpunit', '--testsuite=unit'], modern)).toBeNull();

    const old = tempDir();
    writeFiles(old, lock('9.6.38'));
    const c = recognize(['vendor/bin/phpunit'], old);
    expect(c?.kind === 'tests' && c.narrow(['tests/ATest.php'])).toEqual(['vendor/bin/phpunit', 'tests/ATest.php']);
    expect(c?.kind === 'tests' && c.narrow(files)).toBeNull(); // 9.x would silently run only the first path
  });

  it('resolves composer scripts and always forwards paths after --', () => {
    const cwd = tempDir();
    writeFiles(cwd, {
      'composer.lock': JSON.stringify({ 'packages-dev': [{ name: 'phpunit/phpunit', version: '11.0.0' }] }),
      'composer.json': JSON.stringify({ scripts: { test: '@php vendor/bin/phpunit', unit: ['phpunit'], ci: '@test', stan: 'phpstan analyse', both: ['phpunit', 'pint'] } }),
    });
    expect(narrowed(['composer', 'test'], cwd)).toEqual(['composer', 'run-script', 'test', '--', ...files]);
    expect(narrowed(['composer', '-n', 'run', 'unit'], cwd)).toEqual(['composer', '-n', 'run-script', 'unit', '--', ...files]);
    expect(narrowed(['composer', 'ci'], cwd)).toEqual(['composer', 'run-script', 'ci', '--', ...files]);
    expect(recognize(['composer', 'stan'], cwd)).toMatchObject({ kind: 'check', tool: 'phpstan' });
    expect(recognize(['composer', 'both'], cwd)).toBeNull();
    expect(recognize(['composer', 'install'], cwd)).toBeNull();
  });

  it('treats type checkers and static analyzers as whole-project checks', () => {
    for (const [cmd, tool, ecosystem] of [
      [['npx', 'tsc', '--noEmit'], 'tsc', 'js'],
      [['vue-tsc', '-b'], 'vue-tsc', 'js'],
      [['vendor/bin/phpstan', 'analyse', 'src'], 'phpstan', 'php'],
      [['php', 'vendor/bin/php-cs-fixer', 'fix', '--dry-run'], 'php-cs-fixer', 'php'],
    ] as const) {
      expect(recognize([...cmd], tempDir())).toEqual({ kind: 'check', tool, ecosystem });
    }
  });
});

describe('recognize: per-file linters', () => {
  it('narrows ESLint with a flat config and keeps ignored files silent', () => {
    const cwd = tempDir();
    writeFiles(cwd, { 'eslint.config.js': 'export default [];' });
    const c = recognize(['npx', 'eslint', '--max-warnings', '0', 'src', 'test'], cwd);
    expect(c).toMatchObject({ kind: 'files', targets: ['src', 'test'], withDependents: true });
    expect(c?.kind === 'files' && c.narrow(['src/a.ts'])).toEqual(['npx', 'eslint', '--max-warnings', '0', '--no-warn-ignored', 'src/a.ts']);
    expect(recognize(['eslint'], cwd)).toMatchObject({ targets: ['.'] });
  });

  it('runs ESLint whole when narrowing could change its result', () => {
    const legacy = tempDir();
    writeFiles(legacy, { '.eslintrc.json': '{}' });
    expect(recognize(['eslint', '.'], legacy)).toMatchObject({ kind: 'check', tool: 'eslint' });

    const crossFile = tempDir();
    writeFiles(crossFile, { 'eslint.config.js': "export default [{ rules: { 'import/no-unused-modules': 'error' } }];" });
    expect(recognize(['eslint', '.'], crossFile)).toMatchObject({ kind: 'check' });

    const flat = tempDir();
    writeFiles(flat, { 'eslint.config.js': 'export default [];' });
    expect(recognize(['eslint', '--some-new-flag', 'value', '.'], flat)).toBeNull(); // unknown option: value or path?
  });

  it('narrows prettier, adding --ignore-unknown', () => {
    const c = recognize(['prettier', '--check', '.'], tempDir());
    expect(c?.kind === 'files' && c.narrow(['a.md'])).toEqual(['prettier', '--check', '--ignore-unknown', 'a.md']);
  });

  it('runs a narrowed lint script through the local binary, not the package manager', () => {
    const cwd = tempDir();
    writeFiles(cwd, {
      'eslint.config.js': 'export default [];',
      'node_modules/.bin/eslint': '',
      'package.json': JSON.stringify({ scripts: { lint: 'eslint .', prelinted: 'x', fmt: 'prettier --check .', prefmt: 'echo' } }),
    });
    const c = recognize(['CI=1', 'npm', 'run', 'lint'], cwd);
    expect(c?.kind === 'files' && c.narrow(['a.ts'])).toEqual(['CI=1', `${cwd}/node_modules/.bin/eslint`, '--no-warn-ignored', 'a.ts']);
    expect(recognize(['npm', 'run', 'fmt'], cwd)).toBeNull(); // a pre-script would be skipped
  });
});
