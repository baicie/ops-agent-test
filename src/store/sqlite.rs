use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    model::ModelItem,
    runtime::{
        ApprovalId, ContextBudget, EventEnvelope, EvidenceId, RuntimeEvent, StreamKind, Thread,
        ThreadId, ThreadStatus, TurnId, TurnStatus, WorkspaceId,
    },
    store::{
        AppendEvent, ApprovalStatus, CheckpointPhase, CheckpointRecord, DurableApproval,
        EventStore, Lease, PendingOperation, ResumePolicy, ThreadSummary, TurnRecord,
        projection::{
            model_items_from_events, reconstruct_thread, status_after, summary_from_thread,
            title_from_event,
        },
    },
};

pub const SCHEMA_VERSION: i64 = 1;

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    checksum TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    parent_thread_id TEXT,
    forked_at_seq INTEGER,
    status TEXT NOT NULL,
    title TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(id)
);
CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    status TEXT NOT NULL,
    active_lease_id TEXT,
    last_checkpoint_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(thread_id) REFERENCES threads(id)
);
CREATE INDEX IF NOT EXISTS idx_turns_status ON turns(status);
CREATE TABLE IF NOT EXISTS events (
    thread_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    event_id TEXT NOT NULL UNIQUE,
    schema_version INTEGER NOT NULL,
    stream_kind TEXT NOT NULL,
    turn_id TEXT,
    item_id TEXT,
    causation_id TEXT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (thread_id, seq),
    FOREIGN KEY(thread_id) REFERENCES threads(id)
);
CREATE INDEX IF NOT EXISTS idx_events_thread_seq ON events(thread_id, seq);
CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at DESC);
CREATE TABLE IF NOT EXISTS evidence (
    evidence_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    artifact_ref TEXT,
    sha256 TEXT,
    meta TEXT NOT NULL,
    FOREIGN KEY(thread_id) REFERENCES threads(id)
);
CREATE TABLE IF NOT EXISTS checkpoints (
    checkpoint_id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    step INTEGER NOT NULL,
    phase TEXT NOT NULL,
    state TEXT NOT NULL,
    input_hash TEXT,
    last_committed_seq INTEGER NOT NULL,
    resume_policy TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_checkpoints_turn ON checkpoints(turn_id, created_at DESC);
CREATE TABLE IF NOT EXISTS approvals (
    approval_id TEXT PRIMARY KEY,
    thread_id TEXT,
    turn_id TEXT,
    tool TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    status TEXT NOT NULL,
    expires_at TEXT,
    payload TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS leases (
    lease_id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL UNIQUE,
    thread_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    fencing_token INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS resume_ops (
    idempotency_key TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);
"#;

struct StoreLock {
    _file: File,
}

struct Inner {
    path: PathBuf,
    conn: Mutex<Connection>,
    _lock: StoreLock,
}

#[derive(Clone)]
pub struct SqliteStore {
    inner: Arc<Inner>,
}

impl SqliteStore {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                OpsCodexError::Storage(format!(
                    "failed to create sqlite directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        tokio::task::spawn_blocking(move || open_blocking(path))
            .await
            .map_err(|error| OpsCodexError::Storage(format!("sqlite open join failed: {error}")))?
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    async fn with_conn<T, F>(&self, function: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            function(&mut conn)
        })
        .await
        .map_err(|error| OpsCodexError::Storage(format!("sqlite worker join failed: {error}")))?
    }

    pub async fn create_thread(
        &self,
        thread_id: ThreadId,
        workspace_id: WorkspaceId,
    ) -> Result<EventEnvelope> {
        workspace_id.validate()?;
        self.with_conn(move |conn| {
            let envelope =
                EventEnvelope::new(1, thread_id.clone(), None, RuntimeEvent::ThreadCreated)
                    .with_workspace(workspace_id.clone());
            let tx = conn
                .transaction()
                .map_err(|error| sqlite_error("begin", error))?;
            insert_workspace(&tx, &workspace_id, envelope.timestamp)?;
            insert_thread(&tx, &envelope, None, None, ThreadStatus::Idle, None)?;
            insert_event(&tx, &envelope)?;
            tx.commit().map_err(|error| sqlite_error("commit", error))?;
            Ok(envelope)
        })
        .await
    }

    pub async fn append_event(&self, command: AppendEvent) -> Result<EventEnvelope> {
        self.with_conn(move |conn| append_event(conn, command))
            .await
    }

    pub async fn events_after(
        &self,
        thread_id: &ThreadId,
        after_seq: u64,
    ) -> Result<Vec<EventEnvelope>> {
        let thread_id = thread_id.clone();
        self.with_conn(move |conn| load_events(conn, &thread_id, after_seq))
            .await
    }

    pub async fn import_thread(&self, events: Vec<EventEnvelope>) -> Result<()> {
        self.with_conn(move |conn| import_thread(conn, events))
            .await
    }

    pub async fn fork_thread(
        &self,
        thread_id: ThreadId,
        at_seq: u64,
        new_thread_id: ThreadId,
        title: Option<String>,
    ) -> Result<EventEnvelope> {
        self.with_conn(move |conn| fork_thread(conn, &thread_id, at_seq, new_thread_id, title))
            .await
    }

    pub async fn summarize_thread(&self, thread_id: &ThreadId) -> Result<ThreadSummary> {
        Ok(summary_from_thread(self.get_thread(thread_id).await?))
    }
}

fn open_blocking(path: PathBuf) -> Result<SqliteStore> {
    let lock = acquire_lock(&path)?;
    let conn = Connection::open(&path).map_err(|error| sqlite_error("open", error))?;
    conn.busy_timeout(Duration::from_millis(5_000))
        .map_err(|error| sqlite_error("busy_timeout", error))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| sqlite_error("journal_mode", error))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| sqlite_error("foreign_keys", error))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| sqlite_error("synchronous", error))?;
    apply_schema(&conn)?;
    Ok(SqliteStore {
        inner: Arc::new(Inner {
            path,
            conn: Mutex::new(conn),
            _lock: lock,
        }),
    })
}

