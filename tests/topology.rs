use chrono::{Duration, Utc};
use opscodex::{
    evidence::EvidenceMeta,
    runtime::{EventEnvelope, RuntimeEvent, ThreadId, WorkspaceId},
    topology::{TopologyQuery, project_topology, query_topology},
};
use serde_json::json;

#[test]
fn topology_keeps_conflicting_edges_and_hides_stale_from_model_query() {
    let workspace = WorkspaceId::default();
    let thread = ThreadId::new();
    let mut k8s = EventEnvelope::new(
        2,
        thread.clone(),
        None,
        RuntimeEvent::ToolCompleted {
            call_id: "c1".into(),
            tool: "k8s_get".into(),
            output: json!({
                "kind": "Service",
                "name": "order-service",
                "object": {
                    "kind": "Service",
                    "metadata": {"name": "order-service"},
                    "spec": {"selector": {"app": "order"}}
                }
            }),
            evidence: {
                let mut meta = EvidenceMeta::new("kubernetes");
                meta.evidence_id = Some(opscodex::runtime::EvidenceId::new());
                meta
            },
            success: true,
        },
    );
    k8s.workspace_id = workspace.clone();
    let mut trace = EventEnvelope::new(
        3,
        thread,
        None,
        RuntimeEvent::ToolCompleted {
            call_id: "c2".into(),
            tool: "trace_search".into(),
            output: json!({
                "traces": [{"client": "checkout-ui", "server": "order-service"}]
            }),
            evidence: {
                let mut meta = EvidenceMeta::new("tempo");
                meta.evidence_id = Some(opscodex::runtime::EvidenceId::new());
                meta
            },
            success: true,
        },
    );
    trace.workspace_id = workspace.clone();
    let graph = project_topology(&workspace, &[k8s, trace]);
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| node.id.contains("order-service"))
    );
    assert!(graph.edges.iter().any(|edge| edge.source == "trace"));
    assert!(graph.edges.iter().any(|edge| edge.source == "kubernetes"));
    assert!(
        graph
            .edges
            .iter()
            .all(|edge| edge.expires_at > edge.observed_at)
    );
    assert!(graph.edges.iter().all(|edge| !edge.evidence_ids.is_empty()));
    let queried = query_topology(
        graph.clone(),
        TopologyQuery {
            depth: 2,
            max_nodes: 8,
            include_stale: false,
        },
    );
    assert!(queried.edges.iter().all(|edge| !edge.stale));

    let mut expired = graph;
    expired.edges[0].expires_at = Utc::now() - Duration::minutes(1);
    let hidden = query_topology(
        expired.clone(),
        TopologyQuery {
            depth: 2,
            max_nodes: 8,
            include_stale: false,
        },
    );
    assert!(hidden.edges.iter().all(|edge| !edge.stale));
    let visible = query_topology(
        expired,
        TopologyQuery {
            depth: 2,
            max_nodes: 8,
            include_stale: true,
        },
    );
    assert!(visible.edges.iter().any(|edge| edge.stale));
}

#[test]
fn topology_projects_wrapped_trace_tool_output_from_the_agent_loop() {
    let workspace = WorkspaceId::default();
    let thread = ThreadId::new();
    let mut trace = EventEnvelope::new(
        2,
        thread,
        None,
        RuntimeEvent::ToolCompleted {
            call_id: "c1".into(),
            tool: "trace_search".into(),
            output: json!({
                "success": true,
                "evidence_id": "ev-1",
                "summary": "1 traces",
                "truncated": false,
                "sha256": "abc",
                "content": {
                    "traces": [{
                        "client": "order-service",
                        "server": "payment-service"
                    }]
                }
            }),
            evidence: {
                let mut meta = EvidenceMeta::new("tempo");
                meta.evidence_id = Some(opscodex::runtime::EvidenceId::new());
                meta
            },
            success: true,
        },
    );
    trace.workspace_id = workspace.clone();
    let graph = project_topology(&workspace, &[trace]);
    assert!(
        graph.edges.iter().any(|edge| {
            edge.from.contains("order-service") && edge.to.contains("payment-service")
        }),
        "{graph:?}"
    );
}
