import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

/**
 * Kotlin (and Java) dependency extraction. Like PHP, JVM files link by fully-qualified name,
 * not by path, so the graph indexes every top-level declaration, then resolves the names each
 * file mentions: imports (aliases and `*`), same-package names, and fully-qualified names.
 * Names are read from the raw text, strings and comments included (`Class.forName("a.B")`,
 * KDoc links); an extra edge only ever selects more. Java files are parsed too: Gradle compiles
 * and tests them together with Kotlin, and Kotlin code calls Java classes by name.
 */

export interface JvmFile {
  package: string;
  /** Fully-qualified top-level declarations: classes, objects, interfaces, type aliases, functions, properties. */
  declarations: string[];
  /** Fully-qualified names this file may reference. */
  references: Set<string>;
  /** Top-level classes a test task could run: concrete, with a test annotation in the file or a supertype. */
  testClasses: string[];
}

type Language = 'kotlin' | 'java' | 'groovy';

export const isJvmSource = (file: string) => /\.(?:kt|java)$/.test(file);

/** A file under a Gradle/Maven test source root, with the project directory ('' for the root) in group 1. */
const TEST_ROOT = /^(?:(.*?)\/)?src\/test\/(?:kotlin|java)\//;
const RESOURCES = /^(?:(.*?)\/)?src\/(?:main|test)\/resources\//;

