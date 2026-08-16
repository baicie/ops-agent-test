import { ArrowUp, ChevronDown, LoaderCircle, Square } from "lucide-react";
import { useRef, useState, type FormEvent, type KeyboardEvent } from "react";

import type { IncidentContext } from "../types";
import { Button } from "./ui/button";
import { Textarea } from "./ui/textarea";
import { Tooltip } from "./ui/tooltip";

interface ComposerProps {
  running: boolean;
  stopping: boolean;
  onSend: (input: string, incidentContext?: IncidentContext) => void;
  onStop: () => void;
}

function parsePairs(value: string): Record<string, string> | undefined {
  const pairs: Record<string, string> = {};
  for (const part of value.split(/[,;\n]/)) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const index = trimmed.indexOf("=");
    if (index <= 0) continue;
    const key = trimmed.slice(0, index).trim();
    const item = trimmed.slice(index + 1).trim();
    if (key) pairs[key] = item;
  }
  return Object.keys(pairs).length > 0 ? pairs : undefined;
}

function buildIncidentContext(fields: {
  service: string;
  environment: string;
  startsAt: string;
  endsAt: string;
  labels: string;
}): IncidentContext | undefined {
  const labels = parsePairs(fields.labels);
  const context: IncidentContext = {};
  if (fields.service.trim()) context.service = fields.service.trim();
  if (fields.environment.trim()) context.environment = fields.environment.trim();
  if (fields.startsAt.trim()) context.starts_at = fields.startsAt.trim();
  if (fields.endsAt.trim()) context.ends_at = fields.endsAt.trim();
  if (labels) context.labels = labels;
  return Object.keys(context).length > 0 ? context : undefined;
}

export function Composer({ running, stopping, onSend, onStop }: ComposerProps) {
  const [value, setValue] = useState("");
  const [showAlert, setShowAlert] = useState(false);
  const [service, setService] = useState("");
  const [environment, setEnvironment] = useState("");
  const [startsAt, setStartsAt] = useState("");
  const [endsAt, setEndsAt] = useState("");
  const [labels, setLabels] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    const input = value.trim();
    if (!input || running) return;
    onSend(input, buildIncidentContext({ service, environment, startsAt, endsAt, labels }));
    setValue("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      submit();
    }
  };

  return (
    <form onSubmit={submit} className="border-t border-zinc-200 bg-white px-3 py-3 sm:px-6 sm:py-4">
      <div className="mx-auto w-full max-w-3xl">
        <div className="rounded-md border border-zinc-300 bg-white p-2 shadow-sm focus-within:border-zinc-400 focus-within:ring-2 focus-within:ring-zinc-100">
          <div className="flex items-end gap-2">
            <Textarea
              ref={textareaRef}
              value={value}
              rows={1}
              disabled={running}
              placeholder={running ? "Investigation in progress..." : "Ask about your infrastructure..."}
              aria-label="Message"
              className="max-h-44 min-h-10 px-2 py-2"
              onKeyDown={handleKeyDown}
              onChange={(event) => {
                setValue(event.target.value);
                event.target.style.height = "auto";
                event.target.style.height = `${Math.min(event.target.scrollHeight, 176)}px`;
              }}
            />
            {running ? (
              <Button
                type="button"
                variant="outline"
                className="h-10"
                disabled={stopping}
                aria-label={stopping ? "Stopping" : "Stop investigation"}
                onClick={onStop}
              >
                {stopping ? (
                  <LoaderCircle aria-hidden="true" className="h-4 w-4 animate-spin" />
                ) : (
                  <Square aria-hidden="true" className="h-3.5 w-3.5 fill-current" />
                )}
                {stopping ? "Stopping" : "Stop"}
              </Button>
            ) : (
              <Tooltip label="Send message">
                <Button
                  type="submit"
                  size="icon"
                  aria-label="Send message"
                  disabled={!value.trim()}
                  className="h-10 w-10"
                >
                  <ArrowUp aria-hidden="true" className="h-4 w-4" />
                </Button>
              </Tooltip>
            )}
          </div>
          <button
            type="button"
            className="mt-1 flex items-center gap-1 px-2 py-1 text-[11px] font-medium text-zinc-500 hover:text-zinc-800"
            aria-expanded={showAlert}
            onClick={() => setShowAlert((open) => !open)}
          >
            Alert context
            <ChevronDown aria-hidden="true" className={`h-3.5 w-3.5 ${showAlert ? "rotate-180" : ""}`} />
          </button>
          {showAlert && (
            <div className="mt-1 grid gap-2 px-2 pb-2 sm:grid-cols-2">
              <label className="grid gap-1 text-[11px] text-zinc-500">
                Service
                <input
                  value={service}
                  onChange={(event) => setService(event.target.value)}
                  disabled={running}
                  placeholder="order-service"
                  aria-label="Alert service"
                  className="h-8 rounded border border-zinc-200 px-2 text-sm text-zinc-800 outline-none focus:border-zinc-400"
                />
              </label>
              <label className="grid gap-1 text-[11px] text-zinc-500">
                Environment
                <input
                  value={environment}
                  onChange={(event) => setEnvironment(event.target.value)}
                  disabled={running}
                  placeholder="staging"
                  aria-label="Alert environment"
                  className="h-8 rounded border border-zinc-200 px-2 text-sm text-zinc-800 outline-none focus:border-zinc-400"
                />
              </label>
              <label className="grid gap-1 text-[11px] text-zinc-500">
                Starts at
                <input
                  value={startsAt}
                  onChange={(event) => setStartsAt(event.target.value)}
                  disabled={running}
                  placeholder="2026-08-16T00:00:00Z"
                  aria-label="Alert start time"
                  className="h-8 rounded border border-zinc-200 px-2 font-mono text-sm text-zinc-800 outline-none focus:border-zinc-400"
                />
              </label>
              <label className="grid gap-1 text-[11px] text-zinc-500">
                Ends at
                <input
                  value={endsAt}
                  onChange={(event) => setEndsAt(event.target.value)}
                  disabled={running}
                  placeholder="2026-08-16T00:15:00Z"
                  aria-label="Alert end time"
                  className="h-8 rounded border border-zinc-200 px-2 font-mono text-sm text-zinc-800 outline-none focus:border-zinc-400"
                />
              </label>
              <label className="grid gap-1 text-[11px] text-zinc-500 sm:col-span-2">
                Labels
                <input
                  value={labels}
                  onChange={(event) => setLabels(event.target.value)}
                  disabled={running}
                  placeholder="severity=critical, team=checkout"
                  aria-label="Alert labels"
                  className="h-8 rounded border border-zinc-200 px-2 font-mono text-sm text-zinc-800 outline-none focus:border-zinc-400"
                />
              </label>
            </div>
          )}
        </div>
      </div>
    </form>
  );
}
