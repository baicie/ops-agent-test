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
const MAX_ARTIFACT_PROJECTION_ITEMS: usize = 16;
const MAX_ARTIFACT_PROJECTION_STRING_BYTES: usize = 64;
const MAX_ARTIFACT_PROJECTION_BYTES: usize = 8 * 1024;

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
            let output = projection_payload(output);
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

fn projection_payload(output: &Value) -> &Value {
    if output.get("success").and_then(Value::as_bool) == Some(true) {
        if let Some(content) = output.get("content") {
            return content;
        }
        if let Some(projection) = output.get("topology_projection") {
            return projection;
        }
    }
    output
}

pub(crate) fn artifact_topology_projection(tool: &str, output: &Value) -> Option<Value> {
    let projection = match tool {
        "trace_search" | "trace_get" => trace_artifact_projection(output),
        "k8s_get" => k8s_artifact_projection(output),
        _ => None,
    }?;
    (serde_json::to_vec(&projection).ok()?.len() <= MAX_ARTIFACT_PROJECTION_BYTES)
        .then_some(projection)
}

fn trace_artifact_projection(output: &Value) -> Option<Value> {
    let traces: Vec<_> = output
        .get("traces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|trace| {
            let mut projected = serde_json::Map::new();
            for field in ["rootServiceName", "service", "client", "server"] {
                if let Some(value) = trace.get(field).and_then(Value::as_str) {
                    projected.insert(field.into(), Value::String(bounded_string(value)));
                }
            }
            (!projected.is_empty()).then_some(Value::Object(projected))
        })
        .take(MAX_ARTIFACT_PROJECTION_ITEMS)
        .collect();
    let batches: Vec<_> = output
        .get("batches")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|batch| {
            batch
                .pointer("/resource/service.name")
                .and_then(Value::as_str)
                .map(|service| {
                    serde_json::json!({
                        "resource": {"service.name": bounded_string(service)}
                    })
                })
        })
        .take(MAX_ARTIFACT_PROJECTION_ITEMS)
        .collect();
    if traces.is_empty() && batches.is_empty() {
        return None;
    }
    let mut projection = serde_json::Map::new();
    if !traces.is_empty() {
        projection.insert("traces".into(), Value::Array(traces));
    }
    if !batches.is_empty() {
        projection.insert("batches".into(), Value::Array(batches));
    }
    Some(Value::Object(projection))
}

fn k8s_artifact_projection(output: &Value) -> Option<Value> {
    let object = output.get("object").unwrap_or(output);
    let name = object
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .or_else(|| output.get("name").and_then(Value::as_str))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .or_else(|| output.get("kind").and_then(Value::as_str))
        .unwrap_or("Object");
    let owners: Vec<_> = object
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|owner| {
            let owner_name = owner.get("name").and_then(Value::as_str)?;
            Some(serde_json::json!({
                "kind": bounded_string(
                    owner.get("kind").and_then(Value::as_str).unwrap_or("Owner")
                ),
                "name": bounded_string(owner_name),
            }))
        })
        .take(MAX_ARTIFACT_PROJECTION_ITEMS)
        .collect();
    let selector: serde_json::Map<String, Value> = object
        .pointer("/spec/selector")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|selector| selector.iter())
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(|value| (bounded_string(key), Value::String(bounded_string(value))))
        })
        .take(MAX_ARTIFACT_PROJECTION_ITEMS)
        .collect();
    Some(serde_json::json!({
        "object": {
            "kind": bounded_string(kind),
            "metadata": {
                "name": bounded_string(name),
                "ownerReferences": owners,
            },
            "spec": {"selector": selector},
        }
    }))
}

fn bounded_string(value: &str) -> String {
    let mut end = value.len().min(MAX_ARTIFACT_PROJECTION_STRING_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn artifact_projection_is_bounded_and_keeps_trace_relationships() {
        let long_name = "service".repeat(100);
        let traces: Vec<_> = (0..100)
            .map(|_| {
                json!({
                    "rootServiceName": long_name,
                    "client": long_name,
                    "server": long_name,
                    "ignored": "x".repeat(10_000),
                })
            })
            .collect();
        let projection = artifact_topology_projection(
            "trace_search",
            &json!({"traces": traces, "ignored": "x".repeat(100_000)}),
        )
        .expect("trace projection");
        assert_eq!(projection["traces"].as_array().unwrap().len(), 16);
        assert!(
            projection["traces"][0]["client"].as_str().unwrap().len()
                <= MAX_ARTIFACT_PROJECTION_STRING_BYTES
        );
        assert!(serde_json::to_vec(&projection).unwrap().len() <= MAX_ARTIFACT_PROJECTION_BYTES);
    }

    #[test]
    fn artifact_projection_keeps_kubernetes_ownership_and_selectors() {
        let projection = artifact_topology_projection(
            "k8s_get",
            &json!({
                "object": {
                    "kind": "Service",
                    "metadata": {
                        "name": "orders",
                        "ownerReferences": [{"kind": "Deployment", "name": "orders"}],
                    },
                    "spec": {"selector": {"app": "orders"}},
                    "data": "x".repeat(100_000),
                }
            }),
        )
        .expect("kubernetes projection");
        assert_eq!(projection["object"]["metadata"]["name"], "orders");
        assert_eq!(
            projection["object"]["metadata"]["ownerReferences"][0]["kind"],
            "Deployment"
        );
        assert_eq!(projection["object"]["spec"]["selector"]["app"], "orders");
        assert!(serde_json::to_vec(&projection).unwrap().len() <= MAX_ARTIFACT_PROJECTION_BYTES);
    }
}
