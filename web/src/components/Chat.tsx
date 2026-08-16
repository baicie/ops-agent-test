import { AlertCircle, Info, Plus, SearchCode, X } from "lucide-react";
import { useEffect, useRef } from "react";

import type { IncidentContext, OpsState, SkillSummary, TopologyGraph } from "../types";
import { Approval } from "./Approval";
import { Composer } from "./Composer";
import { Message } from "./Message";
import { ToolCall } from "./ToolCall";
import { Topology } from "./Topology";
import { Button } from "./ui/button";
import { ScrollArea } from "./ui/scroll-area";

interface ChatProps {
  state: OpsState;
  topology: TopologyGraph | null;
  skills: SkillSummary[];
  hasThread: boolean;
  stopping: boolean;
  resolvingApprovals: Set<string>;
  onNewThread: () => void;
  onSend: (input: string, incidentContext?: IncidentContext) => void;
  onStop: () => void;
  onApproval: (approvalId: string, approved: boolean) => void;
  onResume?: () => void;
  onFork?: () => void;
  onDismissError: () => void;
  onSelectEvidence: (evidenceId: string | null) => void;
}

function evidenceIdOf(item: OpsState["items"][number]): string | undefined {
  if (item.kind !== "tool") return undefined;
  const evidenceId = item.evidence?.evidence_id;
  return typeof evidenceId === "string" ? evidenceId : undefined;
}

