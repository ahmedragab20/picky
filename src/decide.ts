import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import picomatch from 'picomatch';
import { loadConfig, type Config, type Mode } from './config.ts';
import { changedFiles, git, hunks, resolveRange, trackedFiles, type Change, type Range } from './git.ts';
import { ecosystemOf, type Ecosystem } from './commands.ts';
import { buildGraph, importersOfMissing, isVendored, phpReferencersOf, transitiveDependents, type Graph } from './graph.ts';
import { askJev, type Question } from './judge.ts';
import { declaredNames } from './php.ts';

export interface TestDecision {
  run: boolean;
  reason: string;
  /** Model probability that the change affects this test, when asked. */
  p?: number;
}

export interface Decision {
  version: 1;
  mode: Mode;
  model: string;
  /** True means every marked command runs unchanged. */
  full: boolean;
  reason: string;
  /** Ecosystems whose commands all run in full, with the reason (e.g. a lockfile changed). */
  ecosystems?: Partial<Record<Ecosystem, string>>;
  /** Changed files that still exist plus every file that transitively depends on a changed file. */
  affected?: string[];
  base?: string;
  head?: string;
  changed: Change[];
  tests: Record<string, TestDecision>;
  /** Present when the model was called, so a run can prove it used the model. */
  modelUsage?: ModelUsage;
  /** The GitHub job that computed this decision; other jobs reuse it when it's shared as an artifact. */
  job?: string;
  ms: number;
}

export interface ModelUsage {
  model: string;
  questions: number;
  requests: number;
  inputTokens: number;
  ms: number;
  error?: string;
}

export const THRESHOLD = 0.15;
const DEFAULT_BUDGET_SECONDS = 30;
const MAX_STATE_CHARS = 200_000;
const TEST_GLOBS = ['**/*.{test,spec}.{ts,tsx,mts,cts,js,jsx,mjs,cjs}', '**/*Test.php'];
/** Changes here can alter anything: every marked command runs in full. */
const FULL_RUN_GLOBS = ['.picky.yml', '.github/workflows/**', '.github/picky/**'];
/** Changes here can alter any result in one ecosystem: its commands run in full. */
const ECOSYSTEM_GLOBS: Record<Ecosystem, string[]> = {
  js: [
    '**/package.json',
    '**/package-lock.json',
    '**/pnpm-lock.yaml',
    '**/yarn.lock',
    '**/bun.lock',
    '**/bun.lockb',
    '**/tsconfig*.json',
    '**/jsconfig.json',
    '**/{vitest,vite,jest}.config.*',
  ],
  php: ['**/composer.json', '**/composer.lock', '**/phpunit.xml', '**/phpunit.xml.dist', '**/phpunit.dist.xml'],
};
/** Bump when the decision's shape changes, so cached decisions from older versions aren't reused. */
const DECISION_SCHEMA = 3;

export interface DecideOptions {
  root: string;
  env: NodeJS.ProcessEnv;
  fetchImpl?: typeof fetch;
}

export interface CachedDecision {
  decision: Decision;
  cached: boolean;
  /** True the first time this job sees the decision: computed here, or shared by another job. */
  report: boolean;
  /** Where the decision is stored; null when it couldn't be keyed. */
  file: string | null;
}

/**
 * Computes the decision, reusing a cached one for the same inputs. Never throws.
 * The cache lives in the runner's temp directory, so a decision computed once (by `picky decide`)
 * and downloaded there as an artifact is reused by every job: they all act on the same model answers.
 */
export async function decideCached(opts: DecideOptions): Promise<CachedDecision> {
  let file: string | null = null;
  try {
    const config = loadConfig(opts.root, opts.env);
    const head = git(opts.root, ['rev-parse', 'HEAD']).trim();
    const key = createHash('sha256')
      .update(JSON.stringify([DECISION_SCHEMA, head, config.source, opts.env.PICKY_BASE, opts.env.GITHUB_EVENT_NAME, opts.env.GITHUB_EVENT_PATH]))
      .digest('hex')
      .slice(0, 16);
    file = path.join(cacheDir(opts.root, opts.env), `decision-${key}.json`);
    const decision: Decision = JSON.parse(readFileSync(file, 'utf8'));
    const job = opts.env.GITHUB_JOB;
    return { decision, cached: true, report: decision.job !== job && firstSight(file, job), file };
  } catch {
    // no cached decision
  }
  const decision = await decide(opts);
  if (opts.env.GITHUB_JOB) decision.job = opts.env.GITHUB_JOB;
  if (file) {
    try {
      mkdirSync(path.dirname(file), { recursive: true });
      writeFileSync(file, JSON.stringify(decision, null, 2));
    } catch {
      // caching is an optimization only
    }
  }
  return { decision, cached: false, report: true, file };
}

/** Claims the report of a shared decision for this job, so only its first marked command prints it. */
function firstSight(file: string, job: string | undefined): boolean {
  try {
    writeFileSync(`${file}.seen-${job ?? 'local'}`, '', { flag: 'wx' });
    return true;
  } catch {
    return false;
  }
}

