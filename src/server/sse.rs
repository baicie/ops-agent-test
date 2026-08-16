use std::{convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use tokio::sync::broadcast;

use crate::{runtime::EventEnvelope, telemetry::RuntimeMetrics};

use super::{
    ServerState,
    routes::{ApiResult, EventQuery, parse_thread_query},
};

pub(crate) async fn thread_events(
    State(state): State<ServerState>,
    Path(thread_id): Path<String>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let thread_id = parse_thread_query(&thread_id)?;
    state.runtime.store().get_thread(&thread_id).await?;
    let sender = state.event_hub.sender(&thread_id);
    let mut receiver = sender.subscribe();
    let store = state.runtime.store();
    let header_after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let after = query.after.max(header_after);
    let mut replay = store.events_after(&thread_id, after).await?;
    if let Some(kind) = query.stream_kind {
        replay.retain(|envelope| envelope.stream_kind == kind);
    }
    let stream_kind = query.stream_kind;
    let metrics = state.runtime.metrics();
    RuntimeMetrics::add(&metrics.sse_replay_events, replay.len() as u64);
    let output = stream! {
        let mut last_seq = after;
        for envelope in replay {
            last_seq = last_seq.max(envelope.seq);
            yield Ok::<Event, Infallible>(sse_event(envelope));
        }
        loop {
            match receiver.recv().await {
                Ok(envelope) if envelope.seq > last_seq
                    && stream_kind.is_none_or(|kind| envelope.stream_kind == kind) => {
                    last_seq = envelope.seq;
                    yield Ok::<Event, Infallible>(sse_event(envelope));
                }
                Ok(envelope) if envelope.seq > last_seq => {
                    last_seq = envelope.seq;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    RuntimeMetrics::inc(&metrics.sse_lag_recoveries);
                    match store.events_after(&thread_id, last_seq).await {
                        Ok(mut missed) => {
                            if let Some(kind) = stream_kind {
                                missed.retain(|envelope| envelope.stream_kind == kind);
                            }
                            for envelope in missed {
                                last_seq = last_seq.max(envelope.seq);
                                yield Ok::<Event, Infallible>(sse_event(envelope));
                            }
                        }
                        Err(error) => {
                            tracing::warn!(thread_id = %thread_id, error = %error, "SSE replay failed");
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn sse_event(envelope: EventEnvelope) -> Event {
    Event::default()
        .id(envelope.seq.to_string())
        .event(envelope.event.event_name())
        .json_data(envelope)
        .expect("EventEnvelope is serializable")
}
