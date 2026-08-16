import type {
  IncidentContext,
  NormalizedEvent,
  RuntimeEventType,
  ThreadDetail,
  ThreadSummary,
} from "./types";

const EVENT_TYPES: RuntimeEventType[] = [
  "thread_created",
  "user_message",
  "turn_started",
  "assistant_delta",
  "assistant_completed",
  "tool_started",
  "tool_proposed",
  "tool_authorized",
  "tool_execution_started",
  "tool_completed",
  "approval_required",
  "approval_resolved",
  "turn_completed",
  "turn_failed",
  "turn_cancelled",
];

const EVENT_TYPE_SET = new Set<string>(EVENT_TYPES);

type UnknownRecord = Record<string, unknown>;

export interface EventSourceLike {
  onopen: ((event: Event) => void) | null;
  onerror: ((event: Event) => void) | null;
  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void;
  close(): void;
}

type EventSourceConstructor = new (url: string | URL, eventSourceInitDict?: EventSourceInit) => EventSourceLike;

interface SubscribeCallbacks {
  onEvent: (event: NormalizedEvent) => void;
  onOpen?: () => void;
  onError?: (error: Event | Error) => void;
}

export interface OpsApiClient {
  listThreads(): Promise<ThreadSummary[]>;
  createThread(): Promise<{ id: string }>;
  getThread(threadId: string): Promise<ThreadDetail>;
  startTurn(threadId: string, input: string, incidentContext?: IncidentContext | null): Promise<{ turnId: string; status: string }>;
  interruptTurn(turnId: string): Promise<void>;
  resolveApproval(approvalId: string, approved: boolean): Promise<void>;
  subscribe(
    threadId: string,
    after: number,
    callbacks: SubscribeCallbacks,
  ): { close(): void };
}

export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

function asRecord(value: unknown): UnknownRecord {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return {};
  }
  return value as UnknownRecord;
}

function asString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function asNumber(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
}

function runtimeEventType(value: unknown, fallback?: string): RuntimeEventType {
  const candidate = asString(value) ?? fallback;
  if (!candidate) return "unknown";
  if (!EVENT_TYPE_SET.has(candidate)) return "unknown";
  return candidate as RuntimeEventType;
}

export function normalizeEventEnvelope(
  value: unknown,
  eventName?: string,
  lastEventId?: string,
): NormalizedEvent {
  const envelope = asRecord(value);
  const nested = asRecord(envelope.event);
  const hasNestedEvent = Object.keys(nested).length > 0;
  const rawType = asString(hasNestedEvent ? nested.type : envelope.type)
    ?? (eventName === "message" ? undefined : eventName);
  const type = runtimeEventType(rawType);
  const data = hasNestedEvent ? { ...nested } : { ...envelope };
  delete data.type;
  if (!hasNestedEvent) {
    delete data.seq;
    delete data.thread_id;
    delete data.threadId;
    delete data.turn_id;
    delete data.turnId;
    delete data.timestamp;
  }
  if (type === "unknown" && rawType) {
    data._event_type = rawType;
  }

  return {
    seq: asNumber(envelope.seq) ?? asNumber(lastEventId) ?? 0,
    threadId: asString(envelope.thread_id) ?? asString(envelope.threadId) ?? "",
    turnId: asString(envelope.turn_id) ?? asString(envelope.turnId) ?? null,
    timestamp: asString(envelope.timestamp),
    type,
    data,
  };
}

function normalizeThread(value: unknown): ThreadSummary {
  const thread = asRecord(value);
  return {
    id: asString(thread.id) ?? "",
    status: asString(thread.status) ?? "idle",
    title: asString(thread.title) ?? null,
    createdAt: asString(thread.created_at) ?? asString(thread.createdAt),
    updatedAt: asString(thread.updated_at) ?? asString(thread.updatedAt),
  };
}

function errorMessage(value: unknown, fallback: string): string {
  const body = asRecord(value);
  const nestedError = asRecord(body.error);
  return (
    asString(body.message) ??
    asString(body.error) ??
    asString(nestedError.message) ??
    fallback
  );
}