function cacheDir(root: string, env: NodeJS.ProcessEnv): string {
  if (env.RUNNER_TEMP) return path.join(env.RUNNER_TEMP, 'picky');
  return path.resolve(root, git(root, ['rev-parse', '--git-path', 'picky']).trim());
}

/** Computes a fresh decision. Any internal error becomes a full run. */
export async function decide(opts: DecideOptions): Promise<Decision> {
  const started = Date.now();
  let config: Config | undefined;
  try {
    config = loadConfig(opts.root, opts.env);
    return await decideInner(opts, config, started);
  } catch (err) {
    return fullRun(config, `internal error: ${(err as Error).message}`, [], started);
  }
}

async function decideInner(opts: DecideOptions, config: Config, started: number): Promise<Decision> {
  const { root, env } = opts;
  if (config.problem) return fullRun(config, config.problem, [], started);

  const event = env.GITHUB_ACTIONS === 'true' ? env.GITHUB_EVENT_NAME : undefined;
  if (event && event !== 'pull_request') return fullRun(config, `event "${event}" always runs everything`, [], started);
  if (config.model.provider === 'typesafe' && !config.apiKey) {
    return fullRun(config, 'no API key (set PICKY_API_KEY)', [], started);
  }

  const range = resolveRange(root, env);
  if (!range) return fullRun(config, 'no base commit to compare against (set PICKY_BASE)', [], started);

  const changed = changedFiles(root, range);
  const base = { base: range.base, head: range.head };

  const touches = (globs: string[]) => {
    const match = picomatch(globs, { dot: true });
    return changed.find((c) => match(c.path) || (c.from !== undefined && match(c.from)));
  };
  const trigger = touches(FULL_RUN_GLOBS);
  if (trigger) return { ...fullRun(config, `${trigger.path} changed`, changed, started), ...base };

  const isCritical = picomatch(config.critical.length ? config.critical : ['\0'], { dot: true });
  const criticalChange = changed.find((c) => isCritical(c.path));
  if (criticalChange) return { ...fullRun(config, `critical path ${criticalChange.path} changed`, changed, started), ...base };

  const ecosystems: Partial<Record<Ecosystem, string>> = {};
  for (const eco of ['js', 'php'] as const) {
    const hit = touches(ECOSYSTEM_GLOBS[eco]);
    if (hit) ecosystems[eco] = `${hit.path} changed`;
  }

  const files = trackedFiles(root);
  const isTest = picomatch(TEST_GLOBS, { dot: true });
  const tests = files.filter((f) => isTest(f) && !isVendored(f));
  const graph = buildGraph(root, files);

  const isLow = picomatch(config.low.length ? config.low : ['\0'], { dot: true });
  const relevant = changed.filter((c) => !isLow(c.path));
  const present = relevant.filter((c) => c.status !== 'D').map((c) => c.path);
  const missing = relevant.flatMap((c) => (c.status === 'D' ? [c.path] : c.from ? [c.from] : []));

  // Floor: changed tests, tests importing changed files, tests importing removed files.
  const seeds = new Set([...present, ...importersOfMissing(graph, missing), ...usersOfRemovedPhp(root, range, graph, missing)]);
  const affected = new Set([...seeds, ...transitiveDependents(graph, seeds)]);

  const decisions: Record<string, TestDecision> = {};
  const candidates: string[] = [];
  let modelUsage: ModelUsage | undefined;
  for (const test of tests) {
    const ecosystemFull = ecosystems[ecosystemOf(test)];
    if (ecosystemFull) decisions[test] = { run: true, reason: ecosystemFull };
    else if (isCritical(test)) decisions[test] = { run: true, reason: 'critical test' };
    else if (present.includes(test)) decisions[test] = { run: true, reason: 'test changed' };
    else if (affected.has(test)) decisions[test] = { run: true, reason: `imports ${firstChangedImport(graph, test, seeds)}` };
    else candidates.push(test);
  }

  if (relevant.length === 0) {
    for (const test of candidates) decisions[test] = { run: false, reason: 'only low-risk files changed' };
  } else if (candidates.length > 0) {
    modelUsage = await judgeCandidates(candidates, decisions, { root, config, range, relevant, graph, opts, started });
  }

  return {
    version: 1,
    mode: config.mode,
    model: config.model.id,
    full: false,
    reason: `selected ${Object.values(decisions).filter((d) => d.run).length} of ${tests.length} test files`,
    ...(Object.keys(ecosystems).length > 0 && { ecosystems }),
    // Every existing changed file (low-risk ones too: formatters check them) plus dependents.
    affected: [...new Set([...changed.filter((c) => c.status !== 'D').map((c) => c.path), ...affected])].sort(),
    ...base,
    changed,
    tests: decisions,
    ...(modelUsage && { modelUsage }),
    ms: Date.now() - started,
  };
}

interface JudgeContext {
  root: string;
  config: Config;
  range: Range;
  relevant: Change[];
  graph: Graph;
  opts: DecideOptions;
  started: number;
}

