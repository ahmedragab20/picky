import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import picomatch from 'picomatch';

/**
 * Rust dependency extraction, at crate level. A crate compiles as one unit, so its unit
 * tests and doctests depend on every `.rs` file of the package, on the files those sources
 * `include!`, on its build script's inputs, and on the workspace crates it depends on. The
 * crate is a test unit named by its `Cargo.toml`. Each integration test target
 * (`tests/<name>.rs`, `tests/<name>/main.rs`) is a unit of its own, so a change to one test
 * runs only that target.
 */

export interface Dependency {
  name: string;
  package?: string;
  path?: string;
  workspace?: boolean;
}

export interface Workspace {
  members: string[];
  exclude: string[];
  defaultMembers: string[] | null;
  dependencies: Map<string, Dependency>;
}

export interface Manifest {
  /** `[package] name`; null for a virtual workspace manifest. */
  name: string | null;
  workspace: Workspace | null;
  dependencies: Dependency[];
  /** False when `[[test]]` targets or `autotests = false` make `tests/<name>.rs` names unreliable. */
  autotests: boolean;
  /** `package.build`: a path, false for none, or null for the default `build.rs`. */
  build: string | false | null;
}

const DEPENDENCY_TABLES = new Set(['dependencies', 'dev-dependencies', 'build-dependencies']);

export function parseManifest(text: string): Manifest {
  const manifest: Manifest = {
    name: null,
    workspace: /^\s*\[workspace(?:\.[^\]]*)?\]/m.test(text)
      ? { members: [], exclude: [], defaultMembers: null, dependencies: new Map() }
      : null,
    dependencies: [],
    autotests: !/^\s*\[\[\s*test\s*\]\]/m.test(text),
    build: null,
  };
  const listed = new Map<string, Dependency>();
  const dependency = (where: Map<string, Dependency>, kind: string, name: string) => {
    const key = `${kind}\0${name}`;
    if (!where.has(key)) where.set(key, { name });
    return where.get(key)!;
  };
  const workspaceDeps = new Map<string, Dependency>();

  for (const { table, key, value } of tomlEntries(text)) {
    const t = table.join('.');
    if (t === 'package' && key.length === 1) {
      if (key[0] === 'name') manifest.name = tomlString(value);
      else if (key[0] === 'autotests' && value === 'false') manifest.autotests = false;
      else if (key[0] === 'build') manifest.build = value === 'false' ? false : tomlString(value);
      continue;
    }
    if (t === 'workspace' && manifest.workspace && key.length === 1) {
      if (key[0] === 'members') manifest.workspace.members = tomlStrings(value);
      else if (key[0] === 'exclude') manifest.workspace.exclude = tomlStrings(value);
      else if (key[0] === 'default-members') manifest.workspace.defaultMembers = tomlStrings(value);
      continue;
    }
    // [dependencies], [dev-dependencies], [target.'cfg(..)'.dependencies], [workspace.dependencies],
    // and their per-dependency tables ([dependencies.foo]).
    const at = table.findIndex(
      (s, i) => DEPENDENCY_TABLES.has(s) && (i === 0 || (i === 1 && table[0] === 'workspace') || (i === 2 && table[0] === 'target')),
    );
    if (at < 0 || table.length > at + 2) continue;
    const where = table[0] === 'workspace' ? workspaceDeps : listed;
    const kind = table.slice(0, at + 1).join('.');
    if (table.length === at + 2) {
      setField(dependency(where, kind, table[at + 1]), key.join('.'), value);
    } else if (key.length === 2) {
      setField(dependency(where, kind, key[0]), key[1], value);
    } else if (key.length === 1) {
      const dep = dependency(where, kind, key[0]);
      if (value.startsWith('{')) for (const field of ['path', 'package', 'workspace']) setField(dep, field, inlineField(value, field));
    }
  }
  manifest.dependencies = [...listed.values()];
  if (manifest.workspace) for (const dep of workspaceDeps.values()) manifest.workspace.dependencies.set(dep.name, dep);
  return manifest;
}

