use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::evidence::{Diagnosis, EvidenceMeta};

use super::{ApprovalId, EventId, IncidentContext, ItemId, ThreadId, TurnId, WorkspaceId};

pub const EVENT_SCHEMA_VERSION: u32 = 2;
const V1_EVENT_NAMESPACE: Uuid = uuid::uuid!("7c3d2e91-5a4b-4f6c-8d1e-0a9b8c7d6e5f");

const ENVELOPE_KEYS: &[&str] = &[
    "schema_version",
    "stream_kind",
    "event_id",
    "seq",
    "workspace_id",
    "thread_id",
    "turn_id",
    "item_id",
    "causation_id",
    "timestamp",
    "event",
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    #[default]
    Domain,
    Delivery,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    ThreadCreated,
    UserMessage {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        incident_context: Option<IncidentContext>,
    },
    TurnStarted,
    AssistantDelta {
        delta: String,
    },
    AssistantCompleted {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnosis: Option<Diagnosis>,
    },
    ToolStarted {
        call_id: String,
        tool: String,
        arguments: Value,
    },
    ToolProposed {
        call_id: String,
        tool: String,
        arguments: Value,
    },
    ToolAuthorized {
        call_id: String,
        tool: String,
        decision: String,
    },
    ToolExecutionStarted {
        call_id: String,
        tool: String,
    },
    ToolCompleted {
        call_id: String,
        tool: String,
        output: Value,
        evidence: EvidenceMeta,
        success: bool,
    },
    ApprovalRequired {
        approval_id: ApprovalId,
        tool: String,
        arguments: Value,
    },
    ApprovalResolved {
        approval_id: ApprovalId,
        approved: bool,
    },
    TurnCompleted,
    TurnFailed {
        error: String,
    },
    TurnCancelled,
    ContextCompacted {
        summary_id: String,
        covers_seq_start: u64,
        covers_seq_end: u64,
        source_item_ids: Vec<String>,
        source_evidence_ids: Vec<String>,
        input_hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_version: Option<String>,
        summary: String,
    },
}

impl RuntimeEvent {
    pub fn user_message(content: impl Into<String>) -> Self {
        Self::UserMessage {
            content: content.into(),
            incident_context: None,
        }
    }

    pub fn assistant_completed(content: impl Into<String>) -> Self {
        Self::AssistantCompleted {
            content: content.into(),
            diagnosis: None,
        }
    }

    pub fn event_name(&self) -> &'static str {
        match self {
            Self::ThreadCreated => "thread_created",
            Self::UserMessage { .. } => "user_message",
            Self::TurnStarted => "turn_started",
            Self::AssistantDelta { .. } => "assistant_delta",
            Self::AssistantCompleted { .. } => "assistant_completed",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolProposed { .. } => "tool_proposed",
            Self::ToolAuthorized { .. } => "tool_authorized",
            Self::ToolExecutionStarted { .. } => "tool_execution_started",
            Self::ToolCompleted { .. } => "tool_completed",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::TurnCompleted => "turn_completed",
            Self::TurnFailed { .. } => "turn_failed",
            Self::TurnCancelled => "turn_cancelled",
            Self::ContextCompacted { .. } => "context_compacted",
        }
    }

    pub fn is_tool_proposal(&self) -> bool {
        matches!(self, Self::ToolStarted { .. } | Self::ToolProposed { .. })
    }

    pub fn tool_call_parts(&self) -> Option<(&str, &str, &Value)> {
        match self {
            Self::ToolStarted {
                call_id,
                tool,
                arguments,
            }
            | Self::ToolProposed {
                call_id,
                tool,
                arguments,
            } => Some((call_id, tool, arguments)),
            _ => None,
        }
    }
}