fn acquire_lock(db_path: &Path) -> Result<StoreLock> {
    let lock_path = db_path.with_extension("sqlite3.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to open store lock {}: {error}",
                lock_path.display()
            ))
        })?;
    try_exclusive_lock(&file)?;
    Ok(StoreLock { _file: file })
}

fn try_exclusive_lock(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(OpsCodexError::Storage(
                "another OpsCodex process is using this SQLite store".into(),
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

fn apply_schema(conn: &Connection) -> Result<()> {
    let checksum = schema_checksum();
    conn.execute_batch(SCHEMA_SQL)
        .map_err(|error| sqlite_error("schema", error))?;
    let applied: Option<String> = conn
        .query_row(
            "SELECT checksum FROM schema_migrations WHERE version = ?1",
            params![SCHEMA_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite_error("schema_migrations", error))?;
    match applied {
        Some(existing) if existing == checksum => Ok(()),
        Some(_) => Err(OpsCodexError::Storage(
            "sqlite schema checksum mismatch; refusing to start".into(),
        )),
        None => {
            conn.execute(
                "INSERT INTO schema_migrations(version, checksum, applied_at) VALUES (?1, ?2, ?3)",
                params![SCHEMA_VERSION, checksum, Utc::now().to_rfc3339()],
            )
            .map_err(|error| sqlite_error("schema_migrations insert", error))?;
            Ok(())
        }
    }
}

pub fn schema_checksum() -> String {
    let digest = Sha256::digest(SCHEMA_SQL.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn append_event(conn: &mut Connection, command: AppendEvent) -> Result<EventEnvelope> {
    let tx = conn
        .transaction()
        .map_err(|error| sqlite_error("begin", error))?;
    let thread_id = command.thread_id.to_string();
    let (last_seq, workspace_id, status, title): (i64, String, String, Option<String>) = tx
        .query_row(
            "SELECT
                COALESCE((SELECT MAX(seq) FROM events WHERE thread_id = ?1), 0),
                workspace_id, status, title
             FROM threads WHERE id = ?1",
            params![thread_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                OpsCodexError::NotFound(format!("thread {}", command.thread_id))
            }
            other => sqlite_error("thread lookup", other),
        })?;
    let seq = u64::try_from(last_seq)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| {
            OpsCodexError::Storage(format!(
                "thread {} sequence is exhausted",
                command.thread_id
            ))
        })?;
    let mut envelope = EventEnvelope::with_causation(
        seq,
        command.thread_id.clone(),
        command.turn_id,
        command.item_id,
        command.causation_id,
        command.event,
    );
    if let Some(stream_kind) = command.stream_kind {
        envelope.stream_kind = stream_kind;
    }
    envelope.workspace_id = WorkspaceId::new(workspace_id);
    let next_status = status_after(parse_status(&status), &envelope.event);
    let next_title = title.or_else(|| title_from_event(&envelope.event));
    insert_event(&tx, &envelope)?;
    tx.execute(
        "UPDATE threads SET status = ?2, title = ?3, updated_at = ?4 WHERE id = ?1",
        params![
            thread_id,
            status_sql(next_status),
            next_title,
            envelope.timestamp.to_rfc3339()
        ],
    )
    .map_err(|error| sqlite_error("update thread", error))?;
    if let RuntimeEvent::ToolCompleted {
        evidence, success, ..
    } = &envelope.event
        && *success
    {
        upsert_evidence(&tx, &envelope.thread_id, envelope.seq, evidence)?;
    }
    tx.commit().map_err(|error| sqlite_error("commit", error))?;
    Ok(envelope)
}

fn import_thread(conn: &mut Connection, events: Vec<EventEnvelope>) -> Result<()> {
    let first = events
        .first()
        .ok_or_else(|| OpsCodexError::Storage("cannot import an empty thread".into()))?;
    let thread_id = first.thread_id.clone();
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM threads WHERE id = ?1",
            params![thread_id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("import exists", error))?;
    if exists > 0 {
        let current = load_events(conn, &thread_id, 0)?;
        if event_hash(&current) != event_hash(&events) {
            return Err(OpsCodexError::Storage(format!(
                "thread {thread_id} already exists with different content"
            )));
        }
        return Ok(());
    }
    let tx = conn
        .transaction()
        .map_err(|error| sqlite_error("begin import", error))?;
    insert_workspace(&tx, &first.workspace_id, first.timestamp)?;
    let reconstructed = reconstruct_thread(&thread_id, &events)?;
    let title = reconstructed.items.iter().find_map(|item| match item {
        crate::runtime::Item::UserMessage { content } => Some(content.chars().take(120).collect()),
        _ => None,
    });
    insert_thread(&tx, first, None, None, reconstructed.status, title)?;
    tx.execute(
        "UPDATE threads SET created_at = ?2, updated_at = ?3 WHERE id = ?1",
        params![
            thread_id.to_string(),
            reconstructed.created_at.to_rfc3339(),
            reconstructed.updated_at.to_rfc3339()
        ],
    )
    .map_err(|error| sqlite_error("import timestamps", error))?;
    for envelope in &events {
        insert_event(&tx, envelope)?;
        if let RuntimeEvent::ToolCompleted {
            evidence, success, ..
        } = &envelope.event
            && *success
        {
            upsert_evidence(&tx, &envelope.thread_id, envelope.seq, evidence)?;
        }
    }
    tx.commit()
        .map_err(|error| sqlite_error("commit import", error))?;
    Ok(())
}

pub fn event_hash(events: &[EventEnvelope]) -> String {
    let mut hasher = Sha256::new();
    for envelope in events {
        hasher.update(envelope.seq.to_string().as_bytes());
        hasher.update(envelope.event_id.to_string().as_bytes());
        hasher.update(serde_json::to_vec(&envelope.event).unwrap_or_default());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fork_thread(
    conn: &mut Connection,
    thread_id: &ThreadId,
    at_seq: u64,
    new_thread_id: ThreadId,
    title: Option<String>,
) -> Result<EventEnvelope> {
    if at_seq == 0 {
        return Err(OpsCodexError::Protocol(
            "fork requires a durable seq greater than zero".into(),
        ));
    }
    let events = load_events(conn, thread_id, 0)?;
    if events.is_empty() {
        return Err(OpsCodexError::NotFound(format!("thread {thread_id}")));
    }
    if events.last().map(|event| event.seq).unwrap_or(0) < at_seq {
        return Err(OpsCodexError::Protocol(format!(
            "fork seq {at_seq} is past the last committed event"
        )));
    }
    let inherited: Vec<_> = events
        .into_iter()
        .filter(|envelope| envelope.seq <= at_seq && inheritable(&envelope.event))
        .collect();
    let workspace_id = inherited
        .first()
        .map(|envelope| envelope.workspace_id.clone())
        .unwrap_or_default();
    let tx = conn
        .transaction()
        .map_err(|error| sqlite_error("begin fork", error))?;
    let created = EventEnvelope::new(1, new_thread_id.clone(), None, RuntimeEvent::ThreadCreated)
        .with_workspace(workspace_id.clone());
    insert_workspace(&tx, &workspace_id, created.timestamp)?;
    insert_thread(
        &tx,
        &created,
        Some(thread_id),
        Some(at_seq),
        ThreadStatus::Idle,
        title.clone(),
    )?;
    insert_event(&tx, &created)?;
    let mut seq = 1_u64;
    let mut status = ThreadStatus::Idle;
    let mut derived_title = title;
    for source in inherited.into_iter().skip(1) {
        seq += 1;
        let mut envelope = EventEnvelope::with_causation(
            seq,
            new_thread_id.clone(),
            None,
            source.item_id.clone(),
            source.causation_id.clone(),
            source.event.clone(),
        );
        envelope.workspace_id = workspace_id.clone();
        envelope.stream_kind = source.stream_kind;
        status = status_after(status, &envelope.event);
        derived_title = derived_title.or_else(|| title_from_event(&envelope.event));
        insert_event(&tx, &envelope)?;
        if let RuntimeEvent::ToolCompleted {
            evidence, success, ..
        } = &envelope.event
            && *success
        {
            upsert_evidence(&tx, &envelope.thread_id, envelope.seq, evidence)?;
        }
    }
    tx.execute(
        "UPDATE threads SET status = ?2, title = ?3, updated_at = ?4 WHERE id = ?1",
        params![
            new_thread_id.to_string(),
            status_sql(status),
            derived_title,
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(|error| sqlite_error("update forked thread", error))?;
    tx.commit()
        .map_err(|error| sqlite_error("commit fork", error))?;
    Ok(created)
}

fn inheritable(event: &RuntimeEvent) -> bool {
    !matches!(
        event,
        RuntimeEvent::AssistantDelta { .. }
            | RuntimeEvent::ApprovalRequired { .. }
            | RuntimeEvent::ApprovalResolved { .. }
            | RuntimeEvent::TurnStarted
            | RuntimeEvent::TurnCompleted
            | RuntimeEvent::TurnCancelled
            | RuntimeEvent::TurnFailed { .. }
            | RuntimeEvent::ToolAuthorized { .. }
            | RuntimeEvent::ToolExecutionStarted { .. }
    )
}

fn insert_workspace(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &WorkspaceId,
    created_at: DateTime<Utc>,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO workspaces(id, created_at) VALUES (?1, ?2)",
        params![workspace_id.as_str(), created_at.to_rfc3339()],
    )
    .map_err(|error| sqlite_error("insert workspace", error))?;
    Ok(())
}

fn insert_thread(
    tx: &rusqlite::Transaction<'_>,
    envelope: &EventEnvelope,
    parent: Option<&ThreadId>,
    forked_at_seq: Option<u64>,
    status: ThreadStatus,
    title: Option<String>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO threads(id, workspace_id, parent_thread_id, forked_at_seq, status, title, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            envelope.thread_id.to_string(),
            envelope.workspace_id.as_str(),
            parent.map(ToString::to_string),
            forked_at_seq.map(|seq| i64::try_from(seq).unwrap_or(i64::MAX)),
            status_sql(status),
            title,
            envelope.timestamp.to_rfc3339(),
            envelope.timestamp.to_rfc3339()
        ],
    )
    .map_err(|error| {
        if error.to_string().contains("UNIQUE") {
            OpsCodexError::Storage(format!("thread {} already exists", envelope.thread_id))
        } else {
            sqlite_error("insert thread", error)
        }
    })?;
    Ok(())
}

fn insert_event(tx: &rusqlite::Transaction<'_>, envelope: &EventEnvelope) -> Result<()> {
    let payload = serde_json::to_string(envelope)
        .map_err(|error| OpsCodexError::Storage(format!("failed to encode event: {error}")))?;
    tx.execute(
        "INSERT INTO events(
            thread_id, seq, event_id, schema_version, stream_kind, turn_id, item_id,
            causation_id, timestamp, event_type, payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            envelope.thread_id.to_string(),
            envelope.seq as i64,
            envelope.event_id.to_string(),
            envelope.schema_version as i64,
            stream_kind_sql(envelope.stream_kind),
            envelope.turn_id.as_ref().map(ToString::to_string),
            envelope.item_id.as_ref().map(ToString::to_string),
            envelope.causation_id.as_ref().map(ToString::to_string),
            envelope.timestamp.to_rfc3339(),
            envelope.event.event_name(),
            payload
        ],
    )
    .map_err(|error| sqlite_error("insert event", error))?;
    Ok(())
}

fn upsert_evidence(
    tx: &rusqlite::Transaction<'_>,
    thread_id: &ThreadId,
    seq: u64,
    evidence: &EvidenceMeta,
) -> Result<()> {
    let id = evidence.evidence_id_or_synthesize(thread_id, seq);
    let meta = serde_json::to_string(evidence)
        .map_err(|error| OpsCodexError::Storage(format!("failed to encode evidence: {error}")))?;
    tx.execute(
        "INSERT OR REPLACE INTO evidence(evidence_id, thread_id, artifact_ref, sha256, meta)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id.to_string(),
            thread_id.to_string(),
            evidence.artifact_ref.clone(),
            evidence.sha256.clone(),
            meta
        ],
    )
    .map_err(|error| sqlite_error("upsert evidence", error))?;
    Ok(())
}

fn load_events(
    conn: &Connection,
    thread_id: &ThreadId,
    after_seq: u64,
) -> Result<Vec<EventEnvelope>> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM threads WHERE id = ?1",
            params![thread_id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("thread exists", error))?;
    if exists == 0 {
        return Err(OpsCodexError::NotFound(format!("thread {thread_id}")));
    }
    let mut statement = conn
        .prepare("SELECT payload FROM events WHERE thread_id = ?1 AND seq > ?2 ORDER BY seq ASC")
        .map_err(|error| sqlite_error("prepare events", error))?;
    let rows = statement
        .query_map(params![thread_id.to_string(), after_seq as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| sqlite_error("query events", error))?;
    let mut events = Vec::new();
    for row in rows {
        let payload = row.map_err(|error| sqlite_error("event row", error))?;
        let envelope: EventEnvelope = serde_json::from_str(&payload)
            .map_err(|error| OpsCodexError::Storage(format!("invalid sqlite event: {error}")))?;
        events.push(envelope);
    }
    Ok(events)
}

fn load_lineage(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<(Option<ThreadId>, Option<u64>)> {
    conn.query_row(
        "SELECT parent_thread_id, forked_at_seq FROM threads WHERE id = ?1",
        params![thread_id.to_string()],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<i64>>(1)?,
            ))
        },
    )
    .map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => {
            OpsCodexError::NotFound(format!("thread {thread_id}"))
        }
        other => sqlite_error("lineage", other),
    })
    .and_then(|(parent, seq)| {
        Ok((
            parent
                .map(|value| value.parse())
                .transpose()
                .map_err(|error| {
                    OpsCodexError::Storage(format!("invalid parent thread id: {error}"))
                })?,
            seq.map(|value| u64::try_from(value).unwrap_or(0)),
        ))
    })
}

