import { ChevronDown, Database, ExternalLink } from "lucide-react";
import { useState } from "react";

import type { EvidenceMeta } from "../types";
import { cn } from "../lib/utils";
import { Badge } from "./ui/badge";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "./ui/collapsible";

function remainingEvidence(evidence: EvidenceMeta) {
  const { source: _source, query: _query, ...rest } = evidence;
  return rest;
}

export function Evidence({ evidence }: { evidence: EvidenceMeta }) {
  const [open, setOpen] = useState(true);
  const rest = remainingEvidence(evidence);
  const hasRest = Object.keys(rest).length > 0;

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <button
          type="button"
          className="flex w-full items-center gap-2 py-2 text-left text-xs font-medium text-zinc-600 hover:text-zinc-950"
        >
          <Database aria-hidden="true" className="h-3.5 w-3.5" />
          Evidence
          {evidence.source && (
            <Badge className="ml-1 bg-emerald-50 text-emerald-700 ring-emerald-200">
              {evidence.source}
            </Badge>
          )}
          <ChevronDown
            aria-hidden="true"
            className={cn("ml-auto h-3.5 w-3.5 transition-transform", open && "rotate-180")}
          />
        </button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="pb-3 pl-5">
          {evidence.query && (
            <div className="flex min-w-0 items-start gap-2 rounded-md bg-emerald-50/70 px-3 py-2 text-xs text-emerald-950 ring-1 ring-inset ring-emerald-100">
              <ExternalLink aria-hidden="true" className="mt-0.5 h-3.5 w-3.5 shrink-0 text-emerald-600" />
              <code className="min-w-0 break-all font-mono leading-5">{evidence.query}</code>
            </div>
          )}
          {hasRest && (
            <pre className="code-scroll mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-all font-mono text-[11px] leading-5 text-zinc-600">
              {JSON.stringify(rest, null, 2)}
            </pre>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}
