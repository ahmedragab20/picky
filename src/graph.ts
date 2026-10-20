import { readFileSync } from 'node:fs';
import path from 'node:path';
import ts from 'typescript';
import { jvmGraph } from './kotlin.ts';
import { parsePhp, type PhpFile } from './php.ts';
import { rustGraph } from './rust.ts';

const SOURCE_EXT = ['.ts', '.tsx', '.mts', '.cts', '.js', '.jsx', '.mjs', '.cjs'];
const RESOLVE_EXT = [...SOURCE_EXT, '.json'];
const JS_TO_TS: Record<string, string[]> = {
  '.js': ['.ts', '.tsx'],
  '.jsx': ['.tsx'],
  '.mjs': ['.mts'],
  '.cjs': ['.cts'],
};

export interface Graph {
  /** file -> repo files it imports */
  imports: Map<string, Set<string>>;
  /** file -> repo files that import it */
  dependents: Map<string, Set<string>>;
  /** file -> extension-less paths of relative imports that resolved to nothing */
  unresolved: Map<string, string[]>;
  /** PHP file -> lowercased fully-qualified names it may reference */
  phpReferences: Map<string, Set<string>>;
  /** Kotlin/Java file -> fully-qualified names it may reference */
  jvmReferences: Map<string, Set<string>>;
  /** Tests found from the build layout, not the file name: Rust crates (`Cargo.toml`) and integration targets, Kotlin/Java test classes. */
  testUnits: string[];
  /** Rust package directories ('.' for the root), to find the crate of a removed file. */
  rustCrates: string[];
}

export function isSource(file: string): boolean {
  return SOURCE_EXT.includes(path.extname(file)) && !file.endsWith('.d.ts');
}

export const isVendored = (f: string) => f.includes('node_modules/') || f.startsWith('vendor/') || f.includes('/vendor/');

/** Builds the dependency graph for JS/TS, PHP, Kotlin/Java and Rust files from the working tree. */
export function buildGraph(root: string, files: string[]): Graph {
  const fileSet = new Set(files.filter((f) => !isVendored(f)));
  const resolver = makeResolver(root, fileSet);
  const imports = new Map<string, Set<string>>();
  const dependents = new Map<string, Set<string>>();
  const unresolved = new Map<string, string[]>();

  for (const file of fileSet) {
    if (!isSource(file)) continue;
    let text: string;
    try {
      text = readFileSync(path.join(root, file), 'utf8');
    } catch {
      continue;
    }
    const deps = new Set<string>();
    for (const { fileName: spec } of ts.preProcessFile(text, true, true).importedFiles) {
      const target = resolver(spec, file);
      if (target) {
        deps.add(target);
        if (!dependents.has(target)) dependents.set(target, new Set());
        dependents.get(target)!.add(file);
      } else if (spec.startsWith('.')) {
        const stem = stripExt(path.posix.join(path.posix.dirname(file), spec));
        unresolved.set(file, [...(unresolved.get(file) ?? []), stem]);
      }
    }
    imports.set(file, deps);
  }
  const graph: Graph = {
    imports,
    dependents,
    unresolved,
    phpReferences: new Map(),
    jvmReferences: new Map(),
    testUnits: [],
    rustCrates: [],
  };
  addPhp(root, fileSet, graph);
  addJvm(root, fileSet, graph);
  addRust(root, fileSet, graph);
  return graph;
}

function addEdge(graph: Graph, from: string, to: string): void {
  if (from === to) return;
  if (!graph.imports.has(from)) graph.imports.set(from, new Set());
  graph.imports.get(from)!.add(to);
  if (!graph.dependents.has(to)) graph.dependents.set(to, new Set());
  graph.dependents.get(to)!.add(from);
}

