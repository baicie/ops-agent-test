mod continuity;
mod jsonl;
mod migrate;
mod port;
mod projection;
mod sqlite;

pub use continuity::{
    ApprovalStatus, CheckpointPhase, CheckpointRecord, DurableApproval, Lease, PendingOperation,
    RecoveryReport, ResumePolicy, TurnRecord, approval_request_hash, context_input_hash,
};
pub use jsonl::JsonlStore;
pub use migrate::{
    ExportReport, MigrateOptions, MigrateReport, export_thread_jsonl, migrate_jsonl_to_sqlite,
};
pub use port::{AppendEvent, EventStore, ThreadSummary};
pub use projection::model_items_from_events;
pub use sqlite::{SqliteStore, event_hash};

use std::{path::Path, sync::Arc};

use crate::{OpsCodexError, Result, config::StoreConfig};

pub async fn open_store(config: &StoreConfig, data_dir: &Path) -> Result<Arc<dyn EventStore>> {
    match config.backend.as_str() {
        "jsonl" => {
            let directory = config
                .jsonl_dir
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| data_dir.join("threads"));
            Ok(Arc::new(JsonlStore::new(directory).await?))
        }
        "sqlite" => {
            let path = config
                .sqlite_path
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| data_dir.join("state.sqlite3"));
            warn_if_unmigrated_jsonl(data_dir).await;
            Ok(Arc::new(SqliteStore::open(path).await?))
        }
        other => Err(OpsCodexError::Protocol(format!(
            "unsupported store.backend `{other}` (expected sqlite or jsonl)"
        ))),
    }
}

async fn warn_if_unmigrated_jsonl(data_dir: &Path) {
    let jsonl_dir = data_dir.join("threads");
    let Ok(mut entries) = tokio::fs::read_dir(&jsonl_dir).await else {
        return;
    };
    let mut found = false;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl") {
            found = true;
            break;
        }
    }
    if found {
        tracing::warn!(
            path = %jsonl_dir.display(),
            "JSONL thread logs are present; run `opscodex migrate` to import them into SQLite"
        );
    }
}
