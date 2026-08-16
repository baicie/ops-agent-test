use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{OpsCodexError, Result};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: ModelConfig,
    pub runtime: RuntimeSettings,
    pub prometheus: PrometheusConfig,
    pub loki: LokiConfig,
    pub tempo: TempoConfig,
    pub targets: TargetConfig,
    pub tools: ToolsConfig,
    pub server: ServerConfig,
    pub workspaces: Vec<WorkspaceConfigEntry>,
}

impl Config {
    pub fn from_toml(source: &str) -> Result<Self> {
        let config: Self = toml::from_str(source)
            .map_err(|error| OpsCodexError::Protocol(format!("invalid config: {error}")))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(Self::default_path);
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = std::fs::read_to_string(&path).map_err(|error| {
            OpsCodexError::Storage(format!("could not read {}: {error}", path.display()))
        })?;
        Self::from_toml(&source)
    }

    pub fn default_path() -> PathBuf {
        Self::home_dir().join("config.toml")
    }

    pub fn data_dir() -> PathBuf {
        Self::home_dir()
    }

    fn home_dir() -> PathBuf {
        if let Some(path) = std::env::var_os("OPSCODEX_HOME") {
            return PathBuf::from(path);
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".opscodex")
    }

    pub fn api_key(&self) -> Result<String> {
        std::env::var(&self.model.api_key_env).map_err(|_| {
            OpsCodexError::Model(format!(
                "environment variable {} is not set",
                self.model.api_key_env
            ))
        })
    }

    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("max_steps", self.runtime.max_steps as u64),
            (
                "max_concurrent_turns",
                self.runtime.max_concurrent_turns as u64,
            ),
            ("tool_timeout_seconds", self.runtime.tool_timeout_seconds),
            ("model_timeout_seconds", self.runtime.model_timeout_seconds),
            ("max_output_bytes", self.runtime.max_output_bytes as u64),
            ("context_items", self.runtime.context_items as u64),
            ("context_max_tokens", self.runtime.context_max_tokens as u64),
            ("context_max_bytes", self.runtime.context_max_bytes as u64),
            (
                "context_max_evidence",
                self.runtime.context_max_evidence as u64,
            ),
            (
                "context_max_tool_calls",
                self.runtime.context_max_tool_calls as u64,
            ),
            (
                "inline_artifact_bytes",
                self.runtime.inline_artifact_bytes as u64,
            ),
        ] {
            if value == 0 {
                return Err(OpsCodexError::Protocol(format!(
                    "runtime.{name} must be greater than zero"
                )));
            }
        }
        if self.model.model.trim().is_empty() {
            return Err(OpsCodexError::Protocol(
                "model.model must not be empty".into(),
            ));
        }
        if let Some(reasoning_effort) = self.model.reasoning_effort.as_deref()
            && !matches!(
                reasoning_effort,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
        {
            return Err(OpsCodexError::Protocol(
                "model.reasoning_effort must be one of none, minimal, low, medium, high, or xhigh"
                    .into(),
            ));
        }
        if self.loki.max_range_seconds == 0 || self.tempo.max_range_seconds == 0 {
            return Err(crate::OpsCodexError::Protocol(
                "loki.max_range_seconds and tempo.max_range_seconds must be greater than zero"
                    .into(),
            ));
        }
        if self.loki.max_lines == 0 {
            return Err(crate::OpsCodexError::Protocol(
                "loki.max_lines must be greater than zero".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for workspace in &self.workspaces {
            crate::runtime::WorkspaceId::new(workspace.id.trim()).validate()?;
            if !seen.insert(workspace.id.trim().to_owned()) {
                return Err(OpsCodexError::Protocol(format!(
                    "duplicate workspace id `{}`",
                    workspace.id
                )));
            }
            if let Some(turns) = workspace.max_concurrent_turns
                && turns == 0
            {
                return Err(OpsCodexError::Protocol(
                    "workspace.max_concurrent_turns must be greater than zero".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    pub endpoint: String,
    pub reasoning_effort: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            model: "gpt-5.2".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            endpoint: "https://api.openai.com/v1/responses".into(),
            reasoning_effort: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeSettings {
    pub max_steps: usize,
    pub max_concurrent_turns: usize,
    pub tool_timeout_seconds: u64,
    pub model_timeout_seconds: u64,
    pub max_output_bytes: usize,
    pub context_items: usize,
    pub context_max_tokens: usize,
    pub context_max_bytes: usize,
    pub context_max_evidence: usize,
    pub context_max_tool_calls: usize,
    pub context_max_cost_micros: u64,
    pub inline_artifact_bytes: usize,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            max_steps: 12,
            max_concurrent_turns: 4,
            tool_timeout_seconds: 30,
            model_timeout_seconds: 120,
            max_output_bytes: 64 * 1024,
            context_items: 100,
            context_max_tokens: 24_000,
            context_max_bytes: 96_000,
            context_max_evidence: 32,
            context_max_tool_calls: 24,
            context_max_cost_micros: 0,
            inline_artifact_bytes: 8 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PrometheusConfig {
    pub url: String,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:9090".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LokiConfig {
    pub url: String,
    pub tenant_header: String,
    pub tenant_env: String,
    pub max_range_seconds: u64,
    pub max_lines: u32,
}

impl Default for LokiConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:3100".into(),
            tenant_header: "X-Scope-OrgID".into(),
            tenant_env: "LOKI_TENANT".into(),
            max_range_seconds: 3600,
            max_lines: 200,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TempoConfig {
    pub url: String,
    pub max_range_seconds: u64,
}

impl Default for TempoConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:3200".into(),
            max_range_seconds: 3600,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetConfig {
    pub allowed_containers: Vec<String>,
    pub allowed_hosts: Vec<String>,
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            allowed_containers: vec!["order-service".into()],
            allowed_hosts: vec![
                "localhost".into(),
                "127.0.0.1".into(),
                "order-service".into(),
            ],
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub exec: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 3000,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfigEntry {
    pub id: String,
    pub display_name: Option<String>,
    pub environment: Option<String>,
    pub prometheus_url: Option<String>,
    pub loki_url: Option<String>,
    pub loki_tenant_env: Option<String>,
    pub tempo_url: Option<String>,
    pub allowed_hosts: Option<Vec<String>>,
    pub allowed_containers: Option<Vec<String>>,
    pub kubeconfig_env: Option<String>,
    pub kube_context: Option<String>,
    pub cluster_alias: Option<String>,
    pub allowed_namespaces: Option<Vec<String>>,
    pub allowed_kinds: Option<Vec<String>>,
    pub runbook_dir: Option<String>,
    pub max_concurrent_turns: Option<usize>,
}