function addPhp(root: string, fileSet: Set<string>, graph: Graph): void {
  const parsed = new Map<string, PhpFile>();
  for (const file of fileSet) {
    if (!file.endsWith('.php')) continue;
    try {
      parsed.set(file, parsePhp(file, readFileSync(path.join(root, file), 'utf8')));
    } catch {
      // unreadable: no edges, so nothing is skipped because of it
    }
  }

  // Index every declared name; duplicates (stubs, fixtures) all count.
  const declaredIn = new Map<string, string[]>();
  for (const [file, php] of parsed) {
    for (const name of [...php.classes, ...php.functions]) declaredIn.set(name, [...(declaredIn.get(name) ?? []), file]);
  }

  for (const [file, php] of parsed) {
    graph.imports.set(file, graph.imports.get(file) ?? new Set());
    graph.phpReferences.set(file, php.references);
    for (const name of php.references) for (const target of declaredIn.get(name) ?? []) addEdge(graph, file, target);
    for (const target of php.includes) if (fileSet.has(target)) addEdge(graph, file, target);
  }

  // Implicit test setup: Pest.php binds base classes by directory, and phpunit.xml names a bootstrap file.
  const tests = [...parsed.keys()].filter((f) => f.endsWith('Test.php'));
  for (const file of fileSet) {
    const name = path.posix.basename(file);
    const dir = path.posix.dirname(file);
    const under = (t: string) => dir === '.' || t.startsWith(`${dir}/`);
    if (name === 'Pest.php') for (const t of tests.filter(under)) addEdge(graph, t, file);
    if (/^phpunit(\.dist)?\.xml(\.dist)?$/.test(name)) {
      const bootstrap = phpunitBootstrap(path.join(root, file));
      const target = bootstrap && path.posix.normalize(path.posix.join(dir, bootstrap));
      if (target && fileSet.has(target)) for (const t of tests.filter(under)) addEdge(graph, t, target);
    }
  }
}

function addJvm(root: string, fileSet: Set<string>, graph: Graph): void {
  const jvm = jvmGraph(root, fileSet);
  for (const [file, refs] of jvm.references) {
    graph.jvmReferences.set(file, refs);
    if (!graph.imports.has(file)) graph.imports.set(file, new Set());
  }
  for (const [from, to] of jvm.edges) addEdge(graph, from, to);
  graph.testUnits.push(...jvm.units);
}

function addRust(root: string, fileSet: Set<string>, graph: Graph): void {
  const rust = rustGraph(root, fileSet);
  for (const unit of rust.units) if (!graph.imports.has(unit)) graph.imports.set(unit, new Set());
  for (const [from, to] of rust.edges) addEdge(graph, from, to);
  // An include of a path that's gone: the file now fails to compile, like a JS import of a removed file.
  for (const [file, target] of rust.missing) graph.unresolved.set(file, [...(graph.unresolved.get(file) ?? []), stripExt(target)]);
  graph.testUnits.push(...rust.units);
  graph.rustCrates = rust.crates;
}

function phpunitBootstrap(file: string): string | null {
  try {
    return /<phpunit\b[^>]*\bbootstrap\s*=\s*"([^"]+)"/.exec(readFileSync(file, 'utf8'))?.[1] ?? null;
  } catch {
    return null;
  }
}

/** PHP files that reference any of the given names (lowercased, fully qualified). */
export function phpReferencersOf(graph: Graph, names: string[]): Set<string> {
  const out = new Set<string>();
  for (const [file, refs] of graph.phpReferences) if (names.some((n) => refs.has(n))) out.add(file);
  return out;
}

/** Kotlin/Java files that reference any of the given fully-qualified names. */
export function jvmReferencersOf(graph: Graph, names: string[]): Set<string> {
  const out = new Set<string>();
  for (const [file, refs] of graph.jvmReferences) if (names.some((n) => refs.has(n))) out.add(file);
  return out;
}

/** All files that transitively import any of `changed` (excluding `changed` itself). */
export function transitiveDependents(graph: Graph, changed: Iterable<string>): Set<string> {
  const seen = new Set<string>();
  const queue = [...changed];
  while (queue.length) {
    const file = queue.pop()!;
    for (const dep of graph.dependents.get(file) ?? []) {
      if (!seen.has(dep)) {
        seen.add(dep);
        queue.push(dep);
      }
    }
  }
  return seen;
}

