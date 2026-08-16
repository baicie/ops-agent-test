use opscodex::runtime::{
    EVENT_SCHEMA_VERSION, EventEnvelope, RuntimeEvent, ThreadId, TurnId, WorkspaceId,
};
use serde_json::{Value, json};

#[test]
fn v2_envelopes_include_identity_and_nested_event() {
    let envelope = EventEnvelope::new(
        1,
        ThreadId::new(),
        Some(TurnId::new()),
        RuntimeEvent::user_message("hello"),
    );
    let value = serde_json::to_value(&envelope).unwrap();
    assert_eq!(value["schema_version"], EVENT_SCHEMA_VERSION);
    assert_eq!(value["stream_kind"], "domain");
    assert_eq!(value["workspace_id"], "default");
    assert_eq!(value["type"], "user_message");
    assert_eq!(value["event"]["type"], "user_message");
    assert_eq!(value["event"]["content"], "hello");
    assert!(value.get("event_id").is_some());
}

#[test]
fn v1_jsonl_records_are_read_losslessly() {
    let thread_id = ThreadId::new();
    let turn_id = TurnId::new();
    let v1 = json!({
        "seq": 2,
        "thread_id": thread_id.to_string(),
        "turn_id": turn_id.to_string(),
        "timestamp": "2026-08-16T00:00:00Z",
        "type": "user_message",
        "content": "Why is order-service failing?"
    });
    let envelope: EventEnvelope = serde_json::from_value(v1).unwrap();
    assert_eq!(envelope.schema_version, 1);
    assert_eq!(envelope.seq, 2);
    assert_eq!(envelope.workspace_id.as_str(), "default");
    assert!(matches!(
        envelope.event,
        RuntimeEvent::UserMessage { ref content, .. } if content == "Why is order-service failing?"
    ));
    let again: EventEnvelope =
        serde_json::from_value(serde_json::to_value(&envelope).unwrap()).unwrap();
    assert_eq!(again.event, envelope.event);
}

#[test]
fn unknown_optional_fields_are_ignored() {
    let thread_id = ThreadId::new();
    let event_id = opscodex::runtime::EventId::new();
    let value = json!({
        "schema_version": 2,
        "stream_kind": "domain",
        "event_id": event_id.to_string(),
        "seq": 1,
        "workspace_id": "default",
        "thread_id": thread_id.to_string(),
        "timestamp": "2026-08-16T00:00:00Z",
        "future_field": {"ignored": true},
        "type": "thread_created",
        "event": {"type": "thread_created", "also_ignored": 1}
    });
    let envelope: EventEnvelope = serde_json::from_value(value).unwrap();
    assert!(matches!(envelope.event, RuntimeEvent::ThreadCreated));
}

#[test]
fn assistant_delta_is_delivery_and_tool_proposed_is_domain() {
    let delta = EventEnvelope::new(
        2,
        ThreadId::new(),
        None,
        RuntimeEvent::AssistantDelta { delta: "hi".into() },
    );
    let proposed = EventEnvelope::new(
        3,
        ThreadId::new(),
        None,
        RuntimeEvent::ToolProposed {
            call_id: "c1".into(),
            tool: "promql_query".into(),
            arguments: Value::Null,
        },
    );
    assert_eq!(
        serde_json::to_value(&delta).unwrap()["stream_kind"],
        "delivery"
    );
    assert_eq!(
        serde_json::to_value(&proposed).unwrap()["stream_kind"],
        "domain"
    );
}

#[tokio::test]
async fn v1_tool_started_still_pairs_in_model_history() {
    use opscodex::store::JsonlStore;
    use tempfile::tempdir;

    let directory = tempdir().unwrap();
    let store = JsonlStore::new(directory.path()).await.unwrap();
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await
        .unwrap();
    let path = directory.path().join(format!("{thread_id}.jsonl"));
    let v1_call = json!({
        "seq": 2,
        "thread_id": thread_id.to_string(),
        "timestamp": "2026-08-16T00:00:00Z",
        "type": "tool_started",
        "call_id": "call-1",
        "tool": "promql_query",
        "arguments": {"query": "up"}
    });
    let v1_result = json!({
        "seq": 3,
        "thread_id": thread_id.to_string(),
        "timestamp": "2026-08-16T00:00:01Z",
        "type": "tool_completed",
        "call_id": "call-1",
        "tool": "promql_query",
        "output": {"status": "success"},
        "evidence": {
            "source": "prometheus",
            "query": "up",
            "timestamp": "2026-08-16T00:00:01Z",
            "duration_ms": 4,
            "truncated": false
        },
        "success": true
    });
    let mut existing = tokio::fs::read(&path).await.unwrap();
    existing.extend_from_slice(&serde_json::to_vec(&v1_call).unwrap());
    existing.push(b'\n');
    existing.extend_from_slice(&serde_json::to_vec(&v1_result).unwrap());
    existing.push(b'\n');
    tokio::fs::write(&path, existing).await.unwrap();

    let history = store.model_history(&thread_id, 10).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(matches!(
        history[0],
        opscodex::model::ModelItem::ToolCall { .. }
    ));
    assert!(matches!(
        history[1],
        opscodex::model::ModelItem::ToolResult { .. }
    ));
}