fn list_threads(conn: &Connection) -> Result<Vec<ThreadSummary>> {
    let mut statement = conn
        .prepare(
            "SELECT id, workspace_id, status, title, created_at, updated_at, parent_thread_id, forked_at_seq
             FROM threads
             ORDER BY updated_at DESC, id DESC",
        )
        .map_err(|error| sqlite_error("prepare list", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })
        .map_err(|error| sqlite_error("query list", error))?;
    let mut summaries = Vec::new();
    for row in rows {
        let (id, workspace_id, status, title, created_at, updated_at, parent, forked_at_seq) =
            row.map_err(|error| sqlite_error("list row", error))?;
        summaries.push(ThreadSummary {
            id: id.parse().map_err(|error| {
                OpsCodexError::Storage(format!("invalid thread id {id}: {error}"))
            })?,
            workspace_id: WorkspaceId::new(workspace_id),
            status: parse_status(&status),
            title,
            created_at: parse_time(&created_at)?,
            updated_at: parse_time(&updated_at)?,
            parent_thread_id: parent
                .map(|value| value.parse())
                .transpose()
                .map_err(|error| {
                    OpsCodexError::Storage(format!("invalid parent thread id: {error}"))
                })?,
            forked_at_seq: forked_at_seq.map(|value| u64::try_from(value).unwrap_or(0)),
        });
    }
    Ok(summaries)
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OpsCodexError::Storage(format!("invalid timestamp: {error}")))
}

