import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { hookScript } from './assets.ts';
import { decideCached } from './decide.ts';
import { repoRoot } from './git.ts';
import { bashFlags, rewriteScript, scanScript } from './hook.ts';
import { planInit, type InitFile } from './init.ts';
import { currentStep, loadJob, planSetup } from './job.ts';
import { planCommand, type Narrow, type Outcome, type Skip } from './plan.ts';
import { formatDecision, shadowMissMessage, shadowReport, writeJobSummary } from './report.ts';
import { defaultShell, extension, finalize, invocation, isShell, rewriteStep, shadowLine } from './shell.ts';
import { quoteArgv, splitAssignments, tokenize } from './sh.ts';

const USAGE = `picky: runs only the tests, lint and type checks a change can affect (JS/TS and PHP)

usage:
  picky init [--write]                  show (or apply) the files and workflow edits that turn picky on
  picky explain                         show the decision for the current branch
  picky decide                          compute the decision once, for other jobs to reuse
  picky -- <command...>                 run a test, lint or check command, narrowed to the change
  picky shadow-report <kind> [arg]      report a shadow-mode check (used by the step rewrites)
  picky hook-exec <script> [args...]    run a CI step script, handling '# picky' lines
  picky rewrite --script <s> --line <n> print the shell text for one marked line
  picky [bash|sh|pwsh|powershell] <script>
                                        run a whole step: \`shell: picky {0}\` in a workflow

environment:
  PICKY_MODE=shadow|enforce   PICKY_MODEL=jev|typesafe/<version>|none
  PICKY_API_KEY               PICKY_BASE=<ref>   PICKY_BUDGET_SECONDS=30`;

const log = (msg: string) => process.stderr.write(`${msg}\n`);
const SELF = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../bin/picky');

async function main(argv: string[]): Promise<number> {
  const [cmd, ...rest] = argv;
  if (cmd === '--') return runCommand(rest);
  if (cmd === 'shadow-report') return shadowReport(rest[0], rest[1] ?? '', process.env, log), 0;
  if (cmd === 'explain') return explain();
  if (cmd === 'decide') return decideOnce();
  if (cmd === 'init') return init(rest.includes('--write'));
  if (cmd === 'hook-exec') return hookExec(rest[0], rest.slice(1));
  if (cmd === 'rewrite') return rewrite(rest);
  if (isShell(cmd) && rest.length === 1) return shellExec(cmd, rest[0]);
  if (cmd && rest.length === 0 && path.isAbsolute(cmd) && existsSync(cmd)) return shellExec(undefined, cmd);
  log(USAGE);
  return cmd === undefined || cmd === '--help' || cmd === '-h' ? 0 : 2;
}

