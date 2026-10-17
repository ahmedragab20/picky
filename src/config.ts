import { readFileSync } from 'node:fs';
import path from 'node:path';
import { parse } from 'yaml';

export type Mode = 'shadow' | 'enforce';

export interface ModelRef {
  /** `none` means graph-only: no model call, nothing outside the graph is selected. */
  provider: 'typesafe' | 'none';
  name: string;
  id: string;
}

export interface Config {
  mode: Mode;
  model: ModelRef;
  critical: string[];
  low: string[];
  apiKey?: string;
  /** Set when the config can't be honored safely; the run becomes a full run. */
  problem?: string;
  /** Raw config text, part of the decision cache key. */
  source: string;
}

export const DEFAULT_JEV = 'jev-1.14.0';
const DEFAULT_LOW = ['**/*.md', '**/*.mdx', 'docs/**', 'LICENSE*'];
const KNOWN_KEYS = new Set(['extends', 'mode', 'model', 'critical', 'low']);

export function loadConfig(root: string, env: NodeJS.ProcessEnv): Config {
  let source = '';
  let raw: Record<string, unknown> = {};
  let problem: string | undefined;
  try {
    source = readFileSync(path.join(root, '.picky.yml'), 'utf8');
    raw = (parse(source) as Record<string, unknown>) ?? {};
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== 'ENOENT') problem = `.picky.yml is invalid: ${(err as Error).message}`;
  }

  for (const key of Object.keys(raw)) {
    if (!KNOWN_KEYS.has(key)) problem ??= `.picky.yml has unknown key "${key}"`;
  }
  // Ignoring an org preset could drop its critical rules, so it forces a full run until supported.
  if (raw.extends) problem ??= '`extends` is not supported in this version yet';

  const mode = (env.PICKY_MODE ?? raw.mode ?? 'shadow') as string;
  if (mode !== 'shadow' && mode !== 'enforce') problem ??= `unknown mode "${mode}"`;

  const modelId = String(env.PICKY_MODEL ?? raw.model ?? 'jev');
  const model = parseModel(modelId);
  if (!model) problem ??= `model "${modelId}" is not supported in this version (use jev, typesafe/<jev-version> or none)`;

  const resolvedModel = model ?? { provider: 'none', name: 'none', id: modelId };
  return {
    mode: mode === 'enforce' ? 'enforce' : 'shadow',
    model: resolvedModel,
    critical: list(raw.critical),
    low: raw.low === undefined ? DEFAULT_LOW : list(raw.low),
    apiKey: resolvedModel.provider === 'typesafe' ? (env.PICKY_API_KEY ?? env.TYPESAFE_API_KEY) : undefined,
    problem,
    // Git may check the file out with CRLF (Windows autocrlf); the line endings don't change the config.
    source: JSON.stringify({ source: source.replace(/\r\n/g, '\n'), mode, modelId }),
  };
}

export function parseModel(id: string): ModelRef | null {
  if (id === 'none') return { provider: 'none', name: 'none', id };
  if (id === 'jev') return { provider: 'typesafe', name: DEFAULT_JEV, id };
  const m = /^typesafe\/(jev-[\w.-]+)$/.exec(id);
  return m ? { provider: 'typesafe', name: m[1], id } : null;
}

function list(value: unknown): string[] {
  if (value === undefined || value === null) return [];
  return (Array.isArray(value) ? value : [value]).map(String);
}
