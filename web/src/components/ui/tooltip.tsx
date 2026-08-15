import * as React from "react";
import * as TooltipPrimitive from "@radix-ui/react-tooltip";

import { cn } from "../../lib/utils";

export const TooltipProvider = TooltipPrimitive.Provider;

export function Tooltip({
  children,
  label,
}: {
  children: React.ReactElement;
  label: string;
}) {
  return (
    <TooltipPrimitive.Root delayDuration={400}>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          sideOffset={6}
          className={cn(
            "z-50 rounded bg-zinc-950 px-2 py-1 text-xs text-white shadow-md",
            "data-[state=delayed-open]:animate-in data-[state=closed]:animate-out",
          )}
        >
          {label}
          <TooltipPrimitive.Arrow className="fill-zinc-950" />
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}
