'use client';
import { useEffect, useState } from 'react';
import hero from '@/data/hero.json';
import { tip } from '@/lib/tip';
import { TipScope } from './tip-scope';

// bench/hero.json: what picky's graph ran for 4 real zod commits, file by file. One label per commit, in order.
const LABELS = ['Schema fix', 'Locale fix', 'Docs edit', 'Lockfile bump'];
const SUMMARY = ['the tests that import core/schemas.ts', 'only the locale test that changed', 'only an .mdx file changed: nothing to run', 'pnpm-lock.yaml changed: JS runs in full'];

const TOTAL = hero.files.length;
const repoName = hero.repo.replace('https://github.com/', '');
const short = (file: string) => file.replace(/^packages\//, '');
const SCENARIOS = hero.commits.map((c, i) => ({
  ...c,
  label: LABELS[i] ?? c.sha,
  summary: SUMMARY[i] ?? c.subject,
  run: c.run as Record<string, string>,
  count: Object.keys(c.run).length,
}));
const CELLS = hero.files.map((file) => {
  const path = short(file);
  const slash = path.lastIndexOf('/');
  return { name: path.slice(slash + 1), dir: path.slice(0, slash) };
});

/** zod's test files, one cell each, lit by what picky ran for 4 real commits. Cycles until a chip is picked. */
export function HeroMatrix() {
  const [index, setIndex] = useState(0);
  const [pinned, setPinned] = useState(false);

  useEffect(() => {
    if (pinned || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const timer = setInterval(() => setIndex((i) => (i + 1) % SCENARIOS.length), 3600);
    return () => clearInterval(timer);
  }, [pinned]);

  const s = SCENARIOS[index];
  const all = s.count === TOTAL;
  const tone = all ? 'text-rp-gold' : s.count === 0 ? 'text-rp-foam' : 'text-rp-iris';
  const dot = all ? 'bg-rp-gold' : s.count === 0 ? 'bg-rp-foam' : 'bg-rp-iris';
  const lit = all ? 'bg-rp-gold text-rp-gold' : 'bg-rp-iris text-rp-iris';
  const skipped = TOTAL - s.count;

  return (
    <div className="relative">
      <div aria-hidden="true" className="pointer-events-none absolute -inset-10 bg-[radial-gradient(closest-side,color-mix(in_oklab,var(--color-rp-iris)_16%,transparent),transparent)]" />
      <div className="relative overflow-hidden rounded-[20px] border border-rp-hl-med bg-rp-surface shadow-2xl shadow-black/20">
        <div className="flex items-center justify-between gap-3 border-b px-4 py-3 text-[13px]">
          <div className="flex items-center gap-2.5">
            <span className={`size-2 animate-pulse rounded-full ${dot}`} />
            <span className="font-mono">{repoName}</span>
            <span className="text-fd-muted-foreground">· {TOTAL} test files</span>
          </div>
          <span className="text-xs text-fd-muted-foreground">real commits, replayed</span>
        </div>
        <div className="flex flex-wrap gap-2 px-4 pt-3.5">
          {SCENARIOS.map((x, i) => (
            <button
              key={x.sha}
              type="button"
              aria-pressed={i === index}
              onClick={() => {
                setIndex(i);
                setPinned(true);
              }}
              className={`rounded-full border px-3 py-1.5 text-[12.5px] transition-colors ${
                i === index ? 'border-rp-iris bg-rp-overlay text-fd-foreground' : 'border-rp-hl-med text-fd-muted-foreground hover:border-rp-muted'
              }`}
            >
              {x.label}
            </button>
          ))}
        </div>
        <TipScope
          role="img"
          aria-label={`${s.count} of ${TOTAL} test files run`}
          className="grid grid-cols-[repeat(auto-fill,minmax(13px,1fr))] gap-1 px-4 pt-4 pb-1.5"
        >
          {CELLS.map((cell, i) => {
            const why = s.run[i];
            const delay = `${(i % 29) * 9}ms`;
            return (
              <div
                // biome-ignore lint/suspicious/noArrayIndexKey: one cell per test file, in a fixed order
                key={i}
                {...tip({
                  kicker: `test file ${i + 1} of ${TOTAL}`,
                  title: cell.name,
                  rows: [
                    ['in', cell.dir],
                    ['status', why ? 'runs' : 'skipped'],
                  ],
                  note: why ? `Why: ${short(why)}.` : 'Nothing it imports changed.',
                  tone: why ? (all ? 'bg-rp-gold' : 'bg-rp-iris') : 'bg-rp-muted',
                })}
                className={`aspect-square rounded-[3px] hover:relative hover:z-10 hover:-translate-y-0.5 hover:scale-[1.45] hover:opacity-100 hover:shadow-[0_0_0_1.5px_var(--color-rp-text),0_0_14px_2px_currentColor] ${
                  why ? lit : 'bg-rp-overlay text-rp-overlay opacity-70'
                }`}
                // Only the color change ripples across the grid; the hover lift starts at once.
                style={{
                  transition: `background-color 500ms ease ${delay}, color 500ms ease ${delay}, opacity 180ms ease, translate 180ms ease, scale 180ms ease, box-shadow 180ms ease`,
                }}
              />
            );
          })}
        </TipScope>
        <TipScope className="grid items-end gap-3.5 px-4 pt-4 pb-4 sm:grid-cols-2">
          <div>
            <div className={`font-display text-[44px] leading-none font-bold tracking-[-0.03em] tabular-nums ${tone}`}>
              {s.count === 0 ? '0 run' : all ? `All ${TOTAL}` : `${s.count} of ${TOTAL}`}
            </div>
            <div className="mt-1.5 text-[13.5px] text-fd-muted-foreground">
              {all ? 'a full-run gate: nothing skipped' : `test files run · ${skipped} skipped (${Math.round((skipped / TOTAL) * 100)}%)`}
            </div>
          </div>
          <div
            {...tip({
              kicker: `${repoName} · ${s.sha}`,
              title: s.subject,
              rows: [
                ['changed', s.changed.length === 1 ? '1 file' : `${s.changed.length} files`],
                ['ran', `${s.count} of ${TOTAL} test files`],
              ],
              note: s.changed.map(short).join(', '),
              tone: dot,
            })}
            className="min-w-0 rounded-[10px] border bg-rp-base px-3 py-2.5 font-mono text-xs leading-relaxed text-fd-muted-foreground transition-colors hover:border-rp-hl-high"
          >
            <div className="truncate text-fd-foreground">
              {s.sha} {s.subject}
            </div>
            <div className={tone}>→ {s.summary}</div>
          </div>
        </TipScope>
      </div>
    </div>
  );
}
