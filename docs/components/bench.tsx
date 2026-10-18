import benchmarks from '@/data/benchmarks.json';
import { commitTip, percent, type RepoBench, seconds, skipTone } from '@/lib/bench-format';
import { tip } from '@/lib/tip';
import { Explorer } from './bench-explorer';
import { TipScope } from './tip-scope';

interface Run {
  sha: string;
  subject: string;
  changed: number;
  tests: number;
  run: number;
  reason: string;
  ms: number;
}

interface Bench {
  repo: string;
  head: string;
  commits: number;
  /** Runs dropped because the harness failed (e.g. a git fetch timed out), not counted anywhere. */
  excluded?: { sha: string; reason: string }[];
  testFiles: number;
  medianSelected: number;
  meanSelected: number;
  shareNone: number;
  decisionMsP50: number;
  decisionMsP95: number;
  measured: string;
  runs: Run[];
}

const ORDER = ['zod', 'hono', 'composer', 'framework'];
const LANGUAGE: Record<string, string> = { hono: 'TypeScript', zod: 'TypeScript', composer: 'PHP', framework: 'PHP' };
const STORY: Record<string, string> = {
  zod: 'A third of PRs touched only docs, CI scripts or assets and ran nothing. Most source fixes ran the ~15% of tests that import the changed schema code. Releases and lockfile bumps ran everything.',
  hono: 'Adapter and middleware fixes ran a few percent of tests. Changes to the core router or context reach most of the suite, and those ran in full.',
  composer:
    'Docs and CI-only PRs skipped everything. Almost every source change ran the full suite: the shared TestCase imports the console Application, which reaches every class.',
  framework:
    'A tightly coupled framework core: nearly every change reaches nearly every test through the class graph. picky still decides in about a second on 1,000+ test files.',
};

const data = benchmarks as Record<string, Bench>;
const keys = [...ORDER.filter((k) => k in data), ...Object.keys(data).filter((k) => !ORDER.includes(k))];
const share = (r: Run) => (r.tests ? r.run / r.tests : 0);

export const repos: (RepoBench & { head: string; excluded: Bench['excluded'] })[] = keys.map((key) => {
  const b = data[key];
  const runs = [...b.runs].reverse();
  return {
    key,
    name: b.repo.replace('https://github.com/', ''),
    url: b.repo,
    language: LANGUAGE[key] ?? '',
    testFiles: b.testFiles,
    skipped: 1 - b.meanSelected,
    medianSelected: b.medianSelected,
    p50: b.decisionMsP50,
    p95: b.decisionMsP95,
    bands: [
      runs.filter((r) => r.run === 0).length,
      runs.filter((r) => r.run > 0 && share(r) < 0.5).length,
      runs.filter((r) => share(r) >= 0.5).length,
    ],
    ran: runs.map((r) => r.run),
    of: runs.map((r) => r.tests),
    commits: runs.map((r) => ({
      sha: r.sha.slice(0, 7),
      subject: r.subject,
      changed: r.changed,
      ms: r.ms,
      ...(r.reason.startsWith('selected ') ? {} : { gate: r.reason }),
    })),
    story: STORY[key] ?? '',
    head: b.head,
    excluded: b.excluded,
  };
});

const best = repos.reduce((a, b) => (b.skipped > a.skipped ? b : a), repos[0]);
const typescript = repos.filter((r) => r.language === 'TypeScript');

export const benchSummary = {
  repos: repos.length,
  commits: repos.reduce((n, r) => n + r.ran.length, 0),
  best,
  typescriptP50: typescript.reduce((n, r) => n + r.p50, 0) / Math.max(typescript.length, 1),
};

