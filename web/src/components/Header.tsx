import { AlertCircle, Menu, Radio, RotateCw, Square } from "lucide-react";

import type { ConnectionStatus, TurnStatus } from "../types";
import { Brand } from "./Brand";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";

interface HeaderProps {
  title: string;
  workspaceLabel?: string | null;
  connectionStatus: ConnectionStatus;
  turnStatus: TurnStatus;
  onOpenSidebar: () => void;
}

function RuntimeStatus({
  connectionStatus,
  turnStatus,
}: Pick<HeaderProps, "connectionStatus" | "turnStatus">) {
  if (connectionStatus === "reconnecting") {
    return (
      <Badge className="gap-1.5 bg-white text-zinc-600">
        <RotateCw aria-hidden="true" className="h-3 w-3 animate-spin" />
        Reconnecting
      </Badge>
    );
  }
  if (turnStatus === "running") {
    return (
      <Badge className="gap-1.5 bg-sky-50 text-sky-700 ring-sky-200">
        <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-sky-500" />
        Investigating
      </Badge>
    );
  }
  if (turnStatus === "failed") {
    return (
      <Badge className="gap-1.5 bg-red-50 text-red-700 ring-red-200">
        <AlertCircle aria-hidden="true" className="h-3 w-3" />
        Needs attention
      </Badge>
    );
  }
  if (turnStatus === "cancelled") {
    return (
      <Badge className="gap-1.5 bg-zinc-100 text-zinc-600">
        <Square aria-hidden="true" className="h-2.5 w-2.5 fill-current" />
        Stopped
      </Badge>
    );
  }
  if (turnStatus === "interrupted") {
    return (
      <Badge className="gap-1.5 bg-amber-50 text-amber-800 ring-amber-200">
        <RotateCw aria-hidden="true" className="h-3 w-3" />
        Interrupted
      </Badge>
    );
  }
  if (turnStatus === "needs_reconciliation") {
    return (
      <Badge className="gap-1.5 bg-red-50 text-red-700 ring-red-200">
        <AlertCircle aria-hidden="true" className="h-3 w-3" />
        Needs reconciliation
      </Badge>
    );
  }
  return (
    <Badge className="gap-1.5 bg-emerald-50 text-emerald-700 ring-emerald-200">
      <Radio aria-hidden="true" className="h-3 w-3" />
      Ready
    </Badge>
  );
}

export function Header({ title, workspaceLabel, connectionStatus, turnStatus, onOpenSidebar }: HeaderProps) {
  return (
    <header className="flex h-16 shrink-0 items-center border-b border-zinc-200 bg-white px-3 sm:px-5">
      <Button
        variant="ghost"
        size="icon"
        aria-label="Open navigation"
        className="mr-2 md:hidden"
        onClick={onOpenSidebar}
      >
        <Menu aria-hidden="true" className="h-5 w-5" />
      </Button>
      <div className="mr-3 md:hidden">
        <Brand compact />
      </div>
      <div className="min-w-0 flex-1">
        <h1 className="truncate text-sm font-semibold text-zinc-950">
          <span className="sm:hidden">OpsCodex</span>
          <span className="hidden sm:inline">{title}</span>
        </h1>
        <p className="mt-0.5 truncate text-[11px] text-zinc-500">
          <span className="sm:hidden">{title}</span>
          <span className="hidden sm:inline">
            {workspaceLabel ? `Workspace ${workspaceLabel}` : "Runtime investigation"}
          </span>
        </p>
      </div>
      {workspaceLabel && (
        <Badge className="mr-2 max-w-[9rem] truncate bg-zinc-50 text-zinc-600" title="Thread workspace is fixed">
          {workspaceLabel}
        </Badge>
      )}
      <div className="ml-2 shrink-0">
        <RuntimeStatus connectionStatus={connectionStatus} turnStatus={turnStatus} />
      </div>
    </header>
  );
}
