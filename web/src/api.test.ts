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
      workspaceId: null,
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:3000/api/v1/threads",
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
      "/api/v1/approvals/approval-1",
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
      "http://localhost:3000/api/v1/threads/thread%2Fone/events?after=7",
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

  it("keeps unknown event types as a compatible unknown envelope", () => {
    const event = normalizeEventEnvelope({
      seq: 40,
      thread_id: "thread-1",
      turn_id: "turn-1",
      event: { type: "future_checkpoint", checkpoint_id: "cp-1" },
    });

    expect(event).toMatchObject({
      seq: 40,
      type: "unknown",
      data: { checkpoint_id: "cp-1", _event_type: "future_checkpoint" },
    });
  });

  it("posts incident context with a turn", async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ turn_id: "turn-1", status: "running" }));

    const api = createApiClient("");
    await api.startTurn("thread-1", "Investigate 5xx", {
      service: "order-service",
      environment: "staging",
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/threads/thread-1/turns",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          input: "Investigate 5xx",
          incident_context: { service: "order-service", environment: "staging" },
        }),
      }),
    );
  });

  it("lists workspaces and creates a thread in the selected workspace", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({
          workspaces: [
            {
              id: "staging",
              display_name: "Staging",
              environment: "staging",
              connectors: ["kubernetes"],
            },
          ],
        }),
      )
      .mockResolvedValueOnce(jsonResponse({ id: "thread-1", workspace_id: "staging" }, { status: 201 }));

    const api = createApiClient("");
    const workspaces = await api.listWorkspaces();
    expect(workspaces[0]).toEqual({
      id: "staging",
      displayName: "Staging",
      environment: "staging",
      connectors: ["kubernetes"],
    });

    const created = await api.createThread("staging");
    expect(created).toEqual({ id: "thread-1", workspaceId: "staging" });
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/v1/threads",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ workspace_id: "staging" }),
      }),
    );
  });

  it("loads a thread topology projection", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({
        nodes: [{ id: "service:order-service", kind: "service", workspace_id: "staging", evidence_ids: ["ev-1"] }],
        edges: [
          {
            from: "service:checkout-ui",
            to: "service:order-service",
            relation: "calls",
            confidence: "high",
            source: "trace",
            evidence_ids: ["ev-1"],
            stale: false,
          },
        ],
      }),
    );

    const api = createApiClient("");
    const graph = await api.getTopology("thread-1", "staging");
    expect(graph.nodes[0]?.evidenceIds).toEqual(["ev-1"]);
    expect(graph.edges[0]?.source).toBe("trace");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/threads/thread-1/topology?workspace_id=staging",
      expect.objectContaining({ headers: { accept: "application/json" } }),
    );
  });
});
