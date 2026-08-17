use std::str::FromStr;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError,
    runtime::{
        ActionId, ApprovalId, ClaimId, EventEnvelope, EvidenceId, IncidentContext, Item,
        StreamKind, Thread, ThreadId, TurnId, TurnInput, WorkspaceId,
    },
    topology::{TopologyQuery, project_topology, query_topology},
};

use super::{ServerState, sse};

pub(crate) fn api_router(state: ServerState) -> Router {
    let api = Router::new()
        .route("/workspaces", get(list_workspaces))
        .route("/extensions", get(list_extensions))
        .route("/skills", get(list_skills))
        .route("/threads", get(list_threads).post(create_thread))
        .route("/threads/{thread_id}", get(get_thread))
        .route("/threads/{thread_id}/turns", post(create_turn))
        .route("/threads/{thread_id}/forks", post(fork_thread))
        .route("/threads/{thread_id}/events", get(sse::thread_events))
        .route("/threads/{thread_id}/topology", get(get_topology))
        .route(
            "/threads/{thread_id}/evidence/{evidence_id}",
            get(get_evidence),
        )
        .route("/artifacts/{sha256}", get(get_artifact))
        .route("/approvals/{approval_id}", post(resolve_approval))
        .route(
            "/threads/{thread_id}/action-plans",
            get(list_action_plans).post(propose_action_plan),
        )
        .route("/actions/{action_id}/approve", post(approve_action))
        .route("/actions/{action_id}/execute", post(execute_action))
        .route("/remediation", get(get_remediation))
        .route("/remediation/kill-switch", post(set_kill_switch))
        .route("/audit", get(list_audit))
        .route("/turns/{turn_id}/interrupt", post(interrupt_turn))
        .route("/turns/{turn_id}/resume", post(resume_turn))
        .route("/turns/{turn_id}/recovery", get(get_recovery))
        .fallback(api_not_found);
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .nest("/api", api.clone())
        .nest("/api/v1", api)
        .with_state(state)
}

async fn api_not_found() -> Response {
    ApiError::not_found("API route not found").into_response()
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<ServerState>) -> impl IntoResponse {
    match state.runtime.store().list_threads().await {
        Ok(threads) => {
            let workspaces = state.runtime.workspaces().iter().count();
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ready",
                    "store": "ok",
                    "threads": threads.len(),
                    "workspaces": workspaces,
                })),
            )
        }
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "store": "error",
                "error": {"code": "storage_error", "message": error.to_string()},
            })),
        ),
    }
}

async fn metrics(State(state): State<ServerState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.runtime.metrics().render_prometheus(),
    )
}

async fn list_workspaces(State(state): State<ServerState>) -> Json<serde_json::Value> {
    Json(json!({ "workspaces": state.runtime.workspaces().summaries() }))
}

#[derive(Default, Deserialize)]
struct WorkspaceQuery {
    workspace_id: Option<String>,
}

async fn list_extensions(
    State(state): State<ServerState>,
    Query(query): Query<WorkspaceQuery>,
) -> Json<serde_json::Value> {
    let extensions = if let Some(workspace_id) = query.workspace_id.as_deref() {
        state
            .runtime
            .extensions()
            .for_workspace(workspace_id)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
    } else {
        state.runtime.extensions().summaries().to_vec()
    };
    Json(json!({ "extensions": extensions }))
}

async fn list_skills(
    State(state): State<ServerState>,
    Query(query): Query<WorkspaceQuery>,
) -> Json<serde_json::Value> {
    Json(json!({
        "skills": state.runtime.skill_summaries(query.workspace_id.as_deref())
    }))
}

async fn list_threads(
    State(state): State<ServerState>,
    Query(query): Query<ThreadListQuery>,
) -> ApiResult<impl IntoResponse> {
    let mut threads = state.runtime.store().list_threads().await?;
    if let Some(workspace_id) = query.workspace_id {
        let workspace_id = WorkspaceId::new(workspace_id);
        workspace_id.validate()?;
        threads.retain(|thread| thread.workspace_id == workspace_id);
    }
    Ok(Json(threads))
}