function setField(dep: Dependency, field: string, value: string | null): void {
  if (value === null) return;
  if (field === 'path') dep.path = tomlString(value) ?? undefined;
  else if (field === 'package') dep.package = tomlString(value) ?? undefined;
  else if (field === 'workspace') dep.workspace = value === 'true';
}

function inlineField(table: string, field: string): string | null {
  return new RegExp(String.raw`(?:^|[{,\s])${field}\s*=\s*("(?:\\.|[^"\\])*"|'[^']*'|\w+)`).exec(table)?.[1] ?? null;
}

/** Raw `key = value` entries of a TOML document: enough to read Cargo manifests without a TOML parser. */
export function tomlEntries(text: string): Array<{ table: string[]; key: string[]; value: string }> {
  const out: Array<{ table: string[]; key: string[]; value: string }> = [];
  let table: string[] = [];
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const line = lexToml(lines[i]).text.trim();
    if (!line) continue;
    if (line.startsWith('[')) {
      table = splitKey(line.replace(/^\[\[?/, '').replace(/\]\]?$/, ''));
      continue;
    }
    const eq = indexOutsideQuotes(line, '=');
    if (eq < 0) continue;
    let raw = lines[i].slice(lines[i].indexOf('=') + 1);
    // Arrays, inline tables and multi-line strings can span lines.
    while (!lexToml(raw).complete && i + 1 < lines.length) raw += `\n${lines[++i]}`;
    out.push({ table, key: splitKey(line.slice(0, eq)), value: lexToml(raw).text.trim() });
  }
  return out;
}

/** Strips comments and reports whether brackets, braces and multi-line strings are closed. */
function lexToml(s: string): { text: string; complete: boolean } {
  let text = '';
  let depth = 0;
  let quote: string | null = null;
  for (let i = 0; i < s.length;) {
    const c = s[i];
    if (quote) {
      if (quote.length === 1 && c === '\n') quote = null;
      else if (quote[0] === '"' && c === '\\') {
        text += s.slice(i, i + 2);
        i += 2;
        continue;
      } else if (s.startsWith(quote, i)) {
        text += quote;
        i += quote.length;
        quote = null;
        continue;
      }
      text += c;
      i++;
      continue;
    }
    if (c === '#') {
      const nl = s.indexOf('\n', i);
      i = nl < 0 ? s.length : nl;
      continue;
    }
    if (c === '"' || c === "'") {
      quote = s.startsWith(c.repeat(3), i) ? c.repeat(3) : c;
      text += quote;
      i += quote.length;
      continue;
    }
    if (c === '[' || c === '{') depth++;
    else if (c === ']' || c === '}') depth--;
    text += c;
    i++;
  }
  return { text, complete: depth <= 0 && !(quote && quote.length === 3) };
}

function splitKey(s: string): string[] {
  const out: string[] = [];
  let current = '';
  let quote: string | null = null;
  for (const c of s) {
    if (quote) {
      if (c === quote) quote = null;
      else current += c;
    } else if (c === '"' || c === "'") quote = c;
    else if (c === '.') {
      out.push(current.trim());
      current = '';
    } else current += c;
  }
  out.push(current.trim());
  return out;
}

function indexOutsideQuotes(s: string, char: string): number {
  let quote: string | null = null;
  for (let i = 0; i < s.length; i++) {
    if (quote) {
      if (s[i] === quote) quote = null;
    } else if (s[i] === '"' || s[i] === "'") quote = s[i];
    else if (s[i] === char) return i;
  }
  return -1;
}

const TOML_STRING = /"((?:\\.|[^"\\\n])*)"|'([^'\n]*)'/g;

function tomlString(value: string): string | null {
  const m = /^\s*(?:"((?:\\.|[^"\\])*)"|'([^']*)')\s*$/.exec(value);
  return m ? (m[1] ?? m[2]) : null;
}

function tomlStrings(value: string): string[] {
  return [...value.matchAll(TOML_STRING)].map((m) => m[1] ?? m[2]);
}

