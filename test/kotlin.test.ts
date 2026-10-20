import { describe, expect, it } from 'bun:test';
import path from 'node:path';
import { recognize } from '../src/commands.ts';
import { buildGraph, jvmReferencersOf, transitiveDependents } from '../src/graph.ts';
import { includedBuilds, parseJvm, settingsIncludes, stripCode } from '../src/kotlin.ts';
import { SHOP_KOTLIN, tempDir, writeFiles } from './helpers.ts';

function build(files: Record<string, string> = SHOP_KOTLIN) {
  const dir = tempDir();
  writeFiles(dir, { '.git/HEAD': 'ref: refs/heads/main\n', ...files });
  return dir;
}

function graphOf(files: Record<string, string>) {
  return buildGraph(build(files), Object.keys(files));
}

const dependents = (g: ReturnType<typeof graphOf>, file: string) => [...transitiveDependents(g, [file])].sort();

describe('stripCode', () => {
  it('blanks comments and strings, keeping offsets, templates and nested comments', () => {
    const text = 'val a = "x ${b["k"]} // y" /* c /* d */ e */ + \'q\'\n// tail\nval z = """raw "quoted" """';
    const code = stripCode(text, 'kotlin');
    expect(code.length).toBe(text.length);
    expect(code.split('\n').length).toBe(3);
    expect(code).not.toMatch(/[xyceq]|tail|raw|quoted/);
    expect(code).toContain('val a = "');
    expect(code).toContain('val z = """');
    const kts = 'include("a") // include("b")';
    expect(stripCode(kts, 'kotlin', true)).toBe('include("a")'.padEnd(kts.length));
    const groovy = "include 'a' // 'b'";
    expect(stripCode(groovy, 'groovy', true)).toBe("include 'a'".padEnd(groovy.length));
  });
});

describe('parseJvm', () => {
  it('reads declarations, test classes and the names a Kotlin file references', () => {
    const jvm = parseJvm(
      'src/test/kotlin/shop/CartTest.kt',
      [
        '@file:JvmName("CartTests")',
        'package shop',
        '',
        'import shop.money.Money as Cash',
        'import shop.text.*',
        'import org.junit.jupiter.api.Test',
        '',
        'abstract class Base',
        'enum class Kind { A }',
        'sealed interface Shape',
        'typealias Id = Long',
        '',
        'class CartTest : Base() {',
        '    class Nested { fun inner() = 1 }',
        '    @Test fun totals() { Cash(1); shout("a"); Class.forName("shop.repo.Store") }',
        '}',
        '',
        'fun helper() = Kind.A',
        'val VERSION = 1',
      ].join('\n'),
    );
    expect(jvm.package).toBe('shop');
    expect(jvm.declarations).toEqual([
      'shop.Base',
      'shop.Kind',
      'shop.Shape',
      'shop.Id',
      'shop.CartTest',
      'shop.helper',
      'shop.VERSION',
      'shop.CartTestKt',
      'shop.CartTests',
    ]);
    expect(jvm.testClasses).toEqual(['shop.CartTest']); // not abstract, enum, interface or nested
    expect(jvm.references).toContain('shop.money.Money'); // through the alias
    expect(jvm.references).toContain('shop.text.shout'); // through the wildcard
    expect(jvm.references).toContain('shop.repo.Store'); // a name in a string
    expect(jvm.references).not.toContain('shop.CartTest');
  });

  it('finds test classes that inherit their tests, and Java classes', () => {
    expect(parseJvm('a/FooSpec.kt', 'package a\nclass FooSpec : FunSpec({ test("x") {} })\n').testClasses).toEqual(['a.FooSpec']);
    expect(parseJvm('a/FooTest.kt', 'package a\nclass FooTest : BaseTest()\n').testClasses).toEqual(['a.FooTest']);
    expect(parseJvm('a/Helper.kt', 'package a\nclass Helper\n').testClasses).toEqual([]);
    const java = parseJvm(
      'a/FooTest.java',
      'package a;\nimport static org.junit.Assert.*;\npublic class FooTest {\n  @Test public void x() {}\n}\n',
    );
    expect(java).toMatchObject({ package: 'a', declarations: ['a.FooTest'], testClasses: ['a.FooTest'] });
  });
});

