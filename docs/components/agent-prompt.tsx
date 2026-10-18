import { agentPrompt } from '@/lib/agent-prompt';
import { CopyButton } from './copy-button';

/** The agent setup prompt with a copy button, for pages that offer the agent path. */
export function AgentPrompt() {
  return (
    <div className="not-prose overflow-hidden rounded-[14px] border border-rp-hl-med bg-rp-surface">
      <div className="flex items-center justify-between gap-3 border-b py-2 pr-2 pl-4">
        <span className="font-mono text-xs text-fd-muted-foreground">prompt.txt</span>
        <CopyButton text={agentPrompt} label="Copy prompt" />
      </div>
      <pre className="max-h-90 overflow-auto px-4 py-3.5 font-mono text-[12.5px] leading-[1.75] whitespace-pre-wrap">{agentPrompt}</pre>
    </div>
  );
}