/** A crate: a directory with a `Cargo.toml` that has a `[package]`. */
interface Crate {
  /** Repo-relative, '.' for the root. */
  dir: string;
  manifest: string;
  name: string;
  parsed: Manifest;
}

export interface RustGraph {
  edges: Array<[from: string, to: string]>;
  /** Crate manifests and integration test targets. */
  units: string[];
  /** A file that names a path not in the repo, with that path. */
  missing: Array<[file: string, target: string]>;
  /** Crate directories, to find the crate a removed file belonged to. */
  crates: string[];
}

const INTEGRATION_TARGET = /^tests\/(?:[^/]+\.rs|[^/]+\/main\.rs)$/;
/** Files a build script without `rerun-if-changed` is assumed not to read. */
const NOT_BUILD_INPUT = picomatch(['**/*.{md,mdx,rst,adoc}', '**/LICENSE*', '**/.gitignore'], { dot: true });

export function rustGraph(root: string, files: Set<string>): RustGraph {
  const crates = new Map<string, Crate>();
  const workspaces = new Map<string, Manifest>();
  for (const file of files) {
    if (path.posix.basename(file) !== 'Cargo.toml') continue;
    const text = read(path.join(root, file));
    if (text === null) continue;
    const parsed = parseManifest(text);
    const dir = path.posix.dirname(file);
    if (parsed.workspace) workspaces.set(dir, parsed);
    if (parsed.name) crates.set(dir, { dir, manifest: file, name: parsed.name, parsed });
  }
  const graph: RustGraph = { edges: [], units: [], missing: [], crates: [...crates.keys()] };
  if (crates.size === 0) return graph;

  const dirs = new Set(crates.keys());
  const owned = new Map<Crate, string[]>();
  for (const file of files) {
    const dir = crateDirOf(dirs, file);
    if (dir === null) continue;
    const crate = crates.get(dir)!;
    if (!owned.has(crate)) owned.set(crate, []);
    owned.get(crate)!.push(file);
  }
  const under = (dir: string) => [...files].filter((f) => dir === '.' || f.startsWith(`${dir}/`));
  const link = (from: string, target: string) => {
    if (files.has(target)) graph.edges.push([from, target]);
    else {
      const inside = under(target);
      if (inside.length) for (const f of inside) graph.edges.push([from, f]);
      else graph.missing.push([from, target]);
    }
  };

  const byName = new Map([...crates.values()].map((c) => [c.name, c]));
  for (const crate of crates.values()) {
    const { manifest, parsed } = crate;
    graph.units.push(manifest);
    const crateFiles = owned.get(crate) ?? [];
    const rel = (f: string) => (crate.dir === '.' ? f : f.slice(crate.dir.length + 1));
    const targets = parsed.autotests ? crateFiles.filter((f) => INTEGRATION_TARGET.test(rel(f))) : [];
    graph.units.push(...targets);

    const build = buildInputs(root, crate, files);
    for (const file of crateFiles) {
      if (file === manifest) continue;
      if (targets.includes(file)) graph.edges.push([file, manifest]);
      // Helpers and fixtures under tests/ belong to every integration test (or to the crate, without per-target units).
      else if (rel(file).startsWith('tests/')) for (const t of targets.length ? targets : [manifest]) graph.edges.push([t, file]);
      else if (file.endsWith('.rs') || (build?.all && !NOT_BUILD_INPUT(file))) graph.edges.push([manifest, file]);
      if (file.endsWith('.rs')) {
        const text = read(path.join(root, file));
        if (text !== null) for (const target of sourceIncludes(crate.dir, file, text)) link(file, target);
      }
    }
    for (const input of build?.paths ?? []) if (files.has(input) || under(input).length) link(manifest, input);

    for (const dep of parsed.dependencies) {
      for (const target of resolveDependency(crate, dep, crates, byName, workspaces)) {
        if (target !== crate) graph.edges.push([manifest, target.manifest]);
      }
    }
  }
  return graph;
}

