import { describe, expect, it } from 'bun:test';
import { buildGraph, phpReferencersOf, transitiveDependents } from '../src/graph.ts';
import { declaredNames, parsePhp } from '../src/php.ts';
import { SHOP_PHP, tempDir, writeFiles } from './helpers.ts';

function graphOf(files: Record<string, string>) {
  const dir = tempDir();
  writeFiles(dir, files);
  return buildGraph(dir, Object.keys(files));
}

const deps = (g: ReturnType<typeof graphOf>, file: string) => [...(g.imports.get(file) ?? [])].sort();

describe('parsePhp', () => {
  it('reads namespaces, declarations, imports (aliases and groups) and includes', () => {
    const php = parsePhp(
      'src/Http/Kernel.php',
      [
        '<?php',
        'namespace App\\Http;',
        'use App\\Models\\{User, Post as P};',
        'use Illuminate\\Support\\Str as S;',
        'use function App\\helpers\\money;',
        "require __DIR__ . '/routes.php';",
        "require_once dirname(__DIR__) . '/bootstrap.php';",
        'final class Kernel { use Concerns\\Boots; }',
      ].join('\n'),
    );
    expect(php.classes).toEqual(['app\\http\\kernel']);
    expect(php.references).toContain('app\\models\\user');
    expect(php.references).toContain('app\\models\\post');
    expect(php.references).toContain('illuminate\\support\\str');
    expect(php.references).toContain('app\\helpers\\money');
    expect(php.references).toContain('app\\http\\concerns\\boots'); // trait, relative to the namespace
    expect(php.references).not.toContain('app\\http\\kernel');
    expect(php.includes).toEqual(['src/Http/routes.php', 'src/bootstrap.php']);
  });

  it('reads statements that share a line with the open tag', () => {
    const php = parsePhp('a.php', '<?php namespace App; use App\\Models\\User; final class Money { use HasCents; }');
    expect(php.classes).toEqual(['app\\money']);
    expect(php.references).toContain('app\\models\\user');
    expect(php.references).toContain('app\\hascents');
  });

  it('finds class names in docblocks and strings', () => {
    const php = parsePhp('a.php', "<?php\nnamespace A;\n/** @return \\App\\Dto */\nfunction f() { return app('App\\\\Models\\\\User'); }\n");
    expect(php.references).toContain('app\\dto');
    expect(php.references).toContain('app\\models\\user');
  });

  it('declares global functions only in files without classes', () => {
    expect(declaredNames("<?php\nfunction helper() {}\n")).toEqual(['helper']);
    expect(declaredNames('<?php\nnamespace A;\nclass B { function c() {} }\n')).toEqual(['a\\b']);
  });
});

describe('buildGraph: PHP', () => {
  it('links files by class name, including same-namespace and docblock references', () => {
    const g = graphOf(SHOP_PHP);
    expect(deps(g, 'app/Cart.php')).toEqual(['app/Money.php']); // only a @var docblock names Money
    expect(deps(g, 'tests/Unit/CartTest.php')).toEqual(['app/Cart.php', 'tests/TestCase.php']);
    expect(deps(g, 'tests/Unit/UserTest.php')).toEqual(['app/Models/User.php', 'tests/TestCase.php']);
    expect(deps(g, 'tests/Unit/FormatTest.php')).toEqual(['app/helpers.php', 'tests/TestCase.php']);
  });

  it('makes every Pest test depend on its Pest.php, and so on the base test case it binds', () => {
    const g = graphOf(SHOP_PHP);
    expect(deps(g, 'tests/Feature/HomeTest.php')).toEqual(['tests/Feature/Pest.php']);
    expect([...transitiveDependents(g, ['tests/TestCase.php'])].sort()).toEqual([
      'tests/Feature/HomeTest.php',
      'tests/Feature/Pest.php',
      'tests/Unit/CartTest.php',
      'tests/Unit/FormatTest.php',
      'tests/Unit/UserTest.php',
    ]);
  });

  it('links tests to a phpunit.xml bootstrap file in the repo', () => {
    const g = graphOf({
      ...SHOP_PHP,
      'phpunit.xml': '<?xml version="1.0"?>\n<phpunit bootstrap="tests/bootstrap.php" colors="true"></phpunit>',
      'tests/bootstrap.php': '<?php\n',
    });
    expect(deps(g, 'tests/Unit/UserTest.php')).toContain('tests/bootstrap.php');
  });

  it('ignores vendor/ and finds who referenced a removed class', () => {
    const g = graphOf({ ...SHOP_PHP, 'vendor/acme/lib/Cart.php': '<?php\nnamespace App;\nclass Cart {}\n' });
    expect(g.imports.has('vendor/acme/lib/Cart.php')).toBe(false);
    expect([...phpReferencersOf(g, ['app\\models\\user'])]).toEqual(['tests/Unit/UserTest.php']);
  });
});
