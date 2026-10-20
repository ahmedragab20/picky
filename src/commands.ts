import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { gradleBuild, gradleInitScript, jvmTestClasses } from './kotlin.ts';
import { cargoScope, cargoTarget } from './rust.ts';
import { splitAssignments, tokenize } from './sh.ts';

export type Ecosystem = 'js' | 'php' | 'kotlin' | 'rust';

/** Runs test files: picky narrows it to the selected ones. */
export interface TestCommand {
  kind: 'tests';
  tool: string;
  ecosystem: Ecosystem;
  /** The argv that runs only `files` (relative to the cwd), or null when the runner can't take them all. */
  narrow(files: string[]): string[] | null;
  /** Why `narrow` can return null, for the log. */
  whyNot?: string;
  /** Whether a test file (relative to the cwd) is one this command runs, when not every test under the cwd is. */
  owns?(file: string): boolean;
}

/** Checks files one by one (a linter or formatter): picky narrows it to the affected files. */
export interface FilesCommand {
  kind: 'files';
  tool: string;
  ecosystem: Ecosystem;
  /** What the command checks, relative to its cwd. */
  targets: string[];
  /** True when a file's result can depend on files it imports (type-aware lint rules). */
  withDependents: boolean;
  narrow(files: string[]): string[];
}

/** Checks the project as a whole (a type checker or static analyzer): picky runs it in full or skips it. */
export interface CheckCommand {
  kind: 'check';
  tool: string;
  ecosystem: Ecosystem;
}

export type Command = TestCommand | FilesCommand | CheckCommand;

export function ecosystemOf(file: string): Ecosystem {
  if (file.endsWith('.php')) return 'php';
  if (file.endsWith('.rs') || path.posix.basename(file) === 'Cargo.toml') return 'rust';
  if (/\.(?:kt|kts|java)$/.test(file)) return 'kotlin';
  return 'js';
}

const MAX_SCRIPT_DEPTH = 3;
const CHECKS: Record<string, Ecosystem> = {
  tsc: 'js',
  'vue-tsc': 'js',
  'svelte-check': 'js',
  biome: 'js',
  phpstan: 'php',
  psalm: 'php',
  phpcs: 'php',
  'php-cs-fixer': 'php',
  pint: 'php',
  rector: 'php',
  detekt: 'kotlin',
  'detekt-cli': 'kotlin',
};

/**
 * Recognizes a test, lint or type-check command. Returns null when the command is
 * unknown or already targets specific tests; picky then runs it unchanged.
 */
export function recognize(argv: string[], cwd: string, depth = 0): Command | null {
  const { rest } = splitAssignments(argv);
  const prefixLen = argv.length - rest.length;
  if (rest.length === 0) return null;

  let i = prefixLen;
  // Launchers that run a local binary: npx [-y] X, bunx X, pnpm|yarn|bun exec X, php [-d k=v] X.
  if (argv[i] === 'npx' || argv[i] === 'bunx') {
    i++;
    while (i < argv.length && argv[i].startsWith('-')) i++;
  } else if (['pnpm', 'yarn', 'bun'].includes(argv[i]) && argv[i + 1] === 'exec') {
    i += 2;
  } else if (argv[i] === 'php') {
    i++;
    while (i < argv.length && argv[i].startsWith('-')) {
      if (argv[i] === '-d') i += 2;
      else if (/^-d./.test(argv[i])) i++;
      else return null;
    }
  }
  if (i >= argv.length) return null;

  const before = argv.slice(0, i + 1);
  const tool = path.basename(argv[i]).replace(/\.(?:phar|bat|cmd)$/, '');
  const args = argv.slice(i + 1);

  if (CHECKS[tool]) return { kind: 'check', tool, ecosystem: CHECKS[tool] };

  switch (tool) {
    case 'vitest': {
      const sub = args[0] === 'run' ? ['run'] : [];
      const flags = args.slice(sub.length);
      if (!onlyFlags(flags)) return null;
      return { kind: 'tests', tool, ecosystem: 'js', narrow: (files) => [...before, ...sub, ...flags, ...files] };
    }
    case 'jest':
      if (!onlyFlags(args)) return null;
      return { kind: 'tests', tool, ecosystem: 'js', narrow: (files) => [...before, ...args, '--runTestsByPath', ...files] };
    case 'node':
      if (!args.includes('--test') || !onlyFlags(args)) return null;
      return { kind: 'tests', tool: 'node-test', ecosystem: 'js', narrow: (files) => [...before, ...args, ...files] };
    case 'phpunit':
    case 'pest': {
      // Paths replace the configured suites; an explicit suite would conflict with them.
      if (!onlyFlags(args) || args.some((a) => a.startsWith('--testsuite'))) return null;
      // PHPUnit 9 silently runs only the first of several paths.
      const severalOk = phpunitMajor(cwd) >= 10;
      return {
        kind: 'tests',
        tool,
        ecosystem: 'php',
        narrow: (files) => (files.length > 1 && !severalOk ? null : [...before, ...args, ...files]),
        whyNot: `${tool} before 10 runs only the first of several paths`,
      };
    }
    case 'eslint':
      return eslint(before, args, cwd);
    case 'prettier':
      return prettier(before, args);
    case 'npm':
    case 'pnpm':
    case 'yarn':
      return recognizeScript(tool, argv.slice(0, i), args, cwd, depth);
    case 'composer':
      return recognizeComposer(argv.slice(0, i + 1), args, cwd, depth);
    case 'cargo':
      return cargo(before, args, cwd);
    case 'gradle':
    case 'gradlew':
      return gradle(before, args, cwd);
    case 'ktlint':
      return ktlint(before, args);
    default:
      return null;
  }
}

