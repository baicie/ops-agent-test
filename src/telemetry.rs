use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct RuntimeMetrics {
    pub turns_started: AtomicU64,
    pub turns_completed: AtomicU64,
    pub turns_failed: AtomicU64,
    pub turns_cancelled: AtomicU64,
    pub model_calls: AtomicU64,
    pub model_errors: AtomicU64,
    pub model_latency_ms_sum: AtomicU64,
    pub tool_calls: AtomicU64,
    pub tool_errors: AtomicU64,
    pub tool_latency_ms_sum: AtomicU64,
    pub store_appends: AtomicU64,
    pub store_errors: AtomicU64,
    pub sse_replay_events: AtomicU64,
    pub sse_lag_recoveries: AtomicU64,
    pub queue_waiters: AtomicU64,
}

impl RuntimeMetrics {
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }

    pub fn set_queue_waiters(&self, value: u64) {
        self.queue_waiters.store(value, Ordering::Relaxed);
    }

    pub fn render_prometheus(&self) -> String {
        let mut lines = vec![
            "# HELP opscodex_turns_total Agent turns by terminal status.".to_owned(),
            "# TYPE opscodex_turns_total counter".to_owned(),
            metric(
                "opscodex_turns_total",
                "status",
                "started",
                &self.turns_started,
            ),
            metric(
                "opscodex_turns_total",
                "status",
                "completed",
                &self.turns_completed,
            ),
            metric(
                "opscodex_turns_total",
                "status",
                "failed",
                &self.turns_failed,
            ),
            metric(
                "opscodex_turns_total",
                "status",
                "cancelled",
                &self.turns_cancelled,
            ),
            "# HELP opscodex_model_calls_total Model completions.".to_owned(),
            "# TYPE opscodex_model_calls_total counter".to_owned(),
            metric(
                "opscodex_model_calls_total",
                "status",
                "ok",
                &self.model_calls,
            ),
            metric(
                "opscodex_model_calls_total",
                "status",
                "error",
                &self.model_errors,
            ),
            "# HELP opscodex_model_latency_ms_sum Model completion latency.".to_owned(),
            "# TYPE opscodex_model_latency_ms_sum counter".to_owned(),
            format!(
                "opscodex_model_latency_ms_sum {}",
                self.model_latency_ms_sum.load(Ordering::Relaxed)
            ),
            "# HELP opscodex_tool_calls_total Tool executions.".to_owned(),
            "# TYPE opscodex_tool_calls_total counter".to_owned(),
            metric(
                "opscodex_tool_calls_total",
                "status",
                "ok",
                &self.tool_calls,
            ),
            metric(
                "opscodex_tool_calls_total",
                "status",
                "error",
                &self.tool_errors,
            ),
            "# HELP opscodex_tool_latency_ms_sum Tool execution latency.".to_owned(),
            "# TYPE opscodex_tool_latency_ms_sum counter".to_owned(),
            format!(
                "opscodex_tool_latency_ms_sum {}",
                self.tool_latency_ms_sum.load(Ordering::Relaxed)
            ),
            "# HELP opscodex_store_appends_total Event store appends.".to_owned(),
            "# TYPE opscodex_store_appends_total counter".to_owned(),
            metric(
                "opscodex_store_appends_total",
                "status",
                "ok",
                &self.store_appends,
            ),
            metric(
                "opscodex_store_appends_total",
                "status",
                "error",
                &self.store_errors,
            ),
            "# HELP opscodex_sse_replay_events_total Events replayed to SSE clients.".to_owned(),
            "# TYPE opscodex_sse_replay_events_total counter".to_owned(),
            format!(
                "opscodex_sse_replay_events_total {}",
                self.sse_replay_events.load(Ordering::Relaxed)
            ),
            "# HELP opscodex_sse_lag_recoveries_total SSE lag recoveries from the event store."
                .to_owned(),
            "# TYPE opscodex_sse_lag_recoveries_total counter".to_owned(),
            format!(
                "opscodex_sse_lag_recoveries_total {}",
                self.sse_lag_recoveries.load(Ordering::Relaxed)
            ),
            "# HELP opscodex_queue_waiters Current waiters for a global turn slot.".to_owned(),
            "# TYPE opscodex_queue_waiters gauge".to_owned(),
            format!(
                "opscodex_queue_waiters {}",
                self.queue_waiters.load(Ordering::Relaxed)
            ),
        ];
        lines.push(String::new());
        lines.join("\n")
    }

    pub fn uses_high_cardinality_labels(&self) -> bool {
        let rendered = self.render_prometheus();
        rendered.contains("thread_id=")
            || rendered.contains("turn_id=")
            || rendered.contains("evidence_id=")
    }
}

fn metric(name: &str, label: &str, value: &str, counter: &AtomicU64) -> String {
    format!(
        "{name}{{{label}=\"{value}\"}} {}",
        counter.load(Ordering::Relaxed)
    )
}