#[derive(Default, Deserialize)]
struct ThreadListQuery {
    workspace_id: Option<String>,
}

#[derive(Serialize)]
struct CreateThreadResponse {
    id: ThreadId,
    workspace_id: WorkspaceId,
}

#[derive(Default, Deserialize)]
struct CreateThreadRequest {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    incident_context: Option<IncidentContext>,
}

async fn create_thread(
    State(state): State<ServerState>,
    body: Option<Json<CreateThreadRequest>>,
) -> ApiResult<impl IntoResponse> {
    let request = body.map(|Json(request)| request).unwrap_or_default();
    if let Some(context) = &request.incident_context {
        context.validate()?;
    }
    let workspace_id = request
        .workspace_id
        .map(WorkspaceId::new)
        .unwrap_or_default();
    workspace_id.validate()?;
    if !state.runtime.workspaces().is_empty() {
        state.runtime.workspaces().require(&workspace_id)?;
    }
    let thread_id = ThreadId::new();
    let created = state
        .runtime
        .store()
        .create_thread(thread_id.clone(), workspace_id.clone())
        .await?;
    let _ = state.event_hub.sender(&thread_id).send(created);
    Ok((
        StatusCode::CREATED,
        Json(CreateThreadResponse {
            id: thread_id,
            workspace_id,
        }),
    ))
}

async fn get_thread(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
    Query(query): Query<ThreadQuery>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    let store = state.runtime.store();
    let thread = store.get_thread(&thread_id).await?;
    let title = thread.items.iter().find_map(|item| match item {
        Item::UserMessage { content } => Some(content.chars().take(120).collect()),
        _ => None,
    });
    let mut events = store.events_after(&thread_id, query.after).await?;
    if let Some(kind) = query.stream_kind {
        events.retain(|envelope| envelope.stream_kind == kind);
    }
    if let Some(limit) = query.limit {
        events.truncate(limit.max(1));
    }
    Ok(Json(ThreadDetailResponse {
        thread,
        title,
        events,
    }))
}

#[derive(Default, Deserialize)]
struct ThreadQuery {
    #[serde(default)]
    after: u64,
    limit: Option<usize>,
    stream_kind: Option<StreamKind>,
}

#[derive(Serialize)]
struct ThreadDetailResponse {
    #[serde(flatten)]
    thread: Thread,
    title: Option<String>,
    events: Vec<EventEnvelope>,
}

#[derive(Deserialize)]
struct CreateTurnRequest {
    input: String,
    #[serde(default)]
    incident_context: Option<IncidentContext>,
}

#[derive(Serialize)]
struct CreateTurnResponse {
    turn_id: TurnId,
    status: &'static str,
}

async fn create_turn(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
    Json(request): Json<CreateTurnRequest>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    state.runtime.store().get_thread(&thread_id).await?;
    let input = request.input.trim();
    if input.is_empty() {
        return Err(ApiError::bad_request("input must not be empty"));
    }
    if input.len() > 32 * 1024 {
        return Err(ApiError::bad_request("input exceeds 32 KiB"));
    }

    if let Some(context) = &request.incident_context {
        context.validate()?;
    }
    let turn_id = TurnId::new();
    let cancellation = CancellationToken::new();
    let inserted = state
        .active_turns
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(thread_id.clone(), turn_id.clone(), cancellation.clone());
    if !inserted {
        return Err(ApiError::conflict("thread already has an active turn"));
    }

    let runtime = state.runtime.clone();
    let events = state.event_hub.sender(&thread_id);
    let active_turns = state.active_turns.clone();
    let task_thread_id = thread_id.clone();
    let task_turn_id = turn_id.clone();
    let input = TurnInput {
        content: input.to_owned(),
        incident_context: request.incident_context,
    };
    tokio::spawn(async move {
        let result = runtime
            .run_turn(
                task_thread_id.clone(),
                task_turn_id.clone(),
                input,
                events,
                cancellation,
            )
            .await;
        if let Err(error) = result {
            tracing::warn!(
                thread_id = %task_thread_id,
                turn_id = %task_turn_id,
                error = %error,
                "turn finished with an error"
            );
        }
        active_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&task_thread_id, &task_turn_id);
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateTurnResponse {
            turn_id,
            status: "running",
        }),
    ))
}

