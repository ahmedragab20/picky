import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { decideCached } from './decide.ts';
import { repoRoot } from './git.ts';
import { bashFlags, rewriteScript, scanScript } from './hook.ts';
import { currentStep, loadJob, planSetup } from './job.ts';
import { planCommand, type Outcome } from './plan.ts';
import { formatDecision, writeJobSummary } from './report.ts';
import { quoteArgv, splitAssignments, tokenize } from './sh.ts';

const USAGE = `picky: runs only the tests, lint and type checks a change can affect (JS/TS and PHP)

usage:
  picky explain                         show the decision for the current branch
  picky decide                          compute the decision once, for other jobs to reuse
  picky -- <command...>                 run a test, lint or check command, narrowed to the change
  picky hook-exec <script> [args...]    run a CI step script, handling '# picky' lines
  picky rewrite --script <s> --line <n> print the shell text for one marked line

environment:
  PICKY_MODE=shadow|enforce   PICKY_MODEL=jev|typesafe/<version>|none
  PICKY_API_KEY               PICKY_BASE=<ref>   PICKY_BUDGET_SECONDS=30`;

const log = (msg: string) => process.stderr.write(`${msg}\n`);
const SELF = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../bin/picky');

async function main(argv: string[]): Promise<number> {
  const [cmd, ...rest] = argv;
  if (cmd === '--') return runCommand(rest);
  if (cmd === 'explain') return explain();
  if (cmd === 'decide') return decideOnce();
  if (cmd === 'hook-exec') return hookExec(rest[0], rest.slice(1));
  if (cmd === 'rewrite') return rewrite(rest);
  log(USAGE);
  return cmd === undefined || cmd === '--help' || cmd === '-h' ? 0 : 2;
}

async function decisionFor(cwd: string) {
  const root = repoRoot(cwd);
  const { decision, report } = await decideCached({ root, env: process.env });
  if (report) {
    if (decision.job && decision.job !== process.env.GITHUB_JOB) log(`picky: using the decision computed by job "${decision.job}"`);
    log(formatDecision(decision));
    writeJobSummary(decision, process.env);
  }
  return { root, decision };
}

async function outcomeFor(argv: string[]): Promise<Outcome> {
  try {
    const { root, decision } = await decisionFor(process.cwd());
    return planCommand(argv, process.cwd(), root, decision);
  } catch (err) {
    return { action: 'full', note: `error, running in full: ${(err as Error).message}` };
  }
}

async function explain(): Promise<number> {
  const root = repoRoot(process.cwd());
  const { decision } = await decideCached({ root, env: { ...process.env } });
  log(formatDecision(decision));
  return 0;
}

/**
 * `picky decide`: computes the decision into the runner's temp directory. A workflow uploads that
 * directory as an artifact and every other job downloads it there, so one model call serves the PR.
 * Always exits 0: a failure here must not block the jobs that need it; they decide on their own.
 */
async function decideOnce(): Promise<number> {
  try {
    const root = repoRoot(process.cwd());
    const { decision, file } = await decideCached({ root, env: process.env });
    log(formatDecision(decision));
    writeJobSummary(decision, process.env);
    if (file) log(`picky: decision saved to ${file}`);
  } catch (err) {
    log(`picky: no shared decision (${(err as Error).message}); each job decides on its own`);
  }
  return 0;
}

/** `picky -- cmd args`: the prefix form for CIs without a step hook. */
async function runCommand(argv: string[]): Promise<number> {
  if (argv.length === 0) return log(USAGE), 2;
  const outcome = await outcomeFor(argv);
  log(`picky: ${outcome.note}`);
  if (outcome.action === 'skip') return 0;

  const { env, rest } = splitAssignments(outcome.action === 'narrow' ? outcome.argv : argv);
  if (outcome.action === 'narrow') log(`picky: $ ${quoteArgv(rest)}`);
  const res = spawnSync(rest[0], rest.slice(1), { stdio: 'inherit', env: { ...process.env, ...env } });
  if (res.error) return log(`picky: ${res.error.message}`), 127;
  return res.status ?? 1;
}

/** Called by the bash hook for a step script that contains `# picky` markers. */
function hookExec(script: string, args: string[]): number {
  const flags = bashFlags(process.env.PICKY_SHELL_FLAGS, process.env.PICKY_PIPEFAIL === '1');
  let target = script;
  try {
    const self = process.env.PICKY_BIN ? path.resolve(process.env.PICKY_BIN) : SELF;
    const text = readFileSync(script, 'utf8');
    const rewritten = rewriteScript(text, path.resolve(script), self, markedInWorkflow(text));
    // Nothing to decide in this step (its workflow is marked elsewhere): run it as is, keeping $0.
    if (rewritten !== text) {
      target = `${script}.picky.sh`;
      writeFileSync(target, rewritten);
    }
  } catch (err) {
    log(`picky: could not prepare the step (${(err as Error).message}); running it unchanged`);
  }
  const res = spawnSync('bash', [...flags, target, ...args], { stdio: 'inherit' });
  if (res.error) return log(`picky: ${res.error.message}`), 127;
  return res.status ?? (res.signal ? 128 : 1);
}

/** True when the workflow marks this step with `run: cmd  # picky`, a YAML comment the script never sees. */
function markedInWorkflow(script: string): boolean {
  try {
    const job = loadJob(repoRoot(process.cwd()), process.env);
    return typeof job !== 'string' && currentStep(job, script)?.markedInYaml === true;
  } catch {
    return false;
  }
}

/** Prints the shell text to run for one marked line. Always prints something runnable. */
async function rewrite(args: string[]): Promise<number> {
  const script = args[args.indexOf('--script') + 1];
  const line = Number(args[args.indexOf('--line') + 1]);
  const text = readFileSync(script, 'utf8');
  const original = text.split('\n')[line - 1].trim();
  const words = tokenize(original);

  if (!scanScript(text, args.includes('--all')).find((l) => l.line === line)?.marked) {
    let note: string;
    let skip = false;
    try {
      const root = repoRoot(process.cwd());
      ({ skip, note } = await planSetup(text, root, process.env, async () => (await decisionFor(process.cwd())).decision));
    } catch (err) {
      note = `setup runs (error: ${(err as Error).message})`;
    }
    log(`picky: ${original} -> ${note}`);
    process.stdout.write(skip ? ': skipped by picky\n' : `${original}\n`);
    return 0;
  }

  const outcome: Outcome = words?.length
    ? await outcomeFor(words)
    : { action: 'full', note: 'not a simple command; runs in full' };

  const label = original.replace(/\s*#\s*picky.*$/, '');
  log(`picky: ${label} -> ${outcome.note}`);
  if (outcome.action === 'skip') process.stdout.write(`: skipped by picky\n`);
  else if (outcome.action === 'narrow') {
    log(`picky: $ ${quoteArgv(outcome.argv)}`);
    process.stdout.write(`${quoteArgv(outcome.argv)}\n`);
  } else process.stdout.write(`${original}\n`);
  return 0;
}

main(process.argv.slice(2)).then(
  (code) => process.exit(code),
  (err) => {
    log(`picky: ${(err as Error).stack ?? err}`);
    process.exit(1);
  },
);