function recognizeScript(pm: string, prefix: string[], args: string[], cwd: string, depth: number): Command | null {
  if (depth >= MAX_SCRIPT_DEPTH || args.includes('--')) return null;
  // Package-manager flags (--silent, --if-present, ...) are kept; the words must name a script.
  const words = args.filter((a) => !a.startsWith('-'));
  const pmFlags = args.filter((a) => a.startsWith('-'));
  let script: string | undefined;
  if (words.length === 1 && (words[0] === 'test' || words[0] === 't')) script = 'test';
  else if (words.length === 2 && (words[0] === 'run' || words[0] === 'run-script')) script = words[1];
  if (!script) return null;

  const scripts = readJson(path.join(cwd, 'package.json'))?.scripts ?? {};
  const bodyWords = typeof scripts[script] === 'string' ? tokenize(scripts[script]) : null;
  if (!bodyWords) return null;
  const inner = recognize(bodyWords, cwd, depth + 1);
  if (!inner) return null;
  if (inner.kind === 'check') return inner;

  if (inner.kind === 'files') {
    // Forwarded files would be added to the script's own targets (`eslint . a.ts` checks everything),
    // so the narrowed body runs directly with the package's local binary.
    if (scripts[`pre${script}`] || scripts[`post${script}`]) return null;
    const bin = localBin(cwd, inner.tool);
    if (!bin) return null;
    const { env } = splitAssignments(bodyWords);
    return {
      ...inner,
      narrow: (files) => {
        const narrowed = inner.narrow(files);
        const at = narrowed.indexOf(inner.tool);
        if (at !== Object.keys(env).length) return [...prefix, ...narrowed]; // launcher in the body: keep it
        return [...prefix, ...narrowed.slice(0, at), bin, ...narrowed.slice(at + 1)];
      },
    };
  }

  // npm needs `--` to forward arguments to the script; pnpm and yarn forward them directly.
  const call = [...prefix, pm, ...pmFlags, 'run', script];
  return {
    ...inner,
    narrow: (files) => {
      if (inner.narrow(files) === null) return null;
      return pm === 'npm' ? [...call, '--', ...files] : [...call, ...files];
    },
  };
}

function recognizeComposer(call: string[], args: string[], cwd: string, depth: number): Command | null {
  if (depth >= MAX_SCRIPT_DEPTH || args.includes('--')) return null;
  const words = args.filter((a) => !a.startsWith('-'));
  const flags = args.filter((a) => a.startsWith('-'));
  const script = words.length === 2 && (words[0] === 'run' || words[0] === 'run-script') ? words[1] : words.length === 1 ? words[0] : null;
  if (!script) return null;

  const body = scriptBody(readJson(path.join(cwd, 'composer.json'))?.scripts?.[script]);
  if (!body) return null;
  let bodyWords: string[] | null;
  if (body.startsWith('@php ')) bodyWords = tokenize(`php ${body.slice(5)}`);
  else if (body.startsWith('@composer ')) bodyWords = tokenize(`composer ${body.slice(10)}`);
  else if (/^@[\w:-]+$/.test(body)) bodyWords = ['composer', 'run-script', body.slice(1)];
  else if (body.startsWith('@')) return null;
  else bodyWords = tokenize(body);
  if (!bodyWords) return null;

  const inner = recognize(bodyWords, cwd, depth + 1);
  if (!inner || inner.kind === 'files') return null;
  if (inner.kind === 'check') return inner;
  // Without `--`, composer drops every argument when an option comes first.
  const run = [...call, ...flags, 'run-script', script, '--'];
  return { ...inner, narrow: (files) => (inner.narrow(files) === null ? null : [...run, ...files]) };
}

