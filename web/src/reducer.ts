import type {
  ApprovalItem,
  Diagnosis,
  EvidenceMeta,
  IncidentContext,
  MessageItem,
  NormalizedEvent,
  OpsState,
  ThreadDetail,
  ThreadSummary,
  TimelineItem,
  ToolItem,
} from "./types";

export const initialState: OpsState = {
  threads: [],
  activeThreadId: null,
  activeTurnId: null,
  items: [],
  loadStatus: "idle",
  connectionStatus: "idle",
  turnStatus: "idle",
  lastSeq: 0,
  error: null,
  selectedEvidenceId: null,
  clientUpgradeHint: null,
  sidebarOpen: false,
};

export type OpsAction =
  | { type: "threads/loading" }
  | { type: "threads/loaded"; payload: ThreadSummary[] }
  | { type: "threads/failed"; payload: string }
  | { type: "thread/select"; payload: string }
  | { type: "thread/created"; payload: ThreadSummary }
  | { type: "thread/loading" }
  | { type: "thread/loaded"; payload: ThreadDetail }
  | {
      type: "message/optimistic";
      payload: { id: string; content: string; incidentContext?: IncidentContext | null };
    }
  | { type: "turn/started"; payload: { turnId: string } }
  | { type: "event/received"; payload: NormalizedEvent }
  | { type: "connection/changed"; payload: OpsState["connectionStatus"] }
  | { type: "approval/resolved"; payload: { approvalId: string; approved: boolean } }
  | { type: "error/set"; payload: string }
  | { type: "error/clear" }
  | { type: "evidence/select"; payload: string | null }
  | { type: "sidebar/set"; payload: boolean };

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function booleanValue(value: unknown): boolean | undefined {
  return typeof value === "boolean" ? value : undefined;
}

function recordValue(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function asDiagnosis(value: unknown): Diagnosis | null {
  const record = recordValue(value);
  const summary = stringValue(record.summary);
  if (!summary && !Array.isArray(record.claims)) return null;
  const claims = Array.isArray(record.claims)
    ? record.claims.map((claim) => {
        const item = recordValue(claim);
        return {
          claim_id: stringValue(item.claim_id),
          kind: stringValue(item.kind) ?? "inferred",
          statement: stringValue(item.statement) ?? "",
          evidence_ids: Array.isArray(item.evidence_ids)
            ? item.evidence_ids.filter((id): id is string => typeof id === "string")
            : [],
          confidence: stringValue(item.confidence),
        };
      })
    : [];
  return {
    summary: summary ?? "",
    claims,
    recommended_actions: Array.isArray(record.recommended_actions)
      ? record.recommended_actions.filter((item): item is string => typeof item === "string")
      : [],
    limitations: Array.isArray(record.limitations)
      ? record.limitations.filter((item): item is string => typeof item === "string")
      : [],
  };
}

function asIncidentContext(value: unknown): IncidentContext | null {
  const record = recordValue(value);
  if (Object.keys(record).length === 0) return null;
  return record as IncidentContext;
}

function eventText(data: Record<string, unknown>): string {
  return (
    stringValue(data.delta) ??
    stringValue(data.content) ??
    stringValue(data.text) ??
    stringValue(recordValue(data.message).content) ??
    ""
  );
}

function toolCallId(event: NormalizedEvent): string {
  return (
    stringValue(event.data.tool_call_id) ??
    stringValue(event.data.call_id) ??
    stringValue(event.data.id) ??
    `tool:${event.turnId ?? "unknown"}:${event.seq}`
  );
}

function toolName(data: Record<string, unknown>): string {
  return stringValue(data.tool) ?? stringValue(data.name) ?? "tool";
}

function lastStreamingAssistantIndex(items: TimelineItem[], turnId: string | null): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (
      item.kind === "message" &&
      item.role === "assistant" &&
      item.streaming &&
      item.turnId === turnId
    ) {
      return index;
    }
  }
  return -1;
}

