use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::json;

use crate::{
    OpsCodexError, Result,
    action::verify_audit_chain,
    config::{Config, is_loopback_bind_host},
    store::{EventStore, SqliteStore},
    workspace::WorkspaceCatalog,
};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Degraded,
    Error,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DoctorReport {
    pub status: CheckStatus,
    pub checks: Vec<Check>,
}

impl DoctorReport {
    pub fn is_ok(&self) -> bool {
        !matches!(self.status, CheckStatus::Error)
    }
}

pub fn validate_config(config: &Config) -> Result<()> {
    config.validate()?;
    deny_non_loopback_bind(&config.server.host)
}

pub fn deny_non_loopback_bind(host: &str) -> Result<()> {
    if is_loopback_bind_host(host) {
        Ok(())
    } else {
        Err(OpsCodexError::Protocol(
            "without TLS, OpsCodex can only bind loopback (127.0.0.1, localhost, or ::1)".into(),
        ))
    }
}

pub fn parse_listen_addr(host: &str, port: u16) -> Result<SocketAddr> {
    deny_non_loopback_bind(host)?;
    let host = host.trim().trim_matches(|ch| ch == '[' || ch == ']');
    let dotted = format!("{host}:{port}");
    if let Ok(address) = dotted.parse() {
        return Ok(address);
    }
    format!("[{host}]:{port}").parse().map_err(|error| {
        OpsCodexError::Protocol(format!("invalid listen address {dotted}: {error}"))
    })
}

pub async fn doctor(config: &Config) -> Result<DoctorReport> {
    let mut checks = Vec::new();
    match validate_config(config) {
        Ok(()) => checks.push(ok("config", "configuration is valid")),
        Err(error) => checks.push(error_check("config", error.to_string())),
    }
    checks.push(if is_loopback_bind_host(&config.server.host) {
        ok(
            "bind",
            format!("server binds loopback {}", config.server.host),
        )
    } else {
        error_check(
            "bind",
            format!(
                "server.host `{}` is not loopback; TLS is required for non-loopback binds",
                config.server.host
            ),
        )
    });
    match WorkspaceCatalog::from_config(config) {
        Ok(catalog) => {
            let count = catalog.iter().count();
            checks.push(ok("workspaces", format!("{count} workspace(s) configured")));
        }
        Err(error) => checks.push(error_check("workspaces", error.to_string())),
    }
    if config.extensions.production_safe && config.tools.exec {
        checks.push(error_check(
            "exec",
            "production_safe profile cannot enable exec",
        ));
    } else if config.tools.exec {
        checks.push(degraded(
            "exec",
            "exec is enabled; it remains approval-gated and is not remediation",
        ));
    } else {
        checks.push(ok("exec", "exec is disabled"));
    }
    if config.remediation.enabled {
        checks.push(degraded(
            "remediation",
            "remediation is enabled; change operations still require exact request-hash approval",
        ));
    } else {
        checks.push(ok("remediation", "remediation is disabled"));
    }

    let sqlite_path = config.sqlite_path();
    if sqlite_path.exists() {
        match verify_store(config).await {
            Ok(detail) => checks.push(ok("store", detail)),
            Err(error) => checks.push(error_check("store", error.to_string())),
        }
        match verify_audit(config).await {
            Ok(detail) => checks.push(ok("audit", detail)),
            Err(error) => checks.push(error_check("audit", error.to_string())),
        }
    } else {
        checks.push(degraded(
            "store",
            format!(
                "sqlite file {} is not present yet; it will be created on first run",
                sqlite_path.display()
            ),
        ));
    }

    let status = if checks
        .iter()
        .any(|check| check.status == CheckStatus::Error)
    {
        CheckStatus::Error
    } else if checks
        .iter()
        .any(|check| check.status == CheckStatus::Degraded)
    {
        CheckStatus::Degraded
    } else {
        CheckStatus::Ok
    };
    Ok(DoctorReport { status, checks })
}

pub async fn verify_store(config: &Config) -> Result<String> {
    if config.store.backend != "sqlite" {
        return Err(OpsCodexError::Protocol(
            "storage verify requires store.backend = sqlite".into(),
        ));
    }
    let path = config.sqlite_path();
    if !path.exists() {
        return Err(OpsCodexError::Storage(format!(
            "sqlite file {} does not exist",
            path.display()
        )));
    }
    let store = SqliteStore::open(path).await?;
    store.integrity_check().await?;
    let versions = store.applied_schema_versions().await?;
    if !versions.contains(&1) || !versions.contains(&2) {
        return Err(OpsCodexError::Storage(format!(
            "sqlite schema versions {versions:?} are incomplete"
        )));
    }
    let threads = store.list_threads().await?.len();
    Ok(format!(
        "integrity ok; schema {:?}; {} thread(s)",
        versions, threads
    ))
}

pub async fn verify_audit(config: &Config) -> Result<String> {
    if config.store.backend != "sqlite" {
        return Ok("audit verify skipped for jsonl store".into());
    }
    if !config.sqlite_path().exists() {
        return Ok("no audit log yet".into());
    }
    let store = SqliteStore::open(config.sqlite_path()).await?;
    let records = store.list_audit().await?;
    verify_audit_chain(&records)?;
    Ok(format!("{} audit record(s) verified", records.len()))
}

pub async fn backup_store(config: &Config, dest: impl AsRef<Path>) -> Result<PathBuf> {
    if config.store.backend != "sqlite" {
        return Err(OpsCodexError::Protocol(
            "storage backup requires store.backend = sqlite".into(),
        ));
    }
    let source = config.sqlite_path();
    if !source.exists() {
        return Err(OpsCodexError::Storage(format!(
            "sqlite file {} does not exist",
            source.display()
        )));
    }
    let dest = dest.as_ref();
    let extension = dest.extension().and_then(|value| value.to_str());
    let file = if matches!(extension, Some("sqlite3" | "db")) {
        dest.to_path_buf()
    } else {
        dest.join("state.sqlite3")
    };
    let store = SqliteStore::open(source).await?;
    store.backup_to(file).await
}

fn ok(name: &str, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: CheckStatus::Ok,
        detail: detail.into(),
    }
}

fn degraded(name: &str, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: CheckStatus::Degraded,
        detail: detail.into(),
    }
}

fn error_check(name: &str, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: CheckStatus::Error,
        detail: detail.into(),
    }
}

pub fn print_json(value: impl Serialize) -> Result<()> {
    let body = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|_| json!({"error": "serialize"}).to_string());
    println!("{body}");
    Ok(())
}