/** A composer script body: a string, or an array holding exactly one command. */
function scriptBody(value: unknown): string | null {
  if (typeof value === 'string') return value;
  if (Array.isArray(value) && value.length === 1 && typeof value[0] === 'string') return value[0];
  return null;
}

// Cargo options that take a value as the next word. Package and target selection (-p, --lib,
// --test, a test name) means the command already targets specific tests, so it isn't listed.
const CARGO_VALUE = [
  '--features',
  '-F',
  '--target',
  '--target-dir',
  '--profile',
  '-j',
  '--jobs',
  '--color',
  '--message-format',
  '--config',
  '-Z',
  '--exclude',
  '--lockfile-path',
];
const CARGO_BOOLEAN = [
  '--workspace',
  '--all',
  '--all-features',
  '--no-default-features',
  '--release',
  '-r',
  '--locked',
  '--frozen',
  '--offline',
  '--no-fail-fast',
  '--quiet',
  '-q',
  '--verbose',
  '-v',
  '-vv',
  '--all-targets',
  '--tests',
  '--bins',
  '--examples',
  '--benches',
  '--timings',
  '--keep-going',
  '--ignore-rust-version',
  '--no-run',
];
const NEXTEST_VALUE = [
  ...CARGO_VALUE,
  '-P',
  '-E',
  '--filterset',
  '--filter-expr',
  '--partition',
  '--test-threads',
  '--retries',
  '--cargo-profile',
  '--run-ignored',
  '--status-level',
  '--final-status-level',
  '--success-output',
  '--failure-output',
  '--no-tests',
  '--max-fail',
];
const NEXTEST_BOOLEAN = [...CARGO_BOOLEAN, '--no-capture', '--nocapture', '--fail-fast', '--hide-progress-bar'];
/** Target kinds: with one of these, crates are narrowed only as a whole. */
const CARGO_TARGET_KINDS = new Set(['--all-targets', '--tests', '--bins', '--examples', '--benches']);

function cargo(before: string[], args: string[], cwd: string): Command | null {
  const toolchain = args[0]?.startsWith('+') ? 1 : 0; // cargo +nightly test
  const sub = args[toolchain];
  const call = [...before, ...args.slice(0, toolchain + 1)];
  const rest = args.slice(toolchain + 1);
  if (sub === 'clippy' || sub === 'check' || sub === 'fmt') return { kind: 'check', tool: `cargo ${sub}`, ecosystem: 'rust' };
  if (sub === 'test') return cargoTests(call, rest, cwd, 'cargo test', CARGO_VALUE, CARGO_BOOLEAN);
  if (sub === 'nextest' && rest[0] === 'run') {
    return cargoTests([...call, 'run'], rest.slice(1), cwd, 'cargo nextest', NEXTEST_VALUE, NEXTEST_BOOLEAN);
  }
  return null;
}

