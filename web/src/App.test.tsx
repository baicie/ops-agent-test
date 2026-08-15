import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type { OpsApiClient } from "./api";
import type { NormalizedEvent, ThreadDetail } from "./types";

function event(
  seq: number,
  type: NormalizedEvent["type"],
  data: Record<string, unknown>,
): NormalizedEvent {
  return {
    seq,
    type,
    data,
    threadId: "thread-1",
    turnId: "turn-1",
    timestamp: "2026-08-14T12:00:00Z",
  };
}

function createClient(detail?: ThreadDetail): OpsApiClient {
  return {
    listThreads: vi.fn().mockResolvedValue(
      detail
        ? [
            {
              id: detail.id,
              title: detail.title,
              status: detail.status,
              createdAt: detail.createdAt,
              updatedAt: detail.updatedAt,
            },
          ]
        : [],
    ),
    createThread: vi.fn().mockResolvedValue({ id: "thread-new" }),
    getThread: vi.fn().mockImplementation(async (id: string) =>
      detail ?? { id, title: null, status: "idle", events: [] },
    ),
    startTurn: vi.fn().mockResolvedValue({ turnId: "turn-new", status: "running" }),
    interruptTurn: vi.fn().mockResolvedValue(undefined),
    resolveApproval: vi.fn().mockResolvedValue(undefined),
    subscribe: vi.fn().mockReturnValue({ close: vi.fn() }),
  };
}

describe("OpsCodex app", () => {
  beforeEach(() => {
    window.HTMLElement.prototype.scrollIntoView = vi.fn();
  });

  it("replays a thread and expands tool evidence", async () => {
    const client = createClient({
      id: "thread-1",
      title: "Order service incident",
      status: "completed",
      events: [
        event(1, "user_message", { content: "Why is order-service failing?" }),
        event(2, "assistant_completed", { content: "The error rate is elevated." }),
        event(3, "tool_started", {
          tool_call_id: "call-1",
          tool: "promql_query",
          arguments: { query: "rate(http_requests_total[5m])" },
        }),
        event(4, "tool_completed", {
          tool_call_id: "call-1",
          tool: "promql_query",
          duration_ms: 21,
          output: { value: "31%" },
          evidence: { source: "prometheus", query: "rate(http_requests_total[5m])" },
        }),
      ],
    });

    render(<App client={client} />);

    expect(await screen.findByText("Why is order-service failing?")).toBeInTheDocument();
    expect(screen.getByText("The error rate is elevated.")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: /promql_query/i }));

    expect(screen.getByText("prometheus")).toBeInTheDocument();
    expect(screen.getAllByText(/rate\(http_requests_total\[5m\]\)/).length).toBeGreaterThan(0);
  });

  it("creates a thread, sends a turn, and exposes stop while it runs", async () => {
    const client = createClient();
    const user = userEvent.setup();
    render(<App client={client} />);

    await screen.findByText("No investigations yet");
    await user.click(screen.getByRole("button", { name: /new thread/i }));
    const composer = await screen.findByPlaceholderText("Ask about your infrastructure...");
    await user.type(composer, "order-service 怎么了？");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(client.startTurn).toHaveBeenCalledWith("thread-new", "order-service 怎么了？");
    expect(screen.getAllByText("order-service 怎么了？").length).toBeGreaterThan(0);

    const stop = await screen.findByRole("button", { name: /stop/i });
    await user.click(stop);
    await waitFor(() => expect(client.interruptTurn).toHaveBeenCalledWith("turn-new"));
  });

  it("allows a pending tool approval", async () => {
    const client = createClient({
      id: "thread-1",
      title: "Inspect service",
      status: "running",
      events: [
        event(1, "turn_started", {}),
        event(2, "approval_required", {
          approval_id: "approval-1",
          tool: "exec",
          arguments: { command: "journalctl -u order-service" },
        }),
      ],
    });

    render(<App client={client} />);
    const allow = await screen.findByRole("button", { name: "Allow" });
    await userEvent.click(allow);

    await waitFor(() => expect(client.resolveApproval).toHaveBeenCalledWith("approval-1", true));
    expect(screen.getByText("Allowed")).toBeInTheDocument();
  });
});
