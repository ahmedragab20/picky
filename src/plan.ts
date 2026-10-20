import { existsSync } from 'node:fs';
import path from 'node:path';
import picomatch from 'picomatch';
import { ecosystemOf, recognize, type CheckCommand, type Command, type FilesCommand, type TestCommand } from './commands.ts';
import type { Decision } from './decide.ts';
import { isVendored } from './graph.ts';

export type Skip = { action: 'skip'; note: string };
export type Narrow = { action: 'narrow'; argv: string[]; note: string };
/** `enforce`: in shadow mode, what enforce mode would have done instead of the full run. */
export type Outcome = { action: 'full'; note: string; enforce?: Skip | Narrow } | Skip | Narrow;

const DOCS = ['**/*.{md,mdx,txt,rst,adoc}', '**/LICENSE*'];
const MEDIA = ['**/*.{png,jpg,jpeg,gif,webp,avif,svg,ico,bmp,woff,woff2,ttf,otf,eot,mp3,mp4,wav,webm,pdf}'];
const STYLES = ['**/*.{css,scss,sass,less,styl,pcss}'];
const JS = [
  '**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs,vue,svelte,astro}',
  '**/{package,package-lock,tsconfig*,jsconfig}.json',
  '**/{pnpm-lock.yaml,yarn.lock,bun.lock,bun.lockb}',
];
const PHP = ['**/*.php', '**/composer.{json,lock}', '**/*.neon', '**/*.neon.dist', '**/phpunit*.xml*'];
const KOTLIN = [
  '**/*.{kt,kts,java}',
  '**/{gradle.properties,gradlew,gradlew.bat,pom.xml}',
  '**/gradle/wrapper/**',
  '**/gradle/*.versions.toml',
];
const RUST = ['**/*.rs', '**/{Cargo.toml,Cargo.lock,rust-toolchain,rust-toolchain.toml}'];
const OTHER = ['**/*.{go,py,rb,swift,cs,c,h,cpp,hpp}', '**/{go.mod,go.sum}'];

/**
 * Files that can't change a whole-project check's result. Anything not listed counts,
 * including unknown file types: a step may generate code from them before the check runs.
 */
const IRRELEVANT: Record<string, string[]> = {
  tsc: [...DOCS, ...MEDIA, ...STYLES, ...PHP, ...KOTLIN, ...RUST, ...OTHER],
  'vue-tsc': [...DOCS, ...MEDIA, ...STYLES, ...PHP, ...KOTLIN, ...RUST, ...OTHER],
  'svelte-check': [...DOCS, ...MEDIA, ...STYLES, ...PHP, ...KOTLIN, ...RUST, ...OTHER],
  biome: [...MEDIA, ...PHP, ...KOTLIN, ...RUST, ...OTHER],
  eslint: [...MEDIA, ...STYLES, ...PHP, ...KOTLIN, ...RUST, ...OTHER],
  // Every other check of an ecosystem: phpstan, cargo clippy, gradle check, detekt, ...
  php: [...DOCS, ...MEDIA, ...STYLES, ...JS, ...KOTLIN, ...RUST, ...OTHER],
  kotlin: [...DOCS, ...MEDIA, ...STYLES, ...JS, ...PHP, ...RUST, ...OTHER],
  rust: [...DOCS, ...MEDIA, ...STYLES, ...JS, ...PHP, ...KOTLIN, ...OTHER],
};

/** Per-file tools that only take these files: anything else isn't passed to them. */
const ONLY: Record<string, string[]> = {
  ktlint: ['**/*.{kt,kts}'],
};

/**
 * Ecosystems whose checks also see files their sources read: a Rust `include_str!("../README.md")`,
 * or a resource a `gradle check` test loads. The graph links those files to their readers, so a
 * check counts an affected source, not only a changed one.
 */
const READS_FILES = new Set(['kotlin', 'rust']);

/** Per-file tools: a change to these (or to a file they import) can change every file's result. */
const TOOL_CONFIG: Record<string, string[]> = {
  eslint: ['**/eslint.config.*', '**/.eslintrc*', '**/.eslintignore'],
  prettier: ['**/.prettierrc*', '**/prettier.config.*', '**/.prettierignore', '**/.editorconfig'],
  ktlint: ['**/.editorconfig', '**/.ktlint*', '**/baseline.xml'],
};