/** The deepest crate directory in `dirs` that contains `file`, or null. */
export function crateDirOf(dirs: Set<string>, file: string): string | null {
  for (let dir = path.posix.dirname(file); ; dir = path.posix.dirname(dir)) {
    if (dirs.has(dir)) return dir;
    if (dir === '.' || dir === '/') return null;
  }
}

function resolveDependency(
  crate: Crate,
  dep: Dependency,
  crates: Map<string, Crate>,
  byName: Map<string, Crate>,
  workspaces: Map<string, Manifest>,
): Crate[] {
  let { path: depPath, package: pkg } = dep;
  let base = crate.dir;
  if (dep.workspace) {
    for (let dir = crate.dir; ; dir = path.posix.dirname(dir)) {
      const shared = workspaces.get(dir)?.workspace?.dependencies.get(dep.name);
      if (shared) {
        ({ path: depPath, package: pkg } = shared);
        base = dir;
        break;
      }
      if (dir === '.') break;
    }
  }
  const out: Crate[] = [];
  if (depPath) {
    const byPath = crates.get(path.posix.normalize(path.posix.join(base, depPath)).replace(/\/$/, '') || '.');
    if (byPath) out.push(byPath);
  }
  // By name too: a renamed or workspace-inherited dependency still names the crate. An extra edge only selects more.
  const named = byName.get(pkg ?? dep.name);
  if (named) out.push(named);
  return out;
}

/**
 * What a build script reads. Cargo reruns a script without `rerun-if-changed` on any change in
 * its package, so all of the package counts (docs aside). Paths written as string literals in the
 * script (`compile_protos(&["proto/api.proto"])`) count too: such tools print their own
 * `rerun-if-changed` at build time, where picky can't see it.
 */
