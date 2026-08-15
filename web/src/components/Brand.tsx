import { SquareActivity } from "lucide-react";

export function Brand({ compact = false }: { compact?: boolean }) {
  return (
    <div className="flex min-w-0 items-center gap-2.5">
      <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-zinc-950 text-white">
        <SquareActivity aria-hidden="true" className="h-[18px] w-[18px]" strokeWidth={1.8} />
      </span>
      {!compact && (
        <span className="truncate text-[15px] font-semibold text-zinc-950">OpsCodex</span>
      )}
    </div>
  );
}