function cargoTests(call: string[], args: string[], cwd: string, tool: string, value: string[], boolean: string[]): Command | null {
  const dash = args.indexOf('--');
  const harness = dash < 0 ? [] : args.slice(dash);
  // A bare word is a test name filter, before `--` or after it.
  if (!onlyFlags(harness.slice(1))) return null;
  const parsed = splitOptions(dash < 0 ? args : args.slice(0, dash), new Set(value), new Set(boolean));
  if (!parsed || parsed.targets.length) return null;

  // Package selection is replaced by `-p` for each selected crate.
  const options: string[] = [];
  const excluded: string[] = [];
  for (let i = 0; i < parsed.options.length; i++) {
    const o = parsed.options[i];
    if (o === '--exclude') excluded.push(parsed.options[++i]);
    else if (o.startsWith('--exclude=')) excluded.push(o.slice('--exclude='.length));
    else if (o !== '--workspace' && o !== '--all') options.push(o);
  }
  const workspaceWide = parsed.options.includes('--workspace') || parsed.options.includes('--all');
  if (excluded.length && !workspaceWide) return null;
  const scope = cargoScope(cwd, workspaceWide, excluded);
  if (!scope) return null;
  const wholeCrates = options.some((o) => CARGO_TARGET_KINDS.has(o));
  // nextest fails a run that selects no tests, as a crate without tests would.
  const noTests = tool === 'cargo nextest' && !options.some((o) => o.startsWith('--no-tests')) ? ['--no-tests=pass'] : [];

  return {
    kind: 'tests',
    tool,
    ecosystem: 'rust',
    owns: (file) => scope.owns(path.resolve(cwd, file)),
    narrow: (files) => {
      // Package name -> its selected integration targets, or null when the whole crate runs.
      const crates = new Map<string, Set<string> | null>();
      for (const file of files) {
        const target = cargoTarget(path.resolve(cwd, file));
        if (!target) return null;
        const selected = crates.get(target.name);
        if (!target.test || wholeCrates) crates.set(target.name, null);
        else if (selected !== null) crates.set(target.name, new Set([...(selected ?? []), target.test]));
      }
      // `--test` applies to every listed package, so one whole crate makes every crate whole.
      const partial = [...crates.values()].every((t) => t !== null);
      const tests = partial ? [...new Set([...crates.values()].flatMap((t) => [...t!]))].sort() : [];
      const packages = [...crates.keys()].sort().flatMap((name) => ['-p', name]);
      return [...call, ...options, ...noTests, ...packages, ...tests.flatMap((t) => ['--test', t]), ...harness];
    },
  };
}

// Gradle options without a value. `-P`/`-D` properties and `--name=value` options are accepted too.
const GRADLE_BOOLEAN = new Set([
  '--no-daemon',
  '--daemon',
  '--stacktrace',
  '-s',
  '--full-stacktrace',
  '-S',
  '--info',
  '-i',
  '--debug',
  '-d',
  '--quiet',
  '-q',
  '--warn',
  '-w',
  '--scan',
  '--no-scan',
  '--parallel',
  '--no-parallel',
  '--build-cache',
  '--no-build-cache',
  '--configuration-cache',
  '--no-configuration-cache',
  '--offline',
  '--refresh-dependencies',
  '--rerun-tasks',
  '--continue',
  '--watch-fs',
  '--no-watch-fs',
  '--profile',
]);
const GRADLE_VALUE = new Set(['-x', '--exclude-task']);
/** Options that move the build somewhere else, so test files no longer map to its projects. */
const GRADLE_RELOCATE = /^--(?:project-dir|settings-file|build-file|include-build|tests)\b/;
/** Tasks that only compile, lint or verify (`check` runs `test` too): run in full or skipped. */
const GRADLE_CHECKS = new Set([
  'check',
  'detekt',
  'detektMain',
  'detektTest',
  'ktlintCheck',
  'lintKotlin',
  'spotlessCheck',
  'spotlessKotlinCheck',
  'apiCheck',
  'lint',
  'compileKotlin',
  'compileTestKotlin',
  'compileJava',
  'compileTestJava',
]);

function gradle(before: string[], args: string[], cwd: string): Command | null {
  const tasks: string[] = [];
  const flags: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (!a.startsWith('-')) tasks.push(a);
    else if (GRADLE_VALUE.has(a) && i + 1 < args.length) flags.push(a, args[++i]);
    else if (GRADLE_RELOCATE.test(a)) return null;
    else if (GRADLE_BOOLEAN.has(a) || /^-[PD]\S/.test(a) || /^--[\w-]+=/.test(a)) flags.push(a);
    else return null;
  }
  if (tasks.length && tasks.every((t) => GRADLE_CHECKS.has(t.split(':').pop()!))) {
    return { kind: 'check', tool: `gradle ${tasks.join(' ')}`, ecosystem: 'kotlin' };
  }
  if (tasks.length !== 1 || tasks[0] !== 'test') return null;
  const build = gradleBuild(cwd);
  if (!build) return null;

  return {
    kind: 'tests',
    tool: 'gradle test',
    ecosystem: 'kotlin',
    owns: (file) => build.projectOf(path.resolve(cwd, file)) !== null,
    whyNot: 'Android and Kotlin Multiplatform test tasks take no --tests filter',
    narrow: (files) => {
      // `test` runs every project's tests; each selected project's task gets its own `--tests` filters.
      const tasks = new Map<string, Set<string>>();
      for (const file of files) {
        const abs = path.resolve(cwd, file);
        const project = build.projectOf(abs);
        const classes = jvmTestClasses(abs);
        if (!project || classes.length === 0 || build.cantFilter(project)) return null;
        if (!tasks.has(project.path)) tasks.set(project.path, new Set());
        // The trailing * keeps nested (@Nested) classes on every Gradle version.
        for (const c of classes) tasks.get(project.path)!.add(`${c}*`);
      }
      const init = gradleInitScript();
      if (!init) return null;
      const selected = [...tasks]
        .sort(([a], [b]) => (a < b ? -1 : 1))
        .flatMap(([project, classes]) => [`${project === ':' ? '' : project}:test`, ...[...classes].sort().flatMap((c) => ['--tests', c])]);
      return [...before, ...flags, '--init-script', init, ...selected];
    },
  };
}

