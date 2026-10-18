'use client';
import { type HTMLAttributes, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { readTip, type Tip } from '@/lib/tip';

const GAP = 10;
const MARGIN = 12;

const clamp = (n: number, min: number, max: number) => Math.min(Math.max(n, min), max);

/**
 * Points at the target's `[data-tip-anchor]` child when it has one (the visible bar inside a full-height
 * column), else at the target. Re-placed every frame, so it stays on targets that lift, scale or resize.
 */
function Bubble({ el, tip }: { el: HTMLElement; tip: Tip }) {
  const ref = useRef<HTMLDivElement>(null);
  const arrowRef = useRef<HTMLSpanElement>(null);
  const [below, setBelow] = useState(false);

  useLayoutEffect(() => {
    const node = ref.current;
    const arrow = arrowRef.current;
    if (!node || !arrow) return;
    const anchor = el.querySelector<HTMLElement>('[data-tip-anchor]') ?? el;
    let frame = 0;
    let side: boolean | null = null;
    const place = () => {
      const target = anchor.getBoundingClientRect();
      // Layout size, not the bounding box: the entry animation scales the bubble.
      const width = node.offsetWidth;
      const height = node.offsetHeight;
      const center = target.left + target.width / 2;
      const flip = target.top - height - GAP < MARGIN;
      const left = clamp(center - width / 2, MARGIN, window.innerWidth - width - MARGIN);
      node.style.left = `${left}px`;
      node.style.top = `${flip ? target.bottom + GAP : target.top - height - GAP}px`;
      // The arrow is 8px wide and sits inside the bubble's 1px border.
      arrow.style.left = `${clamp(center - left, 14, width - 14) - 5}px`;
      if (flip !== side) {
        side = flip;
        setBelow(flip);
      }
      frame = requestAnimationFrame(place);
    };
    place();
    return () => cancelAnimationFrame(frame);
  }, [el]);

  return (
    <div
      ref={ref}
      role="tooltip"
      className="pointer-events-none fixed top-0 left-0 z-[60] w-max max-w-[300px] rounded-xl border border-rp-hl-med bg-rp-surface/95 px-3 py-2.5 text-[12.5px] leading-snug text-fd-foreground shadow-[0_18px_40px_-12px_rgb(0_0_0/0.55)] backdrop-blur-md motion-safe:animate-[tip-in_140ms_ease-out]"
    >
      {tip.kicker && <div className="mb-1 font-mono text-[11px] tracking-wide text-fd-muted-foreground">{tip.kicker}</div>}
      <div className="flex items-start gap-2 font-semibold">
        <span className={`mt-[5px] size-2 shrink-0 rounded-full ${tip.tone ?? 'bg-rp-iris'}`} />
        <span className="break-words">{tip.title}</span>
      </div>
      {tip.rows && (
        <dl className="mt-1.5 grid grid-cols-[auto_auto] gap-x-4 gap-y-0.5 font-mono text-xs">
          {tip.rows.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-fd-muted-foreground">{label}</dt>
              <dd className="text-right tabular-nums">{value}</dd>
            </div>
          ))}
        </dl>
      )}
      {tip.note && <p className="mt-1.5 text-xs text-fd-muted-foreground">{tip.note}</p>}
      <span
        ref={arrowRef}
        aria-hidden="true"
        className={`absolute size-2 rotate-45 border-rp-hl-med bg-rp-surface ${below ? '-top-[5px] border-t border-l' : '-bottom-[5px] border-r border-b'}`}
      />
    </div>
  );
}

/**
 * Shows a tooltip for any element inside it that carries `tip()` data attributes. One bubble per scope
 * follows the pointer from element to element; touch taps keep it open until the next tap or scroll.
 */
export function TipScope({ children, ...props }: HTMLAttributes<HTMLDivElement>) {
  const ref = useRef<HTMLDivElement>(null);
  const [active, setActive] = useState<{ el: HTMLElement; tip: Tip } | null>(null);

  const show = (target: EventTarget | null) => {
    const el = target instanceof Element ? target.closest<HTMLElement>('[data-tip]') : null;
    if (!el || !ref.current?.contains(el)) return setActive(null);
    setActive((a) => (a?.el === el ? a : { el, tip: readTip(el) }));
  };

  useEffect(() => {
    if (!active) return;
    const hide = () => setActive(null);
    const outside = (e: PointerEvent) => {
      if (!(e.target instanceof Node && ref.current?.contains(e.target))) hide();
    };
    window.addEventListener('scroll', hide, { capture: true, passive: true });
    window.addEventListener('resize', hide);
    document.addEventListener('pointerdown', outside);
    return () => {
      window.removeEventListener('scroll', hide, { capture: true });
      window.removeEventListener('resize', hide);
      document.removeEventListener('pointerdown', outside);
    };
  }, [active]);

  return (
    <div
      ref={ref}
      onPointerOver={(e) => show(e.target)}
      onPointerLeave={(e) => e.pointerType !== 'touch' && setActive(null)}
      onFocus={(e) => show(e.target)}
      onBlur={() => setActive(null)}
      {...props}
    >
      {children}
      {active && createPortal(<Bubble el={active.el} tip={active.tip} />, document.body)}
    </div>
  );
}
