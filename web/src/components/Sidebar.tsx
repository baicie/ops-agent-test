import { MessageSquareText, Plus, Radio } from "lucide-react";

import type { ThreadSummary, WorkspaceSummary } from "../types";
import { cn } from "../lib/utils";
import { Brand } from "./Brand";
import { Button } from "./ui/button";
import { ScrollArea } from "./ui/scroll-area";
import { Separator } from "./ui/separator";

interface SidebarProps {
  threads: ThreadSummary[];
  workspaces: WorkspaceSummary[];
  selectedWorkspaceId: string;
  activeThreadId: string | null;
  loading: boolean;
  onSelect: (threadId: string) => void;
  onWorkspaceChange: (workspaceId: string) => void;
  onNew: () => void;
}

function threadTitle(thread: ThreadSummary): string {
  return thread.title?.trim() || `Incident ${thread.id.slice(0, 8)}`;
}

function formattedTime(value?: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return null;
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
  }).format(date);
}

function statusColor(status: string) {
  if (status === "running") return "bg-sky-500";
  if (status === "failed") return "bg-red-500";
  if (status === "cancelled") return "bg-zinc-400";
  return "bg-emerald-500";
}

export function Sidebar({
  threads,
  workspaces,
  selectedWorkspaceId,
  activeThreadId,
  loading,
  onSelect,
  onWorkspaceChange,
  onNew,
}: SidebarProps) {
  return (
    <div className="flex h-full min-h-0 flex-col bg-[#f4f5f7]">
      <div className="flex h-16 shrink-0 items-center px-5">
        <Brand />
      </div>
      <div className="space-y-2 px-3 pb-3">
        <label className="block px-1 text-[11px] font-semibold uppercase text-zinc-500">
          Workspace
          <select
            aria-label="Workspace"
            className="mt-1 h-9 w-full rounded-md border border-zinc-200 bg-white px-2 text-[13px] text-zinc-800 shadow-sm"
            value={selectedWorkspaceId}
            onChange={(event) => {
              const next = event.target.value;
              if (next !== selectedWorkspaceId) onWorkspaceChange(next);
            }}
          >
            {(workspaces.length > 0
              ? workspaces
              : [{
                  id: selectedWorkspaceId || "default",
                  displayName: selectedWorkspaceId || "default",
                  environment: "local",
                  connectors: [],
                }]
            ).map((workspace) => (
              <option key={workspace.id} value={workspace.id}>
                {workspace.displayName}
              </option>
            ))}
          </select>
        </label>
        <p className="px-1 text-[11px] leading-4 text-zinc-500">
          New threads bind to this workspace and cannot switch later.
        </p>
        <Button className="w-full justify-start" onClick={onNew}>
          <Plus aria-hidden="true" className="h-4 w-4" />
          New thread
        </Button>
      </div>
      <Separator />
      <div className="px-4 pb-2 pt-4 text-[11px] font-semibold uppercase text-zinc-500">
        Investigations
      </div>
      <ScrollArea className="min-h-0 flex-1 px-2">
        <div className="space-y-1 pb-4">
          {loading && threads.length === 0 && (
            <div aria-label="Loading threads" className="space-y-2 px-2 py-1">
              <div className="h-12 animate-pulse rounded-md bg-zinc-200/70" />
              <div className="h-12 animate-pulse rounded-md bg-zinc-200/50" />
            </div>
          )}
          {!loading && threads.length === 0 && (
            <div className="flex items-center gap-2 px-3 py-3 text-xs text-zinc-500">
              <MessageSquareText aria-hidden="true" className="h-4 w-4" />
              No investigations yet
            </div>
          )}
          {threads.map((thread) => {
            const active = activeThreadId === thread.id;
            const when = formattedTime(thread.updatedAt ?? thread.createdAt);
            return (
              <button
                key={thread.id}
                type="button"
                aria-current={active ? "page" : undefined}
                onClick={() => onSelect(thread.id)}
                className={cn(
                  "group flex min-h-12 w-full items-center gap-3 rounded-md px-3 py-2 text-left transition-colors",
                  active ? "bg-white text-zinc-950 shadow-sm ring-1 ring-zinc-200" : "text-zinc-600 hover:bg-zinc-200/60 hover:text-zinc-950",
                )}
              >
                <span className={cn("h-2 w-2 shrink-0 rounded-full", statusColor(thread.status))} />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-medium">{threadTitle(thread)}</span>
                  <span className="mt-0.5 block truncate text-[11px] text-zinc-500">
                    {thread.workspaceId || selectedWorkspaceId}
                  </span>
                </span>
                {when && <span className="shrink-0 text-[10px] text-zinc-400">{when}</span>}
              </button>
            );
          })}
        </div>
      </ScrollArea>
      <Separator />
      <div className="flex h-12 shrink-0 items-center gap-2 px-5 text-[11px] text-zinc-500">
        <Radio aria-hidden="true" className="h-3.5 w-3.5 text-emerald-600" />
        Local runtime
      </div>
    </div>
  );
}
