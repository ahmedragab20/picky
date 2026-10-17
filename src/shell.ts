import { spawnSync } from 'node:child_process';
import { scanScript } from './hook.ts';
import type { JobPlan } from './job.ts';
import type { Outcome } from './plan.ts';
import { quoteArgv, splitAssignments, tokenize } from './sh.ts';

/**
 * `shell: picky {0}`: the fallback for steps the bash hook can't reach (Windows, containers,
 * other shells). GitHub hands picky the step script; picky decides every command in it, then
 * runs the result in the real shell the way GitHub would have.
 */

export const SHELLS = ['bash', 'sh', 'pwsh', 'powershell'] as const;
export type Shell = (typeof SHELLS)[number];

export function isShell(name: string | undefined): name is Shell {
  return (SHELLS as readonly string[]).includes(name ?? '');
}

/** GitHub's default shell when the step names none: pwsh on Windows, else bash when present, else sh. */
export function defaultShell(platform: NodeJS.Platform = process.platform): Shell {
  if (platform === 'win32') return 'pwsh';
  return spawnSync('bash', ['--version'], { stdio: 'ignore' }).status === 0 ? 'bash' : 'sh';
}

const isPosix = (shell: Shell) => shell === 'bash' || shell === 'sh';

export function extension(shell: Shell): string {
  return isPosix(shell) ? '.sh' : '.ps1';
}

/** The command line GitHub uses for each built-in shell. */
export function invocation(shell: Shell, file: string): { cmd: string; args: string[] } {
  if (shell === 'bash') return { cmd: 'bash', args: ['--noprofile', '--norc', '-eo', 'pipefail', file] };
  if (shell === 'sh') return { cmd: 'sh', args: ['-e', file] };
  return { cmd: shell, args: ['-command', `. '${file.replaceAll("'", "''")}'`] };
}

/** GitHub wraps PowerShell scripts: stop on errors, and exit with the last native command's code. */
export function finalize(script: string, shell: Shell): string {
  if (isPosix(shell)) return script;
  return `$ErrorActionPreference = 'stop'\n${script}\nif ((Test-Path -LiteralPath variable:\\LASTEXITCODE)) { exit $LASTEXITCODE }\n`;
}

export function quoteFor(shell: Shell, argv: string[]): string {
  if (isPosix(shell)) return quoteArgv(argv);
  return argv.map((w) => (/^[\w\-./:=+,]+$/.test(w) ? w : `'${w.replaceAll("'", "''")}'`)).join(' ');
}

export interface StepPlanner {
  line(words: string[]): Promise<Outcome>;
  /** The job-level rule for setup commands; asked at most once per step. */
  setup(): Promise<JobPlan>;
}

/** Decides every command of a step: recognized ones are skipped, narrowed or kept; setup commands follow the job rule. */
export async function rewriteStep(script: string, shell: Shell, planner: StepPlanner, log: (msg: string) => void): Promise<string> {
  const text = script.replace(/\r\n/g, '\n');
  const lines = text.split('\n');
  const noop = isPosix(shell) ? ': skipped by picky' : '# skipped by picky';
  let setup: Promise<JobPlan> | undefined;

  for (const line of scanScript(text, true)) {
    // PowerShell continues lines with a backtick.
    const continued = !isPosix(shell) && (line.text.trimEnd().endsWith('`') || (lines[line.line - 2] ?? '').trimEnd().endsWith('`'));
    const words = line.simple && !continued ? tokenize(line.text) : null;
    if (!words?.length) continue;
    const indent = /^\s*/.exec(line.text)![0];
    const label = line.text.trim().replace(/\s*#\s*picky.*$/, '');

    if (!line.marked) {
      // Under a whole-step marker only setup commands stay unmarked.
      const plan = await (setup ??= planner.setup());
      log(`picky: ${label} -> ${plan.note}`);
      if (plan.skip) lines[line.line - 1] = indent + noop;
      continue;
    }

    const outcome = await planner.line(words);
    if (outcome.action === 'full' && outcome.note === 'command not recognized; runs in full') continue;
    log(`picky: ${label} -> ${outcome.note}`);
    if (outcome.action === 'skip') lines[line.line - 1] = indent + noop;
    else if (outcome.action === 'narrow') {
      // PowerShell has no `VAR=value cmd` form.
      if (!isPosix(shell) && Object.keys(splitAssignments(outcome.argv).env).length) continue;
      log(`picky: $ ${quoteFor(shell, outcome.argv)}`);
      lines[line.line - 1] = indent + quoteFor(shell, outcome.argv);
    }
  }
  return lines.join('\n');
}