export function Chat({
  state,
  topology,
  skills,
  hasThread,
  stopping,
  resolvingApprovals,
  onNewThread,
  onSend,
  onStop,
  onApproval,
  onResume,
  onFork,
  onDismissError,
  onSelectEvidence,
}: ChatProps) {
  const endRef = useRef<HTMLDivElement>(null);
  const evidencePresent = state.items.some(
    (item) => evidenceIdOf(item) === state.selectedEvidenceId,
  );

  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end", behavior: state.turnStatus === "running" ? "smooth" : "auto" });
  }, [state.items, state.turnStatus]);

  useEffect(() => {
    if (!state.selectedEvidenceId || !evidencePresent) return;
    document.getElementById(`evidence-${state.selectedEvidenceId}`)?.scrollIntoView({
      block: "center",
      behavior: "smooth",
    });
  }, [evidencePresent, state.selectedEvidenceId]);

  return (
    <div className="flex min-h-0 flex-1 flex-col bg-[#fafbfc]">
      {state.error && (
        <div role="alert" className="flex shrink-0 items-center gap-2 border-b border-red-200 bg-red-50 px-4 py-2 text-xs text-red-800">
          <AlertCircle aria-hidden="true" className="h-4 w-4 shrink-0" />
          <span className="min-w-0 flex-1 truncate">{state.error}</span>
          <Button variant="ghost" size="icon" className="h-7 w-7 text-red-700" aria-label="Dismiss error" onClick={onDismissError}>
            <X aria-hidden="true" className="h-4 w-4" />
          </Button>
        </div>
      )}
      {state.clientUpgradeHint && (
        <div role="status" className="flex shrink-0 items-center gap-2 border-b border-sky-200 bg-sky-50 px-4 py-2 text-xs text-sky-900">
          <Info aria-hidden="true" className="h-4 w-4 shrink-0" />
          <span className="min-w-0 flex-1 truncate">{state.clientUpgradeHint}</span>
        </div>
      )}
      {(state.turnStatus === "interrupted" || state.turnStatus === "needs_reconciliation") && (
        <div role="status" className="flex shrink-0 items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-950">
          <Info aria-hidden="true" className="h-4 w-4 shrink-0" />
          <span className="min-w-0 flex-1">
            {state.turnStatus === "needs_reconciliation"
              ? "A change operation finished in an unknown state. OpsCodex will not retry it automatically."
              : "This turn was interrupted. Resume continues from the last checkpoint and will not repeat completed tools."}
          </span>
          {state.turnStatus === "interrupted" && onResume && (
            <Button size="sm" variant="outline" className="h-7 px-2" onClick={onResume}>
              Resume
            </Button>
          )}
          {onFork && (
            <Button size="sm" variant="outline" className="h-7 px-2" onClick={onFork}>
              Fork
            </Button>
          )}
        </div>
      )}
      {state.selectedEvidenceId && !evidencePresent && (
        <div role="status" className="flex shrink-0 items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-900">
          <Info aria-hidden="true" className="h-4 w-4 shrink-0" />
          <span className="min-w-0 flex-1">
            Evidence {state.selectedEvidenceId} is not in this timeline. It may be unavailable or past retention.
          </span>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            aria-label="Clear selected evidence"
            onClick={() => onSelectEvidence(null)}
          >
            <X aria-hidden="true" className="h-4 w-4" />
          </Button>
        </div>
      )}
      <ScrollArea className="min-h-0 flex-1">
        {!hasThread ? (
          <div className="flex min-h-full items-center justify-center px-6 py-12">
            <div className="text-center">
              <span className="mx-auto flex h-12 w-12 items-center justify-center rounded-md border border-zinc-200 bg-white text-zinc-500 shadow-sm">
                <SearchCode aria-hidden="true" className="h-5 w-5" />
              </span>
              <h2 className="mt-4 text-sm font-semibold text-zinc-900">No investigation selected</h2>
              <Button size="sm" className="mt-4" onClick={onNewThread}>
                <Plus aria-hidden="true" className="h-4 w-4" />
                Create first thread
              </Button>
            </div>
          </div>
        ) : state.loadStatus === "loading" && state.items.length === 0 ? (
          <div aria-label="Loading investigation" className="mx-auto w-full max-w-3xl space-y-7 px-4 py-10 sm:px-8">
            <div className="h-20 w-3/4 animate-pulse rounded-md bg-zinc-200/70" />
            <div className="ml-11 h-24 animate-pulse rounded-md bg-zinc-200/50" />
          </div>
        ) : state.items.length === 0 ? (
          <div className="flex min-h-full items-center justify-center px-6 py-12">
            <div className="text-center text-zinc-500">
              <SearchCode aria-hidden="true" className="mx-auto h-6 w-6" />
              <h2 className="mt-3 text-sm font-semibold text-zinc-800">Ready for an incident</h2>
            </div>
          </div>
        ) : (
          <div className="mx-auto w-full max-w-3xl space-y-7 px-4 py-8 sm:px-8 sm:py-10">
            {skills.length > 0 && (
              <p className="text-[11px] text-zinc-500" data-testid="loaded-skills">
                Skills in context:{" "}
                {skills
                  .map((skill) => `${skill.id}@${skill.version} (${skill.bytes} B)`)
                  .join(" · ")}
              </p>
            )}
            {topology && (
              <Topology
                graph={topology}
                selectedEvidenceId={state.selectedEvidenceId}
                onSelectEvidence={onSelectEvidence}
              />
            )}
            {state.items.map((item) => {
              if (item.kind === "message") {
                return (
                  <Message
                    key={item.id}
                    item={item}
                    selectedEvidenceId={state.selectedEvidenceId}
                    onSelectEvidence={onSelectEvidence}
                  />
                );
              }
              if (item.kind === "tool") {
                return (
                  <div key={item.id} className="pl-0 sm:pl-12">
                    <ToolCall
                      item={item}
                      highlighted={evidenceIdOf(item) === state.selectedEvidenceId}
                      onSelectEvidence={onSelectEvidence}
                    />
                  </div>
                );
              }
              return (
                <div key={item.id} className="pl-0 sm:pl-12">
                  <Approval
                    item={item}
                    resolving={resolvingApprovals.has(item.approvalId)}
                    onDecision={onApproval}
                  />
                </div>
              );
            })}
            <div ref={endRef} aria-hidden="true" className="h-px" />
          </div>
        )}
      </ScrollArea>
      {hasThread && (
        <div className="flex items-center justify-end gap-2 border-t border-zinc-200 bg-white px-4 py-1.5">
          {onFork && state.turnStatus !== "running" && (
            <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={onFork}>
              Fork at seq {state.lastSeq}
            </Button>
          )}
        </div>
      )}
      {hasThread && (
        <Composer
          running={state.turnStatus === "running"}
          stopping={stopping}
          onSend={onSend}
          onStop={onStop}
        />
      )}
    </div>
  );
}