async function request<T>(url: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("Accept", "application/json");
  if (init.body !== undefined) headers.set("Content-Type", "application/json");

  const response = await fetch(url, { ...init, headers: Object.fromEntries(headers.entries()) });
  const contentType = response.headers.get("content-type") ?? "";
  const body: unknown = response.status === 204
    ? undefined
    : contentType.includes("application/json")
      ? await response.json()
      : await response.text();

  if (!response.ok) {
    const fallback = typeof body === "string" && body ? body : `Request failed (${response.status})`;
    throw new ApiError(response.status, errorMessage(body, fallback));
  }

  return body as T;
}

function joinUrl(baseUrl: string, path: string): string {
  return `${baseUrl.replace(/\/$/, "")}${path}`;
}

export function createApiClient(
  baseUrl = import.meta.env.VITE_API_BASE_URL ?? "",
  eventSourceConstructor?: EventSourceConstructor,
): OpsApiClient {
  const apiUrl = (path: string) => joinUrl(baseUrl, path);

  return {
    async listThreads() {
      const body = await request<unknown>(apiUrl("/api/threads"));
      const threads = Array.isArray(body) ? body : asRecord(body).threads;
      return (Array.isArray(threads) ? threads : []).map(normalizeThread).filter((thread) => thread.id);
    },

    async createThread() {
      const body = asRecord(await request<unknown>(apiUrl("/api/threads"), { method: "POST" }));
      const id = asString(body.id);
      if (!id) throw new Error("The server created a thread without returning its id.");
      return { id };
    },

    async getThread(threadId) {
      const body = asRecord(
        await request<unknown>(apiUrl(`/api/threads/${encodeURIComponent(threadId)}`)),
      );
      const thread = normalizeThread(body);
      const rawEvents = Array.isArray(body.events) ? body.events : [];
      return {
        ...thread,
        id: thread.id || threadId,
        events: rawEvents.map((event) => normalizeEventEnvelope(event)),
      };
    },

    async startTurn(threadId, input, incidentContext) {
      const body = asRecord(
        await request<unknown>(apiUrl(`/api/threads/${encodeURIComponent(threadId)}/turns`), {
          method: "POST",
          body: JSON.stringify({
            input,
            ...(incidentContext ? { incident_context: incidentContext } : {}),
          }),
        }),
      );
      const turnId = asString(body.turn_id) ?? asString(body.turnId);
      if (!turnId) throw new Error("The server started a turn without returning its id.");
      return { turnId, status: asString(body.status) ?? "running" };
    },

    async interruptTurn(turnId) {
      await request<unknown>(apiUrl(`/api/turns/${encodeURIComponent(turnId)}/interrupt`), {
        method: "POST",
      });
    },

    async resolveApproval(approvalId, approved) {
      await request<unknown>(apiUrl(`/api/approvals/${encodeURIComponent(approvalId)}`), {
        method: "POST",
        body: JSON.stringify({ approved }),
      });
    },

    subscribe(threadId, after, callbacks) {
      const EventSourceImpl = eventSourceConstructor ?? (globalThis.EventSource as EventSourceConstructor);
      const params = new URLSearchParams();
      if (after > 0) params.set("after", String(after));
      const query = params.size > 0 ? `?${params.toString()}` : "";
      const source = new EventSourceImpl(
        apiUrl(`/api/threads/${encodeURIComponent(threadId)}/events${query}`),
      );

      source.onopen = () => callbacks.onOpen?.();
      source.onerror = (event) => callbacks.onError?.(event);

      const handleEvent = (eventName: string) => (rawEvent: Event) => {
        const message = rawEvent as MessageEvent<string>;
        try {
          callbacks.onEvent(
            normalizeEventEnvelope(JSON.parse(message.data) as unknown, eventName, message.lastEventId),
          );
        } catch (error) {
          callbacks.onError?.(error instanceof Error ? error : new Error("Invalid SSE event"));
        }
      };

      for (const type of EVENT_TYPES) {
        source.addEventListener(type, handleEvent(type));
      }
      source.addEventListener("message", handleEvent("message"));

      return { close: () => source.close() };
    },
  };
}

export const api = createApiClient();
