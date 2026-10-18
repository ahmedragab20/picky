'use client';
import { useState } from 'react';
import { bandColor, commitTip, percent, ranPercent, type RepoBench, seconds, skipTone } from '@/lib/bench-format';
import { tip } from '@/lib/tip';
import { TipScope } from './tip-scope';

const BANDS = [
  {
    label: 'ran nothing',
    className: 'bg-rp-foam text-rp-base',
    dot: 'bg-rp-foam',
    note: 'Docs, CI and asset changes: no test file selected.',
  },
  { label: 'ran a slice', className: 'bg-rp-iris text-rp-base', dot: 'bg-rp-iris', note: 'Under half the test files ran.' },
  {
    label: 'ran most or all',
    className: 'bg-rp-hl-med text-fd-foreground',
    dot: 'bg-rp-muted',
    note: 'Half or more ran, including full-run gates like lockfiles and workflows.',
  },
];
const SOURCE: [string, string] = ['source', 'bench/results.json'];
const card = 'transition-colors hover:bg-rp-overlay/60';

function Stat({ value, label, note, tone = '' }: { value: string; label: string; note: string; tone?: string }) {
  return (
    <div {...tip({ title: `${value} ${label}`, rows: [SOURCE], note })} className={`bg-rp-surface p-5 ${card}`}>
      <div className={`font-display text-4xl leading-none font-bold tracking-[-0.04em] tabular-nums @3xl:text-5xl ${tone}`}>{value}</div>
      <div className="mt-2 text-sm text-fd-muted-foreground">{label}</div>
    </div>
  );
}

