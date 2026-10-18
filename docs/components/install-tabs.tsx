'use client';
import { useState } from 'react';
import { CopyButton } from './copy-button';

export interface InstallTab {
  label: string;
  code: string;
  note: string;
}

/** The landing page's install box: one tab per way in, each with a copy button. */
export function InstallTabs({ tabs }: { tabs: InstallTab[] }) {
  const [active, setActive] = useState(0);
  const tab = tabs[active];
  return (
    <div className="overflow-hidden rounded-[18px] border border-rp-hl-med bg-rp-surface">
      <div role="tablist" aria-label="Install options" className="flex gap-1 overflow-x-auto border-b px-3">
        {tabs.map((t, i) => (
          <button
            key={t.label}
            type="button"
            role="tab"
            aria-selected={i === active}
            onClick={() => setActive(i)}
            className={`border-b-2 px-3 pt-3.5 pb-3 text-sm font-medium whitespace-nowrap transition-colors ${
              i === active ? 'border-rp-iris text-fd-foreground' : 'border-transparent text-fd-muted-foreground hover:text-fd-foreground'
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>
      <pre className="min-h-40 max-h-80 overflow-auto p-5 font-mono text-[13px] leading-[1.8] break-words whitespace-pre-wrap">{tab.code}</pre>
      <div className="flex flex-wrap items-center justify-between gap-3 border-t bg-rp-base px-5 py-3">
        <span className="text-[13px] text-fd-muted-foreground">{tab.note}</span>
        <CopyButton text={tab.code} />
      </div>
    </div>
  );
}