function withThreadStatus(state: OpsState, status: string): OpsState {
  if (!state.activeThreadId) return state;
  return {
    ...state,
    threads: state.threads.map((thread) =>
      thread.id === state.activeThreadId ? { ...thread, status } : thread,
    ),
  };
}

function applyRuntimeEvent(state: OpsState, event: NormalizedEvent): OpsState {
  if (event.seq > 0 && event.seq <= state.lastSeq) {
    return state;
  }

  const base = {
    ...state,
    activeThreadId: state.activeThreadId ?? event.threadId,
    lastSeq: Math.max(state.lastSeq, event.seq),
    error: null,
  };

  switch (event.type) {
    case "thread_created":
      return base;

    case "user_message": {
      const content = eventText(event.data);
      const optimisticIndex = base.items.findIndex(
        (item) => item.kind === "message" && item.role === "user" && item.optimistic && item.content === content,
      );
      const message: MessageItem = {
        id: stringValue(event.data.message_id) ?? `user:${event.turnId ?? event.seq}:${event.seq}`,
        kind: "message",
        role: "user",
        content,
        streaming: false,
        turnId: event.turnId,
        timestamp: event.timestamp,
        incidentContext: asIncidentContext(event.data.incident_context),
      };
      const items = [...base.items];
      if (optimisticIndex >= 0) {
        items[optimisticIndex] = message;
      } else {
        items.push(message);
      }
      const next = { ...base, items };
      if (!content || !base.activeThreadId) return next;
      return {
        ...next,
        threads: next.threads.map((thread) =>
          thread.id === base.activeThreadId && !thread.title
            ? { ...thread, title: content.slice(0, 64) }
            : thread,
        ),
      };
    }

    case "turn_started":
      return withThreadStatus({
        ...base,
        activeTurnId: event.turnId ?? stringValue(event.data.turn_id) ?? base.activeTurnId,
        turnStatus: "running",
      }, "running");

    case "assistant_delta": {
      const delta = eventText(event.data);
      const existingIndex = lastStreamingAssistantIndex(base.items, event.turnId);
      if (existingIndex < 0) {
        const message: MessageItem = {
          id: `assistant:${event.turnId ?? "unknown"}:${event.seq}`,
          kind: "message",
          role: "assistant",
          content: delta,
          streaming: true,
          turnId: event.turnId,
          timestamp: event.timestamp,
        };
        return { ...base, items: [...base.items, message], turnStatus: "running" };
      }
      const items = [...base.items];
      const existing = items[existingIndex] as MessageItem;
      items[existingIndex] = { ...existing, content: existing.content + delta, streaming: true };
      return { ...base, items, turnStatus: "running" };
    }

    case "assistant_completed": {
      const completedContent = eventText(event.data);
      const diagnosis = asDiagnosis(event.data.diagnosis);
      const existingIndex = lastStreamingAssistantIndex(base.items, event.turnId);
      if (existingIndex < 0) {
        const message: MessageItem = {
          id: `assistant:${event.turnId ?? "unknown"}:${event.seq}`,
          kind: "message",
          role: "assistant",
          content: completedContent,
          streaming: false,
          turnId: event.turnId,
          timestamp: event.timestamp,
          diagnosis,
        };
        return { ...base, items: [...base.items, message] };
      }
      const items = [...base.items];
      const existing = items[existingIndex] as MessageItem;
      items[existingIndex] = {
        ...existing,
        content: completedContent || existing.content,
        streaming: false,
        diagnosis: diagnosis ?? existing.diagnosis,
      };
      return { ...base, items };
    }

    case "tool_proposed":
    case "tool_started": {
      const callId = toolCallId(event);
      const item: ToolItem = {
        id: `tool:${callId}`,
        kind: "tool",
        callId,
        name: toolName(event.data),
        arguments: recordValue(event.data.arguments ?? event.data.input),
        status: event.type === "tool_proposed" ? "proposed" : "running",
        turnId: event.turnId,
        timestamp: event.timestamp,
      };
      const settledItems = base.items.map((timelineItem) =>
        timelineItem.kind === "message" && timelineItem.role === "assistant" && timelineItem.streaming
          ? { ...timelineItem, streaming: false }
          : timelineItem,
      );
      return { ...base, items: [...settledItems, item], turnStatus: "running" };
    }

    case "tool_authorized": {
      const callId = toolCallId(event);
      return {
        ...base,
        items: base.items.map((item): TimelineItem =>
          item.kind === "tool" && item.callId === callId
            ? { ...item, status: item.status === "completed" || item.status === "failed" ? item.status : "authorized" }
            : item,
        ),
      };
    }

    case "tool_execution_started": {
      const callId = toolCallId(event);
      return {
        ...base,
        turnStatus: "running",
        items: base.items.map((item): TimelineItem =>
          item.kind === "tool" && item.callId === callId ? { ...item, status: "running" } : item,
        ),
      };
    }

    case "tool_completed": {
      const callId = toolCallId(event);
      const name = toolName(event.data);
      let existingIndex = base.items.findIndex(
        (item) => item.kind === "tool" && item.callId === callId,
      );
      if (existingIndex < 0) {
        for (let index = base.items.length - 1; index >= 0; index -= 1) {
          const item = base.items[index];
          if (item.kind === "tool" && (item.status === "running" || item.status === "proposed" || item.status === "authorized") && item.name === name) {
            existingIndex = index;
            break;
          }
        }
      }
      const rawOutput = event.data.output ?? event.data.result ?? event.data.content;
      const outputRecord = recordValue(rawOutput);
      const evidence = recordValue(event.data.evidence ?? outputRecord.evidence) as EvidenceMeta;
      const durationMs = numberValue(event.data.duration_ms) ?? numberValue(evidence.duration_ms);
      const success = booleanValue(event.data.success);
      const error =
        stringValue(event.data.error) ??
        stringValue(outputRecord.error) ??
        (success === false ? "Tool execution failed." : undefined);
      const status = error ? "failed" : "completed";
      if (existingIndex < 0) {
        const item: ToolItem = {
          id: `tool:${callId}`,
          kind: "tool",
          callId,
          name,
          arguments: recordValue(event.data.arguments ?? event.data.input),
          status,
          output: rawOutput,
          evidence,
          durationMs,
          error,
          turnId: event.turnId,
          timestamp: event.timestamp,
        };
        return { ...base, items: [...base.items, item] };
      }
      const items = [...base.items];
      const existing = items[existingIndex] as ToolItem;
      items[existingIndex] = {
        ...existing,
        status,
        output: rawOutput,
        evidence,
        durationMs,
        error,
      };
      return { ...base, items };
    }

    case "approval_required": {
      const approvalId =
        stringValue(event.data.approval_id) ?? stringValue(event.data.id) ?? `approval:${event.seq}`;
      const item: ApprovalItem = {
        id: `approval:${approvalId}`,
        kind: "approval",
        approvalId,
        tool: toolName(event.data),
        arguments: recordValue(event.data.arguments ?? event.data.input),
        status: "pending",
        turnId: event.turnId,
        timestamp: event.timestamp,
      };
      return { ...base, items: [...base.items, item], turnStatus: "running" };
    }

    case "approval_resolved": {
      const approvalId = stringValue(event.data.approval_id) ?? stringValue(event.data.id);
      const approved = booleanValue(event.data.approved);
      return {
        ...base,
        items: base.items.map((item): TimelineItem =>
          item.kind === "approval" && item.approvalId === approvalId
            ? { ...item, status: approved ? "approved" : "rejected" }
            : item,
        ),
      };
    }

    case "turn_completed":
      return withThreadStatus({
        ...base,
        activeTurnId: null,
        turnStatus: "idle",
        items: base.items.map((item) =>
          item.kind === "message" && item.role === "assistant" ? { ...item, streaming: false } : item,
        ),
      }, "completed");

    case "turn_failed":
      return withThreadStatus({
        ...base,
        activeTurnId: null,
        turnStatus: "failed",
        error: stringValue(event.data.error) ?? stringValue(event.data.message) ?? "The turn failed.",
      }, "failed");

    case "turn_cancelled":
      return withThreadStatus(
        { ...base, activeTurnId: null, turnStatus: "cancelled" },
        "cancelled",
      );

    default: {
      const originalType = stringValue(event.data._event_type) ?? event.type;
      return {
        ...base,
        clientUpgradeHint: `Client upgrade required for event ${originalType}`,
      };
    }
  }
}

