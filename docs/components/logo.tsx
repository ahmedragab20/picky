export function Logo() {
  return (
    <span className="inline-flex items-center gap-2 font-semibold tracking-tight">
      <svg viewBox="0 0 24 24" className="size-5" aria-hidden="true">
        <rect x="2" y="13" width="4" height="9" rx="1.5" className="fill-rp-muted/50" />
        <rect x="10" y="8" width="4" height="14" rx="1.5" className="fill-rp-iris" />
        <rect x="18" y="16" width="4" height="6" rx="1.5" className="fill-rp-muted/50" />
        <circle cx="12" cy="4" r="2" className="fill-rp-rose" />
      </svg>
      picky
    </span>
  );
}
