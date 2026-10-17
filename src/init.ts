import { existsSync, readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { isMap, isScalar, isSeq, parseDocument, type Node, type Pair, type Scalar, type YAMLMap, type YAMLSeq } from 'yaml';
import { recognize } from './commands.ts';
import { scanScript } from './hook.ts';
import { tokenize } from './sh.ts';

/**
 * `picky init`: the files picky needs in a repo, and the workflow edits that turn it on.
 * Workflows are edited as text at positions the YAML parser reports, so formatting and
 * comments stay exactly as they were.
 */

export interface InitFile {
  path: string;
  /** Current content, or null for a new file. */
  before: string | null;
  after: string | Buffer;
  mode?: number;
}

export interface InitAssets {
  hook: string;
  bundle: Buffer;
}

export const WRAPPER = '#!/bin/sh\n# Runs the picky bundle vendored next to this file.\nexec node --no-warnings "$(dirname "$0")/picky.mjs" "$@"\n';

const CONFIG = `# picky: runs only the tests, lint and type checks a change can affect.
mode: shadow   # decide and log, but run everything. Switch to enforce once the summaries look right.
model: jev     # jev | typesafe/<jev-version> | none (graph only)
# critical: ["db/**"]   # a change here, or a test here, always runs
`;

const ENV = {
  BASH_ENV: '${{ github.workspace }}/.github/picky/hook.sh',
  PICKY_API_KEY: '${{ secrets.PICKY_API_KEY }}',
};

export function planInit(root: string, assets: InitAssets): { files: InitFile[]; notes: string[] } {
  const files: InitFile[] = [];
  const notes: string[] = [];
  const add = (rel: string, after: string | Buffer, mode?: number) => {
    const full = path.join(root, rel);
    const before = existsSync(full) ? readFileSync(full, 'utf8') : null;
    const same = typeof after === 'string' ? before === after : before !== null && readFileSync(full).equals(after);
    if (!same) files.push({ path: rel, before, after, mode });
  };

  if (!existsSync(path.join(root, '.picky.yml'))) add('.picky.yml', CONFIG);
  add('.github/picky/hook.sh', assets.hook);
  add('.github/picky/picky', WRAPPER, 0o755);
  add('.github/picky/picky.mjs', assets.bundle);

  const dir = path.join(root, '.github/workflows');
  const workflows = existsSync(dir) ? readdirSync(dir).filter((f) => /\.ya?ml$/.test(f)).sort() : [];
  if (workflows.length === 0) notes.push('no workflows in .github/workflows; add `# picky` to a step and the env lines by hand');
  for (const name of workflows) {
    const rel = `.github/workflows/${name}`;
    const text = readFileSync(path.join(root, rel), 'utf8');
    const result = editWorkflow(text, root);
    notes.push(...result.notes.map((n) => `${rel}: ${n}`));
    if (result.text !== text) add(rel, result.text);
  }
  return { files, notes };
}

interface Edit {
  at: number;
  /** Characters to replace, starting at `at`. */
  remove?: number;
  text: string;
}

/** Marks the recognized commands of a workflow and wires in the hook. */
export function editWorkflow(text: string, root: string): { text: string; notes: string[] } {
  const notes: string[] = [];
  const doc = parseDocument(text);
  if (doc.errors.length) return { text, notes: [`not changed: invalid YAML (${doc.errors[0].message})`] };
  const workflow = doc.toJS() as any;
  if (!runsOnPullRequests(workflow?.on)) return { text, notes: ['not changed: it does not run on pull_request'] };

  const jobsPair = isMap(doc.contents) ? (doc.contents.items as Pair<Scalar, Node>[]).find((p) => p.key?.value === 'jobs') : undefined;
  const jobs = jobsPair?.value;
  if (!jobsPair?.key?.range || !isMap(jobs) || jobs.items.length === 0) return { text, notes: ['not changed: no jobs'] };
  const jobsKeyAt = jobsPair.key.range[0];

  const edits: Edit[] = [];
  const pickyJobs: Array<{ name: string; map: YAMLMap }> = [];
  let instances = 0;
  let marks = 0;

  for (const pair of jobs.items as Pair<Scalar, Node>[]) {
    const name = String(pair.key?.value);
    const job = workflow.jobs[name];
    if (!isMap(pair.value) || !job || name === 'picky') continue;
    const skip = (why: string) => notes.push(`job "${name}" skipped: ${why}`);
    if (job.uses) continue; // a reusable workflow call has no steps here
    if (/windows/i.test(String(job['runs-on']))) { skip('Windows runner (the hook needs bash)'); continue; }
    if (job.container) { skip('container job (bash and node may be missing)'); continue; }
    const jobShell = job.defaults?.run?.shell ?? workflow.defaults?.run?.shell;
    const steps = pair.value.get('steps', true);
    if (!isSeq(steps)) continue;

    let marked = false;
    steps.items.forEach((node, i) => {
      const step = job.steps?.[i];
      if (!isMap(node) || typeof step?.run !== 'string') return;
      const shell = step.shell ?? jobShell;
      if (shell !== undefined && !/^bash(\s|$)/.test(shell)) return;
      const dir = String(step['working-directory'] ?? job.defaults?.run?.['working-directory'] ?? workflow.defaults?.run?.['working-directory'] ?? '.');
      if (dir.includes('${{')) return;
      const run = node.get('run', true);
      if (!isScalar(run) || !run.range) return;

      const cwd = path.resolve(root, dir);
      if (/^\s*picky\s*$/.test(run.comment ?? '') || scanScript(step.run).some((l) => l.marked)) {
        marked = true;
        return;
      }
      const found = markRun(text, run, step.run, cwd);
      if (found.length) marked = true;
      marks += found.length;
      edits.push(...found);
    });

    if (marked) {
      pickyJobs.push({ name, map: pair.value });
      instances += job.strategy?.matrix ? 2 : 1;
    }
  }

  if (pickyJobs.length === 0) return { text, notes: [...notes, 'not changed: no step runs a command picky recognizes'] };

  const envEdit = envEdits(text, doc.get('env', true), jobsKeyAt);
  if (typeof envEdit === 'string') notes.push(envEdit);
  else edits.push(...envEdit);

  // Several jobs (or matrix legs) would each ask the model; one shared decision keeps them consistent.
  if (instances > 1) {
    if (!workflow.jobs.picky) edits.push(decideJob(text, jobs, jobsKeyAt));
    for (const { name, map } of pickyJobs) {
      const job = workflow.jobs[name];
      if (![job.needs].flat().includes('picky')) {
        const added = needsPicky(text, map);
        if (added) edits.push(added);
        else notes.push(`job "${name}": add \`picky\` to its needs by hand`);
      }
      const downloads = job.steps?.some((s: any) => /^actions\/download-artifact@/.test(String(s?.uses)) && s?.with?.name === 'picky-decision');
      const step = downloads ? null : downloadStep(text, map);
      if (step) edits.push(step);
    }
  }
  notes.push(`marked ${marks} command${marks === 1 ? '' : 's'} in ${pickyJobs.length} job${pickyJobs.length === 1 ? '' : 's'}`);
  return { text: apply(text, edits), notes };
}

function runsOnPullRequests(on: unknown): boolean {
  if (typeof on === 'string') return on === 'pull_request';
  if (Array.isArray(on)) return on.includes('pull_request');
  return typeof on === 'object' && on !== null && 'pull_request' in on;
}

/** Edits that add `# picky` to each recognized command of a `run:` value. */
function markRun(text: string, run: Scalar, script: string, cwd: string): Edit[] {
  const recognized = (line: string) => {
    const words = tokenize(line);
    return words !== null && words.length > 0 && recognize(words, cwd) !== null;
  };

  if (run.type === 'PLAIN') {
    const end = run.range![1];
    // A one-line value: the marker goes in as a YAML comment. An existing comment is left alone.
    if (script.includes('\n') || text.slice(end, lineEnd(text, end)).includes('#')) return [];
    return recognized(script) ? [{ at: end, text: ' # picky' }] : [];
  }
  if (run.type !== 'BLOCK_LITERAL') return [];

  // Literal block lines map one to one onto the source lines after the `|` header.
  const first = lineEnd(text, run.range![0]) + 1;
  const source = text.slice(first, run.range![1]).split('\n');
  const edits: Edit[] = [];
  const lines = script.split('\n');
  for (const line of scanScript(script)) {
    const at = line.line - 1;
    const src = source[at];
    if (src === undefined || src.trim() !== lines[at].trim()) return []; // unexpected layout: change nothing
    if (line.simple && recognized(line.text)) {
      const start = first + source.slice(0, at).reduce((n, l) => n + l.length + 1, 0);
      edits.push({ at: start + src.trimEnd().length, text: ' # picky' });
    }
  }
  return edits;
}

/** Adds the hook's env lines at the top of the workflow, keeping any that exist. */
function envEdits(text: string, env: unknown, jobsKeyAt: number): Edit[] | string {
  const lines = (indent: string, keys: string[]) => keys.map((k) => `${indent}${k}: ${ENV[k as keyof typeof ENV]}\n`).join('');
  if (env === undefined) return [{ at: lineStart(text, jobsKeyAt), text: `env:\n${lines('  ', Object.keys(ENV))}\n` }];
  if (!isMap(env) || env.flow || env.items.length === 0) return 'add BASH_ENV and PICKY_API_KEY to its top-level env by hand';
  const present = new Set(env.items.map((p: any) => String(p.key?.value)));
  const missing = Object.keys(ENV).filter((k) => !present.has(k));
  if (missing.length === 0) return [];
  const last = env.items.at(-1) as any;
  const indent = ' '.repeat(column(text, (env.items[0] as any).key.range[0]));
  return [{ at: lineEnd(text, (last.value ?? last.key).range[1]) + 1, text: lines(indent, missing) }];
}

/** The decide job goes first, right under `jobs:`, so a comment above the first job stays with it. */
function decideJob(text: string, jobs: YAMLMap, jobsKeyAt: number): Edit {
  const keyAt = (jobs.items[0] as any).key.range[0];
  const i = ' '.repeat(column(text, keyAt));
  const job = [
    `${i}# Computes the picky decision once; the jobs that need it reuse it. Never fails the run.`,
    `${i}picky:`,
    `${i}  runs-on: ubuntu-latest`,
    `${i}  steps:`,
    `${i}    - uses: actions/checkout@v4`,
    `${i}      continue-on-error: true`,
    `${i}    - run: .github/picky/picky decide`,
    `${i}      continue-on-error: true`,
    `${i}    - uses: actions/upload-artifact@v4`,
    `${i}      continue-on-error: true`,
    `${i}      with:`,
    `${i}        name: picky-decision`,
    `${i}        path: \${{ runner.temp }}/picky`,
    `${i}        retention-days: 7`,
    '',
    '',
  ].join('\n');
  return { at: lineEnd(text, jobsKeyAt) + 1, text: job };
}

/** Adds `picky` to a job's `needs`, or null when its form isn't one picky can edit safely. */
function needsPicky(text: string, job: YAMLMap): Edit | null {
  const needs = job.get('needs', true) as Node | undefined;
  if (needs === undefined) {
    const firstKey = (job.items[0] as any).key;
    const at = lineStart(text, firstKey.range[0]);
    return { at, text: `${' '.repeat(column(text, firstKey.range[0]))}needs: picky\n` };
  }
  if (isScalar(needs) && needs.type === 'PLAIN' && needs.range) {
    const [start, end] = needs.range;
    return { at: start, remove: end - start, text: `[${String(needs.value)}, picky]` };
  }
  if (isSeq(needs) && needs.range && needs.items.length) {
    const seq = needs as YAMLSeq;
    if (seq.flow) return { at: text.lastIndexOf(']', seq.range![1]), text: ', picky' };
    const last = seq.items.at(-1) as any;
    const at = lineStart(text, last.range[0]);
    const dash = text.slice(at, last.range[0]);
    return { at: lineEnd(text, last.range[1]) + 1, text: `${dash}picky\n` };
  }
  return null;
}

/** Downloads the shared decision before the job's first step. A missing artifact is ignored. */
function downloadStep(text: string, job: YAMLMap): Edit | null {
  const steps = job.get('steps', true);
  if (!isSeq(steps) || steps.flow || steps.items.length === 0) return null;
  const first = steps.items[0] as any;
  const at = lineStart(text, first.range[0]);
  const dash = text.slice(at, first.range[0]); // e.g. "      - "
  const i = ' '.repeat(dash.length);
  return {
    at,
    text: [
      `${dash}uses: actions/download-artifact@v4`,
      `${i}continue-on-error: true`,
      `${i}with:`,
      `${i}  name: picky-decision`,
      `${i}  path: \${{ runner.temp }}/picky`,
      '',
    ].join('\n'),
  };
}

function apply(text: string, edits: Edit[]): string {
  let out = text;
  for (const e of [...edits].sort((a, b) => b.at - a.at)) out = out.slice(0, e.at) + e.text + out.slice(e.at + (e.remove ?? 0));
  return out;
}

function lineStart(text: string, offset: number): number {
  return text.lastIndexOf('\n', offset - 1) + 1;
}

function lineEnd(text: string, offset: number): number {
  const i = text.indexOf('\n', offset);
  return i === -1 ? text.length : i;
}

function column(text: string, offset: number): number {
  return offset - lineStart(text, offset);
}
