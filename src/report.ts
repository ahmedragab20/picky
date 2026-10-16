import { appendFileSync } from 'node:fs';
import type { Decision } from './decide.ts';

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
