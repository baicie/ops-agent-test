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
    pub extensions: ExtensionsConfig,
    #[serde(rename = "extension")]
    pub extension: Vec<ExtensionConfigEntry>,
    pub skills: Vec<SkillConfigEntry>,
    pub store: StoreConfig,
    pub remediation: RemediationConfig,
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
        if self.extensions.max_skill_context_bytes == 0 {
            return Err(OpsCodexError::Protocol(
                "extensions.max_skill_context_bytes must be greater than zero".into(),
            ));
        }
        if self.store.approval_ttl_seconds == 0 || self.store.lease_ttl_seconds == 0 {
            return Err(OpsCodexError::Protocol(
                "store.approval_ttl_seconds and store.lease_ttl_seconds must be greater than zero"
                    .into(),
            ));
        }
        if self.remediation.approval_ttl_seconds == 0 {
            return Err(OpsCodexError::Protocol(
                "remediation.approval_ttl_seconds must be greater than zero".into(),
            ));
        }
        let demo_url = url::Url::parse(&self.remediation.demo_fault_url).map_err(|error| {
            OpsCodexError::Protocol(format!("invalid remediation.demo_fault_url: {error}"))
        })?;
        if !matches!(demo_url.host_str(), Some("127.0.0.1" | "localhost" | "::1")) {
            return Err(OpsCodexError::Protocol(
                "remediation.demo_fault_url must be a loopback host".into(),
            ));
        }
        if !crate::config::is_loopback_bind_host(&self.server.host) {
            return Err(OpsCodexError::Protocol(
                "without TLS, server.host must be a loopback address (127.0.0.1, localhost, or ::1)"
                    .into(),
            ));
        }
        if !matches!(self.store.backend.as_str(), "sqlite" | "jsonl") {
            return Err(OpsCodexError::Protocol(format!(
                "store.backend must be sqlite or jsonl, not `{}`",
                self.store.backend
            )));
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
            if let Some(max_effect) = workspace.max_effect.as_deref() {
                crate::extensions::CapabilityEffect::parse(max_effect)?;
            }
        }
        let mut extension_ids = std::collections::BTreeSet::new();
        for extension in &self.extension {
            if extension.id.trim().is_empty() {
                return Err(OpsCodexError::Protocol(
                    "extension.id must not be empty".into(),
                ));
            }
            if !extension_ids.insert(extension.id.trim().to_owned()) {
                return Err(OpsCodexError::Protocol(format!(
                    "duplicate extension id `{}`",
                    extension.id
                )));
            }
            if !matches!(extension.kind.as_str(), "custom" | "mcp" | "mcp_http") {
                return Err(OpsCodexError::Protocol(format!(
                    "unsupported extension kind `{}`",
                    extension.kind
                )));
            }
        }
        if self.extensions.production_safe && self.tools.exec {
            return Err(OpsCodexError::Protocol(
                "extensions.production_safe cannot be combined with tools.exec".into(),
            ));
        }
        Ok(())
    }

    pub fn sqlite_path(&self) -> PathBuf {
        self.store
            .sqlite_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| Self::data_dir().join("state.sqlite3"))
    }

    pub fn artifact_dir(&self) -> PathBuf {
        Self::data_dir().join("artifacts")
    }
}

pub fn is_loopback_bind_host(host: &str) -> bool {
    let host = host.trim().trim_matches(|ch| ch == '[' || ch == ']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
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
    pub max_effect: Option<String>,
    pub allow_remediation: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionsConfig {
    pub production_safe: bool,
    pub allow_custom_tools: bool,
    pub max_skill_context_bytes: usize,
}

impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            production_safe: false,
            allow_custom_tools: false,
            max_skill_context_bytes: 4 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionConfigEntry {
    pub id: String,
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub trusted_local: bool,
    pub path: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    pub allowlist_hosts: Vec<String>,
    pub workspaces: Vec<String>,
    pub effect: Option<String>,
    pub recovery: Option<String>,
    pub version: Option<String>,
    pub max_restarts: Option<u32>,
    pub env: Vec<String>,
}

impl Default for ExtensionConfigEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: String::new(),
            enabled: true,
            trusted_local: false,
            path: None,
            command: None,
            args: Vec::new(),
            cwd: None,
            url: None,
            allowlist_hosts: Vec::new(),
            workspaces: Vec::new(),
            effect: None,
            recovery: None,
            version: None,
            max_restarts: None,
            env: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillConfigEntry {
    pub path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub workspaces: Vec<String>,
}

impl Default for SkillConfigEntry {
    fn default() -> Self {
        Self {
            path: String::new(),
            enabled: true,
            workspaces: Vec::new(),
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    pub backend: String,
    pub sqlite_path: Option<String>,
    pub jsonl_dir: Option<String>,
    pub approval_ttl_seconds: u64,
    pub lease_ttl_seconds: u64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            backend: "sqlite".into(),
            sqlite_path: None,
            jsonl_dir: None,
            approval_ttl_seconds: 3600,
            lease_ttl_seconds: 30,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RemediationConfig {
    pub enabled: bool,
    pub kill_switch: bool,
    pub demo_fault_url: String,
    pub approval_ttl_seconds: u64,
}

impl Default for RemediationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            kill_switch: false,
            demo_fault_url: "http://127.0.0.1:8080".into(),
            approval_ttl_seconds: 1800,
        }
    }
}
