import { tip, type Tip } from '@/lib/tip';
import { TipScope } from './tip-scope';

const LINES: { text: string; tone?: string; tip?: Tip }[] = [
  {
    text: '$ npm test   # picky',
    tone: 'text-rp-iris',
    tip: { title: 'A marked command', note: 'The # picky comment lets picky skip, narrow or fully run this line.', tone: 'bg-rp-iris' },
  },
  {
    text: 'picky: selective (enforce): selected 2 of 3 test files',
    tone: 'text-rp-rose',
    tip: {
      title: 'The decision',
      rows: [
        ['mode', 'enforce'],
        ['selected', '2 of 3 test files'],
      ],
      note: 'Made once per PR and shared by every job.',
      tone: 'bg-rp-rose',
    },
  },
  {
    text: 'picky:   RUN  test/cart.test.ts   (imports src/math.ts)',
    tone: 'text-rp-foam',
    tip: { title: 'Runs: graph link', note: 'cart.test.ts imports src/math.ts, which this PR changed.', tone: 'bg-rp-foam' },
  },
  {
    text: 'picky:   RUN  test/math.test.ts   (imports src/math.ts)',
    tone: 'text-rp-foam',
    tip: { title: 'Runs: graph link', note: 'math.test.ts imports src/math.ts directly.', tone: 'bg-rp-foam' },
  },
  {
    text: 'picky:   skip test/strings.test.ts (no graph link)',
    tone: 'text-rp-muted',
    tip: { title: 'Skipped', note: 'No import path from strings.test.ts reaches src/math.ts.', tone: 'bg-rp-muted' },
  },
  {
    text: 'picky: npm test -> running 2 of 3 test files',
    tone: 'text-rp-gold',
    tip: { title: 'Narrowed', note: 'npm test runs with only the 2 selected files. If picky fails, the original command runs.', tone: 'bg-rp-gold' },
  },
  { text: '✔ sums the cart', tone: 'text-rp-pine' },
  { text: '✔ adds two numbers', tone: 'text-rp-pine' },
];

/** A CI step log, as `npm run demo` prints it for a change to src/math.ts. */
export function Terminal() {
  return (
    <div className="not-prose overflow-hidden rounded-2xl border bg-rp-surface shadow-xl shadow-rp-iris/5">
      <div className="flex items-center gap-1.5 border-b px-4 py-2.5">
        <span className="size-2.5 rounded-full bg-rp-love/70" />
        <span className="size-2.5 rounded-full bg-rp-gold/70" />
        <span className="size-2.5 rounded-full bg-rp-foam/70" />
        <span className="ml-2 font-mono text-xs text-fd-muted-foreground">CI · PR changes src/math.ts</span>
      </div>
      <TipScope>
        <pre className="overflow-x-auto py-4 font-mono text-[13px] leading-6">
          {LINES.map((l) => (
            <div
              key={l.text}
              {...(l.tip ? tip(l.tip) : {})}
              className={`w-max min-w-full px-4 transition-colors ${l.tip ? 'hover:bg-rp-overlay/70' : ''} ${l.tone}`}
            >
              <span data-tip-anchor>{l.text}</span>
            </div>
          ))}
        </pre>
      </TipScope>
    </div>
  );
}
