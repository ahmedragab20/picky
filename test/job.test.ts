import { describe, expect, it } from 'bun:test';
import { decide } from '../src/decide.ts';
import { isSetupCommand, planSetup } from '../src/job.ts';
import { tokenize } from '../src/sh.ts';
import { cleanEnv, repo, SHOP } from './helpers.ts';

const GRAPH_ONLY = { '.picky.yml': 'mode: enforce\nmodel: none\n' };
const MATH_CHANGE = { 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' };
const GH = { GITHUB_WORKFLOW_REF: 'acme/shop/.github/workflows/ci.yml@refs/pull/7/merge', GITHUB_JOB: 'test' };

function workflow(steps: string): Record<string, string> {
  return { '.github/workflows/ci.yml': `on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n${steps}` };
}

const STEPS = [
  '      - uses: actions/checkout@v4',
  '      - uses: actions/setup-node@v4',
  '      - run: npm ci',
  '      - run: node --test # picky',
  '',
].join('\n');

async function setupPlan(files: Record<string, string>, change: Record<string, string | null>, script = 'npm ci', env = GH) {
  const root = repo({ ...SHOP, ...GRAPH_ONLY, ...files }, change);
  return planSetup(script, root, cleanEnv(env), () => decide({ root, env: cleanEnv(env) }));
}

describe('isSetupCommand', () => {
  it('recognizes installs, not commands that add packages or run scripts', () => {
    for (const cmd of [
      'npm ci',
      'npm install --no-audit',
      'pnpm i --frozen-lockfile',
      'yarn',
      'yarn --immutable',
      'bun install',
      'composer install -n --prefer-dist',
      'npx playwright install --with-deps chromium',
    ]) {
      expect(isSetupCommand(tokenize(cmd)), cmd).toBe(true);
    }
    for (const cmd of ['npm install lodash', 'npm test', 'yarn test', 'composer require x/y', 'composer update']) {
      expect(isSetupCommand(tokenize(cmd)), cmd).toBe(false);
    }
  });
});

describe('planSetup', () => {
  it('skips setup when every marked command in the job skips', async () => {
    expect(await setupPlan(workflow(STEPS), { 'README.md': '# docs\n' })).toEqual({
      skip: true,
      note: 'skip: all 1 marked commands in this job skip',
    });
  });

  it('reads markers inside block scripts too', async () => {
    const block = workflow(
      STEPS.replace('      - run: node --test # picky', '      - run: |\n          echo testing # picky\n          node --test # picky'),
    );
    expect(await setupPlan(block, { 'README.md': 'x' })).toMatchObject({
      skip: false,
      note: expect.stringContaining('`echo testing # picky`: command not recognized'),
    });
  });

  it('keeps setup when a marked command runs', async () => {
    const plan = await setupPlan(workflow(STEPS), MATH_CHANGE);
    expect(plan.skip).toBe(false);
    expect(plan.note).toContain('`node --test`: running 2 of 3 test files');
  });

  it('counts `shell: picky {0}` steps as marked, e.g. on Windows', async () => {
    const steps = [
      '      - uses: actions/checkout@v4',
      '      - uses: ./.github/picky',
      '      - run: npm ci',
      '        shell: picky pwsh {0}',
      '      - run: node --test',
      '        shell: picky pwsh {0}',
      '',
    ].join('\n');
    const windows = { '.github/workflows/ci.yml': workflow(steps)['.github/workflows/ci.yml'].replace('ubuntu-latest', 'windows-latest') };
    expect(await setupPlan(windows, { 'README.md': 'x' })).toEqual({ skip: true, note: 'skip: all 1 marked commands in this job skip' });

    // Without picky as its shell, a Windows step is invisible to picky, so setup stays.
    const plain = {
      '.github/workflows/ci.yml': windows['.github/workflows/ci.yml'].replace(
        '      - run: node --test\n        shell: picky pwsh {0}\n',
        '      - run: node --test # picky\n',
      ),
    };
    expect(await setupPlan(plain, { 'README.md': 'x' })).toMatchObject({
      skip: false,
      note: "setup runs (picky can't decide `node --test` here)",
    });
  });

  it('keeps setup when the job does other work', async () => {
    const extra = workflow(`${STEPS}      - run: npm run build\n`);
    expect(await setupPlan(extra, { 'README.md': 'x' })).toMatchObject({
      skip: false,
      note: 'setup runs (the job also runs `npm run build`)',
    });
    const action = workflow(`${STEPS}      - uses: ./.github/actions/e2e\n`);
    expect(await setupPlan(action, { 'README.md': 'x' })).toMatchObject({
      skip: false,
      note: 'setup runs (step uses ./.github/actions/e2e)',
    });
  });

  it('keeps setup when it cannot match the step to the workflow', async () => {
    expect(await setupPlan(workflow(STEPS), { 'README.md': 'x' }, 'npm ci --legacy-peer-deps')).toMatchObject({ skip: false });
    expect(await setupPlan(workflow(STEPS), { 'README.md': 'x' }, 'npm ci', { ...GH, GITHUB_JOB: 'lint' })).toMatchObject({ skip: false });
    expect(await setupPlan(workflow(STEPS), { 'README.md': 'x' }, 'npm ci', {} as typeof GH)).toMatchObject({ skip: false });
  });

  it('keeps setup for marked steps the hook does not run', async () => {
    const pwsh = workflow(STEPS.replace('      - run: node --test # picky', '      - run: node --test # picky\n        shell: pwsh'));
    expect(await setupPlan(pwsh, { 'README.md': 'x' })).toMatchObject({ skip: false });
  });

  it('only reports the skip in shadow mode', async () => {
    const plan = await setupPlan({ ...workflow(STEPS), '.picky.yml': 'mode: shadow\nmodel: none\n' }, { 'README.md': 'x' });
    expect(plan).toEqual({ skip: false, note: 'shadow mode: would skip setup (all 1 marked commands skip); running' });
  });
});