fn parse_status(value: &str) -> ThreadStatus {
    match value {
        "running" => ThreadStatus::Running,
        "waiting_approval" => ThreadStatus::WaitingApproval,
        "interrupted" => ThreadStatus::Interrupted,
        "needs_reconciliation" => ThreadStatus::NeedsReconciliation,
        "failed" => ThreadStatus::Failed,
        _ => ThreadStatus::Idle,
    }
}

fn status_sql(status: ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Idle => "idle",
        ThreadStatus::Running => "running",
        ThreadStatus::WaitingApproval => "waiting_approval",
        ThreadStatus::Interrupted => "interrupted",
        ThreadStatus::NeedsReconciliation => "needs_reconciliation",
        ThreadStatus::Failed => "failed",
    }
}

fn stream_kind_sql(kind: StreamKind) -> &'static str {
    match kind {
        StreamKind::Domain => "domain",
        StreamKind::Delivery => "delivery",
    }
}

fn turn_status_sql(status: TurnStatus) -> &'static str {
    match status {
        TurnStatus::Running => "running",
        TurnStatus::WaitingApproval => "waiting_approval",
        TurnStatus::Completed => "completed",
        TurnStatus::Failed => "failed",
        TurnStatus::Cancelled => "cancelled",
        TurnStatus::Interrupted => "interrupted",
        TurnStatus::NeedsReconciliation => "needs_reconciliation",
    }
}

