use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::Utc;
use tokio::io::AsyncWriteExt;

use crate::{
    OpsCodexError, Result,
    runtime::{EventEnvelope, ThreadId},
    store::{JsonlStore, SqliteStore, event_hash},
};

#[derive(Clone, Debug, Default)]
pub struct MigrateOptions {
    pub dry_run: bool,
    pub verify: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrateReport {
    pub threads: usize,
    pub events: usize,
    pub hash: String,
    pub backup_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportReport {
    pub thread_id: ThreadId,
    pub events: usize,
    pub path: PathBuf,
}

pub async fn migrate_jsonl_to_sqlite(
    jsonl_dir: impl AsRef<Path>,
    sqlite_path: impl AsRef<Path>,
    options: MigrateOptions,
) -> Result<MigrateReport> {
    let jsonl_dir = jsonl_dir.as_ref();
    let sqlite_path = sqlite_path.as_ref();
    let jsonl = JsonlStore::new(jsonl_dir).await?;
    let summaries = jsonl.list_threads().await?;
    let mut imported = Vec::new();
    for summary in &summaries {
        imported.push(jsonl.events_after(&summary.id, 0).await?);
    }
    let events: usize = imported.iter().map(Vec::len).sum();
    let hash = {
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        for thread in &imported {
            hasher.update(event_hash(thread).as_bytes());
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    if options.dry_run {
        return Ok(MigrateReport {
            threads: imported.len(),
            events,
            hash,
            backup_dir: None,
        });
    }

    let sqlite = SqliteStore::open(sqlite_path).await?;
    if options.verify {
        for thread in &imported {
            let thread_id = &thread[0].thread_id;
            let current = sqlite.events_after(thread_id, 0).await?;
            if event_hash(&current) != event_hash(thread) {
                return Err(OpsCodexError::Storage(format!(
                    "sqlite verification failed for thread {thread_id}"
                )));
            }
        }
        return Ok(MigrateReport {
            threads: imported.len(),
            events,
            hash,
            backup_dir: None,
        });
    }

    for thread in imported {
        sqlite.import_thread(thread).await?;
    }
    let backup_dir = jsonl_dir.join(format!("backup-{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
    tokio::fs::create_dir_all(&backup_dir)
        .await
        .map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to create JSONL backup {}: {error}",
                backup_dir.display()
            ))
        })?;
    let mut entries = tokio::fs::read_dir(jsonl_dir)
        .await
        .map_err(|error| OpsCodexError::Storage(format!("failed to list JSONL store: {error}")))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| OpsCodexError::Storage(format!("failed to read JSONL entry: {error}")))?
    {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let target = backup_dir.join(path.file_name().unwrap_or_default());
        tokio::fs::rename(&path, &target).await.map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to move {} to backup: {error}",
                path.display()
            ))
        })?;
    }
    Ok(MigrateReport {
        threads: summaries.len(),
        events,
        hash,
        backup_dir: Some(backup_dir),
    })
}

pub async fn export_thread_jsonl(
    store: &SqliteStore,
    thread_id: &str,
    out: impl AsRef<Path>,
) -> Result<ExportReport> {
    let thread_id = ThreadId::from_str(thread_id)
        .map_err(|error| OpsCodexError::Protocol(format!("invalid thread id: {error}")))?;
    let events = store.events_after(&thread_id, 0).await?;
    write_jsonl(out.as_ref(), &events).await?;
    Ok(ExportReport {
        thread_id,
        events: events.len(),
        path: out.as_ref().to_path_buf(),
    })
}

pub async fn write_jsonl(path: &Path, events: &[EventEnvelope]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to create export directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let mut file = tokio::fs::File::create(path).await.map_err(|error| {
        OpsCodexError::Storage(format!("failed to create {}: {error}", path.display()))
    })?;
    for envelope in events {
        let mut line = serde_json::to_vec(envelope)
            .map_err(|error| OpsCodexError::Storage(format!("failed to encode event: {error}")))?;
        line.push(b'\n');
        file.write_all(&line).await.map_err(|error| {
            OpsCodexError::Storage(format!("failed to write {}: {error}", path.display()))
        })?;
    }
    file.flush().await.map_err(|error| {
        OpsCodexError::Storage(format!("failed to flush {}: {error}", path.display()))
    })?;
    Ok(())
}
