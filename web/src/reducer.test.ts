import { describe, expect, it } from "vitest";

import { initialState, opsReducer } from "./reducer";

describe("opsReducer runtime events", () => {
  it("builds one streaming assistant message from ordered deltas", () => {
    const started = opsReducer(initialState, {
      type: "event/received",
      payload: {
        seq: 1,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "turn_started",
        data: {},
      },
    });

    const firstDelta = opsReducer(started, {
      type: "event/received",
      payload: {
        seq: 2,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "assistant_delta",
        data: { delta: "Checking metrics" },
      },
    });

    const secondDelta = opsReducer(firstDelta, {
      type: "event/received",
      payload: {
        seq: 3,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "assistant_delta",
        data: { delta: " and logs." },
      },
    });

    expect(secondDelta.items).toHaveLength(1);
    expect(secondDelta.items[0]).toMatchObject({
      kind: "message",
      role: "assistant",
      content: "Checking metrics and logs.",
      streaming: true,
    });
  });

  it("ignores replayed events and completes a matching tool call", () => {
    const toolStarted = opsReducer(initialState, {
      type: "event/received",
      payload: {
        seq: 10,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "tool_started",
        data: {
          tool_call_id: "call-1",
          tool: "promql_query",
          arguments: { query: "rate(http_requests_total[5m])" },
        },
      },
    });

    const replayed = opsReducer(toolStarted, {
      type: "event/received",
      payload: {
        seq: 10,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "tool_started",
        data: { tool_call_id: "call-1", tool: "promql_query" },
      },
    });

    const completed = opsReducer(replayed, {
      type: "event/received",
      payload: {
        seq: 11,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "tool_completed",
        data: {
          tool_call_id: "call-1",
          tool: "promql_query",
          duration_ms: 21,
          output: { value: 0.31 },
          evidence: { source: "prometheus", query: "rate(...)" },
        },
      },
    });

    expect(completed.items).toHaveLength(1);
    expect(completed.items[0]).toMatchObject({
      kind: "tool",
      status: "completed",
      durationMs: 21,
    });
  });

  it("keeps assistant messages on both sides of a tool call in timeline order", () => {
    const firstMessage = opsReducer(initialState, {
      type: "event/received",
      payload: {
        seq: 1,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "assistant_completed",
        data: { content: "I'll inspect the metrics first." },
      },
    });
    const tool = opsReducer(firstMessage, {
      type: "event/received",
      payload: {
        seq: 2,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "tool_started",
        data: { tool_call_id: "call-1", tool: "promql_query" },
      },
    });
    const diagnosis = opsReducer(tool, {
      type: "event/received",
      payload: {
        seq: 3,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "assistant_completed",
        data: { content: "The 5xx rate is elevated." },
      },
    });

    expect(diagnosis.items.map((item) => item.kind)).toEqual(["message", "tool", "message"]);
    expect(diagnosis.items.at(-1)).toMatchObject({ content: "The 5xx rate is elevated." });
  });

  it("replays approval resolution into the pending approval item", () => {
    const required = opsReducer(initialState, {
      type: "event/received",
      payload: {
        seq: 1,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "approval_required",
        data: { approval_id: "approval-1", tool: "exec", arguments: { command: "uptime" } },
      },
    });
    const resolved = opsReducer(required, {
      type: "event/received",
      payload: {
        seq: 2,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "approval_resolved",
        data: { approval_id: "approval-1", approved: false },
      },
    });

    expect(resolved.items[0]).toMatchObject({ kind: "approval", status: "rejected" });
  });

  it("marks a tool result as failed when the runtime reports success false", () => {
    const started = opsReducer(initialState, {
      type: "event/received",
      payload: {
        seq: 1,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "tool_started",
        data: { call_id: "call-1", tool: "docker_logs", arguments: {} },
      },
    });
    const failed = opsReducer(started, {
      type: "event/received",
      payload: {
        seq: 2,
        threadId: "thread-1",
        turnId: "turn-1",
        type: "tool_completed",
        data: {
          call_id: "call-1",
          tool: "docker_logs",
          success: false,
          output: { error: "docker is unavailable" },
          evidence: { source: "docker" },
        },
      },
    });

    expect(failed.items[0]).toMatchObject({
      kind: "tool",
      status: "failed",
      error: "docker is unavailable",
    });
  });
});