async function judgeCandidates(
  candidates: string[],
  decisions: Record<string, TestDecision>,
  ctx: JudgeContext,
): Promise<ModelUsage | undefined> {
  const runAll = (reason: string) => {
    for (const test of candidates) decisions[test] = { run: true, reason };
  };

  if (ctx.config.model.provider === 'none') {
    for (const test of candidates) decisions[test] = { run: false, reason: 'no graph link (graph-only mode, model: none)' };
    return;
  }

  const state = buildState(ctx);
  // Unseen means run: never let the model decide on a diff it can't fully see.
  if (state.length > MAX_STATE_CHARS) {
    runAll(`diff too large for one request (${state.length} chars); split context lands in a later version`);
    return undefined;
  }

  const questions: Question[] = candidates.map((test, i) => ({ id: `t${i}`, instructions: questionFor(ctx, test) }));
  const budget = Number(ctx.opts.env.PICKY_BUDGET_SECONDS ?? DEFAULT_BUDGET_SECONDS) * 1000;
  const usage: ModelUsage = { model: ctx.config.model.name, questions: questions.length, requests: 0, inputTokens: 0, ms: 0 };
  const asked = Date.now();
  let answers: Map<string, number>;
  try {
    const result = await askJev(state, questions, {
      apiKey: ctx.config.apiKey!,
      model: ctx.config.model.name,
      deadline: ctx.started + budget,
      fetchImpl: ctx.opts.fetchImpl,
      endpoint: ctx.opts.env.PICKY_JEV_ENDPOINT,
    });
    answers = result.answers;
    Object.assign(usage, { requests: result.requests, inputTokens: result.inputTokens, ms: Date.now() - asked });
  } catch (err) {
    runAll(`model unavailable (${(err as Error).message})`);
    return { ...usage, ms: Date.now() - asked, error: (err as Error).message };
  }

  candidates.forEach((test, i) => {
    const p = answers.get(`t${i}`);
    if (p === undefined) decisions[test] = { run: true, reason: 'model gave no answer' };
    else if (p >= THRESHOLD) decisions[test] = { run: true, reason: 'model: possibly affected', p };
    else decisions[test] = { run: false, reason: 'model: unaffected', p };
  });
  return usage;
}

function buildState(ctx: JudgeContext): string {
  const overview = ctx.relevant.map((c) => {
    const name = c.from ? `${c.from} -> ${c.path}` : c.path;
    return `${c.status} ${name} (+${c.added} -${c.removed})`;
  });
  const diff = hunks(ctx.root, ctx.range, ctx.relevant.flatMap((c) => (c.from ? [c.from, c.path] : [c.path])));
  return [`Code change under review: ${ctx.relevant.length} files.`, ...overview, '', 'Diff (zero context lines):', diff].join('\n');
}

function questionFor(ctx: JudgeContext, test: string): string {
  const imports = [...(ctx.graph.imports.get(test) ?? [])].slice(0, 10);
  const names = testNames(path.join(ctx.root, test)).slice(0, 12);
  return [
    `Could the code change in the state alter the result of the tests in "${test}"?`,
    names.length ? `Test names: ${names.join('; ')}.` : '',
    imports.length ? `It imports: ${imports.join(', ')}.` : 'It imports no changed project files.',
  ]
    .filter(Boolean)
    .join(' ');
}

/** Files that use a class or function declared in a removed or renamed-away PHP file. */
function usersOfRemovedPhp(root: string, range: Range, graph: Graph, missing: string[]): Set<string> {
  const names = missing
    .filter((f) => f.endsWith('.php'))
    .flatMap((f) => {
      try {
        return declaredNames(git(root, ['show', `${range.base}:${f}`]));
      } catch {
        return [];
      }
    });
  return names.length ? phpReferencersOf(graph, names) : new Set();
}

function testNames(file: string): string[] {
  try {
    const text = readFileSync(file, 'utf8');
    const calls = [...text.matchAll(/\b(?:describe|it|test)\s*\(\s*(['"`])((?:\\.|(?!\1).)*)\1/g)].map((m) => m[2]);
    // PHPUnit: test methods.
    return [...calls, ...[...text.matchAll(/\bfunction\s+(test\w+)\s*\(/g)].map((m) => m[1])];
  } catch {
    return [];
  }
}

function firstChangedImport(graph: Graph, test: string, seeds: Set<string>): string {
  // Breadth-first from the test to the nearest changed file, for a readable reason.
  const seen = new Set([test]);
  const queue = [test];
  while (queue.length) {
    const file = queue.shift()!;
    for (const dep of graph.imports.get(file) ?? []) {
      if (seeds.has(dep)) return dep;
      if (!seen.has(dep)) {
        seen.add(dep);
        queue.push(dep);
      }
    }
  }
  for (const [file, targets] of graph.unresolved) if (file === test || seen.has(file)) return `${targets[0]} (removed)`;
  return 'a changed file';
}

function fullRun(config: Config | undefined, reason: string, changed: Change[], started: number): Decision {
  return {
    version: 1,
    mode: config?.mode ?? 'shadow',
    model: config?.model.id ?? 'unknown',
    full: true,
    reason,
    changed,
    tests: {},
    ms: Date.now() - started,
  };
}
