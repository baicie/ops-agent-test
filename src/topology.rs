use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    evidence::EvidenceMeta,
    runtime::{EventEnvelope, RuntimeEvent, WorkspaceId},
};

const DEFAULT_TTL_MINUTES: i64 = 30;
const DEFAULT_DEPTH: usize = 2;
const DEFAULT_MAX_NODES: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyNode {
    pub id: String,
    pub kind: String,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
    pub evidence_ids: Vec<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyEdge {
    pub from: String,
    pub to: String,
    pub relation: String,
    pub confidence: String,
    pub source: String,
    pub evidence_ids: Vec<String>,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub stale: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyGraph {
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
}

#[derive(Clone, Debug)]
pub struct TopologyQuery {
    pub depth: usize,
    pub max_nodes: usize,
    pub include_stale: bool,
}

impl Default for TopologyQuery {
    fn default() -> Self {
        Self {
            depth: DEFAULT_DEPTH,
            max_nodes: DEFAULT_MAX_NODES,
            include_stale: false,
        }
    }
}

pub fn project_topology(workspace_id: &WorkspaceId, events: &[EventEnvelope]) -> TopologyGraph {
    let now = Utc::now();
    let mut graph = TopologyGraph::default();
    for envelope in events {
        if let RuntimeEvent::ToolCompleted {
            tool,
            evidence,
            success,
            output,
            ..
        } = &envelope.event
        {
            if !success {
                continue;
            }
            match tool.as_str() {
                "k8s_get" => project_k8s_object(&mut graph, workspace_id, evidence, output, now),
                "trace_search" | "trace_get" => {
                    project_trace(&mut graph, workspace_id, evidence, output, now)
                }
                _ => {
                    if let Some(service) = output.get("service").and_then(Value::as_str) {
                        upsert_node(
                            &mut graph,
                            workspace_id,
                            "service",
                            service,
                            evidence,
                            now,
                            "inferred",
                        );
                    }
                }
            }
        }
        if let RuntimeEvent::UserMessage {
            incident_context: Some(context),
            ..
        } = &envelope.event
            && let Some(service) = &context.service
        {
            let mut meta = EvidenceMeta::new("alert");
            meta.evidence_id = None;
            upsert_node(
                &mut graph,
                workspace_id,
                "service",
                service,
                &meta,
                now,
                "inferred",
            );
        }
    }
    graph.edges.sort_by(|left, right| {
        source_rank(&right.source)
            .cmp(&source_rank(&left.source))
            .then_with(|| left.from.cmp(&right.from))
    });
    graph
}

pub fn query_topology(graph: TopologyGraph, query: TopologyQuery) -> TopologyGraph {
    let now = Utc::now();
    let mut graph = graph;
    for edge in &mut graph.edges {
        if edge.expires_at <= now {
            edge.stale = true;
        }
    }
    if !query.include_stale {
        graph.edges.retain(|edge| !edge.stale);
    }

    let depth = query.depth.max(1);
    let max_nodes = query.max_nodes.max(1);
    let mut adjacency: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for node in &graph.nodes {
        adjacency.entry(node.id.clone()).or_default();
    }
    for edge in &graph.edges {
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .insert(edge.to.clone());
        adjacency
            .entry(edge.to.clone())
            .or_default()
            .insert(edge.from.clone());
    }

    let evidenced: Vec<String> = graph
        .nodes
        .iter()
        .filter(|node| !node.evidence_ids.is_empty())
        .map(|node| node.id.clone())
        .collect();
    let seeds = if evidenced.is_empty() {
        graph.nodes.iter().map(|node| node.id.clone()).collect()
    } else {
        evidenced
    };

    let mut kept = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::new();
    for seed in seeds {
        if seen.insert(seed.clone()) {
            queue.push_back((seed, 0usize));
        }
    }
    while let Some((id, distance)) = queue.pop_front() {
        kept.push(id.clone());
        if kept.len() >= max_nodes {
            break;
        }
        if distance >= depth {
            continue;
        }
        if let Some(neighbors) = adjacency.get(&id) {
            for neighbor in neighbors {
                if seen.insert(neighbor.clone()) {
                    queue.push_back((neighbor.clone(), distance + 1));
                }
            }
        }
    }

    let allowed: std::collections::BTreeSet<_> = kept.into_iter().collect();
    graph.nodes.retain(|node| allowed.contains(&node.id));
    graph
        .edges
        .retain(|edge| allowed.contains(&edge.from) && allowed.contains(&edge.to));
    graph
}

fn project_k8s_object(
    graph: &mut TopologyGraph,
    workspace_id: &WorkspaceId,
    evidence: &EvidenceMeta,
    output: &Value,
    now: DateTime<Utc>,
) {
    let object = output.get("object").unwrap_or(output);
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .or_else(|| output.get("kind").and_then(Value::as_str))
        .unwrap_or("Object");
    let name = object
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .or_else(|| output.get("name").and_then(Value::as_str));
    let Some(name) = name else {
        return;
    };
    let node_id = upsert_node(graph, workspace_id, kind, name, evidence, now, "high");
    if let Some(owners) = object
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
    {
        for owner in owners {
            let owner_kind = owner.get("kind").and_then(Value::as_str).unwrap_or("Owner");
            let owner_name = owner
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let owner_id = upsert_node(
                graph,
                workspace_id,
                owner_kind,
                owner_name,
                evidence,
                now,
                "high",
            );
            push_edge(
                graph,
                owner_id,
                node_id.clone(),
                "owns",
                "high",
                "kubernetes",
                evidence,
                now,
            );
        }
    }
    if kind.eq_ignore_ascii_case("Service")
        && let Some(selector) = object.pointer("/spec/selector").and_then(Value::as_object)
    {
        for (key, value) in selector {
            if let Some(label) = value.as_str() {
                let target = upsert_node(
                    graph,
                    workspace_id,
                    "selector",
                    &format!("{key}={label}"),
                    evidence,
                    now,
                    "medium",
                );
                push_edge(
                    graph,
                    node_id.clone(),
                    target,
                    "selects",
                    "medium",
                    "kubernetes",
                    evidence,
                    now,
                );
            }
        }
    }
}

fn project_trace(
    graph: &mut TopologyGraph,
    workspace_id: &WorkspaceId,
    evidence: &EvidenceMeta,
    output: &Value,
    now: DateTime<Utc>,
) {
    let traces = output
        .get("traces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for trace in traces {
        let service = trace
            .get("rootServiceName")
            .or_else(|| trace.get("service"))
            .and_then(Value::as_str);
        if let Some(service) = service {
            upsert_node(
                graph,
                workspace_id,
                "service",
                service,
                evidence,
                now,
                "high",
            );
        }
        if let (Some(client), Some(server)) = (
            trace.get("client").and_then(Value::as_str),
            trace.get("server").and_then(Value::as_str),
        ) {
            let from = upsert_node(
                graph,
                workspace_id,
                "service",
                client,
                evidence,
                now,
                "high",
            );
            let to = upsert_node(
                graph,
                workspace_id,
                "service",
                server,
                evidence,
                now,
                "high",
            );
            push_edge(graph, from, to, "calls", "high", "trace", evidence, now);
        }
    }
    if let Some(batches) = output.pointer("/batches").and_then(Value::as_array) {
        let services: Vec<_> = batches
            .iter()
            .filter_map(|batch| {
                batch
                    .pointer("/resource/service.name")
                    .and_then(Value::as_str)
            })
            .collect();
        for pair in services.windows(2) {
            let from = upsert_node(
                graph,
                workspace_id,
                "service",
                pair[0],
                evidence,
                now,
                "high",
            );
            let to = upsert_node(
                graph,
                workspace_id,
                "service",
                pair[1],
                evidence,
                now,
                "high",
            );
            push_edge(graph, from, to, "calls", "high", "trace", evidence, now);
        }
    }
}

fn upsert_node(
    graph: &mut TopologyGraph,
    workspace_id: &WorkspaceId,
    kind: &str,
    name: &str,
    evidence: &EvidenceMeta,
    now: DateTime<Utc>,
    _confidence: &str,
) -> String {
    let id = format!("{kind}:{name}");
    let evidence_id = evidence
        .evidence_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    if let Some(existing) = graph.nodes.iter_mut().find(|node| node.id == id) {
        if !evidence_id.is_empty() && !existing.evidence_ids.contains(&evidence_id) {
            existing.evidence_ids.push(evidence_id);
        }
        existing.observed_at = now;
        return id;
    }
    graph.nodes.push(TopologyNode {
        id: id.clone(),
        kind: kind.to_owned(),
        workspace_id: workspace_id.clone(),
        labels: std::collections::BTreeMap::new(),
        evidence_ids: if evidence_id.is_empty() {
            Vec::new()
        } else {
            vec![evidence_id]
        },
        observed_at: now,
    });
    id
}

#[allow(clippy::too_many_arguments)]
fn push_edge(
    graph: &mut TopologyGraph,
    from: String,
    to: String,
    relation: &str,
    confidence: &str,
    source: &str,
    evidence: &EvidenceMeta,
    now: DateTime<Utc>,
) {
    let evidence_id = evidence
        .evidence_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let expires_at = now + Duration::minutes(DEFAULT_TTL_MINUTES);
    if let Some(existing) = graph.edges.iter_mut().find(|edge| {
        edge.from == from && edge.to == to && edge.relation == relation && edge.source == source
    }) {
        if !evidence_id.is_empty() && !existing.evidence_ids.contains(&evidence_id) {
            existing.evidence_ids.push(evidence_id);
        }
        existing.observed_at = now;
        existing.expires_at = expires_at;
        existing.stale = false;
        return;
    }
    if let Some(conflict) = graph.edges.iter().find(|edge| {
        edge.from == from && edge.to == to && edge.relation == relation && edge.source != source
    }) {
        let _ = conflict;
    }
    graph.edges.push(TopologyEdge {
        from,
        to,
        relation: relation.into(),
        confidence: confidence.into(),
        source: source.into(),
        evidence_ids: if evidence_id.is_empty() {
            Vec::new()
        } else {
            vec![evidence_id]
        },
        observed_at: now,
        expires_at,
        stale: false,
    });
}

fn source_rank(source: &str) -> u8 {
    match source {
        "trace" => 3,
        "kubernetes" => 2,
        _ => 1,
    }
}
