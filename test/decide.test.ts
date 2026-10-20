import { describe, expect, it, vi } from 'bun:test';
import { decide, decideCached, THRESHOLD } from '../src/decide.ts';
import path from 'node:path';
import { cleanEnv, repo, SHOP, SHOP_KOTLIN, SHOP_RUST, tempDir, writeFiles } from './helpers.ts';

const GRAPH_ONLY = { '.picky.yml': 'mode: enforce\nmodel: none\n' };
const JEV = { '.picky.yml': 'mode: enforce\nmodel: jev\n' };

function runs(decision: Awaited<ReturnType<typeof decide>>) {
  return Object.entries(decision.tests)
    .filter(([, t]) => t.run)
    .map(([f]) => f)
    .sort();
}

function jevReply(answers: Record<string, number>) {
  const body = {
    model: 'jev-1.14.0',
    answers: Object.fromEntries(Object.entries(answers).map(([k, p]) => [k, { type: 'noul', noul: p }])),
    usage: { input_tokens: 321, output_tokens: 0 },
  };
  return new Response(JSON.stringify(body));
}

describe('decide: deterministic floor', () => {
  it('selects tests that import the changed file, directly or transitively', async () => {
    const root = repo({ ...SHOP, ...GRAPH_ONLY }, { 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' });
    const d = await decide({ root, env: cleanEnv() });
    expect(d.full).toBe(false);
    expect(runs(d)).toEqual(['test/cart.test.ts', 'test/math.test.ts']);
    expect(d.tests['test/cart.test.ts'].reason).toBe('imports src/math.ts');
  });

  it('skips everything when only low-risk files changed', async () => {
    const root = repo({ ...SHOP, ...GRAPH_ONLY }, { 'README.md': '# shop, documented\n' });
    const d = await decide({ root, env: cleanEnv() });
    expect(runs(d)).toEqual([]);
  });

  it('runs tests that still import a deleted file', async () => {
    const root = repo({ ...SHOP, ...GRAPH_ONLY }, { 'src/strings.ts': null });
    const d = await decide({ root, env: cleanEnv() });
    expect(runs(d)).toEqual(['test/strings.test.ts']);
  });

  it('always runs critical tests and goes full on critical paths', async () => {
    const critical = { '.picky.yml': 'mode: enforce\nmodel: none\ncritical: ["test/strings.test.ts", "db/**"]\n' };
    const d1 = await decide({ root: repo({ ...SHOP, ...critical }, { 'src/math.ts': '// x\n' }), env: cleanEnv() });
    expect(runs(d1)).toContain('test/strings.test.ts');

    const d2 = await decide({ root: repo({ ...SHOP, ...critical }, { 'db/001.sql': 'select 1;' }), env: cleanEnv() });
    expect(d2.full).toBe(true);
    expect(d2.reason).toBe('critical path db/001.sql changed');
  });
});

describe('decide: full-run gates', () => {
  it.each([
    ['the picky config', { '.picky.yml': 'mode: shadow\nmodel: none\n' }, '.picky.yml changed'],
    ['a workflow', { '.github/workflows/ci.yml': 'on: push' }, '.github/workflows/ci.yml changed'],
  ])('runs everything when %s changes', async (_, change, reason) => {
    const d = await decide({ root: repo({ ...SHOP, ...GRAPH_ONLY }, change), env: cleanEnv() });
    expect(d).toMatchObject({ full: true, reason });
  });

  it('runs one ecosystem in full when its lockfile changes', async () => {
    const d = await decide({ root: repo({ ...SHOP, ...GRAPH_ONLY }, { 'package-lock.json': '{}' }), env: cleanEnv() });
    expect(d).toMatchObject({ full: false, ecosystems: { js: 'package-lock.json changed' } });
    expect(runs(d)).toEqual(['test/cart.test.ts', 'test/math.test.ts', 'test/strings.test.ts']);
    expect(d.tests['test/math.test.ts'].reason).toBe('package-lock.json changed');
  });

  it('runs everything on non-PR events in GitHub Actions', async () => {
    const root = repo({ ...SHOP, ...GRAPH_ONLY }, { 'src/math.ts': '// x\n' });
    const d = await decide({ root, env: cleanEnv({ GITHUB_ACTIONS: 'true', GITHUB_EVENT_NAME: 'push' }) });
    expect(d).toMatchObject({ full: true, reason: 'event "push" always runs everything' });
  });

  it('runs everything without an API key, or with config it cannot honor', async () => {
    const noKey = await decide({ root: repo({ ...SHOP, ...JEV }, { 'src/math.ts': '// x\n' }), env: cleanEnv() });
    expect(noKey).toMatchObject({ full: true, reason: 'no API key (set PICKY_API_KEY)' });

    const preset = { '.picky.yml': 'extends: github:org/.github/picky.yml@v1\nmodel: none\n' };
    const ext = await decide({ root: repo({ ...SHOP, ...preset }, { 'src/math.ts': '// x\n' }), env: cleanEnv() });
    expect(ext).toMatchObject({ full: true, reason: '`extends` is not supported in this version yet' });
  });
});

describe('decide: Jev judge', () => {
  const change = { 'src/strings.ts': 'export const shout = (s: string) => s.toUpperCase() + "!";\n', 'config/app.yml': 'tax: 0.2\n' };

  it('asks one yes/no question per test outside the floor and keeps those at or above the threshold', async () => {
    const root = repo({ ...SHOP, ...JEV }, change);
    const fetchImpl = vi.fn(async (_url: string | URL | Request, init?: RequestInit) => {
      const body = JSON.parse(String(init!.body));
      expect(body.model).toBe('jev-1.14.0');
      expect(body.state).toContain('config/app.yml');
      expect(body.state).toContain('tax: 0.2');
      expect(Object.values(body.questions).every((q: any) => q.type === 'noul')).toBe(true);
      const ids = Object.keys(body.questions);
      expect(ids).toHaveLength(2); // math and cart; strings is in the floor
      return jevReply({ [ids[0]]: 0.9, [ids[1]]: THRESHOLD / 2 });
    });
    const d = await decide({ root, env: cleanEnv({ PICKY_API_KEY: 'k' }), fetchImpl });

    expect(fetchImpl).toHaveBeenCalledTimes(1);
    const [, init] = fetchImpl.mock.calls[0];
    expect((init!.headers as Record<string, string>).authorization).toBe('Bearer k');
    expect(d.tests['test/strings.test.ts']).toMatchObject({ run: true, reason: 'imports src/strings.ts' });
    expect(d.tests['test/cart.test.ts']).toMatchObject({ run: true, p: 0.9 });
    expect(d.tests['test/math.test.ts']).toMatchObject({ run: false, p: THRESHOLD / 2 });
    expect(d.modelUsage).toMatchObject({ model: 'jev-1.14.0', questions: 2, requests: 1, inputTokens: 321 });
  });

  it('asks about a Rust crate by its unit tests, and a Kotlin test by its @Test names', async () => {
    const root = repo({ ...SHOP_RUST, ...SHOP_KOTLIN, ...JEV }, { 'config/app.yml': 'tax: 0.2\n' });
    let questions: string[] = [];
    const fetchImpl = vi.fn(async (_url: string | URL | Request, init?: RequestInit) => {
      const body = JSON.parse(String(init!.body));
      questions = Object.values(body.questions).map((q: any) => q.instructions);
      return jevReply({});
    });
    await decide({ root, env: cleanEnv({ PICKY_API_KEY: 'k' }), fetchImpl });
    const about = (needle: string) => questions.find((q) => q.includes(needle)) ?? '';
    expect(about('crates/cart')).toContain('the unit tests and doctests of the Rust crate in "crates/cart"');
    expect(about('crates/cart')).toContain('Test names: totals.');
    expect(about('crates/money/tests/arith.rs')).toContain('Test names: adds.');
    expect(about('core/src/test/kotlin/shop/CartTest.kt')).toContain('Test names: totals items.');
  });

  it('records no model usage when the graph decides everything', async () => {
    const root = repo({ ...SHOP, ...JEV }, { 'README.md': '# docs\n' });
    const fetchImpl = vi.fn();
    const d = await decide({ root, env: cleanEnv({ PICKY_API_KEY: 'k' }), fetchImpl });
    expect(fetchImpl).not.toHaveBeenCalled();
    expect(d.modelUsage).toBeUndefined();
  });

  it('runs the candidates when the model fails, and retries rate limits', async () => {
    const root = repo({ ...SHOP, ...JEV }, change);
    const fetchImpl = vi
      .fn()
      .mockResolvedValueOnce(new Response('slow down', { status: 429 }))
      .mockResolvedValueOnce(new Response('bad key', { status: 401 }));
    const d = await decide({ root, env: cleanEnv({ PICKY_API_KEY: 'k' }), fetchImpl });
    expect(fetchImpl).toHaveBeenCalledTimes(2);
    expect(runs(d)).toEqual(['test/cart.test.ts', 'test/math.test.ts', 'test/strings.test.ts']);
    expect(d.tests['test/math.test.ts'].reason).toBe('model unavailable (HTTP 401: bad key)');
    expect(d.modelUsage).toMatchObject({ error: 'HTTP 401: bad key', questions: 2 });
  });

  it('runs a candidate the model did not answer', async () => {
    const root = repo({ ...SHOP, ...JEV }, change);
    const fetchImpl = vi.fn(async () => jevReply({}));
    const d = await decide({ root, env: cleanEnv({ PICKY_API_KEY: 'k' }), fetchImpl });
    expect(d.tests['test/math.test.ts']).toMatchObject({ run: true, reason: 'model gave no answer' });
  });
});

describe('decideCached: one decision per PR', () => {
  it('lets every job reuse the decision one job computed, so the model is asked once', async () => {
    const root = repo({ ...SHOP, ...JEV }, { 'src/math.ts': '// x\n' });
    const runnerTemp = tempDir();
    const env = (job: string) => cleanEnv({ PICKY_API_KEY: 'k', RUNNER_TEMP: runnerTemp, GITHUB_JOB: job });
    const fetchImpl = vi.fn(async () => jevReply({ t0: 0.9 }));

    const first = await decideCached({ root, env: env('picky'), fetchImpl });
    expect(first).toMatchObject({ cached: false, report: true, decision: { job: 'picky' } });
    expect(first.decision.tests['test/strings.test.ts']).toMatchObject({ run: true, p: 0.9 });

    // Another job with the artifact downloaded into its runner temp: same answers, no model call, reported once.
    const js = await decideCached({ root, env: env('js'), fetchImpl });
    expect(js).toMatchObject({ cached: true, report: true });
    expect(js.decision.tests).toEqual(first.decision.tests);
    expect((await decideCached({ root, env: env('js'), fetchImpl })).report).toBe(false);
    expect((await decideCached({ root, env: env('picky'), fetchImpl })).report).toBe(false);
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it('shares the decision with a container job, which sees the same event at another path', async () => {
    const root = repo({ ...SHOP, ...JEV }, { 'src/math.ts': '// x\n' });
    const runnerTemp = tempDir();
    let n = 0;
    const event = (dir: string) => {
      // The second job (Windows) gets the same event with a BOM, CRLF and indentation.
      writeFiles(dir, {
        'event.json': n++ === 0 ? '{"pull_request":{"number":7}}' : '\uFEFF{\r\n  "pull_request": {\r\n    "number": 7\r\n  }\r\n}\r\n',
      });
      return path.join(dir, 'event.json');
    };
    const env = (job: string, dir: string) =>
      cleanEnv({
        PICKY_API_KEY: 'k',
        RUNNER_TEMP: runnerTemp,
        GITHUB_JOB: job,
        GITHUB_EVENT_NAME: 'pull_request',
        GITHUB_EVENT_PATH: event(dir),
      });
    const fetchImpl = vi.fn(async () => jevReply({ t0: 0.9 }));
    await decideCached({ root, env: env('picky', tempDir()), fetchImpl });
    expect(await decideCached({ root, env: env('alpine', tempDir()), fetchImpl })).toMatchObject({ cached: true, report: true });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it('ignores CRLF in the config (Windows checkouts), and names the input that differs', async () => {
    const root = repo({ ...SHOP, ...JEV }, { 'src/math.ts': '// x\n' });
    const runnerTemp = tempDir();
    const env = (job: string, extra: Record<string, string> = {}) =>
      cleanEnv({ PICKY_API_KEY: 'k', RUNNER_TEMP: runnerTemp, GITHUB_JOB: job, ...extra });
    const fetchImpl = vi.fn(async () => jevReply({ t0: 0.9 }));
    await decideCached({ root, env: env('picky'), fetchImpl });

    writeFiles(root, { '.picky.yml': JEV['.picky.yml'].replaceAll('\n', '\r\n') });
    expect(await decideCached({ root, env: env('windows'), fetchImpl })).toMatchObject({ cached: true });

    const other = await decideCached({ root, env: env('shadow', { PICKY_MODE: 'shadow' }), fetchImpl });
    expect(other).toMatchObject({ cached: false, mismatch: 'the decision from job "picky" was made from a different config' });
  });
});