function replayThread(state: OpsState, detail: ThreadDetail): OpsState {
  const emptyThread: OpsState = {
    ...state,
    activeThreadId: detail.id,
    activeTurnId: null,
    items: [],
    loadStatus: "ready",
    turnStatus: detail.status === "running" ? "running" : "idle",
    lastSeq: 0,
    error: null,
    selectedEvidenceId: null,
    clientUpgradeHint: null,
  };
  return detail.events.reduce(applyRuntimeEvent, emptyThread);
}

export function opsReducer(state: OpsState, action: OpsAction): OpsState {
  switch (action.type) {
    case "threads/loading":
      return { ...state, loadStatus: "loading", error: null };
    case "threads/loaded":
      return { ...state, threads: action.payload, loadStatus: "ready", error: null };
    case "threads/failed":
      return { ...state, loadStatus: "error", error: action.payload };
    case "thread/select":
      return {
        ...state,
        activeThreadId: action.payload,
        activeTurnId: null,
        items: [],
        lastSeq: 0,
        loadStatus: "loading",
        connectionStatus: "connecting",
        sidebarOpen: false,
        error: null,
        selectedEvidenceId: null,
        clientUpgradeHint: null,
      };
    case "thread/created":
      return {
        ...state,
        threads: [action.payload, ...state.threads.filter((thread) => thread.id !== action.payload.id)],
        activeThreadId: action.payload.id,
        activeTurnId: null,
        items: [],
        lastSeq: 0,
        loadStatus: "ready",
        sidebarOpen: false,
        error: null,
        selectedEvidenceId: null,
        clientUpgradeHint: null,
      };
    case "thread/loading":
      return { ...state, loadStatus: "loading", error: null };
    case "thread/loaded":
      return replayThread(
        {
          ...state,
          threads: state.threads.some((thread) => thread.id === action.payload.id)
            ? state.threads.map((thread) =>
                thread.id === action.payload.id
                  ? {
                      ...thread,
                      status: action.payload.status,
                      title: action.payload.title ?? thread.title,
                      createdAt: action.payload.createdAt ?? thread.createdAt,
                      updatedAt: action.payload.updatedAt ?? thread.updatedAt,
                    }
                  : thread,
              )
            : [action.payload, ...state.threads],
        },
        action.payload,
      );
    case "message/optimistic": {
      const item: MessageItem = {
        id: action.payload.id,
        kind: "message",
        role: "user",
        content: action.payload.content,
        streaming: false,
        optimistic: true,
        turnId: null,
        incidentContext: action.payload.incidentContext,
      };
      const next = { ...state, items: [...state.items, item], error: null };
      if (!state.activeThreadId) return next;
      return {
        ...next,
        threads: next.threads.map((thread) =>
          thread.id === state.activeThreadId
            ? { ...thread, title: thread.title || action.payload.content.slice(0, 64) }
            : thread,
        ),
      };
    }
    case "turn/started":
      return withThreadStatus({
        ...state,
        activeTurnId: action.payload.turnId,
        turnStatus: "running",
        error: null,
      }, "running");
    case "event/received":
      return applyRuntimeEvent(state, action.payload);
    case "connection/changed":
      return { ...state, connectionStatus: action.payload };
    case "approval/resolved":
      return {
        ...state,
        items: state.items.map((item): TimelineItem =>
          item.kind === "approval" && item.approvalId === action.payload.approvalId
            ? { ...item, status: action.payload.approved ? "approved" : "rejected" }
            : item,
        ),
      };
    case "error/set":
      return { ...state, error: action.payload };
    case "error/clear":
      return { ...state, error: null };
    case "evidence/select":
      return { ...state, selectedEvidenceId: action.payload };
    case "sidebar/set":
      return { ...state, sidebarOpen: action.payload };
  }
}
