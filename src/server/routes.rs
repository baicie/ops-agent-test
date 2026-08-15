use std::str::FromStr;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError,
    runtime::{ApprovalId, EventEnvelope, Item, Thread, ThreadId, TurnId},
};

use super::{ServerState, sse};

pub(crate) fn api_router(state: ServerState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/api/threads", get(list_threads).post(create_thread))
        .route("/api/threads/{thread_id}", get(get_thread))
        .route("/api/threads/{thread_id}/turns", post(create_turn))
        .route("/api/threads/{thread_id}/events", get(sse::thread_events))
        .route("/api/approvals/{approval_id}", post(resolve_approval))
        .route("/api/turns/{turn_id}/interrupt", post(interrupt_turn))
        .route("/api", any(api_not_found))
        .route("/api/{*path}", any(api_not_found))
        .with_state(state)
}

async fn api_not_found() -> Response {
    ApiError::not_found("API route not found").into_response()
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok"}))
}

async fn list_threads(State(state): State<ServerState>) -> ApiResult<impl IntoResponse> {
    Ok(Json(state.runtime.store().list_threads().await?))
}

#[derive(Serialize)]
struct CreateThreadResponse {
    id: ThreadId,
}

async fn create_thread(State(state): State<ServerState>) -> ApiResult<impl IntoResponse> {
    let thread_id = ThreadId::new();
    let created = state
        .runtime
        .store()
        .create_thread(thread_id.clone())
        .await?;
    let _ = state.event_hub.sender(&thread_id).send(created);
    Ok((
        StatusCode::CREATED,
        Json(CreateThreadResponse { id: thread_id }),
    ))
}

async fn get_thread(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_id::<ThreadId>("thread", &thread_id)?;
    let store = state.runtime.store();
    let thread = store.get_thread(&thread_id).await?;
    let title = thread.items.iter().find_map(|item| match item {
        Item::UserMessage { content } => Some(content.chars().take(120).collect()),
        _ => None,
    });
    let events = store.events_after(&thread_id, 0).await?;
    Ok(Json(ThreadDetailResponse {
        thread,
        title,
        events,
    }))
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
    let input = input.to_owned();
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
        .policy()
        .broker()
        .resolve(&approval_id, request.approved)?;
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
            OpsCodexError::Protocol(message) => Self::bad_request(message),
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
}
