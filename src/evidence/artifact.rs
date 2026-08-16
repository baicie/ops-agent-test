use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use tokio::fs;

use super::sha256_hex;
use crate::{OpsCodexError, Result};

#[derive(Debug)]
enum Backend {
    Disk(PathBuf),
    Memory(Mutex<HashMap<String, Vec<u8>>>),
}

#[derive(Debug)]
pub struct ArtifactStore {
    backend: Backend,
    max_bytes: usize,
}

impl ArtifactStore {
    pub fn memory() -> Self {
        Self {
            backend: Backend::Memory(Mutex::new(HashMap::new())),
            max_bytes: 1024 * 1024,
        }
    }

    pub async fn disk(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory).await.map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to create artifact directory {}: {error}",
                directory.display()
            ))
        })?;
        Ok(Self {
            backend: Backend::Disk(directory),
            max_bytes: 1024 * 1024,
        })
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes.max(1);
        self
    }

    pub async fn put(&self, bytes: &[u8]) -> Result<String> {
        self.put_in("default", bytes).await
    }

    pub async fn put_in(&self, workspace_id: &str, bytes: &[u8]) -> Result<String> {
        let digest = sha256_hex(bytes);
        let key = scoped_key(workspace_id, &digest);
        match &self.backend {
            Backend::Memory(store) => {
                store
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(key, bytes.to_vec());
            }
            Backend::Disk(directory) => {
                let path = artifact_path(directory, workspace_id, &digest);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).await.map_err(|error| {
                        OpsCodexError::Storage(format!(
                            "failed to create artifact directory {}: {error}",
                            parent.display()
                        ))
                    })?;
                }
                if !path.exists() {
                    fs::write(&path, bytes).await.map_err(|error| {
                        OpsCodexError::Storage(format!(
                            "failed to write artifact {}: {error}",
                            path.display()
                        ))
                    })?;
                }
            }
        }
        Ok(digest)
    }

    pub async fn get(&self, sha256: &str, max_bytes: usize) -> Result<Vec<u8>> {
        self.get_in("default", sha256, max_bytes).await
    }

    pub async fn get_in(
        &self,
        workspace_id: &str,
        sha256: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        if !is_sha256(sha256) {
            return Err(OpsCodexError::Protocol("invalid artifact id".into()));
        }
        let max_bytes = max_bytes.min(self.max_bytes).max(1);
        let key = scoped_key(workspace_id, sha256);
        let bytes = match &self.backend {
            Backend::Memory(store) => store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&key)
                .cloned()
                .ok_or_else(|| OpsCodexError::NotFound(format!("artifact {sha256}")))?,
            Backend::Disk(directory) => {
                let path = artifact_path(directory, workspace_id, sha256);
                fs::read(&path).await.map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        OpsCodexError::NotFound(format!("artifact {sha256}"))
                    } else {
                        OpsCodexError::Storage(format!(
                            "failed to read artifact {}: {error}",
                            path.display()
                        ))
                    }
                })?
            }
        };
        Ok(bytes.into_iter().take(max_bytes).collect())
    }
}

fn scoped_key(workspace_id: &str, sha256: &str) -> String {
    format!("{workspace_id}/{sha256}")
}

fn artifact_path(directory: &std::path::Path, workspace_id: &str, sha256: &str) -> PathBuf {
    let prefix = sha256.get(..2).unwrap_or("00");
    directory.join(workspace_id).join(prefix).join(sha256)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}
