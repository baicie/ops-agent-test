use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use crate::{OpsCodexError, Result};

const MAX_STDERR_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SupervisorOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct ChildSupervisor {
    pub max_restarts: u32,
}

impl Default for ChildSupervisor {
    fn default() -> Self {
        Self { max_restarts: 2 }
    }
}

impl ChildSupervisor {
    pub fn new(max_restarts: u32) -> Self {
        Self {
            max_restarts: max_restarts.min(5),
        }
    }

    pub async fn run_once(
        &self,
        spec: SpawnSpec,
        stdin_bytes: &[u8],
        cancellation: CancellationToken,
    ) -> Result<SupervisorOutput> {
        validate_command(&spec.command)?;
        if let Some(cwd) = &spec.cwd {
            validate_path(cwd)?;
        }
        let mut attempts = 0;
        loop {
            match invoke(&spec, stdin_bytes, cancellation.clone()).await {
                Ok(output) => return Ok(output),
                Err(error)
                    if is_retryable_supervisor_failure(&error)
                        && attempts < self.max_restarts
                        && !cancellation.is_cancelled() =>
                {
                    attempts += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

pub fn validate_command(command: &Path) -> Result<()> {
    validate_path(command)?;
    if !command.is_absolute() {
        return Err(OpsCodexError::Policy(
            "extension command must be an absolute path".into(),
        ));
    }
    if !command.exists() {
        return Err(OpsCodexError::NotFound(format!(
            "extension command {}",
            command.display()
        )));
    }
    Ok(())
}

pub fn validate_path(path: &Path) -> Result<()> {
    if path.to_string_lossy().contains('\0')
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(OpsCodexError::Policy(
            "extension path cannot contain parent directory segments".into(),
        ));
    }
    Ok(())
}

async fn invoke(
    spec: &SpawnSpec,
    stdin_bytes: &[u8],
    cancellation: CancellationToken,
) -> Result<SupervisorOutput> {
    let mut command = Command::new(&spec.command);
    command
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for (key, value) in &spec.env {
        if is_blocked_env(key) {
            continue;
        }
        command.env(key, value);
    }
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }

    let mut child = command.spawn().map_err(|error| {
        OpsCodexError::Tool(format!("failed to start extension process: {error}"))
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_bytes).await.map_err(|error| {
            OpsCodexError::Tool(format!("failed to write extension stdin: {error}"))
        })?;
        drop(stdin);
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| OpsCodexError::Tool("extension process stdout is unavailable".into()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| OpsCodexError::Tool("extension process stderr is unavailable".into()))?;
    let max_output_bytes = spec.max_output_bytes;
    let stdout_task =
        tokio::spawn(async move { read_bounded(&mut stdout, max_output_bytes).await });
    let stderr_task =
        tokio::spawn(async move { read_bounded(&mut stderr, MAX_STDERR_BYTES).await });

    let status = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            let _ = child.start_kill();
            stdout_task.abort();
            stderr_task.abort();
            let _ = child.wait().await;
            return Err(OpsCodexError::Cancelled);
        }
        _ = tokio::time::sleep(spec.timeout) => {
            let _ = child.start_kill();
            stdout_task.abort();
            stderr_task.abort();
            let _ = child.wait().await;
            return Err(OpsCodexError::Timeout("extension process timed out".into()));
        }
        status = child.wait() => {
            status.map_err(|error| OpsCodexError::Tool(format!("extension process wait failed: {error}")))?
        }
    };

    let (stdout, truncated) = stdout_task
        .await
        .map_err(|error| OpsCodexError::Tool(format!("extension stdout join failed: {error}")))??;
    let (stderr, _) = stderr_task
        .await
        .map_err(|error| OpsCodexError::Tool(format!("extension stderr join failed: {error}")))??;
    if !status.success() {
        return Err(OpsCodexError::Tool(format!(
            "extension process exited with {}",
            status.code().unwrap_or(-1)
        )));
    }
    Ok(SupervisorOutput {
        stdout,
        stderr,
        truncated,
    })
}

async fn read_bounded<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 1024];
    loop {
        let read = reader.read(&mut buf).await.map_err(|error| {
            OpsCodexError::Tool(format!("failed to read extension output: {error}"))
        })?;
        if read == 0 {
            return Ok((bytes, false));
        }
        let remaining = max_bytes.saturating_sub(bytes.len());
        if read > remaining {
            bytes.extend_from_slice(&buf[..remaining]);
            let mut drain = [0_u8; 1024];
            while reader.read(&mut drain).await.unwrap_or(0) > 0 {}
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&buf[..read]);
    }
}

pub fn is_blocked_env(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || upper == "AWS_SECRET_ACCESS_KEY"
        || upper == "OPENAI_API_KEY"
}

fn is_retryable_supervisor_failure(error: &OpsCodexError) -> bool {
    matches!(error, OpsCodexError::Timeout(_) | OpsCodexError::Tool(_))
}
