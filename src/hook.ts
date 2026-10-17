import { isSetupCommand } from './job.ts';
import { quote, tokenize } from './sh.ts';

const MARKER = /(^|\s)#\s*picky(\s|$)/;
const STANDALONE = /^\s*#\s*picky\s*$/;
const HEREDOC = /<<-?\s*(['"]?)([A-Za-z_][A-Za-z0-9_]*)\1/;

export interface ScriptLine {
  /** 1-based. */
  line: number;
  text: string;
  marked: boolean;
  /** False for heredocs and continued lines, which picky never rewrites. */
  simple: boolean;
}

/** Every command line of a step script (blank lines, comments and heredoc bodies left out). */
export function scanScript(script: string, markAll = false): ScriptLine[] {
  const lines = script.split('\n');
  const out: ScriptLine[] = [];
  let heredocEnd: string | null = null;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (heredocEnd !== null) {
      if (line.trim() === heredocEnd) heredocEnd = null;
      continue;
    }
    if (STANDALONE.test(line)) {
      markAll = true;
      continue;
    }
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const doc = HEREDOC.exec(line);
    if (doc) heredocEnd = doc[2];
    const continued = (i > 0 && lines[i - 1].trimEnd().endsWith('\\')) || trimmed.endsWith('\\');
    // A whole-step marker leaves setup commands to the job-level rule.
    const marked = MARKER.test(line) || (markAll && !isSetupCommand(tokenize(line)));
    out.push({ line: i + 1, text: line, marked, simple: !doc && !continued });
  }
  return out;
}

/** 1-based line numbers of commands picky should handle. */
export function markedLines(script: string, markAll = false): number[] {
  return scanScript(script, markAll)
    .filter((l) => l.marked && l.simple)
    .map((l) => l.line);
}

/** 1-based line numbers of unmarked setup commands (`npm ci`, `composer install`, ...). */
export function setupLines(script: string, markAll = false): number[] {
  return scanScript(script, markAll)
    .filter((l) => !l.marked && l.simple && isSetupCommand(tokenize(l.text)))
    .map((l) => l.line);
}

/**
 * Rewrites marked and setup lines so picky decides each command at run time. If picky
 * fails for any reason, the original line runs.
 */
export function rewriteScript(script: string, scriptPath: string, pickyBin: string, markAll = false): string {
  const lines = script.split('\n');
  const all = markAll ? ' --all' : '';
  for (const n of [...markedLines(script, markAll), ...setupLines(script, markAll)]) {
    const original = lines[n - 1];
    const indent = /^\s*/.exec(original)![0];
    lines[n - 1] =
      `${indent}__picky_cmd="$(${quote(pickyBin)} rewrite --script ${quote(scriptPath)} --line ${n}${all})" || __picky_cmd=${quote(original.trim())}; ` +
      `eval "$__picky_cmd"`;
  }
  return lines.join('\n');
}

/** bash flags to reproduce the step's shell options (`$-` plus pipefail). */
export function bashFlags(shellFlags: string | undefined, pipefail: boolean): string[] {
  const flags = ['--noprofile', '--norc'];
  for (const f of ['e', 'u', 'x']) if (shellFlags?.includes(f)) flags.push(`-${f}`);
  if (pipefail) flags.push('-o', 'pipefail');
  return flags;
}
