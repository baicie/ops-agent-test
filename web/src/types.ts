export type ThreadStatus = "idle" | "running" | "completed" | "failed" | "cancelled";

export interface ThreadSummary {
  id: string;
  status: ThreadStatus | string;
  title?: string | null;
  createdAt?: string;
  updatedAt?: string;
  workspaceId?: string | null;
}

export interface WorkspaceSummary {
  id: string;
  displayName: string;
  environment: string;
  connectors: string[];
}

export interface ExtensionSummary {
  id: string;
  kind: string;
  version: string;
  hash: string;
  enabled: boolean;
  health: { state: string; detail?: string | null; restartCount?: number };
  workspaces: string[];
}

export interface SkillSummary {
  id: string;
  title: string;
  version: string;
  hash: string;
  bytes: number;
}

export interface TopologyNode {
  id: string;
  kind: string;
  workspaceId: string;
  evidenceIds: string[];
  observedAt?: string;
}

export interface TopologyEdge {
  from: string;
  to: string;
  relation: string;
  confidence: string;
  source: string;
  evidenceIds: string[];
  observedAt?: string;
  expiresAt?: string;
  stale: boolean;
}

export interface TopologyGraph {
  nodes: TopologyNode[];
  edges: TopologyEdge[];
}

export interface ThreadDetail extends ThreadSummary {
  events: NormalizedEvent[];
}

export type RuntimeEventType =
  | "thread_created"
  | "user_message"
  | "turn_started"
  | "assistant_delta"
  | "assistant_completed"
  | "tool_started"
  | "tool_proposed"
  | "tool_authorized"
  | "tool_execution_started"
  | "tool_completed"
  | "approval_required"
  | "approval_resolved"
  | "turn_completed"
  | "turn_failed"
  | "turn_cancelled"
  | "unknown";

export interface NormalizedEvent {
  seq: number;
  threadId: string;
  turnId: string | null;
  timestamp?: string;
  type: RuntimeEventType;
  data: Record<string, unknown>;
}

interface TimelineBase {
  id: string;
  turnId: string | null;
  timestamp?: string;
}

export interface MessageItem extends TimelineBase {
  kind: "message";
  role: "user" | "assistant";
  content: string;
  streaming: boolean;
  optimistic?: boolean;
  incidentContext?: IncidentContext | null;
  diagnosis?: Diagnosis | null;
}

export interface IncidentContext {
  service?: string;
  environment?: string;
  starts_at?: string;
  ends_at?: string;
  labels?: Record<string, string>;
  annotations?: Record<string, string>;
  source?: { kind?: string; fingerprint?: string };
}

export interface Claim {
  claim_id?: string;
  kind: string;
  statement: string;
  evidence_ids?: string[];
  confidence?: string;
}

export interface Diagnosis {
  summary: string;
  claims: Claim[];
  recommended_actions?: string[];
  limitations?: string[];
}

export interface EvidenceMeta {
  source?: string;
  query?: string;
  timestamp?: string;
  duration_ms?: number;
  evidence_id?: string;
  summary?: string;
  sha256?: string;
  truncated?: boolean;
  artifact_ref?: string;
  [key: string]: unknown;
}

export interface ToolItem extends TimelineBase {
  kind: "tool";
  callId: string;
  name: string;
  arguments: Record<string, unknown>;
  status: "proposed" | "authorized" | "running" | "completed" | "failed";
  output?: unknown;
  evidence?: EvidenceMeta;
  durationMs?: number;
  error?: string;
}

export interface ApprovalItem extends TimelineBase {
  kind: "approval";
  approvalId: string;
  tool: string;
  arguments: Record<string, unknown>;
  status: "pending" | "approved" | "rejected";
}

export type TimelineItem = MessageItem | ToolItem | ApprovalItem;

export type LoadStatus = "idle" | "loading" | "ready" | "error";
export type ConnectionStatus = "idle" | "connecting" | "connected" | "reconnecting";
export type TurnStatus = "idle" | "running" | "failed" | "cancelled";

export interface OpsState {
  threads: ThreadSummary[];
  activeThreadId: string | null;
  activeTurnId: string | null;
  items: TimelineItem[];
  loadStatus: LoadStatus;
  connectionStatus: ConnectionStatus;
  turnStatus: TurnStatus;
  lastSeq: number;
  error: string | null;
  selectedEvidenceId: string | null;
  clientUpgradeHint: string | null;
  sidebarOpen: boolean;
}