function buildInputs(root: string, crate: Crate, files: Set<string>): { all: boolean; paths: string[] } | null {
  if (crate.parsed.build === false) return null;
  const script = path.posix.join(crate.dir, crate.parsed.build ?? 'build.rs');
  if (!files.has(script)) return null;
  const text = read(path.join(root, script));
  if (text === null) return { all: true, paths: [] };
  const reruns = [...text.matchAll(/cargo::?rerun-if-changed=([^"\n]*)/g)].map((m) => m[1]);
  const literals = [...text.matchAll(/"((?:\\.|[^"\\\n])*)"/g)].map((m) => m[1]).filter((s) => s && s !== '.' && !/[:={}\\]/.test(s));
  const paths = [...reruns.filter((r) => !r.includes('{')), ...literals]
    .map((p) => path.posix.normalize(path.posix.join(crate.dir, p)).replace(/\/$/, ''))
    .filter((p) => !p.startsWith('..'));
  return { all: reruns.length === 0 || reruns.some((r) => r.includes('{')), paths };
}

// include!/include_str!/include_bytes!("x"), or concat!(env!("CARGO_MANIFEST_DIR"), "/x").
const INCLUDE = /\binclude(?:_str|_bytes)?!\s*\(\s*(concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*)?"([^"]+)"/g;
// Macros that read a path relative to the package: include_dir!("$CARGO_MANIFEST_DIR/assets"), sqlx's migrate!("./migrations").
const PACKAGE_PATH = /\b(?:include_dir|migrate)!\s*\(\s*"(?:\$CARGO_MANIFEST_DIR\/)?([^"]+)"/g;
const PATH_ATTRIBUTE = /#\[\s*path\s*=\s*"([^"]+)"\s*\]/g;

/** Repo-relative paths a Rust source reads at compile time. */
export function sourceIncludes(crateDir: string, file: string, text: string): string[] {
  const out: string[] = [];
  const add = (base: string, target: string) => {
    const resolved = path.posix.normalize(path.posix.join(base, target.replace(/^\//, ''))).replace(/\/$/, '');
    if (!resolved.startsWith('..')) out.push(resolved);
  };
  const dir = path.posix.dirname(file);
  for (const m of text.matchAll(INCLUDE)) add(m[1] ? crateDir : dir, m[2]);
  for (const m of text.matchAll(PACKAGE_PATH)) add(crateDir, m[1]);
  for (const m of text.matchAll(PATH_ATTRIBUTE)) add(dir, m[1]);
  return out;
}

/** Which crates a cargo command covers, from its working directory. */
export interface CargoScope {
  /** True when the crate owning this absolute path is one the command tests. */
  owns(file: string): boolean;
}

/**
 * The packages `cargo test` covers when run in `cwd`: the package there, or, at a workspace root,
 * `default-members` (or every member of a virtual workspace). Null when picky can't tell, or when
 * the command reaches crates outside `cwd`, where picky only looks for tests.
 */
export function cargoScope(cwd: string, workspaceWide: boolean, excluded: string[]): CargoScope | null {
  cwd = path.resolve(cwd);
  const manifestPath = findUp(cwd, 'Cargo.toml');
  const manifest = manifestPath && readManifest(manifestPath);
  if (!manifestPath || !manifest) return null;
  const dir = path.dirname(manifestPath);

  let covers: (crateDir: string) => boolean;
  if (manifest.workspace) {
    if (dir !== cwd) return null;
    const ws = manifest.workspace;
    const isMember = (d: string) =>
      (d === dir && manifest.name !== null) || (matches(ws.members, rel(dir, d)) && !matches(ws.exclude, rel(dir, d)));
    if (workspaceWide) covers = (d) => isMember(d) && !excluded.includes(readManifest(path.join(d, 'Cargo.toml'))?.name ?? '');
    else if (ws.defaultMembers) covers = (d) => isMember(d) && matches(ws.defaultMembers!, rel(dir, d));
    else covers = manifest.name ? (d) => d === dir : isMember;
  } else {
    if (!manifest.name) return null;
    // `--workspace` in a member directory also tests the members outside it.
    if (workspaceWide && workspaceAbove(dir)) return null;
    covers = (d) => d === dir;
  }
  return {
    owns: (file) => {
      const crate = crateDirOfAbs(file);
      return crate !== null && covers(crate);
    },
  };
}

/** The package and integration test target of a test unit (an absolute path), or null. */
export function cargoTarget(file: string): { name: string; test?: string } | null {
  const dir = crateDirOfAbs(file);
  const name = dir && readManifest(path.join(dir, 'Cargo.toml'))?.name;
  if (!dir || !name) return null;
  if (path.basename(file) === 'Cargo.toml') return { name };
  const m = /^tests\/([^/]+?)(?:\.rs|\/main\.rs)$/.exec(path.relative(dir, file).split(path.sep).join('/'));
  return m ? { name, test: m[1] } : null;
}

function matches(patterns: string[], rel: string): boolean {
  const normalized = patterns.map((p) => path.posix.normalize(p).replace(/^\.\/?|\/$/g, ''));
  if (rel === '') return normalized.includes('') || normalized.includes('.');
  return picomatch(normalized.filter(Boolean))(rel);
}

function rel(from: string, to: string): string {
  return path.relative(from, to).split(path.sep).join('/');
}

function workspaceAbove(dir: string): boolean {
  for (let d = path.dirname(dir); d !== path.dirname(d); d = path.dirname(d)) {
    if (readManifest(path.join(d, 'Cargo.toml'))?.workspace) return true;
    if (existsSync(path.join(d, '.git'))) return false;
  }
  return false;
}

/** The directory of the package that owns an absolute path. */
function crateDirOfAbs(file: string): string | null {
  for (let dir = path.dirname(file); ; dir = path.dirname(dir)) {
    if (readManifest(path.join(dir, 'Cargo.toml'))?.name) return dir;
    if (existsSync(path.join(dir, '.git')) || path.dirname(dir) === dir) return null;
  }
}

const manifests = new Map<string, Manifest | null>();

function readManifest(file: string): Manifest | null {
  if (!manifests.has(file)) {
    const text = read(file);
    manifests.set(file, text === null ? null : parseManifest(text));
  }
  return manifests.get(file)!;
}

function findUp(cwd: string, name: string): string | null {
  for (let dir = cwd; ; dir = path.dirname(dir)) {
    if (existsSync(path.join(dir, name))) return path.join(dir, name);
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