pub fn stream_kind_for(event: &RuntimeEvent) -> StreamKind {
    match event {
        RuntimeEvent::AssistantDelta { .. } => StreamKind::Delivery,
        _ => StreamKind::Domain,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventEnvelope {
    pub schema_version: u32,
    pub stream_kind: StreamKind,
    pub event_id: EventId,
    pub seq: u64,
    pub workspace_id: WorkspaceId,
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub item_id: Option<ItemId>,
    pub causation_id: Option<EventId>,
    pub timestamp: DateTime<Utc>,
    pub event: RuntimeEvent,
}

impl EventEnvelope {
    pub fn new(
        seq: u64,
        thread_id: ThreadId,
        turn_id: Option<TurnId>,
        event: RuntimeEvent,
    ) -> Self {
        Self::with_causation(seq, thread_id, turn_id, None, None, event)
    }

    pub fn with_causation(
        seq: u64,
        thread_id: ThreadId,
        turn_id: Option<TurnId>,
        item_id: Option<ItemId>,
        causation_id: Option<EventId>,
        event: RuntimeEvent,
    ) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            stream_kind: stream_kind_for(&event),
            event_id: EventId::new(),
            seq,
            workspace_id: WorkspaceId::default(),
            thread_id,
            turn_id,
            item_id,
            causation_id,
            timestamp: Utc::now(),
            event,
        }
    }

    pub fn with_workspace(mut self, workspace_id: WorkspaceId) -> Self {
        self.workspace_id = workspace_id;
        self
    }
}

impl Serialize for EventEnvelope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("schema_version", &self.schema_version)?;
        map.serialize_entry("stream_kind", &self.stream_kind)?;
        map.serialize_entry("event_id", &self.event_id)?;
        map.serialize_entry("seq", &self.seq)?;
        map.serialize_entry("workspace_id", &self.workspace_id)?;
        map.serialize_entry("thread_id", &self.thread_id)?;
        if let Some(turn_id) = &self.turn_id {
            map.serialize_entry("turn_id", turn_id)?;
        }
        if let Some(item_id) = &self.item_id {
            map.serialize_entry("item_id", item_id)?;
        }
        if let Some(causation_id) = &self.causation_id {
            map.serialize_entry("causation_id", causation_id)?;
        }
        map.serialize_entry("timestamp", &self.timestamp)?;
        map.serialize_entry("type", &self.event.event_name())?;
        map.serialize_entry("event", &self.event)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for EventEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        from_value(value).map_err(serde::de::Error::custom)
    }
}

fn from_value(value: Value) -> Result<EventEnvelope, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "event envelope must be an object".to_owned())?;
    let schema_version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    let seq = object
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| "event envelope missing seq".to_owned())?;
    let thread_id: ThreadId = serde_json::from_value(
        object
            .get("thread_id")
            .cloned()
            .ok_or_else(|| "event envelope missing thread_id".to_owned())?,
    )
    .map_err(|error| error.to_string())?;
    let turn_id = object
        .get("turn_id")
        .cloned()
        .filter(|value| !value.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error: serde_json::Error| error.to_string())?;
    let item_id = object
        .get("item_id")
        .cloned()
        .filter(|value| !value.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error: serde_json::Error| error.to_string())?;
    let causation_id = object
        .get("causation_id")
        .cloned()
        .filter(|value| !value.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error: serde_json::Error| error.to_string())?;
    let timestamp = object
        .get("timestamp")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error: serde_json::Error| error.to_string())?
        .unwrap_or_else(Utc::now);
    let workspace_id = object
        .get("workspace_id")
        .and_then(Value::as_str)
        .map(WorkspaceId::new)
        .unwrap_or_default();
    let event = parse_event(&value)?;
    let stream_kind = object
        .get("stream_kind")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error: serde_json::Error| error.to_string())?
        .unwrap_or_else(|| stream_kind_for(&event));
    let event_id = object
        .get("event_id")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error: serde_json::Error| error.to_string())?
        .unwrap_or_else(|| v1_event_id(&thread_id, seq));
    Ok(EventEnvelope {
        schema_version,
        stream_kind,
        event_id,
        seq,
        workspace_id,
        thread_id,
        turn_id,
        item_id,
        causation_id,
        timestamp,
        event,
    })
}

fn parse_event(value: &Value) -> Result<RuntimeEvent, String> {
    if let Some(nested) = value.get("event")
        && nested.is_object()
    {
        return serde_json::from_value(nested.clone()).map_err(|error| error.to_string());
    }
    let mut event_object = serde_json::Map::new();
    if let Some(object) = value.as_object() {
        for (key, child) in object {
            if !ENVELOPE_KEYS.contains(&key.as_str()) {
                event_object.insert(key.clone(), child.clone());
            }
        }
    }
    serde_json::from_value(Value::Object(event_object)).map_err(|error| error.to_string())
}

fn v1_event_id(thread_id: &ThreadId, seq: u64) -> EventId {
    EventId::from_uuid(Uuid::new_v5(
        &V1_EVENT_NAMESPACE,
        format!("{thread_id}:{seq}").as_bytes(),
    ))
}
