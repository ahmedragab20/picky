import path from 'node:path';

/**
 * PHP dependency extraction. PHP links files by class name (autoloading), so the
 * graph indexes every declared class and function, then resolves the names each
 * file mentions. Names are read from the raw text, comments and strings included:
 * docblock types and `'App\Models\User'` strings are real dependencies for static
 * analysis and containers, and an extra edge only ever selects more.
 */

export interface PhpFile {
  /** Lowercased fully-qualified names this file declares (classes, interfaces, traits, enums). */
  classes: string[];
  /** Lowercased fully-qualified names of functions declared outside classes. */
  functions: string[];
  /** Lowercased fully-qualified names this file may reference. */
  references: Set<string>;
  /** Repo-relative paths named by `require`/`include` with a literal path. */
  includes: string[];
}

const NAME = /\\?[A-Za-z_\x80-￿][\w\x80-￿]*(?:\\[A-Za-z_\x80-￿][\w\x80-￿]*)*/g;
// Statements start a line, or follow `;`, a brace or the open tag: `<?php namespace App; final class Money {}`.
const STATEMENT = String.raw`(?:^|[;{}]|<\?php)\s*`;
const NAMESPACE = new RegExp(String.raw`${STATEMENT}namespace\s+([\w\\]+)\s*[;{]`, 'gm');
const DECLARATION = new RegExp(String.raw`${STATEMENT}(?:(?:abstract|final|readonly)\s+)*(?:class|interface|trait|enum)\s+([A-Za-z_]\w*)`, 'gm');
const FUNCTION = new RegExp(String.raw`${STATEMENT}function\s+&?\s*([A-Za-z_]\w*)\s*\(`, 'gm');
const USE = new RegExp(String.raw`${STATEMENT}use\s+(function\s+|const\s+)?([^;]+);`, 'gm');
const INCLUDE =
  /\b(?:require|include)(?:_once)?\s*\(?\s*(__DIR__\s*\.\s*|dirname\(\s*__DIR__\s*(?:,\s*(\d+)\s*)?\)\s*\.\s*)?(['"])([^'"]+\.php)\3/g;

export function parsePhp(file: string, text: string): PhpFile {
  // Strings often spell namespaces with escaped separators: 'App\\Models\\User'.
  text = text.replace(/\\\\/g, '\\');
  const namespaces = [...text.matchAll(NAMESPACE)].map((m) => m[1].replace(/^\\/, ''));
  const ns = namespaces[0] ?? '';
  const qualify = (name: string) => (ns ? `${ns}\\${name}` : name).toLowerCase();

  const classes = [...text.matchAll(DECLARATION)].map((m) => qualify(m[1]));
  const functions = classes.length ? [] : [...text.matchAll(FUNCTION)].map((m) => qualify(m[1]));

  // `use` at the top imports names; inside a class body it pulls in a trait (a plain reference).
  const firstDeclaration = text.search(DECLARATION);
  const header = firstDeclaration === -1 ? text : text.slice(0, firstDeclaration);
  const aliases = new Map<string, string>();
  for (const m of header.matchAll(USE)) {
    for (const [alias, target] of useClause(m[2])) aliases.set(alias.toLowerCase(), target.toLowerCase());
  }

  const references = new Set<string>(aliases.values());
  for (const token of new Set(Array.from(text.matchAll(NAME), (m) => m[0]))) {
    for (const candidate of resolveName(token, namespaces, aliases)) references.add(candidate);
  }
  for (const own of classes) references.delete(own);

  return { classes, functions, references, includes: includes(file, text) };
}

/** `A\B`, `A\B as C`, `A\{B, C as D}` -> [alias, fully-qualified name] pairs. */
function useClause(clause: string): Array<[string, string]> {
  const out: Array<[string, string]> = [];
  const group = /^([\w\\]*)\\\{([^}]*)\}$/.exec(clause.replace(/\s+/g, ' ').trim());
  const parts = group ? group[2].split(',').map((p) => `${group[1]}\\${p.trim()}`) : clause.split(',');
  for (const part of parts) {
    const m = /^\\?([\w\\]+)(?:\s+as\s+(\w+))?$/.exec(part.trim());
    if (!m) continue;
    out.push([m[2] ?? m[1].split('\\').pop()!, m[1]]);
  }
  return out;
}

function resolveName(token: string, namespaces: string[], aliases: Map<string, string>): string[] {
  if (token.startsWith('\\')) return [token.slice(1).toLowerCase()];
  const lower = token.toLowerCase();
  const [first, ...rest] = lower.split('\\');
  const out: string[] = [];
  const alias = aliases.get(first);
  if (alias) out.push([alias, ...rest].join('\\'));
  for (const ns of namespaces) out.push(`${ns.toLowerCase()}\\${lower}`);
  // Global names: functions and constants fall back to them, and strings hold fully-qualified names.
  if (rest.length > 0 || namespaces.length === 0 || !alias) out.push(lower);
  return out;
}

function includes(file: string, text: string): string[] {
  const dir = path.posix.dirname(file);
  const out: string[] = [];
  for (const m of text.matchAll(INCLUDE)) {
    const [, prefix, levels, , target] = m;
    let base = dir;
    if (prefix?.startsWith('dirname')) for (let i = 0; i < Number(levels ?? 1); i++) base = path.posix.dirname(base);
    // Without __DIR__, PHP tries the include path (usually the working directory), then the file's directory.
    const candidates = prefix ? [path.posix.join(base, target)] : [path.posix.join(dir, target), path.posix.normalize(target)];
    for (const c of candidates) if (!c.startsWith('..')) out.push(c.replace(/^\.\//, ''));
  }
  return out;
}

/** Declared class and function names of a file's old content, to find who used a deleted file. */
export function declaredNames(text: string): string[] {
  const parsed = parsePhp('x.php', text);
  return [...parsed.classes, ...parsed.functions];
}