fn parse_turn_status(value: &str) -> TurnStatus {
    match value {
        "waiting_approval" => TurnStatus::WaitingApproval,
        "completed" => TurnStatus::Completed,
        "failed" => TurnStatus::Failed,
        "cancelled" => TurnStatus::Cancelled,
        "interrupted" => TurnStatus::Interrupted,
        "needs_reconciliation" => TurnStatus::NeedsReconciliation,
        _ => TurnStatus::Running,
    }
}

fn sqlite_error(action: &str, error: rusqlite::Error) -> OpsCodexError {
    OpsCodexError::Storage(format!("sqlite {action} failed: {error}"))
}

fn upsert_turn(conn: &Connection, record: TurnRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO turns(id, thread_id, status, active_lease_id, last_checkpoint_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            status = excluded.status,
            active_lease_id = excluded.active_lease_id,
            last_checkpoint_id = excluded.last_checkpoint_id,
            updated_at = excluded.updated_at",
        params![
            record.id.to_string(),
            record.thread_id.to_string(),
            turn_status_sql(record.status),
            record.active_lease_id,
            record.last_checkpoint_id,
            record.created_at.to_rfc3339(),
            record.updated_at.to_rfc3339()
        ],
    )
    .map_err(|error| sqlite_error("upsert turn", error))?;
    if matches!(
        record.status,
        TurnStatus::Interrupted | TurnStatus::NeedsReconciliation | TurnStatus::WaitingApproval
    ) {
        conn.execute(
            "UPDATE threads SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![
                record.thread_id.to_string(),
                status_sql(match record.status {
                    TurnStatus::WaitingApproval => ThreadStatus::WaitingApproval,
                    TurnStatus::NeedsReconciliation => ThreadStatus::NeedsReconciliation,
                    _ => ThreadStatus::Interrupted,
                }),
                record.updated_at.to_rfc3339()
            ],
        )
        .map_err(|error| sqlite_error("sync thread status", error))?;
    }
    Ok(())
}

fn get_turn(conn: &Connection, turn_id: &TurnId) -> Result<Option<TurnRecord>> {
    conn.query_row(
        "SELECT id, thread_id, status, active_lease_id, last_checkpoint_id, created_at, updated_at
         FROM turns WHERE id = ?1",
        params![turn_id.to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        },
    )
    .optional()
    .map_err(|error| sqlite_error("get turn", error))?
    .map(
        |(id, thread_id, status, lease, checkpoint, created, updated)| {
            Ok(TurnRecord {
                id: id
                    .parse()
                    .map_err(|error| OpsCodexError::Storage(format!("invalid turn id: {error}")))?,
                thread_id: thread_id.parse().map_err(|error| {
                    OpsCodexError::Storage(format!("invalid thread id: {error}"))
                })?,
                status: parse_turn_status(&status),
                active_lease_id: lease,
                last_checkpoint_id: checkpoint,
                created_at: parse_time(&created)?,
                updated_at: parse_time(&updated)?,
            })
        },
    )
    .transpose()
}

fn list_open_turns(conn: &Connection) -> Result<Vec<TurnRecord>> {
    let mut statement = conn
        .prepare(
            "SELECT id, thread_id, status, active_lease_id, last_checkpoint_id, created_at, updated_at
             FROM turns
             WHERE status NOT IN ('completed', 'failed', 'cancelled')",
        )
        .map_err(|error| sqlite_error("prepare open turns", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| sqlite_error("query open turns", error))?;
    let mut turns = Vec::new();
    for row in rows {
        let (id, thread_id, status, lease, checkpoint, created, updated) =
            row.map_err(|error| sqlite_error("open turn row", error))?;
        turns.push(TurnRecord {
            id: id
                .parse()
                .map_err(|error| OpsCodexError::Storage(format!("invalid turn id: {error}")))?,
            thread_id: thread_id
                .parse()
                .map_err(|error| OpsCodexError::Storage(format!("invalid thread id: {error}")))?,
            status: parse_turn_status(&status),
            active_lease_id: lease,
            last_checkpoint_id: checkpoint,
            created_at: parse_time(&created)?,
            updated_at: parse_time(&updated)?,
        });
    }
    Ok(turns)
}

fn put_checkpoint(conn: &mut Connection, checkpoint: CheckpointRecord) -> Result<()> {
    let state =
        serde_json::to_string(&checkpoint.pending_operation).unwrap_or_else(|_| "null".into());
    conn.execute(
        "INSERT INTO checkpoints(
            checkpoint_id, turn_id, thread_id, step, phase, state, input_hash,
            last_committed_seq, resume_policy, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            checkpoint.checkpoint_id,
            checkpoint.turn_id.to_string(),
            checkpoint.thread_id.to_string(),
            checkpoint.step as i64,
            checkpoint.phase.as_str(),
            state,
            checkpoint.context_input_hash,
            checkpoint.last_committed_seq as i64,
            checkpoint.resume_policy.as_str(),
            checkpoint.created_at.to_rfc3339()
        ],
    )
    .map_err(|error| sqlite_error("insert checkpoint", error))?;
    conn.execute(
        "UPDATE turns SET last_checkpoint_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![
            checkpoint.turn_id.to_string(),
            checkpoint.checkpoint_id,
            checkpoint.created_at.to_rfc3339()
        ],
    )
    .map_err(|error| sqlite_error("checkpoint turn", error))?;
    Ok(())
}

