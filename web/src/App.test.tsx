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
    listWorkspaces: vi.fn().mockResolvedValue([
      { id: "default", displayName: "Local demo", environment: "local", connectors: [] },
    ]),
    listExtensions: vi.fn().mockResolvedValue([]),
    listSkills: vi.fn().mockResolvedValue([]),
    listThreads: vi.fn().mockResolvedValue(
      detail
        ? [
            {
              id: detail.id,
              title: detail.title,
              status: detail.status,
              createdAt: detail.createdAt,
              updatedAt: detail.updatedAt,
              workspaceId: detail.workspaceId ?? "default",
            },
          ]
        : [],
    ),
    createThread: vi.fn().mockResolvedValue({ id: "thread-new", workspaceId: "default" }),
    getThread: vi.fn().mockImplementation(async (id: string) =>
      detail ?? { id, title: null, status: "idle", workspaceId: "default", events: [] },
    ),
    getTopology: vi.fn().mockResolvedValue({ nodes: [], edges: [] }),
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
      workspaceId: "default",
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

    expect(client.startTurn).toHaveBeenCalledWith("thread-new", "order-service 怎么了？", undefined);
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
      workspaceId: "default",
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

  it("shows alert context and locates evidence from a diagnosis claim", async () => {
    const client = createClient({
      id: "thread-1",
      title: "Checkout 5xx",
      status: "completed",
      workspaceId: "default",
      events: [
        event(1, "user_message", {
          content: "Investigate checkout 5xx",
          incident_context: {
            service: "order-service",
            environment: "staging",
            labels: { severity: "critical" },
          },
        }),
        event(2, "tool_proposed", {
          tool_call_id: "call-1",
          tool: "log_query",
          arguments: { query: '{service="order-service"}' },
        }),
        event(3, "tool_completed", {
          tool_call_id: "call-1",
          tool: "log_query",
          duration_ms: 12,
          success: true,
          output: { status: "success" },
          evidence: {
            source: "loki",
            evidence_id: "ev-pool-1",
            query: '{service="order-service"}',
            summary: "database pool exhausted",
            sha256: "abc123def456",
          },
        }),
        event(4, "assistant_completed", {
          content: "Pool exhaustion caused the 5xx spike.",
          diagnosis: {
            summary: "Pool exhaustion caused the 5xx spike.",
            claims: [
              {
                kind: "observed",
                statement: "Logs show database pool exhausted.",
                evidence_ids: ["ev-pool-1"],
                confidence: "high",
              },
            ],
            limitations: ["Traces were not queried."],
            recommended_actions: ["Raise the pool cap."],
          },
        }),
      ],
    });

    render(<App client={client} />);

    expect(await screen.findByText("Investigate checkout 5xx")).toBeInTheDocument();
    expect(screen.getAllByText("Alert context").length).toBeGreaterThan(0);
    expect(screen.getByText("order-service")).toBeInTheDocument();
    expect(screen.getByText("Investigation hint only. It is not verified evidence.")).toBeInTheDocument();
    expect(screen.getByText("Logs show database pool exhausted.")).toBeInTheDocument();
    expect(screen.getByText("Traces were not queried.")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Evidence ev-pool-1" }));
    expect(document.getElementById("evidence-ev-pool-1")).toBeTruthy();
    expect(await screen.findByText("database pool exhausted")).toBeInTheDocument();
  });

  it("sends optional alert context with a new turn", async () => {
    const client = createClient();
    const user = userEvent.setup();
    render(<App client={client} />);

    await screen.findByText("No investigations yet");
    await user.click(screen.getByRole("button", { name: /new thread/i }));
    await user.click(await screen.findByRole("button", { name: /alert context/i }));
    await user.type(screen.getByLabelText("Alert service"), "order-service");
    await user.type(screen.getByLabelText("Alert environment"), "staging");
    await user.type(screen.getByPlaceholderText("Ask about your infrastructure..."), "why 5xx?");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(client.startTurn).toHaveBeenCalledWith("thread-new", "why 5xx?", {
      service: "order-service",
      environment: "staging",
    });
  });

  it("binds new threads to the selected workspace and jumps from topology to evidence", async () => {
    const client = createClient({
      id: "thread-1",
      title: "Staging checkout",
      status: "completed",
      workspaceId: "staging",
      events: [
        event(1, "tool_started", {
          tool_call_id: "call-1",
          tool: "k8s_get",
          arguments: { kind: "Pod", namespace: "checkout", name: "order-service" },
        }),
        event(2, "tool_completed", {
          tool_call_id: "call-1",
          tool: "k8s_get",
          success: true,
          output: {
            content: {
              cluster: "staging-cluster",
              kind: "Pod",
              namespace: "checkout",
              name: "order-service",
            },
          },
          evidence: { source: "kubernetes", evidence_id: "ev-k8s-1", summary: "Pod unready" },
        }),
        event(3, "tool_completed", {
          tool_call_id: "call-2",
          tool: "runbook_read",
          success: true,
          output: {
            content: {
              id: "order-service-db-pool",
              title: "Order service DB pool exhaustion",
              version: 1,
              hash: "abc123def4567890",
            },
          },
          evidence: { source: "runbook", evidence_id: "ev-rb-1" },
        }),
      ],
    });
    client.listWorkspaces = vi.fn().mockResolvedValue([
      {
        id: "staging",
        displayName: "Staging",
        environment: "staging",
        connectors: ["kubernetes"],
      },
    ]);
    client.listThreads = vi.fn().mockResolvedValue([
      {
        id: "thread-1",
        title: "Staging checkout",
        status: "completed",
        workspaceId: "staging",
      },
    ]);
    client.createThread = vi.fn().mockResolvedValue({ id: "thread-new", workspaceId: "staging" });
    client.getTopology = vi.fn().mockResolvedValue({
      nodes: [
        {
          id: "Pod:order-service",
          kind: "Pod",
          workspaceId: "staging",
          evidenceIds: ["ev-k8s-1"],
        },
      ],
      edges: [
        {
          from: "Service:order-service",
          to: "Pod:order-service",
          relation: "selects",
          confidence: "medium",
          source: "kubernetes",
          evidenceIds: ["ev-k8s-1"],
          stale: false,
        },
      ],
    });

    render(<App client={client} />);

    expect((await screen.findAllByText("Staging checkout")).length).toBeGreaterThan(0);
    expect(screen.getAllByText("Staging").length).toBeGreaterThan(0);
    expect(screen.getByLabelText("Workspace")).toHaveValue("staging");
    expect(screen.getByText("staging-cluster · checkout · Pod · order-service")).toBeInTheDocument();
    expect(screen.getByText(/Order service DB pool exhaustion v1/)).toBeInTheDocument();
    expect(screen.getByText("Service topology")).toBeInTheDocument();

    await userEvent.click(screen.getAllByRole("button", { name: "Evidence ev-k8s-1" })[0]);
    expect(document.getElementById("evidence-ev-k8s-1")).toBeTruthy();

    await userEvent.click(screen.getAllByRole("button", { name: /new thread/i })[0]);
    expect(client.createThread).toHaveBeenCalledWith("staging");
  });

  it("shows loaded skills and external tool provenance", async () => {
    const client = createClient({
      id: "thread-1",
      title: "MCP ping",
      status: "completed",
      workspaceId: "default",
      events: [
        event(1, "user_message", { content: "ping the extension" }),
        event(2, "tool_completed", {
          tool_call_id: "call-1",
          tool: "mcp/mock/ping",
          success: true,
          output: {
            content: { source: "mcp", version: "1.0.0", hash: "deadbeefcafebabe" },
          },
          evidence: { source: "mcp", evidence_id: "ev-mcp-1" },
        }),
      ],
    });
    client.listSkills = vi.fn().mockResolvedValue([
      { id: "db-pool", title: "Pool", version: "1.0.0", hash: "abc", bytes: 120 },
    ]);
    client.listExtensions = vi.fn().mockResolvedValue([
      {
        id: "mock",
        kind: "mcp_http",
        version: "1.0.0",
        hash: "abc",
        enabled: true,
        health: { state: "healthy" },
        workspaces: ["default"],
      },
    ]);

    render(<App client={client} />);

    expect(await screen.findByTestId("loaded-skills")).toHaveTextContent("db-pool@1.0.0 (120 B)");
    expect(await screen.findByText(/mcp v1.0.0/)).toBeInTheDocument();
    expect(screen.getByText(/mcp_http 1\.0\.0/)).toBeInTheDocument();
  });
});
