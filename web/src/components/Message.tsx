import { Bot, UserRound } from "lucide-react";

import type { MessageItem } from "../types";
import { cn } from "../lib/utils";

export function Message({ item }: { item: MessageItem }) {
  const assistant = item.role === "assistant";

  return (
    <article className={cn("flex gap-3 sm:gap-4", !assistant && "justify-end")}>
      {assistant && (
        <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-zinc-200 bg-white text-zinc-700 shadow-sm">
          <Bot aria-hidden="true" className="h-4 w-4" />
        </span>
      )}
      <div className={cn("min-w-0", assistant ? "max-w-[44rem] flex-1" : "max-w-[85%] sm:max-w-[75%]")}>
        <div className={cn("mb-1.5 flex items-center gap-2", !assistant && "justify-end")}>
          <span className="text-[11px] font-semibold uppercase text-zinc-500">
            {assistant ? "OpsCodex" : "You"}
          </span>
          {item.optimistic && <span className="text-[10px] text-zinc-400">Sending</span>}
        </div>
        <div
          className={cn(
            "whitespace-pre-wrap break-words text-[14px] leading-6 text-zinc-800",
            !assistant && "rounded-md bg-zinc-900 px-4 py-2.5 text-white",
          )}
        >
          {item.content}
          {item.streaming && (
            <span aria-label="Streaming" className="stream-cursor ml-0.5 inline-block h-4 w-0.5 translate-y-0.5 bg-emerald-500" />
          )}
        </div>
      </div>
      {!assistant && (
        <span className="mt-0.5 hidden h-8 w-8 shrink-0 items-center justify-center rounded-md bg-zinc-200 text-zinc-600 sm:flex">
          <UserRound aria-hidden="true" className="h-4 w-4" />
        </span>
      )}
    </article>
  );
}
