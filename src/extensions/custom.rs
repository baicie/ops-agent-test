use std::{fs, path::PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    extensions::{
        CapabilityDescriptor, CapabilityEffect, CapabilitySource, ChildSupervisor, Provenance,
        RecoveryMode, SpawnSpec, capability_id, enforce_workspace_ceiling, hash_bytes, hash_schema,
        validate_command, validate_path,
    },
    tools::{Tool, ToolOutput, ToolRisk},
};

#[derive(Clone, Debug, Deserialize)]
pub struct CustomToolManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: CustomToolMetadata,
    pub spec: CustomToolSpec,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CustomToolMetadata {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CustomToolSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema", default)]
    pub output_schema: Option<Value>,
    pub effect: String,
    #[serde(default)]
    pub recovery: Option<String>,
    #[serde(rename = "timeoutSeconds", default)]
    pub timeout_seconds: Option<u64>,
    #[serde(rename = "maxOutputBytes", default)]
    pub max_output_bytes: Option<usize>,
    #[serde(rename = "envAllowlist", default)]
    pub env_allowlist: Vec<String>,
}

pub fn load_custom_manifest(path: &str) -> Result<CustomToolManifest> {
    let path = PathBuf::from(path);
    validate_path(&path)?;
    let source = fs::read_to_string(&path).map_err(|error| {
        OpsCodexError::Storage(format!("failed to read {}: {error}", path.display()))
    })?;
    let manifest: CustomToolManifest = serde_yaml::from_str(&source).map_err(|error| {
        OpsCodexError::Protocol(format!("invalid custom tool manifest: {error}"))
    })?;
    if manifest.api_version != "opscodex.dev/v1" || manifest.kind != "Tool" {
        return Err(OpsCodexError::Protocol(
            "custom tool manifest must be apiVersion opscodex.dev/v1 kind Tool".into(),
        ));
    }
    Ok(manifest)
}

pub struct CustomJsonTool {
    capability: CapabilityDescriptor,
    command: PathBuf,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    supervisor: ChildSupervisor,
}

impl CustomJsonTool {
    pub fn from_manifest(
        manifest: CustomToolManifest,
        trusted_local: bool,
        effect_override: Option<CapabilityEffect>,
        workspace_max: Option<CapabilityEffect>,
        env: Vec<(String, String)>,
        max_restarts: u32,
    ) -> Result<Self> {
        let command = PathBuf::from(&manifest.spec.command);
        validate_command(&command)?;
        let input_schema = manifest.spec.input_schema.clone();
        let effect = CapabilityEffect::parse(&manifest.spec.effect)?;
        let recovery = manifest
            .spec
            .recovery
            .as_deref()
            .map(RecoveryMode::parse)
            .transpose()?;
        let version = manifest.metadata.version.clone();
        let name = manifest.metadata.name.clone();
        let binary = fs::read(&command).unwrap_or_default();
        let mut descriptor = CapabilityDescriptor {
            id: capability_id(CapabilitySource::Custom.as_str(), &name, &version),
            source: CapabilitySource::Custom,
            name: format!("custom/{name}"),
            description: format!("Custom tool {name}@{version}. Process-local JSON runner."),
            input_schema: input_schema.clone(),
            output_schema: manifest.spec.output_schema.clone(),
            effect,
            target_requirements: Vec::new(),
            timeout_seconds: manifest.spec.timeout_seconds.unwrap_or(10).max(1),
            max_output_bytes: manifest.spec.max_output_bytes.unwrap_or(64 * 1024).max(1),
            recovery,
            provenance: Provenance {
                source: CapabilitySource::Custom,
                version,
                schema_hash: hash_schema(&input_schema),
                binary_hash: Some(hash_bytes(&binary)),
                origin: Some(command.display().to_string()),
            },
            content_sensitivity: crate::evidence::Sensitivity::Internal,
            enabled: true,
            trusted_local,
        };
        descriptor = descriptor.apply_strictest(None, effect_override);
        enforce_workspace_ceiling(&descriptor, workspace_max)?;
        descriptor.validate_for_enablement()?;
        if !trusted_local {
            return Err(OpsCodexError::Policy(format!(
                "custom tool `{}` requires trusted_local",
                descriptor.id
            )));
        }
        Ok(Self {
            capability: descriptor,
            command,
            args: manifest.spec.args,
            cwd: manifest.spec.cwd.map(PathBuf::from),
            env: env
                .into_iter()
                .filter(|(key, _)| {
                    manifest
                        .spec
                        .env_allowlist
                        .iter()
                        .any(|allowed| allowed == key)
                })
                .collect(),
            supervisor: ChildSupervisor::new(max_restarts),
        })
    }
}

#[async_trait]
impl Tool for CustomJsonTool {
    fn name(&self) -> &str {
        &self.capability.name
    }

    fn description(&self) -> &str {
        &self.capability.description
    }

    fn schema(&self) -> Value {
        self.capability.input_schema.clone()
    }

    fn risk(&self) -> ToolRisk {
        match self.capability.effect {
            CapabilityEffect::Observe => ToolRisk::Safe,
            _ => ToolRisk::Ask,
        }
    }

    fn descriptor(&self) -> CapabilityDescriptor {
        self.capability.clone()
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        let current = fs::read(&self.command).unwrap_or_default();
        if Some(hash_bytes(&current)) != self.capability.provenance.binary_hash {
            return Err(OpsCodexError::Policy(format!(
                "custom tool `{}` binary hash changed",
                self.capability.id
            )));
        }
        let stdin = serde_json::to_vec(&arguments).unwrap_or_default();
        let output = self
            .supervisor
            .run_once(
                SpawnSpec {
                    command: self.command.clone(),
                    args: self.args.clone(),
                    cwd: self.cwd.clone(),
                    env: self.env.clone(),
                    timeout: std::time::Duration::from_secs(self.capability.timeout_seconds),
                    max_output_bytes: self.capability.max_output_bytes,
                },
                &stdin,
                cancellation,
            )
            .await?;
        let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            OpsCodexError::Protocol(format!("custom tool returned invalid JSON: {error}"))
        })?;
        if let Some(schema) = &self.capability.output_schema {
            validate_against_schema(&parsed, schema)?;
        }
        Ok(ToolOutput {
            content: json!({
                "source": "custom",
                "tool": self.capability.name,
                "version": self.capability.provenance.version,
                "hash": self.capability.provenance.schema_hash,
                "result": parsed,
                "truncated": output.truncated,
            }),
            evidence: EvidenceMeta::new("custom")
                .with_query(self.capability.id.clone())
                .with_summary(format!("custom tool {}", self.capability.name))
                .with_truncated(output.truncated),
        })
    }
}

fn validate_against_schema(value: &Value, schema: &Value) -> Result<()> {
    let expected = schema.get("type").and_then(Value::as_str);
    let matches = match expected {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("number") | Some("integer") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => true,
    };
    if matches {
        Ok(())
    } else {
        Err(OpsCodexError::Protocol(
            "custom tool output does not match outputSchema".into(),
        ))
    }
}