fn last_checkpoint(conn: &Connection, turn_id: &TurnId) -> Result<Option<CheckpointRecord>> {
    conn.query_row(
        "SELECT checkpoint_id, turn_id, thread_id, step, phase, state, input_hash,
                last_committed_seq, resume_policy, created_at
         FROM checkpoints WHERE turn_id = ?1 ORDER BY created_at DESC, step DESC LIMIT 1",
        params![turn_id.to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        },
    )
    .optional()
    .map_err(|error| sqlite_error("last checkpoint", error))?
    .map(
        |(id, turn, thread, step, phase, state, hash, seq, policy, created)| {
            Ok(CheckpointRecord {
                checkpoint_id: id,
                turn_id: turn
                    .parse()
                    .map_err(|error| OpsCodexError::Storage(format!("invalid turn id: {error}")))?,
                thread_id: thread.parse().map_err(|error| {
                    OpsCodexError::Storage(format!("invalid thread id: {error}"))
                })?,
                step: u32::try_from(step).unwrap_or(0),
                phase: CheckpointPhase::parse(&phase),
                pending_operation: serde_json::from_str::<Option<PendingOperation>>(&state)
                    .unwrap_or(None),
                context_input_hash: hash,
                last_committed_seq: u64::try_from(seq).unwrap_or(0),
                resume_policy: ResumePolicy::parse(&policy),
                created_at: parse_time(&created)?,
            })
        },
    )
    .transpose()
}

fn put_approval(conn: &Connection, approval: DurableApproval) -> Result<()> {
    let payload = serde_json::to_string(&approval)
        .map_err(|error| OpsCodexError::Storage(format!("failed to encode approval: {error}")))?;
    conn.execute(
        "INSERT INTO approvals(approval_id, thread_id, turn_id, tool, request_hash, status, expires_at, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(approval_id) DO UPDATE SET
            status = excluded.status,
            expires_at = excluded.expires_at,
            payload = excluded.payload",
        params![
            approval.approval_id.to_string(),
            approval.thread_id.as_ref().map(ToString::to_string),
            approval.turn_id.as_ref().map(ToString::to_string),
            approval.tool,
            approval.request_hash,
            approval.status.as_str(),
            approval.expires_at.map(|value| value.to_rfc3339()),
            payload
        ],
    )
    .map_err(|error| sqlite_error("put approval", error))?;
    Ok(())
}

fn decode_approval(payload: String) -> Result<DurableApproval> {
    serde_json::from_str(&payload)
        .map_err(|error| OpsCodexError::Storage(format!("invalid approval payload: {error}")))
}

fn get_approval(conn: &Connection, id: &ApprovalId) -> Result<Option<DurableApproval>> {
    conn.query_row(
        "SELECT payload FROM approvals WHERE approval_id = ?1",
        params![id.to_string()],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| sqlite_error("get approval", error))?
    .map(decode_approval)
    .transpose()
}

fn list_pending_approvals(conn: &Connection) -> Result<Vec<DurableApproval>> {
    let mut statement = conn
        .prepare("SELECT payload FROM approvals WHERE status = 'pending'")
        .map_err(|error| sqlite_error("prepare approvals", error))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| sqlite_error("query approvals", error))?;
    let mut approvals = Vec::new();
    for row in rows {
        approvals.push(decode_approval(
            row.map_err(|error| sqlite_error("approval row", error))?,
        )?);
    }
    Ok(approvals)
}

fn approval_for_turn(conn: &Connection, turn_id: &TurnId) -> Result<Option<DurableApproval>> {
    let mut statement = conn
        .prepare("SELECT payload FROM approvals WHERE turn_id = ?1")
        .map_err(|error| sqlite_error("prepare turn approvals", error))?;
    let rows = statement
        .query_map(params![turn_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(|error| sqlite_error("query turn approvals", error))?;
    let mut approvals = Vec::new();
    for row in rows {
        approvals.push(decode_approval(
            row.map_err(|error| sqlite_error("turn approval row", error))?,
        )?);
    }
    approvals.sort_by_key(|approval| match approval.status {
        ApprovalStatus::Pending => 0,
        ApprovalStatus::Approved => 1,
        ApprovalStatus::Rejected => 2,
        ApprovalStatus::Expired => 3,
    });
    Ok(approvals.into_iter().next())
}

fn acquire_lease(
    conn: &Connection,
    turn_id: &TurnId,
    thread_id: &ThreadId,
    owner_id: &str,
    ttl: Duration,
) -> Result<Lease> {
    let now = Utc::now();
    let existing: Option<(String, String, String, i64)> = conn
        .query_row(
            "SELECT lease_id, owner_id, expires_at, fencing_token FROM leases WHERE turn_id = ?1",
            params![turn_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| sqlite_error("lookup lease", error))?;
    if let Some((lease_id, owner, expires_at, token)) = existing {
        let expires = parse_time(&expires_at)?;
        if expires > now {
            return Err(OpsCodexError::TurnAlreadyRunning);
        }
        let next = Lease {
            lease_id,
            turn_id: turn_id.clone(),
            thread_id: thread_id.clone(),
            owner_id: owner_id.to_owned(),
            expires_at: now
                + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(30)),
            fencing_token: token + 1,
        };
        let _ = owner;
        conn.execute(
            "UPDATE leases SET owner_id = ?2, expires_at = ?3, fencing_token = ?4, thread_id = ?5
             WHERE lease_id = ?1",
            params![
                next.lease_id,
                next.owner_id,
                next.expires_at.to_rfc3339(),
                next.fencing_token,
                next.thread_id.to_string()
            ],
        )
        .map_err(|error| sqlite_error("update lease", error))?;
        return Ok(next);
    }
    let lease = Lease {
        lease_id: uuid::Uuid::now_v7().to_string(),
        turn_id: turn_id.clone(),
        thread_id: thread_id.clone(),
        owner_id: owner_id.to_owned(),
        expires_at: now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(30)),
        fencing_token: 1,
    };
    conn.execute(
        "INSERT INTO leases(lease_id, turn_id, thread_id, owner_id, expires_at, fencing_token)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            lease.lease_id,
            lease.turn_id.to_string(),
            lease.thread_id.to_string(),
            lease.owner_id,
            lease.expires_at.to_rfc3339(),
            lease.fencing_token
        ],
    )
    .map_err(|error| sqlite_error("insert lease", error))?;
    Ok(lease)
}