describe('buildGraph: Kotlin and Java', () => {
  it('makes test classes under src/test test units', () => {
    expect(graphOf(SHOP_KOTLIN).testUnits.sort()).toEqual([
      'app/src/test/java/app/ShoutTest.java',
      'core/src/test/kotlin/shop/CartTest.kt',
      'core/src/test/kotlin/shop/MoneyTest.kt',
    ]);
  });

  it('links files by name: same package, imports, Java calls to Kotlin facades, resources', () => {
    const g = graphOf(SHOP_KOTLIN);
    expect(dependents(g, 'core/src/main/kotlin/shop/Money.kt')).toEqual([
      'core/src/main/kotlin/shop/Cart.kt',
      'core/src/test/kotlin/shop/CartTest.kt',
      'core/src/test/kotlin/shop/MoneyTest.kt',
    ]);
    expect(dependents(g, 'core/src/main/kotlin/shop/text/Strings.kt')).toEqual([
      'app/src/main/kotlin/app/Main.kt',
      'app/src/test/java/app/ShoutTest.java',
    ]);
    expect(dependents(g, 'core/src/test/resources/cart.json')).toEqual([
      'core/src/test/kotlin/shop/CartTest.kt',
      'core/src/test/kotlin/shop/MoneyTest.kt',
    ]);
  });

  it('finds who referenced a removed declaration', () => {
    const g = graphOf(SHOP_KOTLIN);
    expect([...jvmReferencersOf(g, ['shop.Cart'])].sort()).toEqual(['core/src/test/kotlin/shop/CartTest.kt']);
  });
});

describe('Gradle settings', () => {
  it('reads literal includes in Kotlin and Groovy settings', () => {
    expect(settingsIncludes('rootProject.name = "x"\ninclude("core", ":app")\ninclude(":libs:db") // include("old")\n', 'kotlin')).toEqual([
      'core',
      ':app',
      ':libs:db',
    ]);
    expect(settingsIncludes("include 'core',\n  'app'\ninclude ':libs:db'\n", 'groovy')).toEqual(['core', 'app', ':libs:db']);
  });

  it('gives up on computed projects', () => {
    expect(settingsIncludes('listOf("a", "b").forEach { include(it) }\n', 'kotlin')).toBeNull();
    expect(settingsIncludes('include("a")\nproject(":a").projectDir = file("modules/a")\n', 'kotlin')).toBeNull();
    expect(settingsIncludes('include("$prefix-core")\n', 'kotlin')).toBeNull();
  });

  it('finds included builds', () => {
    const dir = build({ 'settings.gradle.kts': 'pluginManagement {\n    includeBuild("build-logic")\n}\ninclude("app")\n' });
    expect(includedBuilds(dir, ['settings.gradle.kts'])).toEqual(['build-logic']);
  });
});