#[derive(Deserialize)]
struct ApprovalRequest {
    approved: bool,
}

async fn resolve_approval(
    State(state): State<ServerState>,
    Path(approval_id): Path<String>,
    Json(request): Json<ApprovalRequest>,
) -> ApiResult<impl IntoResponse> {
    let approval_id = parse_id::<ApprovalId>("approval", &approval_id)?;
    state
        .runtime
        .resolve_approval(&approval_id, request.approved)
        .await?;
    Ok(Json(json!({
        "approval_id": approval_id,
        "status": if request.approved { "approved" } else { "rejected" }
    })))
}

async fn interrupt_turn(
    State(state): State<ServerState>,
    Path(turn_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let turn_id = parse_id::<TurnId>("turn", &turn_id)?;
    let cancellation = state
        .active_turns
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .cancellation(&turn_id)
        .ok_or_else(|| ApiError::not_found(format!("active turn {turn_id}")))?;
    cancellation.cancel();
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"turn_id": turn_id, "status": "cancelling"})),
    ))
}

#[derive(Deserialize)]
struct ForkRequest {
    at_seq: u64,
    #[serde(default)]
    title: Option<String>,
}

async fn fork_thread(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
    Json(request): Json<ForkRequest>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    let created = state
        .runtime
        .fork_thread(&thread_id, request.at_seq, request.title.clone())
        .await?;
    let _ = state
        .event_hub
        .sender(&created.thread_id)
        .send(created.clone());
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": created.thread_id,
            "parent_thread_id": thread_id,
            "forked_at_seq": request.at_seq,
            "title": request.title,
        })),
    ))
}

async fn resume_turn(
    State(state): State<ServerState>,
    Path(turn_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let turn_id = parse_id::<TurnId>("turn", &turn_id)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ApiError::bad_request("Idempotency-Key is required"))?;
    let report = state.runtime.recovery_report(&turn_id).await?;
    if report.resume_policy == crate::store::ResumePolicy::Reconcile {
        return Err(ApiError::from(OpsCodexError::NeedsReconciliation(
            report.user_action,
        )));
    }
    let thread_id = report.thread_id.clone();
    let cancellation = CancellationToken::new();
    let inserted = state
        .active_turns
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(thread_id.clone(), turn_id.clone(), cancellation.clone());
    if !inserted {
        return Err(ApiError::conflict("thread already has an active turn"));
    }
    let runtime = state.runtime.clone();
    let events = state.event_hub.sender(&thread_id);
    let active_turns = state.active_turns.clone();
    let task_thread_id = thread_id.clone();
    let task_turn_id = turn_id.clone();
    tokio::spawn(async move {
        let result = runtime
            .resume_turn(
                task_turn_id.clone(),
                events,
                cancellation,
                Some(idempotency_key),
            )
            .await;
        if let Err(error) = result {
            tracing::warn!(
                thread_id = %task_thread_id,
                turn_id = %task_turn_id,
                error = %error,
                "resume finished with an error"
            );
        }
        active_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&task_thread_id, &task_turn_id);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "turn_id": turn_id,
            "status": "resuming",
            "recovery": report,
        })),
    ))
}

