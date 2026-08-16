import {
  BookOpen,
  Box,
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

const SECRET_KEY = /token|secret|password|kubeconfig|authorization|bearer|credential/i;

function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function asString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() !== "" ? value : undefined;
}

function publicArguments(value: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(value).filter(([key]) => !SECRET_KEY.test(key)));
}

function toolPayload(output: unknown): Record<string, unknown> {
  const record = asRecord(output);
  return { ...asRecord(record.content), ...record };
}

function k8sScope(item: ToolItem): string | null {
  if (!item.name.startsWith("k8s_")) return null;
  const payload = toolPayload(item.output);
  const cluster = asString(payload.cluster);
  const namespace = asString(item.arguments.namespace) ?? asString(payload.namespace);
  const kind = asString(item.arguments.kind) ?? asString(payload.kind);
  const name =
    asString(item.arguments.name) ?? asString(item.arguments.pod) ?? asString(payload.name);
  const parts = [cluster, namespace, kind, name].filter((part): part is string => Boolean(part));
  return parts.length > 0 ? parts.join(" · ") : null;
}

function runbookCitation(item: ToolItem): { title?: string; version?: number; hash?: string } | null {
  if (item.name !== "runbook_read") return null;
  const payload = toolPayload(item.output);
  const version = typeof payload.version === "number" ? payload.version : undefined;
  const hash = asString(payload.hash);
  const title = asString(payload.title);
  if (version === undefined && !hash) return null;
  return { title, version, hash };
}

function ToolIcon({ name }: { name: string }) {
  const className = "h-4 w-4";
  if (name.startsWith("k8s_")) return <Box aria-hidden="true" className={className} />;
  if (name.startsWith("runbook_")) return <BookOpen aria-hidden="true" className={className} />;
  if (name.includes("prom")) return <ChartNoAxesCombined aria-hidden="true" className={className} />;
  if (name.includes("log")) return <ScrollText aria-hidden="true" className={className} />;
  if (name.includes("trace") || name === "topology_query") {
    return <Waypoints aria-hidden="true" className={className} />;
  }
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
  const visibleArguments = publicArguments(item.arguments);
  const hasArguments = Object.keys(visibleArguments).length > 0;
  const hasDetails = hasArguments || item.output !== undefined || item.evidence || item.error;
  const scope = k8sScope(item);
  const runbook = runbookCitation(item);

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
            <span className="min-w-0 flex-1">
              <span className="block truncate font-mono text-xs font-semibold text-zinc-800">
                {item.name}
              </span>
              {scope && (
                <span className="mt-0.5 block truncate text-[11px] text-zinc-500">{scope}</span>
              )}
              {runbook && (
                <span className="mt-0.5 block truncate text-[11px] text-zinc-500">
                  {runbook.title ? `${runbook.title} ` : ""}
                  {runbook.version !== undefined ? `v${runbook.version}` : ""}
                  {runbook.hash ? ` ${runbook.hash.slice(0, 12)}` : ""}
                </span>
              )}
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
            {runbook && (
              <section className="py-3">
                <h3 className="mb-1.5 text-[10px] font-semibold uppercase text-zinc-500">Runbook</h3>
                <div className="flex flex-wrap gap-1">
                  {runbook.version !== undefined && (
                    <Badge className="bg-zinc-100 text-zinc-700 ring-zinc-200">v{runbook.version}</Badge>
                  )}
                  {runbook.hash && (
                    <Badge className="bg-zinc-100 font-mono text-zinc-600 ring-zinc-200">
                      {runbook.hash.slice(0, 16)}
                    </Badge>
                  )}
                </div>
              </section>
            )}
            {hasArguments && (
              <section className="py-3">
                <h3 className="mb-1.5 text-[10px] font-semibold uppercase text-zinc-500">Arguments</h3>
                <pre className="code-scroll max-h-40 overflow-auto whitespace-pre-wrap break-all font-mono text-[11px] leading-5 text-zinc-700">
                  {JSON.stringify(visibleArguments, null, 2)}
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
