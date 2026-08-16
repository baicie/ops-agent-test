import { Database, ExternalLink } from "lucide-react";
import { useState } from "react";

import type { EvidenceMeta } from "../types";
import { cn } from "../lib/utils";
import { Badge } from "./ui/badge";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "./ui/collapsible";

export function Evidence({
  evidence,
  highlighted = false,
  onSelect,
}: {
  evidence: EvidenceMeta;
  highlighted?: boolean;
  onSelect?: (evidenceId: string) => void;
}) {
  const [open, setOpen] = useState(true);
  const evidenceId = typeof evidence.evidence_id === "string" ? evidence.evidence_id : undefined;

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <button
          type="button"
          id={evidenceId ? `evidence-${evidenceId}` : undefined}
          className={cn(
            "flex w-full items-center gap-2 py-2 text-left text-xs font-medium text-zinc-600 hover:text-zinc-950",
            highlighted && "rounded bg-emerald-50 px-2 text-emerald-800",
          )}
          onClick={() => evidenceId && onSelect?.(evidenceId)}
        >
          <Database aria-hidden="true" className="h-3.5 w-3.5" />
          Evidence
          {evidence.source && (
            <Badge className="ml-1 bg-emerald-50 text-emerald-700 ring-emerald-200">
              {evidence.source}
            </Badge>
          )}
          {evidence.truncated && (
            <Badge className="bg-amber-50 text-amber-700 ring-amber-200">truncated</Badge>
          )}
        </button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="pb-3 pl-5">
          {evidenceId && (
            <p className="mb-2 font-mono text-[11px] text-zinc-500">id {evidenceId}</p>
          )}
          {evidence.summary && (
            <p className="mb-2 text-xs leading-5 text-zinc-700">{evidence.summary}</p>
          )}
          {evidence.query && (
            <div className="flex min-w-0 items-start gap-2 rounded-md bg-emerald-50/70 px-3 py-2 text-xs text-emerald-950 ring-1 ring-inset ring-emerald-100">
              <ExternalLink aria-hidden="true" className="mt-0.5 h-3.5 w-3.5 shrink-0 text-emerald-600" />
              <code className="min-w-0 break-all font-mono leading-5">{evidence.query}</code>
            </div>
          )}
          {evidence.sha256 && (
            <p className="mt-2 font-mono text-[10px] text-zinc-400">sha256 {evidence.sha256.slice(0, 16)}…</p>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}
