/** A tooltip's content. `tone` is a background class for the dot next to the title. */
export interface Tip {
  title: string;
  kicker?: string;
  rows?: [string, string][];
  note?: string;
  tone?: string;
}

/**
 * The data attributes a `<TipScope>` reads to draw an element's tooltip. Plain attributes, so server
 * components can mark a hundred chart columns without a client component each.
 */
export function tip({ title, kicker, rows, note, tone }: Tip) {
  return {
    'data-tip': title,
    ...(kicker ? { 'data-tip-kicker': kicker } : {}),
    ...(rows?.length ? { 'data-tip-rows': JSON.stringify(rows) } : {}),
    ...(note ? { 'data-tip-note': note } : {}),
    ...(tone ? { 'data-tip-tone': tone } : {}),
  };
}

export function readTip(el: HTMLElement): Tip {
  const { tip: title = '', tipKicker: kicker, tipRows, tipNote: note, tipTone: tone } = el.dataset;
  return { title, kicker, rows: tipRows ? JSON.parse(tipRows) : undefined, note, tone };
}
