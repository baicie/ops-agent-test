import {
  ChartNoAxesCombined,
  Check,
  ChevronDown,
  CircleX,
  Clock,
  Globe2,
  LoaderCircle,
  ScrollText,
  ShieldCheck,
  TerminalSquare,
  Waypoints,
  Wrench,
} from "lucide-react";
import { useEffect, useState } from "react";

import type { ToolItem } from "../types";
import { cn } from "../lib/utils";
import { Evidence } from "./Evidence";
import { Badge } from "./ui/badge";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "./ui/collapsible";

function ToolIcon({ name }: { name: string }) {
  const className = "h-4 w-4";
  if (name.includes("prom")) return <ChartNoAxesCombined aria-hidden="true" className={className} />;
  if (name.includes("log")) return <ScrollText aria-hidden="true" className={className} />;
  if (name.includes("trace")) return <Waypoints aria-hidden="true" className={className} />;
  if (name.includes("http")) return <Globe2 aria-hidden="true" className={className} />;
  if (name === "exec") return <TerminalSquare aria-hidden="true" className={className} />;
  return <Wrench aria-hidden="true" className={className} />;
}

function StatusIcon({ status }: Pick<ToolItem, "status">) {
  if (status === "proposed") {
    return <Clock aria-label="Proposed" className="h-3.5 w-3.5 text-zinc-500" />;
  }
  if (status === "authorized") {
    return <ShieldCheck aria-label="Authorized" className="h-3.5 w-3.5 text-sky-700" />;
  }
  if (status === "running") {
    return <LoaderCircle aria-label="Running" className="h-3.5 w-3.5 animate-spin text-sky-600" />;
  }
  if (status === "failed") {
    return <CircleX aria-label="Failed" className="h-3.5 w-3.5 text-red-600" />;
  }
  return <Check aria-label="Completed" className="h-3.5 w-3.5 text-emerald-600" />;
}

function formattedValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined) return "No output returned";
  return JSON.stringify(value, null, 2);
}

export function ToolCall({
  item,
  highlighted = false,
  onSelectEvidence,
}: {
  item: ToolItem;
  highlighted?: boolean;
  onSelectEvidence?: (evidenceId: string) => void;
}) {
  const [open, setOpen] = useState(highlighted);
  const hasArguments = Object.keys(item.arguments).length > 0;
  const hasDetails = hasArguments || item.output !== undefined || item.evidence || item.error;

  useEffect(() => {
    if (highlighted) setOpen(true);
  }, [highlighted]);

  return (
    <Collapsible open={open} onOpenChange={setOpen} disabled={!hasDetails}>
      <section
        className={cn(
          "overflow-hidden rounded-md border bg-white shadow-sm",
          highlighted ? "border-emerald-300 ring-1 ring-emerald-200" : "border-zinc-200",
        )}
      >
        <CollapsibleTrigger asChild>
          <button
            type="button"
            className="flex min-h-12 w-full items-center gap-3 px-3 text-left transition-colors hover:bg-zinc-50 disabled:cursor-default"
          >
            <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded bg-zinc-100 text-zinc-600">
              <ToolIcon name={item.name} />
            </span>
            <span className="min-w-0 flex-1 truncate font-mono text-xs font-semibold text-zinc-800">
              {item.name}
            </span>
            <StatusIcon status={item.status} />
            {item.status !== "completed" && item.status !== "failed" && (
              <Badge className="bg-zinc-50 capitalize text-zinc-500">{item.status}</Badge>
            )}
            {item.durationMs !== undefined && (
              <Badge className="bg-zinc-50 font-mono text-zinc-500">{item.durationMs}ms</Badge>
            )}
            {hasDetails && (
              <ChevronDown
                aria-hidden="true"
                className={cn("h-4 w-4 text-zinc-400 transition-transform", open && "rotate-180")}
              />
            )}
          </button>
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="border-t border-zinc-200 px-4">
            {hasArguments && (
              <section className="py-3">
                <h3 className="mb-1.5 text-[10px] font-semibold uppercase text-zinc-500">Arguments</h3>
                <pre className="code-scroll max-h-40 overflow-auto whitespace-pre-wrap break-all font-mono text-[11px] leading-5 text-zinc-700">
                  {JSON.stringify(item.arguments, null, 2)}
                </pre>
              </section>
            )}
            {item.evidence && (
              <div className="border-t border-zinc-100">
                <Evidence
                  evidence={item.evidence}
                  highlighted={highlighted}
                  onSelect={onSelectEvidence}
                />
              </div>
            )}
            {(item.output !== undefined || item.error) && (
              <section className="border-t border-zinc-100 py-3">
                <h3 className="mb-1.5 text-[10px] font-semibold uppercase text-zinc-500">
                  {item.error ? "Error" : "Output"}
                </h3>
                <pre
                  className={cn(
                    "code-scroll max-h-52 overflow-auto whitespace-pre-wrap break-all font-mono text-[11px] leading-5",
                    item.error ? "text-red-700" : "text-zinc-700",
                  )}
                >
                  {item.error ?? formattedValue(item.output)}
                </pre>
              </section>
            )}
          </div>
        </CollapsibleContent>
      </section>
    </Collapsible>
  );
}