async fn get_recovery(
    State(state): State<ServerState>,
    Path(turn_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let turn_id = parse_id::<TurnId>("turn", &turn_id)?;
    Ok(Json(state.runtime.recovery_report(&turn_id).await?))
}

#[derive(Deserialize)]
struct ProposeActionRequest {
    kind: String,
    #[serde(default)]
    parameters: serde_json::Value,
    #[serde(default)]
    claim_ids: Vec<String>,
}

async fn list_action_plans(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    state.runtime.store().get_thread(&thread_id).await?;
    Ok(Json(json!({
        "actions": state.runtime.list_thread_actions(&thread_id).await?
    })))
}

async fn propose_action_plan(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
    Json(request): Json<ProposeActionRequest>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    if request.kind.trim().is_empty() {
        return Err(ApiError::bad_request("kind must not be empty"));
    }
    let mut claim_ids = Vec::new();
    for claim_id in request.claim_ids {
        claim_ids.push(parse_id::<ClaimId>("claim", &claim_id)?);
    }
    let after = state
        .runtime
        .store()
        .last_seq(&thread_id)
        .await
        .unwrap_or(0);
    let action = state
        .runtime
        .propose_action_plan(
            &thread_id,
            &request.kind,
            request.parameters,
            claim_ids,
            CancellationToken::new(),
        )
        .await?;
    publish_new_events(&state, &thread_id, after).await?;
    Ok((StatusCode::CREATED, Json(action)))
}

#[derive(Deserialize)]
struct ApproveActionRequest {
    #[serde(default)]
    request_hash: String,
    approved: bool,
}

async fn approve_action(
    State(state): State<ServerState>,
    Path(action_id): Path<String>,
    Json(request): Json<ApproveActionRequest>,
) -> ApiResult<impl IntoResponse> {
    if request.request_hash.trim().is_empty() {
        return Err(ApiError::bad_request(
            "request_hash is required to bind an approval",
        ));
    }
    let action_id = parse_id::<ActionId>("action", &action_id)?;
    let existing = state
        .runtime
        .store()
        .get_action(&action_id)
        .await?
        .ok_or_else(|| OpsCodexError::NotFound(format!("action {action_id}")))?;
    let after = state
        .runtime
        .store()
        .last_seq(&existing.thread_id)
        .await
        .unwrap_or(0);
    let action = state
        .runtime
        .approve_action(&action_id, &request.request_hash, request.approved)
        .await?;
    publish_new_events(&state, &action.thread_id, after).await?;
    Ok(Json(action))
}

async fn execute_action(
    State(state): State<ServerState>,
    Path(action_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let action_id = parse_id::<ActionId>("action", &action_id)?;
    let existing = state
        .runtime
        .store()
        .get_action(&action_id)
        .await?
        .ok_or_else(|| OpsCodexError::NotFound(format!("action {action_id}")))?;
    let after = state
        .runtime
        .store()
        .last_seq(&existing.thread_id)
        .await
        .unwrap_or(0);
    let action = state
        .runtime
        .execute_action(&action_id, CancellationToken::new())
        .await?;
    publish_new_events(&state, &action.thread_id, after).await?;
    Ok(Json(action))
}

#[derive(Deserialize)]
struct KillSwitchRequest {
    enabled: bool,
}

async fn get_remediation(State(state): State<ServerState>) -> Json<serde_json::Value> {
    Json(json!({
        "enabled": state.runtime.remediation.enabled,
        "kill_switch": state.runtime.kill_switch(),
        "mutations": state.runtime.mutation_count(),
    }))
}

async fn set_kill_switch(
    State(state): State<ServerState>,
    Json(request): Json<KillSwitchRequest>,
) -> Json<serde_json::Value> {
    state.runtime.set_kill_switch(request.enabled);
    Json(json!({
        "enabled": state.runtime.remediation.enabled,
        "kill_switch": state.runtime.kill_switch(),
        "mutations": state.runtime.mutation_count(),
    }))
}

async fn list_audit(State(state): State<ServerState>) -> ApiResult<impl IntoResponse> {
    state.runtime.verify_audit_log().await?;
    Ok(Json(json!({
        "records": state.runtime.store().list_audit().await?
    })))
}

async fn publish_new_events(
    state: &ServerState,
    thread_id: &ThreadId,
    after: u64,
) -> ApiResult<()> {
    let events = state.runtime.store().events_after(thread_id, after).await?;
    let sender = state.event_hub.sender(thread_id);
    for envelope in events {
        let _ = sender.send(envelope);
    }
    Ok(())
}

async fn get_topology(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
    Query(query): Query<TopologyApiQuery>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    let thread = state.runtime.store().get_thread(&thread_id).await?;
    if let Some(workspace_id) = query.workspace_id {
        let workspace_id = WorkspaceId::new(workspace_id);
        crate::workspace::deny_cross_workspace(&workspace_id, &thread.workspace_id, "thread")?;
    }
    let events = state.runtime.store().events_after(&thread_id, 0).await?;
    let graph = query_topology(
        project_topology(&thread.workspace_id, &events),
        TopologyQuery {
            depth: query.depth.unwrap_or(2),
            max_nodes: query.max_nodes.unwrap_or(32),
            include_stale: query.include_stale.unwrap_or(false),
        },
    );
    Ok(Json(graph))
}

#[derive(Default, Deserialize)]
struct TopologyApiQuery {
    workspace_id: Option<String>,
    depth: Option<usize>,
    max_nodes: Option<usize>,
    include_stale: Option<bool>,
}

async fn get_evidence(
    State(state): State<ServerState>,
    Path((thread_id, evidence_id)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    let evidence_id = parse_id::<EvidenceId>("evidence", &evidence_id)?;
    let evidence = if let Some(workspace_id) = query.workspace_id {
        state
            .runtime
            .store()
            .get_evidence_in(&WorkspaceId::new(workspace_id), &thread_id, &evidence_id)
            .await?
    } else {
        state
            .runtime
            .store()
            .get_evidence(&thread_id, &evidence_id)
            .await?
    };
    Ok(Json(evidence))
}

#[derive(Default, Deserialize)]
struct ScopeQuery {
    workspace_id: Option<String>,
}

#[derive(Deserialize)]
struct ArtifactQuery {
    #[serde(default = "default_artifact_bytes")]
    max_bytes: usize,
    #[serde(default)]
    workspace_id: Option<String>,
}

const fn default_artifact_bytes() -> usize {
    64 * 1024
}

async fn get_artifact(
    State(state): State<ServerState>,
    Path(sha256): Path<String>,
    Query(query): Query<ArtifactQuery>,
) -> ApiResult<impl IntoResponse> {
    let workspace_id = query
        .workspace_id
        .unwrap_or_else(|| WorkspaceId::default().as_str().to_owned());
    let bytes = state
        .runtime
        .artifacts()
        .get_in(&workspace_id, &sha256, query.max_bytes)
        .await?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes))
}

pub(crate) fn parse_thread_query(value: &str) -> ApiResult<ThreadId> {
    parse_id("thread", value)
}

fn parse_id<T>(kind: &str, value: &str) -> ApiResult<T>
where
    T: FromStr,
{
    value
        .parse()
        .map_err(|_| ApiError::bad_request(format!("invalid {kind} id")))
}

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
        }
    }
}

impl From<OpsCodexError> for ApiError {
    fn from(error: OpsCodexError) -> Self {
        match error {
            OpsCodexError::NotFound(message) => Self::not_found(message),
            OpsCodexError::TurnAlreadyRunning => {
                Self::conflict("thread already has an active turn")
            }
            OpsCodexError::NeedsReconciliation(message) => Self::conflict(message),
            OpsCodexError::Protocol(message) => Self::bad_request(message),
            OpsCodexError::Policy(message) => Self {
                status: StatusCode::FORBIDDEN,
                code: "forbidden",
                message,
            },
            other => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "internal_error",
                message: other.to_string(),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": {"code": self.code, "message": self.message}})),
        )
            .into_response()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct EventQuery {
    #[serde(default)]
    pub(crate) after: u64,
    pub(crate) stream_kind: Option<StreamKind>,
}
