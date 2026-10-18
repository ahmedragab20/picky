import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { isMap, isScalar, isSeq, parseDocument } from 'yaml';
import type { Decision } from './decide.ts';
import { scanScript } from './hook.ts';
import { planCommand } from './plan.ts';
import { splitAssignments, tokenize } from './sh.ts';

/**
 * Job-level skip: a setup command (`npm ci`, `composer install`, ...) is skipped when
 * every other command in the GitHub Actions job is a marked command picky skips.
 * Anything picky can't account for keeps the setup.
 */

/** Recognized install commands: they only prepare the job, so they're safe to drop when it does nothing. */
export function isSetupCommand(words: string[] | null): boolean {
  if (!words) return false;
  const [tool, sub, ...args] = splitAssignments(words).rest;
  const flagsOnly = args.every((a) => a.startsWith('-'));
  switch (tool) {
    case 'npm':
      return ['ci', 'clean-install', 'install', 'i'].includes(sub) && flagsOnly;
    case 'pnpm':
    case 'bun':
    case 'composer':
      return ['install', 'i'].includes(sub) && flagsOnly;
    case 'yarn':
      return sub === undefined || sub.startsWith('-') || (sub === 'install' && flagsOnly);
    case 'npx':
      return sub === 'playwright' && (args[0] === 'install' || args[0] === 'install-deps');
    default:
      return false;
  }
}

/** Actions that only fetch code, set up tools, or move caches and artifacts. Others keep the setup. */
const PASSIVE_ACTIONS = [
  /^actions\/(checkout|setup-[\w-]+|cache(\/(restore|save))?|upload-artifact|download-artifact)@/,
  /^(shivammathur\/setup-php|pnpm\/action-setup|oven-sh\/setup-bun)@/,
  /^\.\/\.github\/picky\/?$/, // the setup action `picky init` vendors
];

export interface JobPlan {
  skip: boolean;
  note: string;
}

interface Step {
  run?: string;
  uses?: string;
  /** `run: cmd  # picky` on one line: YAML drops the comment, so the marker never reaches the script. */
  markedInYaml: boolean;
  /** `shell: picky {0}`: picky runs the whole step, every command in it marked. */
  pickyShell: boolean;
  /** Bash with BASH_ENV, or the picky shell, so picky sees the step. */
  hooked: boolean;
  /** Working directory relative to the workspace, or null when it's an expression. */
  dir: string | null;
}

interface Job {
  workflow: string;
  steps: Step[];
}

/** The current job's steps, read from the workflow file. Returns a reason string when it can't. */
export function loadJob(root: string, env: NodeJS.ProcessEnv): Job | string {
  const ref = /^[^/]+\/[^/]+\/(.+?)@/.exec(env.GITHUB_WORKFLOW_REF ?? '');
  if (!ref || !env.GITHUB_JOB) return 'not in GitHub Actions';
  let doc: ReturnType<typeof parseDocument>;
  try {
    doc = parseDocument(readFileSync(path.join(root, ref[1]), 'utf8'));
  } catch {
    return `can't read ${ref[1]}`;
  }
  const workflow = doc.toJS() as any;
  const job = workflow?.jobs?.[env.GITHUB_JOB];
  const nodes = doc.getIn(['jobs', env.GITHUB_JOB, 'steps'], true);
  if (!job || !isSeq(nodes)) return `no job "${env.GITHUB_JOB}" with steps in ${ref[1]}`;

  const steps = nodes.items.map((node, i): Step => {
    const step = job.steps[i] ?? {};
    const runNode = isMap(node) ? node.get('run', true) : undefined;
    const shell = step.shell ?? job.defaults?.run?.shell ?? workflow.defaults?.run?.shell;
    const pickyShell = typeof shell === 'string' && /^picky(\s|$)/.test(shell);
    // In a container the hook path (a host path) doesn't exist, and on Windows there's no bash by default.
    const hookable = !job.container && !/windows/i.test(String(job['runs-on']));
    const dir = String(
      step['working-directory'] ?? job.defaults?.run?.['working-directory'] ?? workflow.defaults?.run?.['working-directory'] ?? '.',
    );
    return {
      run: typeof step.run === 'string' ? step.run : undefined,
      uses: step.uses === undefined ? undefined : String(step.uses),
      markedInYaml: isScalar(runNode) && /^\s*picky\s*$/.test(runNode.comment ?? ''),
      pickyShell,
      hooked: pickyShell || (hookable && (shell === undefined || /^bash(\s|$)/.test(shell))),
      dir: dir.includes('${{') ? null : dir,
    };
  });
  return { workflow: ref[1], steps };
}

/** The workflow step running `script`; a reusable or composite workflow can share the job id, so it must match. */
export function currentStep(job: Job, script: string): Step | undefined {
  const text = script.replace(/\r\n/g, '\n').trim();
  return job.steps.find((s) => s.run !== undefined && s.run.trim() === text);
}

/** Decides one setup line of the current step. `decide` is only called when the job could be skipped. */
export async function planSetup(script: string, root: string, env: NodeJS.ProcessEnv, decide: () => Promise<Decision>): Promise<JobPlan> {
  const keep = (note: string): JobPlan => ({ skip: false, note });
  const job = loadJob(root, env);
  if (typeof job === 'string') return keep(`setup runs (job-level skip: ${job})`);
  if (!currentStep(job, script)) return keep(`setup runs (this step isn't in job "${env.GITHUB_JOB}" of ${job.workflow})`);

  // The git root is a real path; the workspace may reach it through a symlink.
  const workspace = realpath(env.GITHUB_WORKSPACE ?? root);
  const marked: Array<{ words: string[]; cwd: string; text: string }> = [];
  for (const step of job.steps) {
    if (step.uses !== undefined) {
      if (!PASSIVE_ACTIONS.some((re) => re.test(String(step.uses)))) return keep(`setup runs (step uses ${step.uses})`);
      continue;
    }
    if (step.run === undefined) continue;
    for (const line of scanScript(step.run, step.markedInYaml || step.pickyShell)) {
      const words = line.simple ? tokenize(line.text) : null;
      if (!line.marked && isSetupCommand(words)) continue;
      const label = line.text.trim();
      if (!line.marked || !words?.length) return keep(`setup runs (the job also runs \`${label}\`)`);
      if (!step.hooked || step.dir === null) return keep(`setup runs (picky can't decide \`${label}\` here)`);
      marked.push({ words, cwd: realpath(path.resolve(workspace, step.dir)), text: label });
    }
  }
  if (marked.length === 0) return keep('setup runs (no marked commands in this job)');

  const decision = await decide();
  for (const m of marked) {
    const outcome = planCommand(m.words, m.cwd, realpath(root), decision);
    if (outcome.action !== 'skip') {
      const shadow = decision.mode === 'shadow' && outcome.note.includes('would skip');
      if (!shadow) return keep(`setup runs (\`${m.text}\`: ${outcome.note})`);
    }
  }
  if (decision.mode === 'shadow') return keep(`shadow mode: would skip setup (all ${marked.length} marked commands skip); running`);
  return { skip: true, note: `skip: all ${marked.length} marked commands in this job skip` };
}

function realpath(p: string): string {
  try {
    return realpathSync(p);
  } catch {
    return p;
  }
}