/** Pick a repo, hover its commits, and estimate the runner time picky would save. */
export function Explorer({ repos }: { repos: RepoBench[] }) {
  const [active, setActive] = useState(0);
  const [minutes, setMinutes] = useState(12);
  const [prs, setPrs] = useState(200);
  const r = repos[active];
  const pct = r.ran.map((n, i) => ranPercent(n, r.of[i]));
  const commits = r.ran.length;
  const saved = Math.round(minutes * prs * r.skipped);
  const median = r.medianSelected * 100;

  return (
    <TipScope className="not-prose @container flex flex-col gap-4">
      <div
        role="tablist"
        aria-label="Benchmark repo"
        className="flex max-w-full flex-wrap gap-1.5 self-start rounded-[14px] border bg-rp-surface p-1.5"
      >
        {repos.map((x, i) => (
          <button
            key={x.key}
            type="button"
            role="tab"
            aria-selected={i === active}
            onClick={() => setActive(i)}
            className={`flex items-center gap-2 rounded-[10px] border px-3.5 py-2 font-mono text-[13px] transition-colors ${
              i === active
                ? 'border-rp-hl-high bg-rp-overlay text-fd-foreground'
                : 'border-transparent text-fd-muted-foreground hover:text-fd-foreground'
            }`}
          >
            <span className={`size-[7px] rounded-full ${x.language === 'PHP' ? 'bg-rp-muted' : 'bg-rp-foam'}`} />
            {x.name}
          </button>
        ))}
      </div>

      <section className="overflow-hidden rounded-[22px] border border-rp-hl-med bg-rp-surface">
        <div className="grid grid-cols-2 gap-px border-b bg-fd-border @3xl:grid-cols-4">
          <Stat
            value={percent(r.skipped)}
            label="of test files skipped, on average"
            note={`Mean over ${commits} commits of the share of ${r.testFiles} test files picky did not run.`}
            tone={skipTone(r)}
          />
          <Stat
            value={String(r.bands[0])}
            label={`of ${commits} PRs ran no test file`}
            note="Nothing a test can see changed: docs, CI scripts or assets."
          />
          <Stat
            value={`${median.toFixed(median > 99 && median < 100 ? 1 : 0)}%`}
            label="median share of test files run"
            note="Half of the PRs ran this share of the suite or less."
          />
          <Stat
            value={seconds(r.p50)}
            label={`to decide (p95 ${seconds(r.p95)})`}
            note="Diff, gates and a graph built from scratch, with no cache."
            tone="text-rp-rose"
          />
        </div>

        <div className="flex flex-col gap-3.5 p-5 md:p-6">
          <div className="flex flex-wrap items-baseline justify-between gap-2.5">
            <div className="font-semibold">Test files run, commit by commit</div>
            <div className="text-[13px] text-fd-muted-foreground">Hover a column for the commit</div>
          </div>
          <div
            role="img"
            aria-label={`${r.name}: ${r.bands[0]} PRs ran nothing, ${r.bands[1]} ran a slice, ${r.bands[2]} ran most or all test files`}
            className="group/strip flex h-44 items-stretch gap-0.5 rounded-xl border bg-rp-base p-2"
          >
            {pct.map((v, i) => (
              <div
                // oxlint-disable-next-line react/no-array-index-key -- one column per commit, in a fixed order
                key={i}
                {...tip(commitTip(r, i))}
                className="flex min-w-px flex-1 flex-col justify-end rounded-[3px] transition-[opacity,background-color] duration-150 group-hover/strip:opacity-45 hover:bg-rp-overlay hover:opacity-100!"
              >
                <div
                  data-tip-anchor
                  className={`min-h-[3px] rounded-[2px] transition-[height] duration-500 ${bandColor(v)}`}
                  style={{ height: `${Math.max(v, 2)}%` }}
                />
              </div>
            ))}
          </div>
          <div className="flex flex-wrap justify-between gap-2.5 text-[13px] text-fd-muted-foreground">
            <div className="flex flex-wrap gap-4">
              {BANDS.map((b) => (
                <span key={b.label} className="inline-flex items-center gap-1.5">
                  <span className={`size-2.5 rounded-[3px] ${b.dot}`} />
                  {b.label}
                </span>
              ))}
            </div>
            <span>oldest ← {commits} commits → newest</span>
          </div>
        </div>

        <div className="flex flex-col gap-2.5 px-5 pb-6 md:px-6">
          <div className="font-semibold">What the {commits} PRs ran</div>
          <div className="group/bands flex h-10 overflow-hidden rounded-[10px] bg-rp-base">
            {BANDS.map((b, i) => (
              <div
                key={b.label}
                {...tip({
                  title: `${r.bands[i]} of ${commits} PRs ${b.label}`,
                  rows: [['share', percent(r.bands[i] / commits)]],
                  note: b.note,
                  tone: b.dot,
                })}
                className={`flex items-center overflow-hidden pl-2.5 text-[13px] font-semibold whitespace-nowrap transition-[width,opacity,filter] duration-500 group-hover/bands:opacity-55 hover:opacity-100! hover:brightness-110 ${b.className}`}
                style={{ width: `${(r.bands[i] / commits) * 100}%` }}
              >
                {r.bands[i] / commits >= 0.06 ? `${r.bands[i]} ${b.label}` : ''}
              </div>
            ))}
          </div>
          <p className="text-sm text-fd-muted-foreground">{r.story}</p>
        </div>
      </section>

      <div className="grid gap-4 @4xl:grid-cols-2">
        <section className="flex flex-col gap-4 rounded-[20px] border bg-rp-surface p-5 md:p-6">
          <div>
            <div className="font-semibold">Test files skipped, per repo</div>
            <div className="text-[13.5px] text-fd-muted-foreground">Average over each repo's last {commits} commits</div>
          </div>
          {repos.map((x, i) => (
            <button
              key={x.key}
              type="button"
              onClick={() => setActive(i)}
              {...tip({
                kicker: `${x.language} · click to explore`,
                title: x.name,
                rows: [
                  ['skipped', percent(x.skipped)],
                  ['test files', x.testFiles.toLocaleString('en-US')],
                  ['decide p50', seconds(x.p50)],
                ],
                tone: x.language === 'PHP' ? 'bg-rp-muted' : 'bg-rp-foam',
              })}
              className={`group/row grid grid-cols-[minmax(0,9rem)_minmax(0,1fr)_3.25rem] items-center gap-3.5 rounded-[10px] px-2.5 py-2 text-left transition-colors hover:bg-rp-overlay ${
                i === active ? 'bg-rp-overlay' : ''
              }`}
            >
              <span className="truncate font-mono text-[13px]">{x.name}</span>
              <span className="block h-3 overflow-hidden rounded-full bg-rp-base">
                <span
                  data-tip-anchor
                  className={`block h-full rounded-full transition-[width,filter] duration-500 group-hover/row:brightness-125 ${x.language === 'PHP' ? 'bg-rp-muted' : 'bg-rp-foam'}`}
                  style={{ width: percent(x.skipped) }}
                />
              </span>
              <span className="text-right font-mono text-[13px] tabular-nums">{percent(x.skipped)}</span>
            </button>
          ))}
          <p className="border-t pt-3.5 text-[13.5px] text-fd-muted-foreground">
            TypeScript repos split cleanly along imports. In composer and laravel, the base <code>TestCase</code> imports the whole app, so
            the class graph reaches nearly every test. Symbol-level analysis is on the roadmap.
          </p>
        </section>

        <section className="flex flex-col gap-4 rounded-[20px] border bg-rp-surface p-5 md:p-6">
          <div>
            <div className="font-semibold">What would that save you?</div>
            <div className="text-[13.5px] text-fd-muted-foreground">
              An estimate, if your repo behaves like <span className="font-mono text-fd-foreground">{r.name}</span> and test time scales
              with test files.
            </div>
          </div>
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="flex justify-between">
              <span>Test step, minutes per PR</span>
              <span className="font-mono tabular-nums">{minutes} min</span>
            </span>
            <input
              type="range"
              min={1}
              max={60}
              value={minutes}
              onChange={(e) => setMinutes(Number(e.target.value))}
              className="w-full accent-rp-iris"
            />
          </label>
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="flex justify-between">
              <span>PRs per month</span>
              <span className="font-mono tabular-nums">{prs}</span>
            </span>
            <input
              type="range"
              min={10}
              max={1000}
              step={10}
              value={prs}
              onChange={(e) => setPrs(Number(e.target.value))}
              className="w-full accent-rp-iris"
            />
          </label>
          <div
            {...tip({
              title: 'How this is estimated',
              rows: [
                ['minutes × PRs', `${minutes} × ${prs}`],
                ['skipped share', percent(r.skipped)],
                ['saved', `${saved.toLocaleString('en-US')} min`],
              ],
              note: 'Assumes test time scales with test files. Real savings vary.',
              tone: r.language === 'PHP' ? 'bg-rp-muted' : 'bg-rp-foam',
            })}
            className="flex flex-wrap items-end justify-between gap-3 rounded-[14px] border bg-rp-base p-4 transition-colors hover:border-rp-hl-high"
          >
            <div>
              <div className={`font-display text-[44px] leading-none font-bold tracking-[-0.035em] tabular-nums ${skipTone(r)}`}>
                {saved.toLocaleString('en-US')}
              </div>
              <div className="mt-1.5 text-[13.5px] text-fd-muted-foreground">runner-minutes saved per month</div>
            </div>
            <div className="text-right text-[13.5px] text-fd-muted-foreground">of {(minutes * prs).toLocaleString('en-US')} min today</div>
          </div>
        </section>
      </div>
    </TipScope>
  );
}
