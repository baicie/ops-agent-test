import { Check, ShieldAlert, X } from "lucide-react";

import type { ActionItem } from "../types";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";

interface ActionReviewProps {
  item: ActionItem;
  resolving: boolean;
  onApprove: (actionId: string, requestHash: string, approved: boolean) => void;
  onExecute: (actionId: string) => void;
}

function field(label: string, value: unknown) {
  const text =
    value == null
      ? "—"
      : typeof value === "string"
        ? value
        : JSON.stringify(value, null, 2);
  return (
    <div key={label}>
      <h3 className="text-[10px] font-semibold uppercase tracking-wide text-zinc-500">{label}</h3>
      <pre className="mt-1 whitespace-pre-wrap break-all font-mono text-[11px] leading-5 text-zinc-800">
        {text || "—"}
      </pre>
    </div>
  );
}

export function ActionReview({ item, resolving, onApprove, onExecute }: ActionReviewProps) {
  const pending = item.status === "awaiting_approval";
  const authorized = item.status === "authorized";
  const reconciling = item.status === "needs_reconciliation";
  const review = item.review;

  return (
    <section className="rounded-md border border-amber-200 bg-white shadow-sm" aria-label="Action review">
      <div className="flex items-start gap-3 border-b border-amber-100 bg-amber-50/70 px-4 py-3">
        <ShieldAlert aria-hidden="true" className="mt-0.5 h-5 w-5 shrink-0 text-amber-700" />
        <div className="min-w-0 flex-1">
          <h2 className="text-sm font-semibold text-zinc-900">Remediation action</h2>
          <p className="mt-0.5 text-xs text-zinc-600">
            <code className="font-mono font-semibold">{item.tool}</code> · {item.status.replaceAll("_", " ")}
          </p>
        </div>
        <Badge className="bg-zinc-100 text-zinc-700 ring-zinc-200">{item.status}</Badge>
      </div>
      <div className="space-y-3 px-4 py-3">
        {field("Target", review.target)}
        {field("Effect", review.effect)}
        {field("Parameters", review.parameters)}
        {field("Preconditions", review.preconditions)}
        {field("Blast radius", review.blast_radius)}
        {field("Expires", review.expires_at)}
        {field("Dry-run", review.dry_run)}
        {field("Verification", review.verification)}
        {field("Rollback", review.rollback ?? "not automatic; a new action must be proposed")}
        {field("Request hash", item.requestHash)}
        {reconciling && (
          <p className="rounded-md bg-red-50 px-3 py-2 text-xs text-red-800">
            This change finished in an unknown state. OpsCodex will not retry it. Inspect the target
            and continue from a new diagnosis.
          </p>
        )}
        {pending && (
          <div className="flex justify-end gap-2">
            <Button
              variant="secondary"
              size="sm"
              disabled={resolving}
              onClick={() => onApprove(item.actionId, item.requestHash, false)}
            >
              <X aria-hidden="true" className="h-3.5 w-3.5" />
              Reject
            </Button>
            <Button
              size="sm"
              disabled={resolving}
              onClick={() => onApprove(item.actionId, item.requestHash, true)}
            >
              <Check aria-hidden="true" className="h-3.5 w-3.5" />
              Approve this action
            </Button>
          </div>
        )}
        {authorized && (
          <div className="flex justify-end">
            <Button size="sm" disabled={resolving} onClick={() => onExecute(item.actionId)}>
              Execute approved action
            </Button>
          </div>
        )}
      </div>
    </section>
  );
}
