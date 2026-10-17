import { appendFileSync } from 'node:fs';
import type { Decision } from './decide.ts';
import type { Narrow, Skip } from './plan.ts';
import { quoteArgv } from './sh.ts';

/** Human-readable decision for the terminal. */
export function formatDecision(d: Decision): string {
  const lines = [`picky: ${d.full ? 'FULL RUN' : 'selective'} (${d.mode}, model ${d.model}, ${d.ms} ms): ${d.reason}`];
  if (d.base) lines.push(`picky: diff ${d.base.slice(0, 8)}..${d.head?.slice(0, 8)}, ${d.changed.length} changed files`);
  lines.push(`picky: ${modelLine(d)}`);
  const tests = Object.entries(d.tests).sort(([a], [b]) => a.localeCompare(b));
  for (const [test, t] of tests) {
    const p = t.p === undefined ? '' : ` p=${t.p.toFixed(2)}`;
    lines.push(`picky:   ${t.run ? 'RUN ' : 'skip'} ${test}  (${t.reason}${p})`);
  }
  return lines.join('\n');
}

function modelLine(d: Decision): string {
  const u = d.modelUsage;
  if (u?.error) return `model ${u.model} FAILED after ${u.ms} ms (${u.error}); its ${u.questions} tests run`;
  if (u) return `model ${u.model} called: ${u.questions} questions, ${u.requests} request(s), ${u.inputTokens} input tokens, ${u.ms} ms`;
  if (d.model === 'none') return 'model not used (graph-only mode)';
  if (d.full) return 'model not called (full run)';
  return 'model not called (every test was decided by the graph or by rules)';
}

/** Appends the decision to the GitHub job summary, when running in Actions. */
export function writeJobSummary(d: Decision, env: NodeJS.ProcessEnv): void {
  if (!env.GITHUB_STEP_SUMMARY) return;
  const rows = Object.entries(d.tests)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([test, t]) => `| ${t.run ? 'run' : 'skip'} | \`${test}\` | ${t.reason}${t.p === undefined ? '' : ` (p=${t.p.toFixed(2)})`} |`);
  const md = [
    `### picky: ${d.full ? 'full run' : 'selective'}`,
    `${d.reason} · mode \`${d.mode}\` · ${modelLine(d)} · ${d.ms} ms`,
    '',
    ...(rows.length ? ['| | Test file | Why |', '|---|---|---|', ...rows] : []),
    '',
  ].join('\n');
  try {
    appendFileSync(env.GITHUB_STEP_SUMMARY, md);
  } catch {
    // the summary is informational only
  }
}

/** What a shadow miss on this command says, written when the step is rewritten. */
export function shadowMissMessage(argv: string[], enforce: Skip | Narrow): string {
  const full = quoteArgv(argv);
  if (enforce.action === 'skip') return `enforce mode would have skipped \`${full}\` (${enforce.note.replace(/^skip: /, '')}), and it failed`;
  return `enforce mode would have run only \`${quoteArgv(enforce.argv)}\` (${enforce.note}), which passes, while the full \`${full}\` failed`;
}

/**
 * The shadow check after a failed full run, in steps (`picky shadow-report <kind> [arg]`):
 * - `check <command>`: about to run what enforce mode would have run
 * - `caught`: that failed too
 * - `missed <message>`: that passed
 * - `skipped <message>`: enforce mode would have skipped the command
 */
export function shadowReport(kind: string, arg: string, env: NodeJS.ProcessEnv, log: (msg: string) => void): void {
  const actions = env.GITHUB_ACTIONS === 'true';
  if (kind === 'check') {
    log('picky: shadow check: the full run failed; running what enforce mode would have run');
    log(`picky: $ ${arg}`);
    if (actions) process.stdout.write('::group::picky shadow check\n');
    return;
  }
  if (actions && (kind === 'caught' || kind === 'missed')) process.stdout.write('::endgroup::\n');
  if (kind === 'caught') log('picky: shadow check: that fails too, so enforce mode would have caught this failure');
  else reportShadowMiss(arg, env, log);
}

/**
 * Shadow mode found a failure that enforce mode would have hidden: the signal shadow mode exists for.
 * Goes to the log, a warning annotation on the run, and the job summary.
 */
function reportShadowMiss(message: string, env: NodeJS.ProcessEnv, log: (msg: string) => void): void {
  const advice = 'If the failure is unrelated to the change (a flaky test, a runner problem), ignore it; otherwise enforce mode is not safe yet for this change.';
  log(`picky: SHADOW MISS: ${message}. ${advice}`);
  if (env.GITHUB_ACTIONS === 'true') {
    const escaped = message.replaceAll('%', '%25').replaceAll('\r', '%0D').replaceAll('\n', '%0A');
    process.stdout.write(`::warning title=picky shadow miss::${escaped}\n`);
  }
  if (!env.GITHUB_STEP_SUMMARY) return;
  try {
    appendFileSync(env.GITHUB_STEP_SUMMARY, `### picky: shadow miss\n${message}.\n\n${advice}\n\n`);
  } catch {
    // the summary is informational only
  }
}