/** Decides what to do with one marked command, given the PR-wide decision. */
export function planCommand(argv: string[], cwd: string, root: string, decision: Decision): Outcome {
  const command = recognize(argv, cwd);
  if (!command) return { action: 'full', note: 'command not recognized; runs in full' };
  if (decision.full) return { action: 'full', note: `full run: ${decision.reason}` };
  const ecosystemFull = decision.ecosystems?.[command.ecosystem];
  if (ecosystemFull) return { action: 'full', note: `full run for ${command.ecosystem} commands: ${ecosystemFull}` };

  const outcome = planKind(command, cwd, root, decision);
  if (decision.mode === 'shadow' && outcome.action !== 'full') {
    return { action: 'full', note: `shadow mode: would ${outcome.note.replace(/^running/, 'run')}; running in full`, enforce: outcome };
  }
  return outcome;
}

function planKind(command: Command, cwd: string, root: string, decision: Decision): Outcome {
  if (command.kind === 'tests') return planTests(command, cwd, root, decision);
  if (command.kind === 'files') return planFiles(command, cwd, root, decision);
  return planCheck(command, decision);
}

function planTests(command: TestCommand, cwd: string, root: string, decision: Decision): Outcome {
  // Only this runner's tests under the command's working directory belong to it.
  const here = toPosix(path.relative(root, cwd));
  const rel = (t: string) => toPosix(path.relative(cwd, path.join(root, t)));
  const tests = Object.keys(decision.tests).filter(
    (t) => (here === '' || t.startsWith(`${here}/`)) && ecosystemOf(t) === command.ecosystem && (command.owns?.(rel(t)) ?? true),
  );
  if (tests.length === 0) return { action: 'full', note: 'no test files found under this directory; runs in full' };

  const selected = tests.filter((t) => decision.tests[t].run);
  const summary = `${selected.length} of ${tests.length} test files`;
  if (selected.length === tests.length) return { action: 'full', note: `all ${tests.length} test files affected` };
  if (selected.length === 0) return { action: 'skip', note: `skip: no affected tests (0 of ${tests.length})` };

  const argv = command.narrow(selected.map(rel));
  if (!argv) return { action: 'full', note: `${command.whyNot ?? `${command.tool} can't run only the selected tests`}; runs in full` };
  return { action: 'narrow', argv, note: `running ${summary}` };
}

function planFiles(command: FilesCommand, cwd: string, root: string, decision: Decision): Outcome {
  if (!decision.affected) return { action: 'full', note: 'no file list in this decision; runs in full' };
  const present = decision.changed.filter((c) => c.status !== 'D').map((c) => c.path);
  const isConfig = picomatch(TOOL_CONFIG[command.tool] ?? ['\0'], { dot: true });
  // decision.affected includes files that import a changed file, so a changed local rule or plugin counts too.
  const config = [...decision.changed.map((c) => c.path), ...decision.affected].find((f) => isConfig(f));
  if (config) return { action: 'full', note: `${command.tool} config ${config} affected; runs in full` };

  const targets = command.targets.map((t) => toPosix(path.relative(root, path.resolve(cwd, t))));
  const inTargets = (f: string) => targets.some((t) => t === '' || f === t || f.startsWith(`${t}/`));
  const unchecked = picomatch(IRRELEVANT[command.tool] ?? ['\0'], { dot: true });
  const checked = picomatch(ONLY[command.tool] ?? ['**'], { dot: true });
  const files = (command.withDependents ? decision.affected : present).filter(
    // The tools error on paths that don't exist, so only files on disk are passed.
    (f) => inTargets(f) && checked(f) && !unchecked(f) && !isVendored(f) && existsSync(path.join(root, f)),
  );
  if (files.length === 0) return { action: 'skip', note: `skip: no affected files for ${command.tool}` };
  const argv = command.narrow(files.map((f) => toPosix(path.relative(cwd, path.join(root, f)))));
  return { action: 'narrow', argv, note: `running ${command.tool} on ${files.length} affected files` };
}

function planCheck(command: CheckCommand, decision: Decision): Outcome {
  const irrelevant = picomatch(IRRELEVANT[command.tool] ?? IRRELEVANT[command.ecosystem] ?? ['\0'], { dot: true });
  const paths = decision.changed.flatMap((c) => (c.from ? [c.from, c.path] : [c.path]));
  const relevant = paths.filter((p) => !irrelevant(p));
  if (relevant.length > 0) {
    return { action: 'full', note: `${relevant.length} changed files ${command.tool} checks (e.g. ${relevant[0]}); runs in full` };
  }
  const readers = READS_FILES.has(command.ecosystem) ? (decision.affected ?? []).filter((p) => !irrelevant(p)) : [];
  if (readers.length > 0) {
    return {
      action: 'full',
      note: `${readers.length} files ${command.tool} checks read a changed file (e.g. ${readers[0]}); runs in full`,
    };
  }
  return { action: 'skip', note: `skip: no changes ${command.tool} checks` };
}

function toPosix(p: string): string {
  return p.split(path.sep).join('/');
}
