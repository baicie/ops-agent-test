import type { IncidentContext } from "../types";
import { Badge } from "./ui/badge";

function entries(value?: Record<string, string>): [string, string][] {
  return value ? Object.entries(value) : [];
}

export function AlertContext({ context }: { context: IncidentContext }) {
  const labels = entries(context.labels);
  const annotations = entries(context.annotations);
  const window =
    context.starts_at || context.ends_at
      ? [context.starts_at, context.ends_at].filter(Boolean).join(" → ")
      : null;

  return (
    <aside className="mt-2 rounded-md border border-amber-200 bg-amber-50/70 px-3 py-2 text-xs text-amber-950">
      <p className="text-[10px] font-semibold uppercase tracking-wide text-amber-700">
        Alert context
      </p>
      <p className="mt-1 text-[10px] text-amber-800">
        Investigation hint only. It is not verified evidence.
      </p>
      <dl className="mt-2 grid gap-1">
        {context.service && (
          <div className="flex gap-2">
            <dt className="w-24 shrink-0 text-amber-700">Service</dt>
            <dd className="min-w-0 break-all font-medium">{context.service}</dd>
          </div>
        )}
        {context.environment && (
          <div className="flex gap-2">
            <dt className="w-24 shrink-0 text-amber-700">Environment</dt>
            <dd className="min-w-0 break-all">{context.environment}</dd>
          </div>
        )}
        {window && (
          <div className="flex gap-2">
            <dt className="w-24 shrink-0 text-amber-700">Window</dt>
            <dd className="min-w-0 break-all font-mono text-[11px]">{window}</dd>
          </div>
        )}
        {context.source?.kind && (
          <div className="flex gap-2">
            <dt className="w-24 shrink-0 text-amber-700">Source</dt>
            <dd className="min-w-0 break-all">{context.source.kind}</dd>
          </div>
        )}
      </dl>
      {labels.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {labels.map(([key, value]) => (
            <Badge key={key} className="bg-white text-amber-800 ring-amber-200">
              {key}={value}
            </Badge>
          ))}
        </div>
      )}
      {annotations.length > 0 && (
        <ul className="mt-2 space-y-1 text-[11px] text-amber-900">
          {annotations.map(([key, value]) => (
            <li key={key}>
              <span className="font-medium">{key}:</span> {value}
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
