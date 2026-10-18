'use client';
import { Check, Copy } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';

/** Copies `text` and says so for a moment. Shows nothing new if the clipboard is blocked. */
export function CopyButton({ text, label = 'Copy', className = '' }: { text: string; label?: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);

  const copy = () => {
    navigator.clipboard?.writeText(text).then(
      () => {
        setCopied(true);
        clearTimeout(timer.current);
        timer.current = setTimeout(() => setCopied(false), 1600);
      },
      () => {},
    );
  };

  return (
    <button
      type="button"
      onClick={copy}
      className={`inline-flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-fd-accent ${className}`}
    >
      {copied ? <Check className="size-3.5 text-rp-foam" /> : <Copy className="size-3.5" />}
      <span aria-live="polite">{copied ? 'Copied' : label}</span>
    </button>
  );
}