const TYPE =
  /((?:@[\w.]+(?:\(\s*\))?\s+|\b(?:public|internal|private|protected|abstract|open|sealed|data|final|inner|value|annotation|enum|static|strictfp|expect|actual|external)\s+)*)(?<![:.])\b(class|interface|object|typealias|record|enum)\s+([A-Za-z_]\w*)(?=([^{]{0,300}))/g;
const KEYWORDS = new Set(['class', 'interface', 'object', 'fun', 'val', 'var']);
const FUN = /\bfun\s+(?:<[^>\n]*>\s*)?(?:[\w?<>, .]+\.)?([A-Za-z_]\w*)\s*\(/g;
const PROPERTY = /\b(?:val|var)\s+(?:<[^>\n]*>\s*)?(?:[\w?<>]+\.)*([A-Za-z_]\w*)/g;
const IMPORT = /^[ \t]*import\s+(?:static\s+)?([\w.]+?)(\.\*)?(?:\s+as\s+(\w+))?\s*;?[ \t]*$/gm;
const NAME = /[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*/g;
const TEST_MARKER =
  /@(?:[\w.]+\.)?(?:Test|ParameterizedTest|RepeatedTest|TestFactory|TestTemplate)\b|\b(?:FunSpec|StringSpec|ShouldSpec|DescribeSpec|BehaviorSpec|WordSpec|FreeSpec|FeatureSpec|ExpectSpec|AnnotationSpec|Spek)\s*\(/;

export function parseJvm(file: string, text: string): JvmFile {
  const language: Language = file.endsWith('.java') ? 'java' : 'kotlin';
  const code = stripCode(text, language);
  const pkg = /^\s*package\s+([\w.]+)/m.exec(code)?.[1] ?? '';
  const qualify = (name: string) => (pkg ? `${pkg}.${name}` : name);
  const top = topLevel(code);

  const declarations: string[] = [];
  const testClasses: string[] = [];
  const marked = TEST_MARKER.test(code);
  for (const m of top.matchAll(TYPE)) {
    const [, modifiers, kind, name, header] = m;
    if (KEYWORDS.has(name)) continue; // `enum class X` seen from `enum`, or `Foo::class` before a declaration
    declarations.push(qualify(name));
    const concrete = kind === 'class' && !/\b(?:abstract|sealed|annotation|enum)\b|@interface/.test(modifiers);
    // A class without a test annotation in its file can still inherit its tests from a supertype.
    if (concrete && (marked || /:|\bextends\b/.test(header))) testClasses.push(qualify(name));
  }
  if (language === 'kotlin') {
    const members = [...[...top.matchAll(FUN)].map((m) => m[1]), ...[...top.matchAll(PROPERTY)].map((m) => m[1])];
    declarations.push(...members.map(qualify));
    // Java sees top-level members as static methods of a facade class: FileNameKt, or @file:JvmName.
    if (members.length) {
      declarations.push(qualify(`${path.posix.basename(file, '.kt').replace(/^\w/, (c) => c.toUpperCase())}Kt`));
      const jvmName = /@file\s*:\s*JvmName\s*\(\s*"(\w+)"/.exec(text)?.[1];
      if (jvmName) declarations.push(qualify(jvmName));
    }
  }

  const aliases = new Map<string, string>();
  const wildcards: string[] = [];
  const references = new Set<string>();
  for (const m of code.matchAll(IMPORT)) {
    const [, name, star, alias] = m;
    if (star) wildcards.push(name);
    else aliases.set(alias ?? name.split('.').pop()!, name);
    addPrefixes(references, name);
  }
  for (const token of new Set(Array.from(text.matchAll(NAME), (m) => m[0]))) {
    addPrefixes(references, token);
    const [first, ...rest] = token.split('.');
    references.add(qualify(first));
    const alias = aliases.get(first);
    if (alias) addPrefixes(references, [alias, ...rest].join('.'));
    for (const w of wildcards) references.add(`${w}.${first}`);
  }
  for (const own of declarations) references.delete(own);
  return { package: pkg, declarations, references, testClasses };
}

/** `a.b.C.d` -> a.b, a.b.C, a.b.C.d: a member import or call names its class too. */
function addPrefixes(out: Set<string>, name: string): void {
  const parts = name.split('.');
  for (let i = 1; i <= parts.length; i++) out.add(parts.slice(0, i).join('.'));
}

/**
 * Blanks out comments, and string contents unless `keepStrings`, keeping offsets and line breaks.
 * Handles Kotlin templates (`"${a["b"]}"`), raw strings and text blocks, nested Kotlin comments
 * and Groovy's single-quoted strings.
 */
export function stripCode(text: string, language: Language, keepStrings = false): string {
  const out = text.split('');
  const blank = (from: number, to: number) => {
    for (let k = from; k < to && k < out.length; k++) if (out[k] !== '\n') out[k] = ' ';
  };
  const blankString = (from: number, to: number) => {
    if (!keepStrings) blank(from, to);
  };
  const templates = language !== 'java';
  const stack: Array<{ quote: string; braces: number }> = [];
  let i = 0;
  while (i < text.length) {
    const top = stack.at(-1);
    const c = text[i];
    if (top && top.quote !== '${') {
      if (text.startsWith(top.quote, i)) {
        stack.pop();
        i += top.quote.length;
      } else if (top.quote.length === 1 && c === '\n') {
        stack.pop(); // unterminated: stop at the line end
        i++;
      } else if (c === '\\' && top.quote !== '"""') {
        blankString(i, i + 2);
        i += 2;
      } else if (templates && top.quote[0] === '"' && c === '$' && text[i + 1] === '{') {
        blankString(i, i + 2);
        stack.push({ quote: '${', braces: 0 });
        i += 2;
      } else {
        blankString(i, i + 1);
        i++;
      }
      continue;
    }
    if (text.startsWith('//', i)) {
      const nl = text.indexOf('\n', i);
      const end = nl < 0 ? text.length : nl;
      blank(i, end);
      i = end;
      continue;
    }
    if (text.startsWith('/*', i)) {
      let depth = 1;
      let j = i + 2;
      while (j < text.length && depth > 0) {
        if (language === 'kotlin' && text.startsWith('/*', j)) {
          depth++;
          j += 2;
        } else if (text.startsWith('*/', j)) {
          depth--;
          j += 2;
        } else j++;
      }
      blank(i, j);
      i = j;
      continue;
    }
    const quote = ['"""', "'''", '"', "'"].find((q) => text.startsWith(q, i) && (q[0] === '"' || language === 'groovy'));
    if (quote) {
      stack.push({ quote, braces: 0 });
      i += quote.length;
      continue;
    }
    if (c === "'") {
      const char = /^'(?:\\.[^'\n]{0,5}|[^'\\\n])'/.exec(text.slice(i, i + 9));
      if (char) {
        blankString(i + 1, i + char[0].length - 1);
        i += char[0].length;
        continue;
      }
    }
    if (top) {
      // Code inside a template is part of its string.
      if (c === '{') top.braces++;
      else if (c === '}' && top.braces-- === 0) stack.pop();
      blankString(i, i + 1);
    }
    i++;
  }
  return out.join('');
}

/** Code at the top level of the file: everything inside braces and parentheses blanked. */
function topLevel(code: string): string {
  const out = code.split('');
  let depth = 0;
  for (let i = 0; i < out.length; i++) {
    const c = out[i];
    if (c === '{' || c === '(') {
      if (depth > 0) out[i] = ' ';
      depth++;
    } else if (c === '}' || c === ')') {
      depth = Math.max(0, depth - 1);
      if (depth > 0) out[i] = ' ';
    } else if (depth > 0 && c !== '\n') out[i] = ' ';
  }
  return out.join('');
}

export interface JvmGraph {
  edges: Array<[from: string, to: string]>;
  /** Test sources that declare a class a test task can run. */
  units: string[];
  references: Map<string, Set<string>>;
}

export function jvmGraph(root: string, files: Set<string>): JvmGraph {
  const parsed = new Map<string, JvmFile>();
  for (const file of files) {
    if (!isJvmSource(file)) continue;
    try {
      parsed.set(file, parseJvm(file, readFileSync(path.join(root, file), 'utf8')));
    } catch {
      // unreadable: no edges, so nothing is skipped because of it
    }
  }
  const graph: JvmGraph = { edges: [], units: [], references: new Map() };
  const declaredIn = new Map<string, string[]>();
  for (const [file, jvm] of parsed) {
    for (const name of jvm.declarations) {
      if (!declaredIn.has(name)) declaredIn.set(name, []);
      declaredIn.get(name)!.push(file);
    }
  }
  for (const [file, jvm] of parsed) {
    graph.references.set(file, jvm.references);
    for (const name of jvm.references)
      for (const target of declaredIn.get(name) ?? []) if (target !== file) graph.edges.push([file, target]);
    if (TEST_ROOT.test(file) && jvm.testClasses.length) graph.units.push(file);
  }

  // Tests read their project's resources from the classpath (application.yml, fixtures).
  const resources = new Map<string, string[]>();
  for (const file of files) {
    const m = RESOURCES.exec(file);
    if (!m) continue;
    const dir = m[1] ?? '';
    if (!resources.has(dir)) resources.set(dir, []);
    resources.get(dir)!.push(file);
  }
  for (const unit of graph.units) for (const r of resources.get(TEST_ROOT.exec(unit)![1] ?? '') ?? []) graph.edges.push([unit, r]);
  return graph;
}

/** Declared names of a file's old content, to find who used a deleted file. */
export function jvmDeclaredNames(file: string, text: string): string[] {
  return parseJvm(file, text).declarations;
}

/** The runnable test classes of a test source (an absolute path), or none when it can't be read. */
export function jvmTestClasses(file: string): string[] {
  try {
    return parseJvm(file, readFileSync(file, 'utf8')).testClasses;
  } catch {
    return [];
  }
}

const SETTINGS = ['settings.gradle.kts', 'settings.gradle'];
const BUILD_FILES = ['build.gradle.kts', 'build.gradle'];

export interface GradleProject {
  /** Directory relative to the build root, '' for the root project. */
  dir: string;
  /** Gradle path: ':' for the root project, ':a:b' for a/b. */
  path: string;
}

export interface GradleBuild {
  root: string;
  /** The project whose `src/test` holds this absolute path, or null when no project of this build does. */
  projectOf(file: string): GradleProject | null;
  /** True when the project's `test` task can't take `--tests` (Android, Kotlin Multiplatform). */
  cantFilter(project: GradleProject): boolean;
}

/**
 * The Gradle build `cwd` belongs to, with its projects read from the settings file. Null when the
 * settings compute their projects (non-literal `include`, custom `projectDir`): picky can't map
 * test files to task paths there.
 */
export function gradleBuild(cwd: string): GradleBuild | null {
  cwd = path.resolve(cwd);
  const settings = findUp(cwd, SETTINGS);
  const projects = new Map<string, string>([['', ':']]);
  let root = cwd;
  if (settings) {
    root = path.dirname(settings);
    const included = settingsIncludes(read(settings) ?? '', settings.endsWith('.kts') ? 'kotlin' : 'groovy');
    if (!included) return null;
    for (const name of included) {
      const segments = name.split(':').filter(Boolean);
      for (let k = 1; k <= segments.length; k++) projects.set(segments.slice(0, k).join('/'), `:${segments.slice(0, k).join(':')}`);
    }
  } else if (!BUILD_FILES.some((f) => existsSync(path.join(cwd, f)))) return null;

  return {
    root,
    projectOf: (file) => {
      const rel = path.relative(root, file).split(path.sep).join('/');
      const m = rel.startsWith('..') ? null : TEST_ROOT.exec(rel);
      const projectPath = m && projects.get(m[1] ?? '');
      return projectPath ? { dir: m[1] ?? '', path: projectPath } : null;
    },
    cantFilter: (project) => {
      const dir = path.join(root, project.dir);
      const build = BUILD_FILES.map((f) => read(path.join(dir, f)) ?? '').join('\n');
      return (
        /android|multiplatform/i.test(build) ||
        ['src/main/AndroidManifest.xml', 'src/commonMain', 'src/commonTest'].some((f) => existsSync(path.join(dir, f)))
      );
    },
  };
}

/** Project paths named by `include` in a settings file, or null when any of them isn't a literal. */
export function settingsIncludes(text: string, language: Language): string[] | null {
  const code = stripCode(text, language, true);
  if (/\b(?:projectDir|includeFlat|children)\b/.test(code)) return null;
  const out: string[] = [];
  for (const m of code.matchAll(/(?<![.\w])include\b/g)) {
    const rest = code.slice(m.index + m[0].length);
    let args: string;
    if (/^\s*\(/.test(rest)) {
      const close = rest.indexOf(')');
      if (close < 0) return null;
      args = rest.slice(rest.indexOf('(') + 1, close);
    } else {
      // Groovy: `include 'a', 'b'`, possibly continued after a trailing comma.
      const lines = rest.split('\n');
      args = lines[0];
      for (let k = 1; args.trimEnd().endsWith(',') && k < lines.length; k++) args += `\n${lines[k]}`;
    }
    const literals = [...args.matchAll(/"([^"$]*)"|'([^']*)'/g)];
    if (args.replace(/"[^"$]*"|'[^']*'/g, '').replace(/[\s,]/g, '') !== '') return null;
    out.push(...literals.map((l) => l[1] ?? l[2]));
  }
  return out;
}

/** Directories of builds included with `includeBuild` (build logic, convention plugins), repo-relative. */
export function includedBuilds(root: string, files: string[]): string[] {
  const out: string[] = [];
  for (const file of files) {
    if (!SETTINGS.includes(path.posix.basename(file))) continue;
    const code = stripCode(read(path.join(root, file)) ?? '', file.endsWith('.kts') ? 'kotlin' : 'groovy', true);
    for (const m of code.matchAll(/\bincludeBuild\s*\(?\s*(?:"([^"$]+)"|'([^']+)')/g)) {
      const dir = path.posix.normalize(path.posix.join(path.posix.dirname(file), m[1] ?? m[2])).replace(/\/$/, '');
      if (!dir.startsWith('..') && dir !== '.') out.push(dir);
    }
  }
  return out;
}

const INIT_SCRIPT = `// Written by picky. A narrowed run filters each test task to the selected classes;
// a class without tests (a helper or base class) must not fail the task.
allprojects {
    tasks.withType(Test).configureEach {
        filter.failOnNoMatchingTests = false
    }
}
`;

/** An init script that lets a test filter match nothing. Null when it can't be written. */
export function gradleInitScript(): string | null {
  const file = path.join(tmpdir(), 'picky-1-no-matching-tests.init.gradle');
  try {
    if (read(file) !== INIT_SCRIPT) writeFileSync(file, INIT_SCRIPT);
    return file;
  } catch {
    return null;
  }
}

function findUp(cwd: string, names: string[]): string | null {
  for (let dir = cwd; ; dir = path.dirname(dir)) {
    for (const name of names) if (existsSync(path.join(dir, name))) return path.join(dir, name);
    if (existsSync(path.join(dir, '.git')) || path.dirname(dir) === dir) return null;
  }
}

function read(file: string): string | null {
  try {
    return readFileSync(file, 'utf8');
  } catch {
    return null;
  }
}
