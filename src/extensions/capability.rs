use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{OpsCodexError, Result, evidence::Sensitivity, tools::ToolRisk};

pub const BUILTIN_NAMESPACE: &str = "builtin";
pub const BUILTIN_VERSION: &str = "1.0.0";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    Builtin,
    Mcp,
    Custom,
}

impl CapabilitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Mcp => "mcp",
            Self::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "builtin" => Ok(Self::Builtin),
            "mcp" => Ok(Self::Mcp),
            "custom" => Ok(Self::Custom),
            other => Err(OpsCodexError::Protocol(format!(
                "unsupported capability source `{other}`"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEffect {
    Observe,
    ChangeReversible,
    ChangeIrreversible,
    ExternalSideEffect,
}

impl CapabilityEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::ChangeReversible => "change_reversible",
            Self::ChangeIrreversible => "change_irreversible",
            Self::ExternalSideEffect => "external_side_effect",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "observe" => Ok(Self::Observe),
            "change_reversible" => Ok(Self::ChangeReversible),
            "change_irreversible" => Ok(Self::ChangeIrreversible),
            "external_side_effect" => Ok(Self::ExternalSideEffect),
            other => Err(OpsCodexError::Protocol(format!(
                "unsupported capability effect `{other}`"
            ))),
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Observe => 0,
            Self::ChangeReversible => 1,
            Self::ChangeIrreversible => 2,
            Self::ExternalSideEffect => 3,
        }
    }

    pub fn strictest(effects: impl IntoIterator<Item = Self>) -> Self {
        effects
            .into_iter()
            .max_by_key(|effect| effect.rank())
            .unwrap_or(Self::ExternalSideEffect)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryMode {
    NoneNeeded,
    Idempotent,
    NeedsReconciliation,
}

impl RecoveryMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "none_needed" | "none" => Ok(Self::NoneNeeded),
            "idempotent" => Ok(Self::Idempotent),
            "needs_reconciliation" => Ok(Self::NeedsReconciliation),
            other => Err(OpsCodexError::Protocol(format!(
                "unsupported recovery mode `{other}`"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    pub source: CapabilitySource,
    pub version: String,
    pub schema_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub source: CapabilitySource,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    pub effect: CapabilityEffect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_requirements: Vec<String>,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryMode>,
    pub provenance: Provenance,
    pub content_sensitivity: Sensitivity,
    pub enabled: bool,
    #[serde(default)]
    pub trusted_local: bool,
}

impl CapabilityDescriptor {
    pub fn builtin(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        risk: ToolRisk,
    ) -> Self {
        let name = name.into();
        let effect = match risk {
            ToolRisk::Safe => CapabilityEffect::Observe,
            ToolRisk::Ask | ToolRisk::Forbidden => CapabilityEffect::ExternalSideEffect,
        };
        let schema_hash = hash_schema(&input_schema);
        let id = capability_id(BUILTIN_NAMESPACE, &name, BUILTIN_VERSION);
        Self {
            source: CapabilitySource::Builtin,
            provenance: Provenance {
                source: CapabilitySource::Builtin,
                version: BUILTIN_VERSION.into(),
                schema_hash: schema_hash.clone(),
                binary_hash: None,
                origin: Some("compiled".into()),
            },
            id,
            name,
            description: description.into(),
            input_schema,
            output_schema: None,
            effect,
            target_requirements: Vec::new(),
            timeout_seconds: 30,
            max_output_bytes: crate::tools::MAX_OUTPUT_BYTES,
            recovery: Some(RecoveryMode::NoneNeeded),
            content_sensitivity: Sensitivity::Internal,
            enabled: true,
            trusted_local: false,
        }
    }

    pub fn with_effect(mut self, effect: CapabilityEffect) -> Self {
        self.effect = effect;
        self
    }

    pub fn with_trusted_local(mut self, trusted_local: bool) -> Self {
        self.trusted_local = trusted_local;
        self
    }

    pub fn apply_strictest(
        mut self,
        workspace: Option<CapabilityEffect>,
        local: Option<CapabilityEffect>,
    ) -> Self {
        self.effect = CapabilityEffect::strictest(
            [Some(self.effect), workspace, local].into_iter().flatten(),
        );
        self
    }

    pub fn requires_recovery_metadata(&self) -> bool {
        self.source != CapabilitySource::Builtin
    }

    pub fn validate_for_enablement(&self) -> Result<()> {
        parse_capability_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err(OpsCodexError::Protocol(
                "capability name cannot be empty".into(),
            ));
        }
        if self.requires_recovery_metadata() && (self.recovery.is_none()) {
            return Err(OpsCodexError::Policy(format!(
                "capability `{}` is missing recovery metadata",
                self.id
            )));
        }
        if self.timeout_seconds == 0 || self.max_output_bytes == 0 {
            return Err(OpsCodexError::Protocol(format!(
                "capability `{}` budgets must be greater than zero",
                self.id
            )));
        }
        Ok(())
    }

    pub fn summary(&self) -> CapabilitySummary {
        CapabilitySummary {
            id: self.id.clone(),
            name: self.name.clone(),
            source: self.source,
            version: self.provenance.version.clone(),
            effect: self.effect,
            schema_hash: self.provenance.schema_hash.clone(),
            enabled: self.enabled,
            trusted_local: self.trusted_local,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilitySummary {
    pub id: String,
    pub name: String,
    pub source: CapabilitySource,
    pub version: String,
    pub effect: CapabilityEffect,
    pub schema_hash: String,
    pub enabled: bool,
    pub trusted_local: bool,
}

pub fn capability_id(namespace: &str, name: &str, version: &str) -> String {
    format!("{namespace}/{name}@{version}")
}

pub fn parse_capability_id(id: &str) -> Result<(String, String, String)> {
    let (qualified, version) = id.rsplit_once('@').ok_or_else(|| {
        OpsCodexError::Protocol(format!(
            "capability id `{id}` must be namespace/name@version"
        ))
    })?;
    let (namespace, name) = qualified.split_once('/').ok_or_else(|| {
        OpsCodexError::Protocol(format!(
            "capability id `{id}` must be namespace/name@version"
        ))
    })?;
    if namespace.trim().is_empty() || name.trim().is_empty() || version.trim().is_empty() {
        return Err(OpsCodexError::Protocol(format!(
            "capability id `{id}` must be namespace/name@version"
        )));
    }
    if namespace == BUILTIN_NAMESPACE && name.contains('/') {
        return Err(OpsCodexError::Protocol(
            "builtin capability names cannot contain additional namespaces".into(),
        ));
    }
    Ok((namespace.to_owned(), name.to_owned(), version.to_owned()))
}

pub fn hash_schema(schema: &Value) -> String {
    hash_bytes(&serde_json::to_vec(schema).unwrap_or_default())
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