const KTLINT_VALUE = new Set([
  '--reporter',
  '--editorconfig',
  '--baseline',
  '--log-level',
  '-l',
  '--limit',
  '--color-name',
  '--ruleset',
  '-R',
  '--code-style',
]);
const KTLINT_BOOLEAN = new Set(['--relative', '-F', '--format', '--color', '--verbose', '--debug', '--android', '-a', '--experimental']);

function ktlint(before: string[], args: string[]): Command | null {
  const parsed = splitOptions(args, KTLINT_VALUE, KTLINT_BOOLEAN);
  if (!parsed) return null;
  // Explicit patterns are globs (`src/**/*.kt`, `!**/generated/**`): checked in full or skipped.
  if (parsed.targets.length) return { kind: 'check', tool: 'ktlint', ecosystem: 'kotlin' };
  return {
    kind: 'files',
    tool: 'ktlint',
    ecosystem: 'kotlin',
    targets: ['.'],
    withDependents: false,
    narrow: (files) => [...before, ...parsed.options, ...files],
  };
}

// ESLint options that take a value as the next word. Any other option without `=` must be boolean.
const ESLINT_VALUE = new Set([
  '-c',
  '--config',
  '--ext',
  '--global',
  '--parser',
  '--parser-options',
  '--resolve-plugins-relative-to',
  '--plugin',
  '--rule',
  '--rulesdir',
  '--ignore-path',
  '--ignore-pattern',
  '-f',
  '--format',
  '-o',
  '--output-file',
  '--max-warnings',
  '--cache-location',
  '--cache-strategy',
  '--env',
  '--fix-type',
  '--report-unused-disable-directives-severity',
  '--flag',
  '--concurrency',
]);
const ESLINT_BOOLEAN = new Set([
  '--fix',
  '--fix-dry-run',
  '--no-eslintrc',
  '--no-config-lookup',
  '--cache',
  '--quiet',
  '--color',
  '--no-color',
  '--no-inline-config',
  '--report-unused-disable-directives',
  '--no-error-on-unmatched-pattern',
  '--exit-on-fatal-error',
  '--no-warn-ignored',
  '--pass-on-no-patterns',
  '--debug',
  '--stats',
  '--no-ignore',
]);

function eslint(before: string[], args: string[], cwd: string): Command | null {
  const parsed = splitOptions(args, ESLINT_VALUE, ESLINT_BOOLEAN);
  if (!parsed) return null;
  const config = findUp(cwd, [
    'eslint.config.js',
    'eslint.config.mjs',
    'eslint.config.cjs',
    'eslint.config.ts',
    'eslint.config.mts',
    'eslint.config.cts',
  ]);
  // Legacy .eslintrc configs lint explicitly passed files they would skip in a directory walk,
  // and import/no-unused-modules reports in a file's dependencies; both run whole or not at all.
  if (!config || parsed.options.some((o) => o === '-c' || o.startsWith('--config')) || readText(config).includes('no-unused-modules')) {
    return { kind: 'check', tool: 'eslint', ecosystem: 'js' };
  }
  const options = parsed.options.includes('--no-warn-ignored') ? parsed.options : [...parsed.options, '--no-warn-ignored'];
  return {
    kind: 'files',
    tool: 'eslint',
    ecosystem: 'js',
    targets: parsed.targets.length ? parsed.targets : ['.'],
    withDependents: true,
    narrow: (files) => [...before, ...options, ...files],
  };
}

