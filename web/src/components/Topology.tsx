import { GitFork } from "lucide-react";

import type { TopologyGraph } from "../types";
import { Badge } from "./ui/badge";

function evidenceLabel(evidenceId: string): string {
  return evidenceId.length > 12 ? `${evidenceId.slice(0, 8)}…` : evidenceId;
}

export function Topology({
  graph,
  selectedEvidenceId,
  onSelectEvidence,
}: {
  graph: TopologyGraph;
  selectedEvidenceId?: string | null;
  onSelectEvidence?: (evidenceId: string) => void;
}) {
  if (graph.nodes.length === 0) return null;

  return (
    <section
      aria-label="Service topology"
      className="rounded-md border border-zinc-200 bg-white p-4 shadow-sm"
    >
      <div className="mb-3 flex items-start gap-2">
        <GitFork aria-hidden="true" className="mt-0.5 h-4 w-4 text-zinc-500" />
        <div>
          <h2 className="text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
            Service topology
          </h2>
          <p className="mt-0.5 text-[11px] text-zinc-500">
            Evidence projection with TTL. This is not a CMDB.
          </p>
        </div>
      </div>
      <ul className="space-y-2">
        {graph.nodes.map((node) => (
          <li key={node.id} className="rounded-md bg-zinc-50 px-3 py-2">
            <div className="flex flex-wrap items-center gap-2">
              <Badge className="bg-zinc-100 text-zinc-700 ring-zinc-200">{node.kind}</Badge>
              <span className="font-mono text-[12px] text-zinc-800">{node.id}</span>
            </div>
            {node.evidenceIds.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1">
                {node.evidenceIds.map((evidenceId) => (
                  <button
                    key={`${node.id}-${evidenceId}`}
                    type="button"
                    aria-label={`Evidence ${evidenceId}`}
                    className={
                      selectedEvidenceId === evidenceId
                        ? "rounded bg-emerald-100 px-2 py-0.5 font-mono text-[10px] text-emerald-800"
                        : "rounded bg-white px-2 py-0.5 font-mono text-[10px] text-zinc-600 ring-1 ring-zinc-200"
                    }
                    onClick={() => onSelectEvidence?.(evidenceId)}
                  >
                    {evidenceLabel(evidenceId)}
                  </button>
                ))}
              </div>
            )}
          </li>
        ))}
      </ul>
      {graph.edges.length > 0 && (
        <ul className="mt-3 space-y-1.5 border-t border-zinc-100 pt-3">
          {graph.edges.map((edge) => (
            <li
              key={`${edge.from}:${edge.relation}:${edge.to}:${edge.source}`}
              className={edge.stale ? "opacity-60" : undefined}
            >
              <p className="font-mono text-[11px] text-zinc-700">
                {edge.from} <span className="text-zinc-400">{edge.relation}</span> {edge.to}
              </p>
              <div className="mt-1 flex flex-wrap items-center gap-1">
                <Badge className="bg-zinc-100 text-zinc-600 ring-zinc-200">{edge.source}</Badge>
                <Badge className="bg-sky-50 text-sky-700 ring-sky-200">{edge.confidence}</Badge>
                {edge.stale && (
                  <Badge className="bg-amber-50 text-amber-800 ring-amber-200">stale</Badge>
                )}
                {edge.evidenceIds.map((evidenceId) => (
                  <button
                    key={`${edge.from}-${edge.to}-${evidenceId}`}
                    type="button"
                    aria-label={`Evidence ${evidenceId}`}
                    className={
                      selectedEvidenceId === evidenceId
                        ? "rounded bg-emerald-100 px-2 py-0.5 font-mono text-[10px] text-emerald-800"
                        : "rounded bg-white px-2 py-0.5 font-mono text-[10px] text-zinc-600 ring-1 ring-zinc-200"
                    }
                    onClick={() => onSelectEvidence?.(evidenceId)}
                  >
                    {evidenceLabel(evidenceId)}
                  </button>
                ))}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