/** Files whose relative imports point at a path that no longer exists (deleted or renamed away). */
export function importersOfMissing(graph: Graph, missing: string[]): Set<string> {
  const stems = new Set(missing.flatMap((m) => [stripExt(m), stripExt(m).replace(/\/index$/, '')]));
  const out = new Set<string>();
  for (const [file, targets] of graph.unresolved) {
    if (targets.some((t) => stems.has(t))) out.add(file);
  }
  return out;
}

function stripExt(p: string): string {
  const ext = path.extname(p);
  return RESOLVE_EXT.includes(ext) ? p.slice(0, -ext.length) : p;
}

type Resolver = (spec: string, from: string) => string | null;

function makeResolver(root: string, files: Set<string>): Resolver {
  const tryFile = (p: string): string | null => {
    const candidates = [p];
    const ext = path.extname(p);
    for (const alt of JS_TO_TS[ext] ?? []) candidates.push(p.slice(0, -ext.length) + alt);
    for (const e of RESOLVE_EXT) candidates.push(p + e);
    for (const e of RESOLVE_EXT) candidates.push(`${p}/index${e}`);
    return candidates.find((c) => files.has(c)) ?? null;
  };

  const paths = readTsconfigPaths(root);
  const packages = readWorkspacePackages(root, files);

  return (spec, from) => {
    if (spec.startsWith('.')) return tryFile(path.posix.normalize(path.posix.join(path.posix.dirname(from), spec)));

    for (const { prefix, suffix, targets } of paths) {
      if (!spec.startsWith(prefix) || !spec.endsWith(suffix) || spec.length < prefix.length + suffix.length) continue;
      const star = spec.slice(prefix.length, spec.length - suffix.length);
      for (const target of targets) {
        const hit = tryFile(path.posix.normalize(target.replace('*', star)));
        if (hit) return hit;
      }
    }

    for (const pkg of packages) {
      if (spec === pkg.name) return pkg.entries.map(tryFile).find(Boolean) ?? null;
      if (spec.startsWith(pkg.name + '/')) return tryFile(path.posix.join(pkg.dir, spec.slice(pkg.name.length + 1)));
    }
    return null;
  };
}

interface PathMapping {
  prefix: string;
  suffix: string;
  /** Repo-relative targets with a `*` placeholder. */
  targets: string[];
}

function readTsconfigPaths(root: string): PathMapping[] {
  for (const name of ['tsconfig.json', 'jsconfig.json']) {
    const { config } = ts.readConfigFile(path.join(root, name), ts.sys.readFile);
    const opts = config?.compilerOptions;
    if (!opts?.paths) continue;
    const base = path.posix.normalize(opts.baseUrl ?? '.');
    return Object.entries(opts.paths as Record<string, string[]>).map(([pattern, targets]) => {
      const [prefix, suffix = ''] = pattern.split('*');
      return { prefix, suffix, targets: targets.map((t) => path.posix.normalize(path.posix.join(base, t))) };
    });
  }
  return [];
}

interface WorkspacePackage {
  name: string;
  dir: string;
  entries: string[];
}

function readWorkspacePackages(root: string, files: Set<string>): WorkspacePackage[] {
  const out: WorkspacePackage[] = [];
  for (const file of files) {
    if (!file.endsWith('/package.json')) continue;
    try {
      const pkg = JSON.parse(readFileSync(path.join(root, file), 'utf8'));
      if (typeof pkg.name !== 'string') continue;
      const dir = path.posix.dirname(file);
      const main = typeof pkg.exports === 'string' ? pkg.exports : (pkg.exports?.['.']?.default ?? pkg.exports?.['.'] ?? pkg.main);
      const entries = [typeof main === 'string' ? path.posix.join(dir, main) : null, `${dir}/src/index`, `${dir}/index`];
      out.push({ name: pkg.name, dir, entries: entries.filter((e): e is string => !!e) });
    } catch {
      // unreadable package.json: imports of it stay unresolved
    }
  }
  return out;
}