/** The four headline numbers on the landing page. */
export function ProofStats() {
  const { best: b } = benchSummary;
  const typescriptRepos = repos.filter((r) => r.language === 'TypeScript');
  const stats = [
    {
      value: percent(b.skipped),
      label: 'of test files skipped per PR',
      note: `${b.name}, average of its last ${b.ran.length} commits`,
      tone: 'text-rp-foam',
      dot: 'bg-rp-foam',
      tip: {
        kicker: b.name,
        title: `${percent(b.skipped)} of ${b.testFiles} test files skipped, on average`,
        rows: repos.map((r): [string, string] => [r.name, percent(r.skipped)]),
        note: 'Mean share of test files picky did not run, per repo.',
      },
    },
    {
      value: b.bands[0] > 0 ? `1 in ${Math.round(b.ran.length / b.bands[0])}` : '0',
      label: 'PRs ran no test file at all',
      note: `docs, CI scripts and assets: ${b.bands[0]} of ${b.ran.length}`,
      tone: 'text-rp-iris',
      dot: 'bg-rp-iris',
      tip: {
        kicker: `${b.name} · ${b.ran.length} commits`,
        title: `${b.bands[0]} of ${b.ran.length} PRs ran no test file`,
        rows: [
          ['ran nothing', String(b.bands[0])],
          ['ran a slice', String(b.bands[1])],
          ['ran most or all', String(b.bands[2])],
        ] as [string, string][],
        note: 'Nothing a test imports changed: docs, CI scripts or assets.',
      },
    },
    {
      value: `${(benchSummary.typescriptP50 / 1000).toFixed(1)} s`,
      label: 'to decide, graph included',
      note: 'median on the TypeScript repos, no cache',
      tone: 'text-rp-rose',
      dot: 'bg-rp-rose',
      tip: {
        kicker: 'median / p95',
        title: 'Time to decide, per repo',
        rows: repos.map((r): [string, string] => [r.name, `${seconds(r.p50)} / ${seconds(r.p95)}`]),
        note: `The headline is the mean median of the ${typescriptRepos.length} TypeScript repos. Diff, gates and a fresh import graph.`,
      },
    },
    {
      value: String(benchSummary.commits),
      label: 'real commits replayed',
      note: `${benchSummary.repos} open-source repos, TypeScript and PHP`,
      tone: 'text-rp-gold',
      dot: 'bg-rp-gold',
      tip: {
        kicker: 'graph only, no model',
        title: `${benchSummary.commits} commits across ${benchSummary.repos} repos`,
        rows: repos.map((r): [string, string] => [r.name, `${r.ran.length} commits`]),
        note: 'Each commit is diffed against its first parent, like a PR against its base.',
      },
    },
  ];
  return (
    <TipScope className="grid gap-px overflow-hidden rounded-[20px] border bg-fd-border sm:grid-cols-2 lg:grid-cols-4">
      {stats.map((s) => (
        <div key={s.label} {...tip({ ...s.tip, tone: s.dot })} className="group bg-rp-base px-6 py-7 transition-colors duration-200 hover:bg-rp-surface">
          <div
            className={`font-display text-[56px] leading-none font-bold tracking-[-0.04em] tabular-nums transition-[translate] duration-200 group-hover:-translate-y-1 ${s.tone}`}
          >
            {s.value}
          </div>
          <div className="mt-2.5 font-medium">{s.label}</div>
          <div className="mt-0.5 text-[13px] text-fd-muted-foreground">{s.note}</div>
        </div>
      ))}
    </TipScope>
  );
}

/** One strip per repo, one column per commit: the bright part is what picky skipped. */
export function RepoStrips() {
  return (
    <TipScope className="flex flex-col gap-5 rounded-[20px] border bg-rp-surface p-5 md:p-6">
      {repos.map((r) => (
        <div key={r.key} className="flex flex-wrap items-center gap-x-6 gap-y-2.5">
          <div className="flex flex-[1_1_240px] items-center justify-between gap-3">
            <div>
              <a href={r.url} className="font-mono text-sm hover:underline">
                {r.name}
              </a>
              <div className="text-[12.5px] text-fd-muted-foreground">
                {r.language} · {r.testFiles} test files
              </div>
            </div>
            <div
              {...tip({
                kicker: `${r.name} · ${r.ran.length} commits`,
                title: `${percent(r.skipped)} of test files skipped, on average`,
                rows: [
                  ['ran nothing', `${r.bands[0]} PRs`],
                  ['ran a slice', `${r.bands[1]} PRs`],
                  ['ran most or all', `${r.bands[2]} PRs`],
                ],
                tone: r.language === 'PHP' ? 'bg-rp-muted' : 'bg-rp-foam',
              })}
              className="rounded-lg px-2 py-1 text-right transition-colors hover:bg-rp-overlay"
            >
              <div className={`font-display text-[28px] leading-none font-bold ${skipTone(r)}`}>{percent(r.skipped)}</div>
              <div className="text-[11.5px] text-fd-muted-foreground">skipped</div>
            </div>
          </div>
          <div className="group/strip flex h-14 min-w-0 flex-[999_1_520px] items-stretch gap-px overflow-hidden rounded-lg bg-rp-base p-1">
            {r.ran.map((n, i) => (
              <div
                // biome-ignore lint/suspicious/noArrayIndexKey: one column per commit, in a fixed order
                key={i}
                {...tip(commitTip(r, i))}
                className="flex min-w-px flex-1 flex-col overflow-hidden rounded-[1.5px] transition-[opacity,filter] duration-150 group-hover/strip:opacity-40 hover:opacity-100! hover:brightness-125"
              >
                <div className="bg-rp-foam" style={{ height: `${(1 - (r.of[i] ? n / r.of[i] : 0)) * 100}%` }} />
                <div className="grow bg-rp-hl-med" />
              </div>
            ))}
          </div>
        </div>
      ))}
      <div className="flex flex-wrap justify-between gap-3 border-t pt-4 text-[13px] text-fd-muted-foreground">
        <div className="flex gap-4">
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2.5 rounded-[3px] bg-rp-foam" />
            skipped
          </span>
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2.5 rounded-[3px] bg-rp-hl-med" />
            ran
          </span>
        </div>
        <span className="max-w-2xl">
          PHP cores like composer and laravel: their base TestCase imports the whole app, so the class graph reaches almost every test.
          Symbol-level analysis is next.
        </span>
      </div>
    </TipScope>
  );
}