const PRETTIER_VALUE = new Set([
  '--config',
  '--ignore-path',
  '--log-level',
  '--plugin',
  '--parser',
  '--cache-location',
  '--cache-strategy',
  '--config-precedence',
  '--print-width',
  '--tab-width',
  '--trailing-comma',
  '--quote-props',
  '--arrow-parens',
  '--prose-wrap',
  '--end-of-line',
  '--html-whitespace-sensitivity',
  '--embedded-language-formatting',
  '--object-wrap',
  '--experimental-operator-position',
]);
const PRETTIER_BOOLEAN = new Set([
  '--check',
  '-c',
  '--list-different',
  '-l',
  '--write',
  '-w',
  '--no-semi',
  '--single-quote',
  '--jsx-single-quote',
  '--bracket-same-line',
  '--no-bracket-spacing',
  '--use-tabs',
  '--cache',
  '--no-config',
  '--no-editorconfig',
  '--ignore-unknown',
  '-u',
  '--no-error-on-unmatched-pattern',
  '--with-node-modules',
  '--vue-indent-script-and-style',
  '--single-attribute-per-line',
  '--experimental-ternaries',
  '--color',
  '--no-color',
  '--require-pragma',
  '--insert-pragma',
]);

function prettier(before: string[], args: string[]): Command | null {
  const parsed = splitOptions(args, PRETTIER_VALUE, PRETTIER_BOOLEAN);
  if (!parsed || parsed.targets.length === 0) return null;
  // --ignore-unknown: prettier errors on a file it can't parse when it's named explicitly.
  const unknown = parsed.options.some((o) => o === '--ignore-unknown' || o === '-u');
  const options = unknown ? parsed.options : [...parsed.options, '--ignore-unknown'];
  return {
    kind: 'files',
    tool: 'prettier',
    ecosystem: 'js',
    targets: parsed.targets,
    withDependents: false,
    narrow: (files) => [...before, ...options, ...files],
  };
}

/** Separates options from target paths. Null when an option is unknown, so a value can't pass for a path. */
function splitOptions(args: string[], value: Set<string>, boolean: Set<string>): { options: string[]; targets: string[] } | null {
  const options: string[] = [];
  const targets: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (!a.startsWith('-')) targets.push(a);
    else if (a.includes('=') && value.has(a.slice(0, a.indexOf('=')))) options.push(a);
    else if (value.has(a) && i + 1 < args.length) options.push(a, args[++i]);
    else if (boolean.has(a)) options.push(a);
    else return null;
  }
  return { options, targets };
}

/** Major version of phpunit/phpunit in the nearest composer.lock, or 0 when unknown. */
function phpunitMajor(cwd: string): number {
  const lock = findUp(cwd, ['composer.lock']);
  const json = lock ? readJson(lock) : null;
  const pkg = [...(json?.packages ?? []), ...(json?.['packages-dev'] ?? [])].find((p) => p?.name === 'phpunit/phpunit');
  return Number(/^v?(\d+)/.exec(String(pkg?.version ?? ''))?.[1] ?? 0);
}

function localBin(cwd: string, tool: string): string | null {
  for (let dir = cwd; ; dir = path.dirname(dir)) {
    const bin = path.join(dir, 'node_modules/.bin', tool);
    if (existsSync(bin)) return bin;
    if (existsSync(path.join(dir, '.git')) || path.dirname(dir) === dir) return null;
  }
}

/** The first of `names` found in `cwd` or a parent, up to the repository root. */
function findUp(cwd: string, names: string[]): string | null {
  for (let dir = cwd; ; dir = path.dirname(dir)) {
    for (const name of names) if (existsSync(path.join(dir, name))) return path.join(dir, name);
    if (existsSync(path.join(dir, '.git')) || path.dirname(dir) === dir) return null;
  }
}

function readText(file: string): string {
  try {
    return readFileSync(file, 'utf8');
  } catch {
    return '';
  }
}

function readJson(file: string): any {
  try {
    return JSON.parse(readFileSync(file, 'utf8'));
  } catch {
    return null;
  }
}

// A bare word after the command may be a test filter or a flag's value; both make
// narrowing unsafe, so only `--flag` / `--flag=value` arguments are accepted.
function onlyFlags(args: string[]): boolean {
  return args.every((a) => a.startsWith('-'));
}
