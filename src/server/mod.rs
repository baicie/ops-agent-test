mod routes;
mod sse;

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use axum::Router;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::runtime::{AgentRuntime, EventEnvelope, ThreadId, TurnId};

#[derive(Clone)]
pub struct ServerState {
    pub(crate) runtime: Arc<AgentRuntime>,
    pub(crate) event_hub: Arc<EventHub>,
    pub(crate) active_turns: Arc<Mutex<ActiveTurns>>,
}

impl ServerState {
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        Self {
            runtime,
            event_hub: Arc::new(EventHub::default()),
            active_turns: Arc::new(Mutex::new(ActiveTurns::default())),
        }
    }

    pub fn runtime(&self) -> Arc<AgentRuntime> {
        self.runtime.clone()
    }
}

#[derive(Default)]
pub(crate) struct EventHub {
    senders: Mutex<HashMap<ThreadId, broadcast::Sender<EventEnvelope>>>,
}

impl EventHub {
    pub(crate) fn sender(&self, thread_id: &ThreadId) -> broadcast::Sender<EventEnvelope> {
        self.senders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(thread_id.clone())
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }
}

#[derive(Default)]
pub(crate) struct ActiveTurns {
    by_thread: HashMap<ThreadId, TurnId>,
    by_turn: HashMap<TurnId, CancellationToken>,
}

impl ActiveTurns {
    pub(crate) fn insert(
        &mut self,
        thread_id: ThreadId,
        turn_id: TurnId,
        cancellation: CancellationToken,
    ) -> bool {
        if self.by_thread.contains_key(&thread_id) {
            return false;
        }
        self.by_thread.insert(thread_id, turn_id.clone());
        self.by_turn.insert(turn_id, cancellation);
        true
    }

    pub(crate) fn remove(&mut self, thread_id: &ThreadId, turn_id: &TurnId) {
        if self.by_thread.get(thread_id) == Some(turn_id) {
            self.by_thread.remove(thread_id);
        }
        self.by_turn.remove(turn_id);
    }

    pub(crate) fn cancellation(&self, turn_id: &TurnId) -> Option<CancellationToken> {
        self.by_turn.get(turn_id).cloned()
    }
}

pub fn router(state: ServerState) -> Router {
    routes::api_router(state).layer(TraceLayer::new_for_http())
}

pub fn router_with_web(state: ServerState, web_directory: impl AsRef<Path>) -> Router {
    let web_directory = web_directory.as_ref();
    router(state).fallback_service(
        ServeDir::new(web_directory)
            .append_index_html_on_directories(true)
            .fallback(ServeFile::new(web_directory.join("index.html"))),
    )
}
