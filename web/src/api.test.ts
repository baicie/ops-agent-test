import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createApiClient,
  normalizeEventEnvelope,
  type EventSourceLike,
} from "./api";

function jsonResponse(body: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
    ...init,
  });
}

describe("OpsCodex API client", () => {
  const fetchMock = vi.fn<typeof fetch>();

  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("loads thread summaries from the REST response envelope", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({
        threads: [
          {
            id: "thread-1",
            title: "Checkout latency",
            status: "running",
            created_at: "2026-08-14T12:00:00Z",
            updated_at: "2026-08-14T12:02:00Z",
          },
        ],
      }),
    );

    const api = createApiClient("http://localhost:3000");
    const threads = await api.listThreads();

    expect(threads[0]).toEqual({
      id: "thread-1",
      title: "Checkout latency",
      status: "running",
      createdAt: "2026-08-14T12:00:00Z",
      updatedAt: "2026-08-14T12:02:00Z",
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:3000/api/threads",
      expect.objectContaining({ headers: { accept: "application/json" } }),
    );
  });

  it("normalizes the nested runtime event emitted by the server", () => {
    const event = normalizeEventEnvelope(
      {
        seq: 123,
        thread_id: "thread-1",
        turn_id: "turn-1",
        timestamp: "2026-08-14T12:00:00Z",
        event: {
          type: "tool_completed",
          tool_call_id: "call-1",
          tool: "promql_query",
          output: { value: 0.31 },
        },
      },
      "tool_completed",
      "123",
    );

    expect(event).toEqual({
      seq: 123,
      threadId: "thread-1",
      turnId: "turn-1",
      timestamp: "2026-08-14T12:00:00Z",
      type: "tool_completed",
      data: {
        tool_call_id: "call-1",
        tool: "promql_query",
        output: { value: 0.31 },
      },
    });
  });

  it("posts the documented approval decision payload", async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ status: "accepted" }));

    const api = createApiClient("");
    await api.resolveApproval("approval-1", true);

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/approvals/approval-1",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ approved: true }),
      }),
    );
  });

  it("normalizes approval resolution replay events", () => {
    const event = normalizeEventEnvelope({
      seq: 9,
      thread_id: "thread-1",
      turn_id: "turn-1",
      type: "approval_resolved",
      approval_id: "approval-1",
      approved: true,
    });

    expect(event).toMatchObject({
      seq: 9,
      type: "approval_resolved",
      data: { approval_id: "approval-1", approved: true },
    });
  });

  it("surfaces nested API error messages", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse(
        { error: { code: "conflict", message: "thread already has an active turn" } },
        { status: 409 },
      ),
    );

    const api = createApiClient("");
    await expect(api.startTurn("thread-1", "again")).rejects.toThrow(
      "thread already has an active turn",
    );
  });

  it("subscribes with the replay cursor and forwards named SSE events", () => {
    class FakeEventSource implements EventSourceLike {
      static instance: FakeEventSource;
      readonly url: string;
      onopen: ((event: Event) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;
      listeners = new Map<string, (event: MessageEvent<string>) => void>();
      closed = false;

      constructor(url: string | URL) {
        this.url = String(url);
        FakeEventSource.instance = this;
      }

      addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
        this.listeners.set(type, listener as (event: MessageEvent<string>) => void);
      }

      close() {
        this.closed = true;
      }

      emit(type: string, data: unknown) {
        this.listeners.get(type)?.(
          new MessageEvent(type, { data: JSON.stringify(data), lastEventId: "8" }),
        );
      }
    }

    const onEvent = vi.fn();
    const api = createApiClient("http://localhost:3000", FakeEventSource);
    const subscription = api.subscribe("thread/one", 7, { onEvent });

    expect(FakeEventSource.instance.url).toBe(
      "http://localhost:3000/api/threads/thread%2Fone/events?after=7",
    );

    FakeEventSource.instance.emit("assistant_delta", {
      seq: 8,
      thread_id: "thread/one",
      turn_id: "turn-1",
      event: { type: "assistant_delta", delta: "Investigating" },
    });

    expect(onEvent).toHaveBeenCalledWith(
      expect.objectContaining({ seq: 8, type: "assistant_delta", data: { delta: "Investigating" } }),
    );

    subscription.close();
    expect(FakeEventSource.instance.closed).toBe(true);
  });
});