fn refresh_lease(
    conn: &Connection,
    lease_id: &str,
    fencing_token: i64,
    ttl: Duration,
) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE leases SET expires_at = ?3 WHERE lease_id = ?1 AND fencing_token = ?2",
            params![
                lease_id,
                fencing_token,
                (Utc::now()
                    + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(30)))
                .to_rfc3339()
            ],
        )
        .map_err(|error| sqlite_error("refresh lease", error))?;
    if changed == 0 {
        return Err(OpsCodexError::Storage(
            "lease fencing token mismatch".into(),
        ));
    }
    Ok(())
}

fn release_lease(conn: &Connection, lease_id: &str, fencing_token: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM leases WHERE lease_id = ?1 AND fencing_token = ?2",
        params![lease_id, fencing_token],
    )
    .map_err(|error| sqlite_error("release lease", error))?;
    Ok(())
}

fn remember_resume(
    conn: &Connection,
    key: &str,
    turn_id: &TurnId,
    payload: &str,
) -> Result<Option<String>> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT payload FROM resume_ops WHERE idempotency_key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite_error("resume lookup", error))?;
    if let Some(existing) = existing {
        return Ok(Some(existing));
    }
    conn.execute(
        "INSERT INTO resume_ops(idempotency_key, turn_id, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![key, turn_id.to_string(), payload, Utc::now().to_rfc3339()],
    )
    .map_err(|error| sqlite_error("resume insert", error))?;
    Ok(None)
}

#[async_trait::async_trait]
impl EventStore for SqliteStore {
    async fn create_thread(
        &self,
        thread_id: ThreadId,
        workspace_id: WorkspaceId,
    ) -> Result<EventEnvelope> {
        SqliteStore::create_thread(self, thread_id, workspace_id).await
    }

    async fn append(
        &self,
        thread_id: &ThreadId,
        turn_id: Option<TurnId>,
        event: RuntimeEvent,
    ) -> Result<EventEnvelope> {
        self.append_event(AppendEvent::new(thread_id.clone(), turn_id, event))
            .await
    }

    async fn append_event(&self, command: AppendEvent) -> Result<EventEnvelope> {
        SqliteStore::append_event(self, command).await
    }

    async fn events_after(
        &self,
        thread_id: &ThreadId,
        after_seq: u64,
    ) -> Result<Vec<EventEnvelope>> {
        SqliteStore::events_after(self, thread_id, after_seq).await
    }

    async fn get_thread(&self, thread_id: &ThreadId) -> Result<Thread> {
        let thread_id = thread_id.clone();
        self.with_conn(move |conn| {
            let events = load_events(conn, &thread_id, 0)?;
            let mut thread = reconstruct_thread(&thread_id, &events)?;
            let (parent, seq) = load_lineage(conn, &thread_id)?;
            thread.parent_thread_id = parent;
            thread.forked_at_seq = seq;
            Ok(thread)
        })
        .await
    }