describe('recognize: gradle', () => {
  const narrow = (argv: string[], cwd: string, files: string[]) => {
    const c = recognize(argv, cwd);
    return c?.kind === 'tests' ? c.narrow(files) : c;
  };

  it('narrows `gradle test` to each project task with --tests filters', () => {
    const dir = build();
    const argv = narrow(['./gradlew', 'test', '--no-daemon', '-Pci=true'], dir, [
      'core/src/test/kotlin/shop/MoneyTest.kt',
      'app/src/test/java/app/ShoutTest.java',
      'core/src/test/kotlin/shop/CartTest.kt',
    ]);
    expect(argv).toEqual([
      './gradlew',
      '--no-daemon',
      '-Pci=true',
      '--init-script',
      expect.stringMatching(/picky-1-no-matching-tests\.init\.gradle$/),
      ':app:test',
      '--tests',
      'app.ShoutTest*',
      ':core:test',
      '--tests',
      'shop.CartTest*',
      '--tests',
      'shop.MoneyTest*',
    ]);
  });

  it('addresses the root project as :test', () => {
    const dir = build({
      'build.gradle.kts': 'plugins { kotlin("jvm") }\n',
      'src/test/kotlin/a/ATest.kt': 'package a\nclass ATest { @Test fun x() {} }\n',
    });
    expect(narrow(['gradle', 'test'], dir, ['src/test/kotlin/a/ATest.kt'])).toEqual([
      'gradle',
      '--init-script',
      expect.any(String),
      ':test',
      '--tests',
      'a.ATest*',
    ]);
  });

  it('runs Android and Kotlin Multiplatform projects in full', () => {
    const dir = build({ ...SHOP_KOTLIN, 'app/build.gradle.kts': 'plugins { id("com.android.application") }\n' });
    const c = recognize(['./gradlew', 'test'], dir);
    expect(c).toMatchObject({ kind: 'tests', whyNot: 'Android and Kotlin Multiplatform test tasks take no --tests filter' });
    expect(c?.kind === 'tests' && c.narrow(['app/src/test/java/app/ShoutTest.java'])).toBeNull();
    expect(c?.kind === 'tests' && c.narrow(['core/src/test/kotlin/shop/MoneyTest.kt'])).not.toBeNull();
  });

  it('only owns test files of the build’s projects', () => {
    const dir = build();
    const c = recognize(['./gradlew', 'test'], dir);
    const owns = (f: string) => c?.kind === 'tests' && c.owns!(f);
    expect(owns('core/src/test/kotlin/shop/MoneyTest.kt')).toBe(true);
    expect(owns('tools/src/test/kotlin/ToolTest.kt')).toBe(false); // not an included project
    expect(recognize(['../gradlew', 'test'], path.join(dir, 'core'))).toMatchObject({ kind: 'tests' });
  });

  it('treats verification tasks as checks and leaves other invocations alone', () => {
    const dir = build();
    expect(recognize(['./gradlew', 'check'], dir)).toEqual({ kind: 'check', tool: 'gradle check', ecosystem: 'kotlin' });
    expect(recognize(['./gradlew', 'detekt', ':core:ktlintCheck'], dir)).toMatchObject({
      kind: 'check',
      tool: 'gradle detekt :core:ktlintCheck',
    });
    for (const argv of [
      ['./gradlew', 'test', '--tests', 'shop.MoneyTest'],
      ['./gradlew', ':core:test'],
      ['./gradlew', 'test', 'build'],
      ['./gradlew', 'build'],
      ['./gradlew', '-p', 'core', 'test'],
      ['./gradlew', 'test', '--some-new-flag'],
    ]) {
      expect(recognize(argv, dir), argv.join(' ')).toBeNull();
    }
    const computed = build({ ...SHOP_KOTLIN, 'settings.gradle.kts': 'file("modules").listFiles().forEach { include(it.name) }\n' });
    expect(recognize(['./gradlew', 'test'], computed)).toBeNull();
  });
});

describe('recognize: ktlint and detekt', () => {
  it('narrows ktlint without patterns to the changed Kotlin files', () => {
    const c = recognize(['ktlint', '--reporter', 'plain', '--relative'], tempDir());
    expect(c).toMatchObject({ kind: 'files', tool: 'ktlint', targets: ['.'], withDependents: false });
    expect(c?.kind === 'files' && c.narrow(['a/B.kt'])).toEqual(['ktlint', '--reporter', 'plain', '--relative', 'a/B.kt']);
    expect(recognize(['ktlint', 'src/**/*.kt', '!**/generated/**'], tempDir())).toEqual({
      kind: 'check',
      tool: 'ktlint',
      ecosystem: 'kotlin',
    });
  });

  it('treats detekt as a check', () => {
    expect(recognize(['detekt', '--build-upon-default-config'], tempDir())).toEqual({ kind: 'check', tool: 'detekt', ecosystem: 'kotlin' });
  });
});
