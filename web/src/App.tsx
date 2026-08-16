import * as Dialog from "@radix-ui/react-dialog";
import { X } from "lucide-react";
import { useCallback, useEffect, useMemo, useReducer, useState } from "react";

import { api as defaultApi, type OpsApiClient } from "./api";
import { Chat } from "./components/Chat";
import { Header } from "./components/Header";
import { Sidebar } from "./components/Sidebar";
import { Button } from "./components/ui/button";
import { TooltipProvider } from "./components/ui/tooltip";
import { initialState, opsReducer } from "./reducer";
import type { ExtensionSummary, IncidentContext, SkillSummary, TopologyGraph, WorkspaceSummary } from "./types";

interface AppProps {
  client?: OpsApiClient;
}

function readableError(error: unknown): string {
  return error instanceof Error ? error.message : "An unexpected error occurred.";
}

function newMessageId(): string {
  return globalThis.crypto?.randomUUID?.() ?? `message-${Date.now()}`;
}

export default function App({ client = defaultApi }: AppProps) {
  const [state, dispatch] = useReducer(opsReducer, initialState);
  const [creatingThread, setCreatingThread] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [resolvingApprovals, setResolvingApprovals] = useState<Set<string>>(() => new Set());
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [selectedWorkspaceId, setSelectedWorkspaceId] = useState("default");
  const [topology, setTopology] = useState<TopologyGraph | null>(null);
  const [extensions, setExtensions] = useState<ExtensionSummary[]>([]);
  const [skills, setSkills] = useState<SkillSummary[]>([]);

  useEffect(() => {
    let active = true;
    dispatch({ type: "threads/loading" });
    void (async () => {
      try {
        const listed = await client.listWorkspaces();
        if (!active) return;
        const nextWorkspaces =
          listed.length > 0
            ? listed
            : [{ id: "default", displayName: "Local demo", environment: "local", connectors: [] }];
        setWorkspaces(nextWorkspaces);
        const selected =
          nextWorkspaces.find((workspace) => workspace.id === "default")?.id ?? nextWorkspaces[0].id;
        setSelectedWorkspaceId(selected);
        const threads = await client.listThreads(selected);
        if (!active) return;
        dispatch({ type: "threads/loaded", payload: threads });
        if (threads.length > 0) dispatch({ type: "thread/select", payload: threads[0].id });
      } catch (error: unknown) {
        if (active) dispatch({ type: "threads/failed", payload: readableError(error) });
      }
    })();
    return () => {
      active = false;
    };
  }, [client]);

  useEffect(() => {
    if (!state.activeThreadId) return;

    const threadId = state.activeThreadId;
    let active = true;
    let subscription: { close(): void } | undefined;
    dispatch({ type: "thread/loading" });
    dispatch({ type: "connection/changed", payload: "connecting" });

    void client
      .getThread(threadId)
      .then((thread) => {
        if (!active) return;
        dispatch({ type: "thread/loaded", payload: thread });
        const after = thread.events.reduce((latest, event) => Math.max(latest, event.seq), 0);
        subscription = client.subscribe(threadId, after, {
          onOpen: () => {
            if (active) dispatch({ type: "connection/changed", payload: "connected" });
          },
          onEvent: (event) => {
            if (active) dispatch({ type: "event/received", payload: event });
          },
          onError: (error) => {
            if (!active) return;
            dispatch({ type: "connection/changed", payload: "reconnecting" });
            if (error instanceof Error) dispatch({ type: "error/set", payload: error.message });
          },
        });
      })
      .catch((error: unknown) => {
        if (!active) return;
        dispatch({ type: "threads/failed", payload: readableError(error) });
        dispatch({ type: "connection/changed", payload: "idle" });
      });

    return () => {
      active = false;
      subscription?.close();
    };
  }, [client, state.activeThreadId]);

  const activeThread = useMemo(
    () => state.threads.find((thread) => thread.id === state.activeThreadId),
    [state.activeThreadId, state.threads],
  );
  const topologyWorkspaceId = activeThread?.workspaceId ?? selectedWorkspaceId;

  useEffect(() => {
    if (!state.activeThreadId) {
      setTopology(null);
      return;
    }
    const threadId = state.activeThreadId;
    let active = true;
    void client
      .getTopology(threadId, topologyWorkspaceId ?? undefined)
      .then((graph) => {
        if (active) setTopology(graph.nodes.length > 0 ? graph : null);
      })
      .catch(() => {
        if (active) setTopology(null);
      });
    return () => {
      active = false;
    };
  }, [client, state.activeThreadId, state.lastSeq, topologyWorkspaceId]);

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        const [nextExtensions, nextSkills] = await Promise.all([
          client.listExtensions(selectedWorkspaceId),
          client.listSkills(selectedWorkspaceId),
        ]);
        if (!active) return;
        setExtensions(nextExtensions);
        setSkills(nextSkills);
      } catch {
        if (!active) {
          return;
        }
        setExtensions([]);
        setSkills([]);
      }
    })();
    return () => {
      active = false;
    };
  }, [client, selectedWorkspaceId]);

  const createThread = useCallback(async () => {
    if (creatingThread) return;
    setCreatingThread(true);
    try {
      const created = await client.createThread(selectedWorkspaceId);
      dispatch({
        type: "thread/created",
        payload: {
          id: created.id,
          status: "idle",
          title: null,
          workspaceId: created.workspaceId,
        },
      });
    } catch (error) {
      dispatch({ type: "error/set", payload: readableError(error) });
    } finally {
      setCreatingThread(false);
    }
  }, [client, creatingThread, selectedWorkspaceId]);

  const selectWorkspace = useCallback(
    async (workspaceId: string) => {
      if (workspaceId === selectedWorkspaceId) return;
      setSelectedWorkspaceId(workspaceId);
      dispatch({ type: "threads/loading" });
      try {
        const threads = await client.listThreads(workspaceId);
        dispatch({ type: "threads/loaded", payload: threads });
        dispatch({ type: "thread/select", payload: threads[0]?.id ?? null });
      } catch (error) {
        dispatch({ type: "threads/failed", payload: readableError(error) });
      }
    },
    [client, selectedWorkspaceId],
  );

  const sendMessage = useCallback(
    async (input: string, incidentContext?: IncidentContext) => {
      if (!state.activeThreadId || state.turnStatus === "running") return;
      const threadId = state.activeThreadId;
      dispatch({
        type: "message/optimistic",
        payload: { id: newMessageId(), content: input, incidentContext },
      });
      try {
        const turn = await client.startTurn(threadId, input, incidentContext);
        dispatch({ type: "turn/started", payload: { turnId: turn.turnId } });
      } catch (error) {
        dispatch({ type: "error/set", payload: readableError(error) });
      }
    },
    [client, state.activeThreadId, state.turnStatus],
  );

  const stopTurn = useCallback(async () => {
    if (!state.activeTurnId || stopping) return;
    setStopping(true);
    try {
      await client.interruptTurn(state.activeTurnId);
    } catch (error) {
      dispatch({ type: "error/set", payload: readableError(error) });
    } finally {
      setStopping(false);
    }
  }, [client, state.activeTurnId, stopping]);

  const resumeTurn = useCallback(async () => {
    if (!state.activeTurnId || state.turnStatus === "running") return;
    try {
      const key = globalThis.crypto?.randomUUID?.() ?? `resume-${Date.now()}`;
      const turn = await client.resumeTurn(state.activeTurnId, key);
      dispatch({ type: "turn/started", payload: { turnId: turn.turnId } });
    } catch (error) {
      dispatch({ type: "error/set", payload: readableError(error) });
    }
  }, [client, state.activeTurnId, state.turnStatus]);

  const forkThread = useCallback(async () => {
    if (!state.activeThreadId || state.lastSeq < 1) return;
    try {
      const forked = await client.forkThread(state.activeThreadId, state.lastSeq);
      dispatch({
        type: "thread/created",
        payload: {
          id: forked.id,
          status: "idle",
          title: null,
          workspaceId: selectedWorkspaceId,
          parentThreadId: forked.parentThreadId,
          forkedAtSeq: forked.forkedAtSeq,
        },
      });
    } catch (error) {
      dispatch({ type: "error/set", payload: readableError(error) });
    }
  }, [client, selectedWorkspaceId, state.activeThreadId, state.lastSeq]);

  const resolveApproval = useCallback(
    async (approvalId: string, approved: boolean) => {
      setResolvingApprovals((current) => new Set(current).add(approvalId));
      try {
        await client.resolveApproval(approvalId, approved);
        dispatch({ type: "approval/resolved", payload: { approvalId, approved } });
      } catch (error) {
        dispatch({ type: "error/set", payload: readableError(error) });
      } finally {
        setResolvingApprovals((current) => {
          const next = new Set(current);
          next.delete(approvalId);
          return next;
        });
      }
    },
    [client],
  );

  const workspaceLabel = useMemo(() => {
    const workspaceId = activeThread?.workspaceId ?? selectedWorkspaceId;
    const workspace = workspaces.find((item) => item.id === workspaceId);
    return workspace?.displayName ?? workspaceId;
  }, [activeThread?.workspaceId, selectedWorkspaceId, workspaces]);

  const sidebar = (
    <Sidebar
      threads={state.threads}
      workspaces={workspaces}
      extensions={extensions}
      selectedWorkspaceId={selectedWorkspaceId}
      activeThreadId={state.activeThreadId}
      loading={state.loadStatus === "loading" && state.threads.length === 0}
      onSelect={(threadId) => dispatch({ type: "thread/select", payload: threadId })}
      onWorkspaceChange={(workspaceId) => void selectWorkspace(workspaceId)}
      onNew={() => void createThread()}
    />
  );

  return (
    <TooltipProvider>
      <div className="flex h-dvh min-h-[30rem] w-full overflow-hidden bg-[#f7f8fa] text-zinc-900">
        <aside className="hidden h-full w-72 shrink-0 border-r border-zinc-200 md:block">{sidebar}</aside>

        <Dialog.Root
          open={state.sidebarOpen}
          onOpenChange={(open) => dispatch({ type: "sidebar/set", payload: open })}
        >
          <Dialog.Portal>
            <Dialog.Overlay className="fixed inset-0 z-40 bg-zinc-950/35 md:hidden" />
            <Dialog.Content className="fixed inset-y-0 left-0 z-50 w-[min(86vw,18rem)] border-r border-zinc-200 bg-[#f4f5f7] shadow-xl outline-none md:hidden">
              <Dialog.Title className="sr-only">Navigation</Dialog.Title>
              {sidebar}
              <Dialog.Close asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label="Close navigation"
                  className="absolute right-2 top-3.5 h-9 w-9"
                >
                  <X aria-hidden="true" className="h-4 w-4" />
                </Button>
              </Dialog.Close>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>

        <main className="flex min-w-0 flex-1 flex-col">
          <Header
            title={activeThread?.title?.trim() || (activeThread ? "Untitled investigation" : "OpsCodex")}
            workspaceLabel={activeThread ? workspaceLabel : null}
            connectionStatus={state.connectionStatus}
            turnStatus={state.turnStatus}
            onOpenSidebar={() => dispatch({ type: "sidebar/set", payload: true })}
          />
          <Chat
            state={state}
            topology={topology}
            skills={skills}
            hasThread={Boolean(state.activeThreadId)}
            stopping={stopping}
            resolvingApprovals={resolvingApprovals}
            onNewThread={() => void createThread()}
            onSend={(input, incidentContext) => void sendMessage(input, incidentContext)}
            onStop={() => void stopTurn()}
            onApproval={(approvalId, approved) => void resolveApproval(approvalId, approved)}
            onResume={() => void resumeTurn()}
            onFork={() => void forkThread()}
            onDismissError={() => dispatch({ type: "error/clear" })}
            onSelectEvidence={(evidenceId) => dispatch({ type: "evidence/select", payload: evidenceId })}
          />
        </main>
      </div>
    </TooltipProvider>
  );
}