    async fn get_thread_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
    ) -> Result<Thread> {
        let thread = self.get_thread(thread_id).await?;
        crate::workspace::deny_cross_workspace(workspace_id, &thread.workspace_id, "thread")?;
        Ok(thread)
    }

    async fn list_threads(&self) -> Result<Vec<ThreadSummary>> {
        self.with_conn(|conn| list_threads(conn)).await
    }

    async fn last_seq(&self, thread_id: &ThreadId) -> Result<u64> {
        let thread_id = thread_id.clone();
        self.with_conn(move |conn| {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM threads WHERE id = ?1",
                    params![thread_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(|error| sqlite_error("last_seq exists", error))?;
            if exists == 0 {
                return Err(OpsCodexError::NotFound(format!("thread {thread_id}")));
            }
            let seq: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM events WHERE thread_id = ?1",
                    params![thread_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(|error| sqlite_error("last_seq", error))?;
            Ok(u64::try_from(seq).unwrap_or(0))
        })
        .await
    }

    async fn model_history(&self, thread_id: &ThreadId, limit: usize) -> Result<Vec<ModelItem>> {
        self.model_context(thread_id, &ContextBudget::items_only(limit))
            .await
    }

    async fn model_context(
        &self,
        thread_id: &ThreadId,
        budget: &ContextBudget,
    ) -> Result<Vec<ModelItem>> {
        let events = self.events_after(thread_id, 0).await?;
        Ok(crate::runtime::build_model_context(
            model_items_from_events(&events),
            budget,
        ))
    }

    async fn get_evidence(
        &self,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta> {
        let events = self.events_after(thread_id, 0).await?;
        for envelope in events {
            if let RuntimeEvent::ToolCompleted {
                evidence, success, ..
            } = envelope.event
            {
                if !success {
                    continue;
                }
                let id = evidence.evidence_id_or_synthesize(thread_id, envelope.seq);
                if &id == evidence_id {
                    let mut evidence = evidence;
                    evidence.evidence_id = Some(id);
                    return Ok(evidence);
                }
            }
        }
        Err(OpsCodexError::NotFound(format!("evidence {evidence_id}")))
    }

    async fn get_evidence_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta> {
        let _thread = self.get_thread_in(workspace_id, thread_id).await?;
        self.get_evidence(thread_id, evidence_id).await
    }

    async fn fork_thread(
        &self,
        thread_id: &ThreadId,
        at_seq: u64,
        title: Option<String>,
    ) -> Result<EventEnvelope> {
        SqliteStore::fork_thread(self, thread_id.clone(), at_seq, ThreadId::new(), title).await
    }

    async fn thread_lineage(
        &self,
        thread_id: &ThreadId,
    ) -> Result<(Option<ThreadId>, Option<u64>)> {
        let thread_id = thread_id.clone();
        self.with_conn(move |conn| load_lineage(conn, &thread_id))
            .await
    }

    async fn upsert_turn(&self, record: TurnRecord) -> Result<()> {
        self.with_conn(move |conn| upsert_turn(conn, record)).await
    }

    async fn get_turn(&self, turn_id: &TurnId) -> Result<Option<TurnRecord>> {
        let turn_id = turn_id.clone();
        self.with_conn(move |conn| get_turn(conn, &turn_id)).await
    }

    async fn list_open_turns(&self) -> Result<Vec<TurnRecord>> {
        self.with_conn(|conn| list_open_turns(conn)).await
    }

    async fn put_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<()> {
        self.with_conn(move |conn| put_checkpoint(conn, checkpoint))
            .await
    }

    async fn last_checkpoint(&self, turn_id: &TurnId) -> Result<Option<CheckpointRecord>> {
        let turn_id = turn_id.clone();
        self.with_conn(move |conn| last_checkpoint(conn, &turn_id))
            .await
    }

    async fn put_approval(&self, approval: DurableApproval) -> Result<()> {
        self.with_conn(move |conn| put_approval(conn, approval))
            .await
    }

    async fn get_approval(&self, id: &ApprovalId) -> Result<Option<DurableApproval>> {
        let id = id.clone();
        self.with_conn(move |conn| get_approval(conn, &id)).await
    }

    async fn list_pending_approvals(&self) -> Result<Vec<DurableApproval>> {
        self.with_conn(|conn| list_pending_approvals(conn)).await
    }

    async fn approval_for_turn(&self, turn_id: &TurnId) -> Result<Option<DurableApproval>> {
        let turn_id = turn_id.clone();
        self.with_conn(move |conn| approval_for_turn(conn, &turn_id))
            .await
    }

    async fn acquire_lease(
        &self,
        turn_id: &TurnId,
        thread_id: &ThreadId,
        owner_id: &str,
        ttl: Duration,
    ) -> Result<Lease> {
        let turn_id = turn_id.clone();
        let thread_id = thread_id.clone();
        let owner_id = owner_id.to_owned();
        self.with_conn(move |conn| acquire_lease(conn, &turn_id, &thread_id, &owner_id, ttl))
            .await
    }

    async fn refresh_lease(&self, lease_id: &str, fencing_token: i64, ttl: Duration) -> Result<()> {
        let lease_id = lease_id.to_owned();
        self.with_conn(move |conn| refresh_lease(conn, &lease_id, fencing_token, ttl))
            .await
    }

    async fn release_lease(&self, lease_id: &str, fencing_token: i64) -> Result<()> {
        let lease_id = lease_id.to_owned();
        self.with_conn(move |conn| release_lease(conn, &lease_id, fencing_token))
            .await
    }

    async fn remember_resume(
        &self,
        key: &str,
        turn_id: &TurnId,
        payload: &str,
    ) -> Result<Option<String>> {
        let key = key.to_owned();
        let turn_id = turn_id.clone();
        let payload = payload.to_owned();
        self.with_conn(move |conn| remember_resume(conn, &key, &turn_id, &payload))
            .await
    }

    async fn force_release_turn_lease(&self, turn_id: &TurnId) -> Result<()> {
        let turn_id = turn_id.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM leases WHERE turn_id = ?1",
                params![turn_id.to_string()],
            )
            .map_err(|error| sqlite_error("force release lease", error))?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn schema_checksum_mismatch_refuses_start() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let store = SqliteStore::open(&path).await.unwrap();
        drop(store);
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE schema_migrations SET checksum = 'deadbeef'", [])
            .unwrap();
        drop(conn);
        let error = match SqliteStore::open(&path).await {
            Ok(_) => panic!("expected schema checksum mismatch"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[tokio::test]
    async fn live_lease_blocks_a_second_owner_and_stale_fencing_token() {
        let directory = tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("state.sqlite3"))
            .await
            .unwrap();
        let thread_id = ThreadId::new();
        store
            .create_thread(thread_id.clone(), WorkspaceId::default())
            .await
            .unwrap();
        let turn_id = TurnId::new();
        let lease = store
            .acquire_lease(&turn_id, &thread_id, "owner-a", Duration::from_secs(30))
            .await
            .unwrap();
        let blocked = store
            .acquire_lease(&turn_id, &thread_id, "owner-b", Duration::from_secs(30))
            .await
            .unwrap_err();
        assert!(matches!(blocked, OpsCodexError::TurnAlreadyRunning));
        let stale = store
            .refresh_lease(
                &lease.lease_id,
                lease.fencing_token + 9,
                Duration::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("fencing token"));
        store.force_release_turn_lease(&turn_id).await.unwrap();
        let expired = store
            .acquire_lease(&turn_id, &thread_id, "owner-a", Duration::ZERO)
            .await
            .unwrap();
        let next = store
            .acquire_lease(&turn_id, &thread_id, "owner-b", Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(next.fencing_token, expired.fencing_token + 1);
        store
            .refresh_lease(
                &expired.lease_id,
                expired.fencing_token,
                Duration::from_secs(30),
            )
            .await
            .unwrap_err();
    }
}
