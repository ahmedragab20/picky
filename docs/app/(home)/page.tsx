import type { ReactNode } from 'react';
import Link from 'next/link';
import { ArrowRight, FileText, GitBranch, LayoutGrid, ShieldCheck } from 'lucide-react';
import { ProofStats, RepoStrips, repos } from '@/components/bench';
import { CopyButton } from '@/components/copy-button';
import { HeroMatrix } from '@/components/hero-matrix';
import { InstallTabs } from '@/components/install-tabs';
import { Pipeline } from '@/components/pipeline';
import { TipScope } from '@/components/tip-scope';
import hero from '@/data/hero.json';
import { agentPrompt } from '@/lib/agent-prompt';
import { percent, seconds } from '@/lib/bench-format';
import { basePath, releaseUrl, repoUrl, version } from '@/lib/shared';
import { type Tip, tip } from '@/lib/tip';

// Tooltip examples come from the benchmark data: bench/results.json and bench/hero.json (zod).
const zod = repos.find((r) => r.key === 'zod') ?? repos[0];
const [schema, , docsEdit] = hero.commits;
const typescript = repos.filter((r) => r.language === 'TypeScript');

const USE_CASES: { icon: typeof FileText; tone: string; dot: string; title: string; body: ReactNode; tip: Omit<Tip, 'title' | 'tone'> }[] = [
  {
    icon: FileText,
    tone: 'text-rp-foam',
    dot: 'bg-rp-foam',
    title: 'Docs, assets and CI scripts',
    body: (
      <>
        No test can see the change, so tests, type checks and the <code>npm ci</code> before them all skip.
      </>
    ),
    tip: {
      rows: repos.map((r): [string, string] => [r.name, `${r.bands[0]} of ${r.ran.length} PRs ran no test`]),
      note: `Example: zod ${docsEdit.sha} edited one .mdx file, so 0 tests ran.`,
    },
  },
  {
    icon: LayoutGrid,
    tone: 'text-rp-iris',
    dot: 'bg-rp-iris',
    title: 'Big JS/TS test suites',
    body: (
      <>
        A fix in one module runs the tests that import it. Vitest, Jest, <code>node --test</code>, ESLint, Prettier.
      </>
    ),
    tip: {
      rows: typescript.map((r): [string, string] => [r.name, `${percent(r.skipped)} skipped`]),
      note: `Example: zod ${schema.sha} changed core/schemas.ts and ran ${Object.keys(schema.run).length} of ${hero.files.length} test files.`,
    },
  },
  {
    icon: GitBranch,
    tone: 'text-rp-rose',
    dot: 'bg-rp-rose',
    title: 'Matrix and multi-OS jobs',
    body: 'One decision per PR, shared by every job: Linux, Windows, containers.',
    tip: {
      rows: [
        ['decisions', '1 per PR'],
        ['bash steps', '# picky'],
        ['pwsh, sh, containers', 'shell: picky {0}'],
      ],
      note: `Deciding took ${seconds(zod.p50)} at the median on zod.`,
    },
  },
  {
    icon: ShieldCheck,
    tone: 'text-rp-gold',
    dot: 'bg-rp-gold',
    title: 'Careful rollouts',
    body: 'Shadow mode runs everything and flags any failure enforce mode would have missed.',
    tip: {
      rows: [
        ['mode: shadow', 'runs everything, reports misses'],
        ['mode: enforce', 'skips for real'],
      ],
      note: 'Switch to enforce in .picky.yml once shadow reports no misses.',
    },
  },
];

const INSTALL_TABS = [
  {
    label: 'Any repo',
    code: `curl -fsSLO ${releaseUrl}/picky.mjs\nnode picky.mjs init           # review the diff\nnode picky.mjs init --write   # apply it\nrm picky.mjs`,
    note: 'Node 20+. Starts in shadow mode.',
  },
  {
    label: 'Enterprise',
    code: `gh release download v${version} -R ahmedragab20/picky -p 'picky.mjs*'\nsha256sum -c picky.mjs.sha256\nnode picky.mjs init           # review, then --write\n# zero egress: model: none in .picky.yml`,
    note: 'Checksum-verified. Nothing fetched at CI time.',
  },
  { label: 'With an agent', code: agentPrompt, note: 'Paste into Claude Code, Cursor or Codex.' },
];

