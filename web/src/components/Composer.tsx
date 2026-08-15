import { ArrowUp, LoaderCircle, Square } from "lucide-react";
import { useRef, useState, type FormEvent, type KeyboardEvent } from "react";

import { Button } from "./ui/button";
import { Textarea } from "./ui/textarea";
import { Tooltip } from "./ui/tooltip";

interface ComposerProps {
  running: boolean;
  stopping: boolean;
  onSend: (input: string) => void;
  onStop: () => void;
}

export function Composer({ running, stopping, onSend, onStop }: ComposerProps) {
  const [value, setValue] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    const input = value.trim();
    if (!input || running) return;
    onSend(input);
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
      <div className="mx-auto flex w-full max-w-3xl items-end gap-2 rounded-md border border-zinc-300 bg-white p-2 shadow-sm focus-within:border-zinc-400 focus-within:ring-2 focus-within:ring-zinc-100">
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
    </form>
  );
}
