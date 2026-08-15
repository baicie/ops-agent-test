import { Check, ShieldAlert, X } from "lucide-react";

import type { ApprovalItem } from "../types";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";

interface ApprovalProps {
  item: ApprovalItem;
  resolving: boolean;
  onDecision: (approvalId: string, approved: boolean) => void;
}

function approvalCommand(item: ApprovalItem): string {
  const command = item.arguments.command;
  if (typeof command === "string") return command;
  return JSON.stringify(item.arguments, null, 2);
}

export function Approval({ item, resolving, onDecision }: ApprovalProps) {
  const pending = item.status === "pending";

  return (
    <section className="rounded-md border border-amber-200 bg-white shadow-sm" aria-label="Tool approval">
      <div className="flex items-start gap-3 border-b border-amber-100 bg-amber-50/70 px-4 py-3">
        <ShieldAlert aria-hidden="true" className="mt-0.5 h-5 w-5 shrink-0 text-amber-700" />
        <div className="min-w-0 flex-1">
          <h2 className="text-sm font-semibold text-zinc-900">Approval required</h2>
          <p className="mt-0.5 text-xs text-zinc-600">
            OpsCodex requested <code className="font-mono font-semibold">{item.tool}</code>
          </p>
        </div>
        {!pending && (
          <Badge
            className={
              item.status === "approved"
                ? "gap-1 bg-emerald-50 text-emerald-700 ring-emerald-200"
                : "gap-1 bg-zinc-100 text-zinc-600"
            }
          >
            {item.status === "approved" ? <Check className="h-3 w-3" /> : <X className="h-3 w-3" />}
            {item.status === "approved" ? "Allowed" : "Rejected"}
          </Badge>
        )}
      </div>
      <div className="px-4 py-3">
        <pre className="code-scroll max-h-36 overflow-auto whitespace-pre-wrap break-all rounded-md bg-zinc-950 px-3 py-2.5 font-mono text-xs leading-5 text-zinc-100">
          {approvalCommand(item)}
        </pre>
        {pending && (
          <div className="mt-3 flex justify-end gap-2">
            <Button
              variant="secondary"
              size="sm"
              disabled={resolving}
              onClick={() => onDecision(item.approvalId, false)}
            >
              <X aria-hidden="true" className="h-3.5 w-3.5" />
              Reject
            </Button>
            <Button
              size="sm"
              disabled={resolving}
              onClick={() => onDecision(item.approvalId, true)}
            >
              <Check aria-hidden="true" className="h-3.5 w-3.5" />
              Allow
            </Button>
          </div>
        )}
      </div>
    </section>
  );
}
