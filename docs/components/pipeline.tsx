import hero from '@/data/hero.json';
import { tip, type Tip } from '@/lib/tip';
import { TipScope } from './tip-scope';

// Examples come from bench/hero.json: real zod commits, graph only.
const [schema, , docsEdit, lockfile] = hero.commits;
const TOTAL = hero.files.length;
const short = (file: string) => file.replace(/^packages\/zod\/src\//, '');
const ran = (c: (typeof hero.commits)[number]) => Object.keys(c.run).length;

const STEPS: { title: string; body: string; tone: string; bar: string; border: string; tip: Omit<Tip, 'title' | 'tone'> }[] = [
  {
    title: 'Diff',
    body: 'Merge-base against the PR base. Renames and deletions tracked.',
    tone: 'text-fd-foreground',
    bar: 'bg-fd-foreground',
    border: 'hover:border-fd-foreground/40',
    tip: {
      rows: [
        ['base', 'merge-base with the PR base'],
        ['tracks', 'renames and deletions'],
      ],
      note: `Example: zod ${docsEdit.sha} changed one file, ${docsEdit.changed[0].split('/').pop()}.`,
    },
  },
  {
    title: 'Gates',
    body: 'Config, workflow or critical-path changes run everything. A lockfile runs its ecosystem in full.',
    tone: 'text-rp-gold',
    bar: 'bg-rp-gold',
    border: 'hover:border-rp-gold/50',
    tip: {
      rows: [
        ['everything', 'config, workflows, critical paths'],
        ['one ecosystem', 'its lockfile'],
      ],
      note: `Example: zod ${lockfile.sha} changed only ${lockfile.changed[0]}, so all ${ran(lockfile)} test files ran.`,
    },
  },
  {
    title: 'Graph',
    body: 'Every test that reaches a changed file through imports always runs.',
    tone: 'text-rp-foam',
    bar: 'bg-rp-foam',
    border: 'hover:border-rp-foam/50',
    tip: {
      rows: [
        ['JS/TS', 'import graph'],
        ['PHP', 'class graph'],
        ['Kotlin', 'class graph'],
        ['Rust', 'crate graph'],
      ],
      note: `Example: zod ${schema.sha} changed ${short(schema.changed[0])}: the ${ran(schema)} of ${TOTAL} test files that import it ran.`,
    },
  },
  {
    title: 'Jev',
    body: 'A model judges the tests the graph can’t link. Unsure means run.',
    tone: 'text-rp-iris',
    bar: 'bg-rp-iris',
    border: 'hover:border-rp-iris/50',
    tip: {
      rows: [
        ['adds', 'tests only, never removes'],
        ['off', 'model: none'],
      ],
      note: 'The benchmarks on this site are graph only, so Jev could only raise them.',
    },
  },
  {
    title: 'Run',
    body: 'Each marked command is skipped, narrowed or run in full.',
    tone: 'text-rp-rose',
    bar: 'bg-rp-rose',
    border: 'hover:border-rp-rose/50',
    tip: {
      rows: [
        ['marked by', '# picky or shell: picky {0}'],
        ['if picky fails', 'the original command runs'],
      ],
      note: `Example: zod ${docsEdit.sha} edited docs only, so its test commands skipped.`,
    },
  },
];

/** The decision pipeline, left to right, with the fail-open lane under it. */
export function Pipeline() {
  return (
    <TipScope className="not-prose @container flex flex-col gap-3">
      <ol className="group/steps grid gap-3 @lg:grid-cols-2 @4xl:grid-cols-5">
        {STEPS.map((s, i) => (
          <li
            key={s.title}
            {...tip({ ...s.tip, kicker: `step ${i + 1} of ${STEPS.length}`, title: s.title, tone: s.bar })}
            className={`group flex min-h-36 flex-col gap-2.5 rounded-2xl border bg-rp-surface p-4 transition-[translate,border-color,background-color,opacity] duration-200 group-hover/steps:opacity-70 hover:-translate-y-1 hover:bg-rp-overlay/50 hover:opacity-100! ${s.border}`}
          >
            <div className="flex items-center justify-between">
              <span className="font-mono text-xs text-fd-muted-foreground">{String(i + 1).padStart(2, '0')}</span>
              <span className={`h-1 w-7 rounded-full transition-[width] duration-300 group-hover:w-12 ${s.bar}`} />
            </div>
            <div className={`font-display text-[22px] font-bold tracking-[-0.02em] ${s.tone}`}>{s.title}</div>
            <p className="text-sm leading-normal text-fd-muted-foreground">{s.body}</p>
          </li>
        ))}
      </ol>
      <div
        {...tip({
          kicker: 'any step',
          title: 'Fail-open: doubt runs everything',
          rows: [
            ['error or timeout', 'run everything'],
            ['missing key', 'run everything'],
            ['picky itself breaks', 'original command'],
          ],
          tone: 'bg-rp-love',
        })}
        className="flex flex-wrap items-center gap-3 rounded-[14px] border border-dashed border-rp-love/50 px-4 py-3.5 text-[14.5px] text-fd-muted-foreground transition-colors hover:border-rp-love hover:bg-rp-love/5"
      >
        <span className="rounded-md bg-rp-love px-2 py-0.5 font-mono text-xs font-semibold text-rp-base">FAIL-OPEN</span>
        An error, a timeout, a missing key or config picky can’t honor, at any step: everything runs. The original command runs if picky
        itself breaks.
      </div>
    </TipScope>
  );
}