const h2 = 'font-display text-[clamp(32px,4vw,46px)] leading-[1.05] font-bold tracking-[-0.03em]';

export default function HomePage() {
  return (
    <div className="dot-grid flex-1 overflow-x-clip">
      <main className="mx-auto w-full max-w-[1200px] px-4 md:px-6">
        <section className="grid items-center gap-14 pt-16 pb-16 md:pt-22 lg:grid-cols-2">
          <div className="flex flex-col gap-7">
            <Link
              href="/docs/changelog"
              className="inline-flex items-center gap-2 self-start rounded-full border border-rp-hl-med bg-rp-surface px-3 py-1 text-[13px] text-fd-muted-foreground hover:text-fd-foreground"
            >
              <span className="size-[7px] rounded-full bg-rp-foam" />v{version} · first public release candidate · MIT
            </Link>
            <h1 className="font-display text-[clamp(44px,6vw,76px)] leading-[0.98] font-bold tracking-[-0.035em] text-balance">
              Stop running tests your PR <span className="text-rp-iris">can’t break.</span>
            </h1>
            <p className="max-w-[520px] text-[19px] leading-relaxed text-pretty text-fd-muted-foreground">
              picky reads the diff, follows the import graph, and runs only the tests, lint and type checks a change can reach. In JS/TS
              and PHP. Any doubt runs everything.
            </p>
            <div className="flex flex-wrap items-center gap-3">
              <Link
                href="/docs"
                className="inline-flex items-center gap-2 rounded-xl bg-rp-iris px-5.5 py-3 text-[15px] font-semibold text-rp-base transition hover:-translate-y-px hover:brightness-105"
              >
                Set up in shadow mode
                <ArrowRight className="size-4" />
              </Link>
              <div className="inline-flex items-center gap-2.5 rounded-xl border border-rp-hl-med py-1.5 pr-1.5 pl-4 font-mono text-[13.5px]">
                <span className="text-fd-muted-foreground">$</span> node picky.mjs init
                <CopyButton text={`curl -fsSLO ${releaseUrl}/picky.mjs && node picky.mjs init`} className="border-transparent" />
              </div>
            </div>
            <div className="flex flex-wrap gap-x-5 gap-y-2 text-[13px] text-fd-muted-foreground">
              {['one vendored file', 'nothing downloaded in CI', 'fails open'].map((t) => (
                <span key={t} className="inline-flex items-center gap-1.5">
                  <span className="text-rp-foam">✓</span> {t}
                </span>
              ))}
            </div>
          </div>
          <HeroMatrix />
        </section>

        <ProofStats />

        <section className="flex flex-col gap-7 pt-28">
          <div className="flex flex-wrap items-end justify-between gap-4">
            <div className="max-w-[640px]">
              <h2 className={h2}>Every column is a real commit.</h2>
              <p className="mt-3 text-[17px] text-fd-muted-foreground">
                The bright part is what picky skipped. Graph-only mode, so this is the most it can skip; with Jev on, it can only add
                tests.
              </p>
            </div>
            <Link href="/docs/benchmarks" className="inline-flex items-center gap-2 font-medium text-rp-iris hover:text-fd-foreground">
              Explore the data <ArrowRight className="size-4" />
            </Link>
          </div>
          <RepoStrips />
        </section>

        <section className="flex flex-col gap-7 pt-28">
          <div className="max-w-[640px]">
            <h2 className={h2}>Cautious by construction.</h2>
            <p className="mt-3 text-[17px] text-fd-muted-foreground">Every step can only add tests. Nothing it can’t explain gets skipped.</p>
          </div>
          <Pipeline />
        </section>

        <section className="flex flex-col gap-7 pt-28">
          <h2 className={h2}>Where it pays off</h2>
          <TipScope className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {USE_CASES.map(({ icon: Icon, tone, dot, title, body, tip: t }) => (
              <div
                key={title}
                {...tip({ ...t, title, tone: dot })}
                className="group flex flex-col gap-2.5 rounded-2xl border bg-rp-surface p-5.5 transition-[translate,border-color,background-color] duration-200 hover:-translate-y-1 hover:border-rp-hl-high hover:bg-rp-overlay/50"
              >
                <Icon className={`size-[22px] transition-[scale] duration-200 group-hover:scale-115 ${tone}`} strokeWidth={1.8} />
                <div className="text-[17px] font-semibold">{title}</div>
                <div className="text-[14.5px] text-fd-muted-foreground [&_code]:font-mono [&_code]:text-[13px] [&_code]:text-fd-foreground">{body}</div>
              </div>
            ))}
          </TipScope>
        </section>

        <section id="install" className="grid items-start gap-10 pt-28 lg:grid-cols-2">
          <div className="flex flex-col gap-4">
            <h2 className={h2}>Two commands. Then a PR.</h2>
            <p className="text-[17px] text-fd-muted-foreground">
              <code className="font-mono text-fd-foreground">init</code> prints every change as a diff first. It vendors one file into{' '}
              <code className="font-mono text-fd-foreground">.github/picky/</code>, so CI downloads nothing.
            </p>
            <ol className="mt-2 flex flex-col gap-2.5">
              {[
                <>
                  Run it, review the diff, apply with <code>--write</code>
                </>,
                <>
                  Add the <code>PICKY_API_KEY</code> secret, or set <code>model: none</code>
                </>,
                <>Merge. Shadow mode until no misses, then enforce</>,
              ].map((step, i) => (
                // biome-ignore lint/suspicious/noArrayIndexKey: a fixed list
                <li key={i} className="flex items-baseline gap-3 [&_code]:font-mono [&_code]:text-sm">
                  <span className="font-mono text-xs text-rp-iris">{String(i + 1).padStart(2, '0')}</span>
                  <span>{step}</span>
                </li>
              ))}
            </ol>
            <Link href="/docs/agents" className="mt-2 inline-flex items-center gap-2 self-start font-medium text-rp-iris hover:text-fd-foreground">
              Or let a coding agent do it <ArrowRight className="size-4" />
            </Link>
          </div>
          <InstallTabs tabs={INSTALL_TABS} />
        </section>

        <section className="pt-28 pb-24">
          <div className="relative flex flex-col items-center gap-4.5 overflow-hidden rounded-3xl border border-rp-hl-med bg-rp-surface px-8 py-14 text-center">
            <div aria-hidden="true" className="dot-grid-fine absolute inset-0" />
            <h2 className="relative max-w-[760px] font-display text-[clamp(30px,4.4vw,52px)] leading-[1.02] font-bold tracking-[-0.035em] text-balance">
              Start in shadow mode. <span className="text-rp-iris">Enforce when it’s earned.</span>
            </h2>
            <p className="relative max-w-[540px] text-[17px] text-fd-muted-foreground">
              Shadow mode changes nothing about your CI. It only tells you what picky would have skipped, and whether that was safe.
            </p>
            <Link
              href="/docs"
              className="relative mt-1.5 inline-flex items-center gap-2 rounded-xl bg-rp-iris px-6 py-3 text-[15px] font-semibold text-rp-base transition hover:-translate-y-px hover:brightness-105"
            >
              Set up in shadow mode
            </Link>
          </div>
        </section>
      </main>
      <footer className="border-t">
        <div className="mx-auto flex max-w-[1200px] flex-wrap justify-between gap-3 px-4 py-7 text-[13px] text-fd-muted-foreground md:px-6">
          <span>picky · MIT License</span>
          <div className="flex flex-wrap gap-5">
            <a href={repoUrl} className="hover:text-fd-foreground">
              GitHub
            </a>
            <Link href="/docs/changelog" className="hover:text-fd-foreground">
              Changelog
            </Link>
            <a href={`${basePath}/llms.txt`} className="hover:text-fd-foreground">
              llms.txt
            </a>
          </div>
        </div>
      </footer>
    </div>
  );
}
