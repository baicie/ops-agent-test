use std::sync::Arc;

use chrono::Utc;
use opscodex::{
    model::ModelItem,
    runtime::{EvidenceMeta, RuntimeEvent, ThreadId, TurnId},
    store::JsonlStore,
};
use serde_json::json;
use tempfile::tempdir;

fn evidence(source: &str) -> EvidenceMeta {
    EvidenceMeta {
        source: source.into(),
        query: None,
        timestamp: Utc::now(),
        duration_ms: 1,
        truncated: false,
    }
}

#[tokio::test]
async fn concurrent_appends_have_monotonic_per_thread_sequences() {
    let directory = tempdir().unwrap();
    let store = Arc::new(JsonlStore::new(directory.path()).await.unwrap());
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await.unwrap();

    let mut tasks = Vec::new();
    for index in 0..16 {
        let store = store.clone();
        let thread_id = thread_id.clone();
        tasks.push(tokio::spawn(async move {
            store
                .append(
                    &thread_id,
                    None,
                    RuntimeEvent::UserMessage {
                        content: format!("message-{index}"),
                    },
                )
                .await
                .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    let events = store.events_after(&thread_id, 0).await.unwrap();
    let sequences: Vec<_> = events.iter().map(|event| event.seq).collect();
    assert_eq!(sequences, (1..=17).collect::<Vec<_>>());
}

#[tokio::test]
async fn history_uses_completed_items_and_ignores_streaming_deltas() {
    let directory = tempdir().unwrap();
    let store = JsonlStore::new(directory.path()).await.unwrap();
    let thread_id = ThreadId::new();
    let turn_id = TurnId::new();
    store.create_thread(thread_id.clone()).await.unwrap();

    let events = [
        RuntimeEvent::UserMessage {
            content: "What happened?".into(),
        },
        RuntimeEvent::AssistantDelta {
            delta: "Checking".into(),
        },
        RuntimeEvent::ToolStarted {
            call_id: "call-1".into(),
            tool: "promql_query".into(),
            arguments: json!({"query": "up"}),
        },
        RuntimeEvent::ToolCompleted {
            call_id: "call-1".into(),
            tool: "promql_query".into(),
            output: json!({"status": "success"}),
            evidence: evidence("prometheus"),
            success: true,
        },
        RuntimeEvent::AssistantCompleted {
            content: "The service is down.".into(),
        },
    ];
    for event in events {
        store
            .append(&thread_id, Some(turn_id.clone()), event)
            .await
            .unwrap();
    }

    let history = store.model_history(&thread_id, 100).await.unwrap();
    assert_eq!(history.len(), 4);
    assert!(matches!(history[0], ModelItem::UserMessage { .. }));
    assert!(matches!(history[1], ModelItem::ToolCall { .. }));
    assert!(matches!(history[2], ModelItem::ToolResult { .. }));
    assert!(matches!(history[3], ModelItem::AssistantMessage { .. }));
}

#[tokio::test]
async fn history_limit_keeps_function_call_and_output_together() {
    let directory = tempdir().unwrap();
    let store = JsonlStore::new(directory.path()).await.unwrap();
    let thread_id = ThreadId::new();
    let turn_id = TurnId::new();
    store.create_thread(thread_id.clone()).await.unwrap();

    for event in [
        RuntimeEvent::UserMessage {
            content: "check service".into(),
        },
        RuntimeEvent::ToolStarted {
            call_id: "call-1".into(),
            tool: "promql_query".into(),
            arguments: json!({"query": "up"}),
        },
        RuntimeEvent::ToolCompleted {
            call_id: "call-1".into(),
            tool: "promql_query".into(),
            output: json!({"status": "success"}),
            evidence: evidence("prometheus"),
            success: true,
        },
    ] {
        store
            .append(&thread_id, Some(turn_id.clone()), event)
            .await
            .unwrap();
    }

    let history = store.model_history(&thread_id, 2).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(matches!(history[0], ModelItem::ToolCall { .. }));
    assert!(matches!(history[1], ModelItem::ToolResult { .. }));

    // A one-item budget cannot represent a function exchange, so the pair is
    // dropped instead of sending an orphaned output to the Responses API.
    let history = store.model_history(&thread_id, 1).await.unwrap();
    assert!(history.is_empty());
}

#[tokio::test]
async fn history_drops_unmatched_tool_items_from_interrupted_turns() {
    let directory = tempdir().unwrap();
    let store = JsonlStore::new(directory.path()).await.unwrap();
    let thread_id = ThreadId::new();
    let turn_id = TurnId::new();
    store.create_thread(thread_id.clone()).await.unwrap();

    for event in [
        RuntimeEvent::UserMessage {
            content: "diagnose".into(),
        },
        RuntimeEvent::ToolStarted {
            call_id: "unfinished-call".into(),
            tool: "exec".into(),
            arguments: json!({"command": "uptime"}),
        },
        RuntimeEvent::ToolCompleted {
            call_id: "unknown-call".into(),
            tool: "exec".into(),
            output: json!({"error": "missing call"}),
            evidence: evidence("exec"),
            success: false,
        },
        RuntimeEvent::AssistantCompleted {
            content: "Evidence is incomplete.".into(),
        },
    ] {
        store
            .append(&thread_id, Some(turn_id.clone()), event)
            .await
            .unwrap();
    }

    let history = store.model_history(&thread_id, 100).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(matches!(history[0], ModelItem::UserMessage { .. }));
    assert!(matches!(history[1], ModelItem::AssistantMessage { .. }));
}

#[tokio::test]
async fn replay_ignores_a_partial_tail_but_rejects_a_bad_completed_line() {
    let directory = tempdir().unwrap();
    let store = JsonlStore::new(directory.path()).await.unwrap();
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await.unwrap();
    let path = directory.path().join(format!("{thread_id}.jsonl"));

    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    file.write_all(br#"{"seq":2,"type":"user_mes"#)
        .await
        .unwrap();
    drop(file);

    let events = store.events_after(&thread_id, 0).await.unwrap();
    assert_eq!(events.len(), 1);

    let malformed_thread = ThreadId::new();
    store.create_thread(malformed_thread.clone()).await.unwrap();
    let malformed_path = directory.path().join(format!("{malformed_thread}.jsonl"));
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(malformed_path)
        .await
        .unwrap();
    file.write_all(b"not-json\n").await.unwrap();
    drop(file);

    assert!(store.events_after(&malformed_thread, 0).await.is_err());
}

#[tokio::test]
async fn append_recovers_from_a_partial_tail_without_reusing_a_sequence() {
    let directory = tempdir().unwrap();
    let store = JsonlStore::new(directory.path()).await.unwrap();
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await.unwrap();
    let path = directory.path().join(format!("{thread_id}.jsonl"));

    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .await
        .unwrap();
    file.write_all(br#"{"seq":2,"thread_id"#).await.unwrap();
    drop(file);

    let appended = store
        .append(
            &thread_id,
            None,
            RuntimeEvent::UserMessage {
                content: "recovered".into(),
            },
        )
        .await
        .unwrap();
    let events = store.events_after(&thread_id, 0).await.unwrap();

    assert_eq!(appended.seq, 2);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[1].event,
        RuntimeEvent::UserMessage { content } if content == "recovered"
    ));
}

#[tokio::test]
async fn thread_summaries_reconstruct_title_and_current_status() {
    let directory = tempdir().unwrap();
    let store = JsonlStore::new(directory.path()).await.unwrap();
    let thread_id = ThreadId::new();
    let turn_id = TurnId::new();
    store.create_thread(thread_id.clone()).await.unwrap();
    store
        .append(
            &thread_id,
            Some(turn_id.clone()),
            RuntimeEvent::UserMessage {
                content: "Why is order-service failing?".into(),
            },
        )
        .await
        .unwrap();
    store
        .append(&thread_id, Some(turn_id), RuntimeEvent::TurnStarted)
        .await
        .unwrap();

    let summary = store.summarize_thread(&thread_id).await.unwrap();
    let listed = store.list_threads().await.unwrap();

    assert_eq!(
        summary.title.as_deref(),
        Some("Why is order-service failing?")
    );
    assert_eq!(summary.status, opscodex::runtime::ThreadStatus::Running);
    assert_eq!(listed, vec![summary]);
}
