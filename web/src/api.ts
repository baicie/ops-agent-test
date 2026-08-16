import type {
  IncidentContext,
  NormalizedEvent,
  RuntimeEventType,
  SkillSummary,
  ExtensionSummary,
  ThreadDetail,
  ThreadSummary,
  TopologyEdge,
  TopologyGraph,
  TopologyNode,
  WorkspaceSummary,
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
  "context_compacted",
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
  listWorkspaces(): Promise<WorkspaceSummary[]>;
  listExtensions(workspaceId?: string): Promise<ExtensionSummary[]>;
  listSkills(workspaceId?: string): Promise<SkillSummary[]>;
  listThreads(workspaceId?: string): Promise<ThreadSummary[]>;
  createThread(workspaceId?: string): Promise<{ id: string; workspaceId: string }>;
  getThread(threadId: string): Promise<ThreadDetail>;
  getTopology(threadId: string, workspaceId?: string): Promise<TopologyGraph>;
  startTurn(threadId: string, input: string, incidentContext?: IncidentContext | null): Promise<{ turnId: string; status: string }>;
  interruptTurn(turnId: string): Promise<void>;
  resumeTurn(turnId: string, idempotencyKey: string): Promise<{ turnId: string; status: string }>;
  getRecovery(turnId: string): Promise<{ status: string; userAction: string; risk: string; resumePolicy: string; skippedTools: string[] }>;
  forkThread(threadId: string, atSeq: number, title?: string): Promise<{ id: string; parentThreadId: string; forkedAtSeq: number }>;
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

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function normalizeWorkspace(value: unknown): WorkspaceSummary {
  const workspace = asRecord(value);
  return {
    id: asString(workspace.id) ?? "",
    displayName: asString(workspace.display_name) ?? asString(workspace.displayName) ?? asString(workspace.id) ?? "",
    environment: asString(workspace.environment) ?? "local",
    connectors: stringList(workspace.connectors),
  };
}

function normalizeExtension(value: unknown): ExtensionSummary {
  const extension = asRecord(value);
  const health = asRecord(extension.health);
  return {
    id: asString(extension.id) ?? "",
    kind: asString(extension.kind) ?? "custom",
    version: asString(extension.version) ?? "",
    hash: asString(extension.hash) ?? "",
    enabled: extension.enabled === true,
    health: {
      state: asString(health.state) ?? "disabled",
      detail: asString(health.detail),
      restartCount: typeof health.restart_count === "number" ? health.restart_count : 0,
    },
    workspaces: stringList(extension.workspaces),
  };
}

function normalizeSkill(value: unknown): SkillSummary {
  const skill = asRecord(value);
  return {
    id: asString(skill.id) ?? "",
    title: asString(skill.title) ?? asString(skill.id) ?? "",
    version: asString(skill.version) ?? "",
    hash: asString(skill.hash) ?? "",
    bytes: typeof skill.bytes === "number" ? skill.bytes : 0,
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
    workspaceId: asString(thread.workspace_id) ?? asString(thread.workspaceId) ?? null,
    parentThreadId: asString(thread.parent_thread_id) ?? asString(thread.parentThreadId) ?? null,
    forkedAtSeq: asNumber(thread.forked_at_seq) ?? asNumber(thread.forkedAtSeq) ?? null,
  };
}

function normalizeTopologyNode(value: unknown): TopologyNode | null {
  const node = asRecord(value);
  const id = asString(node.id);
  if (!id) return null;
  return {
    id,
    kind: asString(node.kind) ?? "service",
    workspaceId: asString(node.workspace_id) ?? asString(node.workspaceId) ?? "",
    evidenceIds: stringList(node.evidence_ids ?? node.evidenceIds),
    observedAt: asString(node.observed_at) ?? asString(node.observedAt),
  };
}

function normalizeTopologyEdge(value: unknown): TopologyEdge | null {
  const edge = asRecord(value);
  const from = asString(edge.from);
  const to = asString(edge.to);
  if (!from || !to) return null;
  return {
    from,
    to,
    relation: asString(edge.relation) ?? "related",
    confidence: asString(edge.confidence) ?? "medium",
    source: asString(edge.source) ?? "inferred",
    evidenceIds: stringList(edge.evidence_ids ?? edge.evidenceIds),
    observedAt: asString(edge.observed_at) ?? asString(edge.observedAt),
    expiresAt: asString(edge.expires_at) ?? asString(edge.expiresAt),
    stale: edge.stale === true,
  };
}

function normalizeTopology(value: unknown): TopologyGraph {
  const graph = asRecord(value);
  return {
    nodes: (Array.isArray(graph.nodes) ? graph.nodes : []).map(normalizeTopologyNode).filter((node): node is TopologyNode => Boolean(node)),
    edges: (Array.isArray(graph.edges) ? graph.edges : []).map(normalizeTopologyEdge).filter((edge): edge is TopologyEdge => Boolean(edge)),
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
    async listWorkspaces() {
      const body = await request<unknown>(apiUrl("/api/v1/workspaces"));
      const workspaces = Array.isArray(body) ? body : asRecord(body).workspaces;
      return (Array.isArray(workspaces) ? workspaces : [])
        .map(normalizeWorkspace)
        .filter((workspace) => workspace.id);
    },

    async listExtensions(workspaceId) {
      const params = new URLSearchParams();
      if (workspaceId) params.set("workspace_id", workspaceId);
      const query = params.size > 0 ? `?${params.toString()}` : "";
      const body = await request<unknown>(apiUrl(`/api/v1/extensions${query}`));
      const extensions = Array.isArray(body) ? body : asRecord(body).extensions;
      return (Array.isArray(extensions) ? extensions : [])
        .map(normalizeExtension)
        .filter((extension) => extension.id);
    },

    async listSkills(workspaceId) {
      const params = new URLSearchParams();
      if (workspaceId) params.set("workspace_id", workspaceId);
      const query = params.size > 0 ? `?${params.toString()}` : "";
      const body = await request<unknown>(apiUrl(`/api/v1/skills${query}`));
      const skills = Array.isArray(body) ? body : asRecord(body).skills;
      return (Array.isArray(skills) ? skills : [])
        .map(normalizeSkill)
        .filter((skill) => skill.id);
    },

    async listThreads(workspaceId) {
      const params = new URLSearchParams();
      if (workspaceId) params.set("workspace_id", workspaceId);
      const query = params.size > 0 ? `?${params.toString()}` : "";
      const body = await request<unknown>(apiUrl(`/api/v1/threads${query}`));
      const threads = Array.isArray(body) ? body : asRecord(body).threads;
      return (Array.isArray(threads) ? threads : []).map(normalizeThread).filter((thread) => thread.id);
    },

    async createThread(workspaceId) {
      const body = asRecord(
        await request<unknown>(apiUrl("/api/v1/threads"), {
          method: "POST",
          body: JSON.stringify(workspaceId ? { workspace_id: workspaceId } : {}),
        }),
      );
      const id = asString(body.id);
      if (!id) throw new Error("The server created a thread without returning its id.");
      return {
        id,
        workspaceId: asString(body.workspace_id) ?? asString(body.workspaceId) ?? workspaceId ?? "default",
      };
    },

    async getThread(threadId) {
      const body = asRecord(
        await request<unknown>(apiUrl(`/api/v1/threads/${encodeURIComponent(threadId)}`)),
      );
      const thread = normalizeThread(body);
      const rawEvents = Array.isArray(body.events) ? body.events : [];
      return {
        ...thread,
        id: thread.id || threadId,
        events: rawEvents.map((event) => normalizeEventEnvelope(event)),
      };
    },

    async getTopology(threadId, workspaceId) {
      const params = new URLSearchParams();
      if (workspaceId) params.set("workspace_id", workspaceId);
      const query = params.size > 0 ? `?${params.toString()}` : "";
      const body = await request<unknown>(
        apiUrl(`/api/v1/threads/${encodeURIComponent(threadId)}/topology${query}`),
      );
      return normalizeTopology(body);
    },

    async startTurn(threadId, input, incidentContext) {
      const body = asRecord(
        await request<unknown>(apiUrl(`/api/v1/threads/${encodeURIComponent(threadId)}/turns`), {
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
      await request<unknown>(apiUrl(`/api/v1/turns/${encodeURIComponent(turnId)}/interrupt`), {
        method: "POST",
      });
    },

    async resumeTurn(turnId, idempotencyKey) {
      const body = asRecord(
        await request<unknown>(apiUrl(`/api/v1/turns/${encodeURIComponent(turnId)}/resume`), {
          method: "POST",
          headers: { "Idempotency-Key": idempotencyKey },
        }),
      );
      return {
        turnId: asString(body.turn_id) ?? asString(body.turnId) ?? turnId,
        status: asString(body.status) ?? "resuming",
      };
    },

    async getRecovery(turnId) {
      const body = asRecord(await request<unknown>(apiUrl(`/api/v1/turns/${encodeURIComponent(turnId)}/recovery`)));
      return {
        status: asString(body.status) ?? "interrupted",
        userAction: asString(body.user_action) ?? asString(body.userAction) ?? "",
        risk: asString(body.risk) ?? "none",
        resumePolicy: asString(body.resume_policy) ?? asString(body.resumePolicy) ?? "none",
        skippedTools: stringList(body.skipped_tools ?? body.skippedTools),
      };
    },

    async forkThread(threadId, atSeq, title) {
      const body = asRecord(
        await request<unknown>(apiUrl(`/api/v1/threads/${encodeURIComponent(threadId)}/forks`), {
          method: "POST",
          body: JSON.stringify({ at_seq: atSeq, title }),
        }),
      );
      const id = asString(body.id);
      if (!id) throw new Error("The server forked a thread without returning its id.");
      return {
        id,
        parentThreadId: asString(body.parent_thread_id) ?? asString(body.parentThreadId) ?? threadId,
        forkedAtSeq: asNumber(body.forked_at_seq) ?? asNumber(body.forkedAtSeq) ?? atSeq,
      };
    },

    async resolveApproval(approvalId, approved) {
      await request<unknown>(apiUrl(`/api/v1/approvals/${encodeURIComponent(approvalId)}`), {
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
        apiUrl(`/api/v1/threads/${encodeURIComponent(threadId)}/events${query}`),
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
