use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{OpsCodexError, Result};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunbookMeta {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub signals: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub version: u32,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Runbook {
    pub meta: RunbookMeta,
    pub body: String,
}

#[derive(Clone, Debug, Default)]
pub struct RunbookCatalog {
    runbooks: Vec<Runbook>,
}

impl RunbookCatalog {
    pub fn load(directory: Option<&PathBuf>) -> Result<Self> {
        let Some(directory) = directory else {
            return Ok(Self::default());
        };
        if !directory.exists() {
            return Ok(Self::default());
        }
        let mut catalog = Self::default();
        for entry in fs::read_dir(directory).map_err(|error| {
            OpsCodexError::Storage(format!("failed to read runbook directory: {error}"))
        })? {
            let entry = entry.map_err(|error| {
                OpsCodexError::Storage(format!("failed to read runbook entry: {error}"))
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }
            let source = fs::read_to_string(&path).map_err(|error| {
                OpsCodexError::Storage(format!("failed to read {}: {error}", path.display()))
            })?;
            catalog.insert(parse_runbook(&source)?)?;
        }
        Ok(catalog)
    }

    pub fn insert(&mut self, runbook: Runbook) -> Result<()> {
        if self.runbooks.iter().any(|item| {
            item.meta.id == runbook.meta.id && item.meta.version == runbook.meta.version
        }) {
            return Err(OpsCodexError::Protocol(format!(
                "duplicate runbook {}@{}",
                runbook.meta.id, runbook.meta.version
            )));
        }
        self.runbooks.push(runbook);
        Ok(())
    }

    pub fn search(&self, query: &str, service: Option<&str>) -> Vec<RunbookMeta> {
        let needle = query.to_ascii_lowercase();
        let mut scored: Vec<_> = self
            .runbooks
            .iter()
            .filter(|runbook| {
                service.is_none_or(|service| {
                    runbook
                        .meta
                        .services
                        .iter()
                        .any(|item| item.eq_ignore_ascii_case(service))
                })
            })
            .map(|runbook| {
                let hay = format!(
                    "{} {} {} {}",
                    runbook.meta.id,
                    runbook.meta.title,
                    runbook.meta.tags.join(" "),
                    runbook.meta.signals.join(" ")
                )
                .to_ascii_lowercase();
                let score = if needle.is_empty() || hay.contains(&needle) {
                    1 + runbook
                        .meta
                        .signals
                        .iter()
                        .filter(|signal| signal.to_ascii_lowercase().contains(&needle))
                        .count()
                } else {
                    0
                };
                (score, runbook.meta.clone())
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        scored.into_iter().map(|(_, meta)| meta).collect()
    }

    pub fn read(&self, id: &str, version: Option<u32>) -> Result<&Runbook> {
        if id.contains("..") || id.contains('/') || id.contains('\\') {
            return Err(OpsCodexError::Policy(
                "runbook id cannot contain path segments".into(),
            ));
        }
        self.runbooks
            .iter()
            .filter(|runbook| runbook.meta.id == id)
            .filter(|runbook| version.is_none_or(|version| runbook.meta.version == version))
            .max_by_key(|runbook| runbook.meta.version)
            .ok_or_else(|| OpsCodexError::NotFound(format!("runbook {id}")))
    }
}

pub fn parse_runbook(source: &str) -> Result<Runbook> {
    let trimmed = source.trim_start();
    if !trimmed.starts_with("---") {
        return Err(OpsCodexError::Protocol(
            "runbook is missing YAML front matter".into(),
        ));
    }
    let rest = &trimmed[3..];
    let end = rest
        .find("\n---")
        .ok_or_else(|| OpsCodexError::Protocol("runbook front matter is not terminated".into()))?;
    let front = rest[..end].trim();
    let body = rest[end + 4..].trim().to_owned();
    let mut meta: FrontMatter = serde_yaml::from_str(front).map_err(|error| {
        OpsCodexError::Protocol(format!("invalid runbook front matter: {error}"))
    })?;
    if meta.id.trim().is_empty() || meta.title.trim().is_empty() {
        return Err(OpsCodexError::Protocol(
            "runbook id and title are required".into(),
        ));
    }
    if meta.version == 0 {
        meta.version = 1;
    }
    let hash = sha256_hex(source.as_bytes());
    Ok(Runbook {
        meta: RunbookMeta {
            id: meta.id,
            title: meta.title,
            services: meta.services,
            signals: meta.signals,
            tags: meta.tags,
            version: meta.version,
            hash,
        },
        body,
    })
}

#[derive(Deserialize)]
struct FrontMatter {
    id: String,
    title: String,
    #[serde(default)]
    services: Vec<String>,
    #[serde(default)]
    signals: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    version: u32,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
