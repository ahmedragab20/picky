import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

export function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), 'picky-test-'));
}

export function writeFiles(dir: string, files: Record<string, string | null>): void {
  for (const [file, content] of Object.entries(files)) {
    const full = path.join(dir, file);
    if (content === null) {
      rmSync(full);
      continue;
    }
    mkdirSync(path.dirname(full), { recursive: true });
    writeFileSync(full, content);
  }
}

export function sh(dir: string, cmd: string, args: string[]): string {
  return execFileSync(cmd, args, { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

/** A git repo on `main` with `base` committed, then a `feature` branch with `change` committed. */
export function repo(base: Record<string, string>, change: Record<string, string | null>): string {
  const dir = tempDir();
  const git = (...args: string[]) => sh(dir, 'git', ['-c', 'user.name=t', '-c', 'user.email=t@t', ...args]);
  git('init', '-q', '-b', 'main');
  writeFiles(dir, base);
  git('add', '-A');
  git('commit', '-qm', 'base');
  git('checkout', '-qb', 'feature');
  writeFiles(dir, change);
  git('add', '-A');
  git('commit', '-qm', 'change', '--allow-empty');
  return dir;
}

/** Environment for decide(): no CI variables leak in from the machine running the tests. */
export function cleanEnv(extra: Record<string, string> = {}): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { PATH: process.env.PATH, HOME: process.env.HOME };
  return { ...env, ...extra };
}

export const SHOP: Record<string, string> = {
  'package.json': JSON.stringify({ name: 'shop', type: 'module', scripts: { test: 'node --test' } }),
  'src/math.ts': 'export const add = (a: number, b: number) => a + b;\n',
  'src/cart.ts': "import { add } from './math.ts';\nexport const total = (xs: number[]) => xs.reduce(add, 0);\n",
  'src/strings.ts': 'export const shout = (s: string) => s.toUpperCase();\n',
  'test/math.test.ts': "import { add } from '../src/math.ts';\ntest('adds', () => {});\n",
  'test/cart.test.ts': "import { total } from '../src/cart.ts';\ntest('totals', () => {});\n",
  'test/strings.test.ts': "import { shout } from '../src/strings.ts';\ndescribe('shout', () => {});\n",
  'README.md': '# shop\n',
};

/** A small PHP app: PSR-4 classes, PHPUnit tests and a Pest suite. */
export const SHOP_PHP: Record<string, string> = {
  'composer.json': JSON.stringify({ autoload: { 'psr-4': { 'App\\': 'app/' } }, 'autoload-dev': { 'psr-4': { 'Tests\\': 'tests/' } } }),
  'composer.lock': JSON.stringify({ packages: [], 'packages-dev': [{ name: 'phpunit/phpunit', version: '11.5.0' }] }),
  'app/Money.php': '<?php\nnamespace App;\n\nfinal class Money\n{\n    public function __construct(public int $cents) {}\n}\n',
  'app/Cart.php': '<?php\nnamespace App;\n\nclass Cart\n{\n    /** @var Money[] */\n    private array $items = [];\n}\n',
  'app/Models/User.php': '<?php\nnamespace App\\Models;\n\nclass User {}\n',
  'app/helpers.php': '<?php\n\nfunction money_format_cents(int $c): string { return (string) $c; }\n',
  'tests/TestCase.php': '<?php\nnamespace Tests;\n\nabstract class TestCase extends \\PHPUnit\\Framework\\TestCase {}\n',
  'tests/Unit/CartTest.php':
    '<?php\nnamespace Tests\\Unit;\n\nuse App\\Cart;\nuse Tests\\TestCase;\n\nclass CartTest extends TestCase\n{\n    public function testEmpty(): void {}\n}\n',
  'tests/Unit/UserTest.php':
    '<?php\nnamespace Tests\\Unit;\n\nuse App\\Models\\{User};\nuse Tests\\TestCase;\n\nclass UserTest extends TestCase {}\n',
  'tests/Unit/FormatTest.php':
    "<?php\nnamespace Tests\\Unit;\n\nrequire_once __DIR__ . '/../../app/helpers.php';\n\nclass FormatTest extends \\Tests\\TestCase {}\n",
  'tests/Feature/Pest.php': "<?php\nuses(Tests\\TestCase::class)->in('.');\n",
  'tests/Feature/HomeTest.php': "<?php\ntest('home', function () { expect(true)->toBeTrue(); });\n",
};
