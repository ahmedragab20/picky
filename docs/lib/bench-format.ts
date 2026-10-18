// Shared by the server bench charts and the client explorer, so neither imports the other's helpers.
import type { Tip } from './tip';

/** One benchmark repo, trimmed to what the charts draw. Per-commit arrays run oldest to newest. */
export interface RepoBench {
  key: string;
  name: string;
  url: string;
  language: string;
  testFiles: number;
  /** Mean share of test files skipped, 0 to 1. */
  skipped: number;
  medianSelected: number;
  p50: number;
  p95: number;
  /** PRs that ran nothing, under half the test files, and half or more. */
  bands: [number, number, number];
  ran: number[];
  of: number[];
  /** `gate` is set when a full-run gate decided the commit. */
  commits: { sha: string; subject: string; changed: number; ms: number; gate?: string }[];
  story: string;
}

export const percent = (x: number) => `${Math.round(x * 100)}%`;
export const seconds = (ms: number) => (ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`);
export const skipTone = (r: RepoBench) => (r.language === 'PHP' ? 'text-fd-muted-foreground' : 'text-rp-foam');

// A commit that ran any file shows as at least 1%, so it never reads as "ran nothing".
export const ranPercent = (n: number, of: number) => (n === 0 ? 0 : Math.max(1, Math.round((n / of) * 100)));
export const bandColor = (pct: number) => (pct === 0 ? 'bg-rp-foam' : pct < 50 ? 'bg-rp-iris' : 'bg-rp-muted');
const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;

/** The tooltip for one replayed commit. */
export function commitTip(r: RepoBench, i: number): Tip {
  const c = r.commits[i];
  const n = r.ran[i];
  const of = r.of[i];
  return {
    kicker: `${c.sha} · commit ${i + 1} of ${r.ran.length}`,
    title: c.subject.length > 90 ? `${c.subject.slice(0, 89)}…` : c.subject,
    rows: [
      ['ran', `${n} of ${of} test files`],
      ['skipped', `${of - n} (${Math.round(((of - n) / of) * 100)}%)`],
      ['changed', plural(c.changed, 'file')],
      ['decided in', seconds(c.ms)],
    ],
    note: c.gate ? `Full run: ${c.gate}.` : undefined,
    tone: bandColor(ranPercent(n, of)),
  };
}
