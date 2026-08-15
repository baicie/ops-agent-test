import {
  ChartNoAxesCombined,
  Check,
  ChevronDown,
  CircleX,
  Globe2,
  LoaderCircle,
  ScrollText,
  TerminalSquare,
  Wrench,
} from "lucide-react";
import { useState } from "react";

import type { ToolItem } from "../types";
import { cn } from "../lib/utils";
import { Evidence } from "./Evidence";
import { Badge } from "./ui/badge";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "./ui/collapsible";

function ToolIcon({ name }: { name: string }) {
  const className = "h-4 w-4";
  if (name.includes("prom")) return <ChartNoAxesCombined aria-hidden="true" className={className} />;
  if (name.includes("log")) return <ScrollText aria-hidden="true" className={className} />;
  if (name.includes("http")) return <Globe2 aria-hidden="true" className={className} />;
  if (name === "exec") return <TerminalSquare aria-hidden="true" className={className} />;
  return <Wrench aria-hidden="true" className={className} />;
}

function StatusIcon({ status }: Pick<ToolItem, "status">) {
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

export function ToolCall({ item }: { item: ToolItem }) {
  const [open, setOpen] = useState(false);
  const hasArguments = Object.keys(item.arguments).length > 0;
  const hasDetails = hasArguments || item.output !== undefined || item.evidence || item.error;

  return (
    <Collapsible open={open} onOpenChange={setOpen} disabled={!hasDetails}>
      <section className="overflow-hidden rounded-md border border-zinc-200 bg-white shadow-sm">
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
                <Evidence evidence={item.evidence} />
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