/** The interactive explorer: repo picker, commit strip, bands and a savings estimate. */
export function BenchExplorer() {
  return <Explorer repos={repos.map(({ head: _h, excluded: _e, ...r }) => r)} />;
}

/** Median and 95th percentile decision time, per repo. */
export function LatencyChart() {
  const max = Math.max(...repos.map((r) => r.p95));
  return (
    <TipScope className="not-prose flex flex-col gap-2.5 rounded-[20px] border bg-rp-surface p-5 md:p-6">
      <div className="mb-1.5 flex flex-wrap items-center justify-between gap-2">
        <div className="font-semibold">Time to decide (graph build included, no cache)</div>
        <div className="flex gap-4 text-xs text-fd-muted-foreground">
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2.5 rounded-full bg-rp-rose" />
            median
          </span>
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2.5 rounded-full bg-rp-rose/35" />
            p95
          </span>
        </div>
      </div>
      {repos.map((r) => (
        <div
          key={r.key}
          {...tip({
            kicker: `${r.language} · ${r.ran.length} commits`,
            title: r.name,
            rows: [
              ['median', seconds(r.p50)],
              ['p95', seconds(r.p95)],
              ['test files', r.testFiles.toLocaleString('en-US')],
            ],
            note: 'Diff, gates and a graph built from scratch, with no cache.',
            tone: 'bg-rp-rose',
          })}
          className="group/row -mx-2 grid grid-cols-[minmax(0,9rem)_minmax(0,1fr)_6.5rem] items-center gap-3 rounded-[10px] px-2 py-1.5 text-sm transition-colors hover:bg-rp-overlay"
        >
          <span className="truncate font-mono text-[13px] text-fd-muted-foreground transition-colors group-hover/row:text-fd-foreground">{r.name}</span>
          <div className="relative h-3 rounded-full bg-rp-base transition-[height] duration-200 group-hover/row:h-4">
            <div data-tip-anchor className="absolute inset-y-0 left-0 rounded-full bg-rp-rose/35" style={{ width: `${(r.p95 / max) * 100}%` }} />
            <div
              className="absolute inset-y-0 left-0 rounded-full bg-rp-rose transition-[filter,box-shadow] duration-200 group-hover/row:shadow-[0_0_12px_1px_var(--color-rp-rose)] group-hover/row:brightness-110"
              style={{ width: `${(r.p50 / max) * 100}%` }}
            />
          </div>
          <span className="text-right font-mono text-[13px] tabular-nums">
            {seconds(r.p50)} <span className="text-fd-muted-foreground">/ {seconds(r.p95)}</span>
          </span>
        </div>
      ))}
    </TipScope>
  );
}

/** The raw numbers behind the charts. */
export function BenchTable() {
  return (
    <table>
      <thead>
        <tr>
          <th>Repo</th>
          <th>Head</th>
          <th>Test files</th>
          <th>Skipped (avg)</th>
          <th>Median run</th>
          <th>Ran nothing</th>
          <th>Ran a slice</th>
          <th>Ran most or all</th>
          <th>Decide p50 / p95</th>
          <th>Excluded</th>
        </tr>
      </thead>
      <tbody>
        {repos.map((r) => (
          <tr key={r.key}>
            <td>
              <a href={r.url}>{r.name}</a>
            </td>
            <td>
              <code>{r.head}</code>
            </td>
            <td>{r.testFiles}</td>
            <td>{percent(r.skipped)}</td>
            <td>{percent(r.medianSelected)}</td>
            {r.bands.map((n, i) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: the three bands, in a fixed order
              <td key={i}>{n}</td>
            ))}
            <td>
              {seconds(r.p50)} / {seconds(r.p95)}
            </td>
            <td title={r.excluded?.map((e) => `${e.sha}: ${e.reason}`).join('\n')}>{r.excluded?.length ?? 0}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