async function decisionFor(cwd: string) {
  const root = repoRoot(cwd);
  const { decision, report, mismatch } = await decideCached({ root, env: process.env });
  if (report) {
    if (mismatch) log(`picky: not reusing a shared decision: ${mismatch}`);
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

/** The single-file bundle to vendor: this file when running from it, else the build output. */
function bundlePath(): string {
  const self = fileURLToPath(import.meta.url);
  return self.endsWith('.mjs') ? self : path.resolve(path.dirname(self), '../dist/picky.mjs');
}

/** `picky init`: prints the changes as a diff; `--write` applies them. */
function init(write: boolean): number {
  const bundle = bundlePath();
  if (!existsSync(bundle)) return log(`picky: ${bundle} is missing; run \`npm run build\` first`), 1;
  const root = repoRoot(process.cwd());
  const { files, notes } = planInit(root, { hook: hookScript(), bundle: readFileSync(bundle) });
  const out = (s: string) => process.stdout.write(`${s}\n`);

  if (!write) for (const f of files) out(diffOf(f));
  else {
    for (const f of files) {
      const full = path.join(root, f.path);
      mkdirSync(path.dirname(full), { recursive: true });
      writeFileSync(full, f.after);
      if (f.mode) chmodSync(full, f.mode);
    }
  }
  for (const n of notes) out(`picky: ${n}`);
  if (files.length === 0) return out('picky: already set up; nothing to change'), 0;
  if (!write) return out(`picky: ${files.length} file${files.length === 1 ? '' : 's'} would change; run \`picky init --write\` to apply`), 0;
  out(`picky: wrote ${files.map((f) => f.path).join(', ')}`);
  const shadow = files.some((f) => f.path === '.picky.yml') ? ' picky starts in shadow mode: it logs what it would skip and runs everything.' : '';
  out(`picky: next, add the PICKY_API_KEY secret, then commit.${shadow}`);
  return 0;
}

/** A unified diff of one planned file; the bundle is summarized. */
function diffOf(f: InitFile): string {
  if (typeof f.after !== 'string') return `${f.before === null ? 'new' : 'updated'} file ${f.path} (${(f.after.length / 1e6).toFixed(1)} MB)`;
  const tmp = mkdtempSync(path.join(tmpdir(), 'picky-init-'));
  try {
    const side = (name: string, text: string | null) => {
      if (text === null) return '/dev/null';
      const file = path.join(name, f.path);
      mkdirSync(path.join(tmp, path.dirname(file)), { recursive: true });
      writeFileSync(path.join(tmp, file), text);
      return file;
    };
    const res = spawnSync('git', ['diff', '--no-index', '--no-color', '--no-prefix', '--', side('a', f.before), side('b', f.after)], { cwd: tmp, encoding: 'utf8' });
    return res.stdout.trimEnd();
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

/** `picky -- cmd args`: the prefix form for CIs without a step hook. */
async function runCommand(argv: string[]): Promise<number> {
  if (argv.length === 0) return log(USAGE), 2;
  const outcome = await outcomeFor(argv);
  log(`picky: ${outcome.note}`);
  if (outcome.action === 'skip') return 0;

  if (outcome.action === 'narrow') log(`picky: $ ${quoteArgv(outcome.argv)}`);
  const res = spawnArgv(outcome.action === 'narrow' ? outcome.argv : argv);
  if (res.failed && outcome.action === 'full' && outcome.enforce) shadowCheck(argv, outcome.enforce);
  return res.code;
}

/** `failed`: the command ran and exited non-zero (not: it could not start, or a signal stopped it). */
function spawnArgv(argv: string[]): { code: number; failed: boolean } {
  const { env, rest } = splitAssignments(argv);
  const res = spawnSync(rest[0], rest.slice(1), { stdio: 'inherit', env: { ...process.env, ...env } });
  if (res.error) return log(`picky: ${res.error.message}`), { code: 127, failed: false };
  return { code: res.status ?? 1, failed: (res.status ?? 0) !== 0 };
}

/** Shadow mode, after the full run failed: would enforce mode have let the failure through? */
function shadowCheck(argv: string[], enforce: Skip | Narrow): void {
  const report = (kind: string, arg = '') => shadowReport(kind, arg, process.env, log);
  if (enforce.action === 'skip') return report('skipped', shadowMissMessage(argv, enforce));
  report('check', quoteArgv(enforce.argv));
  if (spawnArgv(enforce.argv).code !== 0) report('caught');
  else report('missed', shadowMissMessage(argv, enforce));
}

/** Called by the bash hook for a step script that contains `# picky` markers. */
function hookExec(script: string, args: string[]): number {
  const flags = bashFlags(process.env.PICKY_SHELL_FLAGS, process.env.PICKY_PIPEFAIL === '1');
  let target = script;
  try {
    const text = readFileSync(script, 'utf8');
    const rewritten = rewriteScript(text, path.resolve(script), selfBin(), markedInWorkflow(text));
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

/** The picky the hook runs. */
function selfBin(): string {
  return process.env.PICKY_BIN ? path.resolve(process.env.PICKY_BIN) : SELF;
}

/** `shell: picky [shell] {0}`: decides every command of the step, then runs it in the real shell. */
async function shellExec(named: string | undefined, file: string): Promise<number> {
  const shell = isShell(named) ? named : defaultShell();
  const text = readFileSync(file, 'utf8');
  let script = text;
  try {
    const root = repoRoot(process.cwd());
    const decision = async () => (await decisionFor(process.cwd())).decision;
    // This process's own command line runs the shadow checks: node and the bundle, wherever they are.
    const self = [process.execPath, ...process.execArgv, process.argv[1]];
    script = await rewriteStep(text, shell, { line: outcomeFor, setup: () => planSetup(text, root, process.env, decision), self }, log);
  } catch (err) {
    log(`picky: could not prepare the step (${(err as Error).message}); running it unchanged`);
  }
  const target = `${file}.picky${extension(shell)}`;
  writeFileSync(target, finalize(script, shell));
  const { cmd, args } = invocation(shell, target);
  // The step's own bash must not hand it to picky a second time through the hook.
  const res = spawnSync(cmd, args, { stdio: 'inherit', env: { ...process.env, PICKY_HOOK_ACTIVE: '1' } });
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
  } else if (outcome.action === 'full' && outcome.enforce) {
    process.stdout.write(`${shadowLine('bash', words!, outcome.enforce, [selfBin()])}\n`);
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
