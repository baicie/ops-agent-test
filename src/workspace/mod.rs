use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    OpsCodexError, Result,
    config::{Config, WorkspaceConfigEntry},
    runtime::WorkspaceId,
};

const DEFAULT_KINDS: &[&str] = &[
    "Namespace",
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "Pod",
    "Service",
    "EndpointSlice",
    "Job",
    "Node",
    "Event",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialRef {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub display_name: String,
    pub environment: String,
    pub prometheus_url: String,
    pub loki_url: String,
    pub loki_tenant_header: String,
    pub loki_tenant_env: String,
    pub tempo_url: String,
    pub allowed_hosts: Vec<String>,
    pub allowed_containers: Vec<String>,
    pub kubernetes: Option<KubernetesScope>,
    pub runbook_dir: Option<PathBuf>,
    pub max_concurrent_turns: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubernetesScope {
    pub cluster_alias: String,
    pub kubeconfig_env: String,
    pub context: Option<String>,
    pub allowed_namespaces: Vec<String>,
    pub allowed_kinds: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub id: String,
    pub display_name: String,
    pub environment: String,
    pub connectors: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceCatalog {
    workspaces: BTreeMap<String, Workspace>,
}

impl WorkspaceCatalog {
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut catalog = Self::default();
        if config.workspaces.is_empty() {
            catalog.insert(legacy_default(config)?)?;
        } else {
            for entry in &config.workspaces {
                catalog.insert(workspace_from_entry(config, entry)?)?;
            }
            if !catalog.workspaces.contains_key("default") {
                catalog.insert(legacy_default(config)?)?;
            }
        }
        Ok(catalog)
    }

    pub fn insert(&mut self, workspace: Workspace) -> Result<()> {
        workspace.id.validate()?;
        let key = workspace.id.as_str().to_owned();
        if self.workspaces.contains_key(&key) {
            return Err(OpsCodexError::Protocol(format!(
                "duplicate workspace id `{key}`"
            )));
        }
        self.workspaces.insert(key, workspace);
        Ok(())
    }

    pub fn get(&self, id: &WorkspaceId) -> Result<&Workspace> {
        self.workspaces
            .get(id.as_str())
            .ok_or_else(|| OpsCodexError::NotFound(format!("workspace {}", id.as_str())))
    }

    pub fn require(&self, id: &WorkspaceId) -> Result<&Workspace> {
        self.get(id)
    }

    pub fn default_id(&self) -> WorkspaceId {
        WorkspaceId::default()
    }

    pub fn contains(&self, id: &WorkspaceId) -> bool {
        self.workspaces.contains_key(id.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Workspace> {
        self.workspaces.values()
    }

    pub fn summaries(&self) -> Vec<WorkspaceSummary> {
        self.workspaces.values().map(Workspace::summary).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }
}

impl Workspace {
    pub fn summary(&self) -> WorkspaceSummary {
        let mut connectors = vec!["prometheus".into(), "loki".into(), "tempo".into()];
        if self.kubernetes.is_some() {
            connectors.push("kubernetes".into());
        }
        if self.runbook_dir.is_some() {
            connectors.push("runbook".into());
        }
        WorkspaceSummary {
            id: self.id.as_str().to_owned(),
            display_name: self.display_name.clone(),
            environment: self.environment.clone(),
            connectors,
        }
    }

    pub fn credential_refs(&self) -> Vec<CredentialRef> {
        let mut refs = vec![CredentialRef {
            name: self.loki_tenant_env.clone(),
        }];
        if let Some(kubernetes) = &self.kubernetes {
            refs.push(CredentialRef {
                name: kubernetes.kubeconfig_env.clone(),
            });
        }
        refs
    }
}

fn legacy_default(config: &Config) -> Result<Workspace> {
    workspace_from_entry(
        config,
        &WorkspaceConfigEntry {
            id: "default".into(),
            display_name: Some("Local demo".into()),
            environment: Some("local".into()),
            ..WorkspaceConfigEntry::default()
        },
    )
}

fn workspace_from_entry(config: &Config, entry: &WorkspaceConfigEntry) -> Result<Workspace> {
    let id = WorkspaceId::new(entry.id.trim());
    id.validate()?;
    let kubernetes = kubernetes_scope(entry);
    Ok(Workspace {
        id: id.clone(),
        display_name: entry
            .display_name
            .clone()
            .unwrap_or_else(|| id.as_str().to_owned()),
        environment: entry.environment.clone().unwrap_or_else(|| "local".into()),
        prometheus_url: entry
            .prometheus_url
            .clone()
            .unwrap_or_else(|| config.prometheus.url.clone()),
        loki_url: entry
            .loki_url
            .clone()
            .unwrap_or_else(|| config.loki.url.clone()),
        loki_tenant_header: config.loki.tenant_header.clone(),
        loki_tenant_env: entry
            .loki_tenant_env
            .clone()
            .unwrap_or_else(|| config.loki.tenant_env.clone()),
        tempo_url: entry
            .tempo_url
            .clone()
            .unwrap_or_else(|| config.tempo.url.clone()),
        allowed_hosts: entry
            .allowed_hosts
            .clone()
            .unwrap_or_else(|| config.targets.allowed_hosts.clone()),
        allowed_containers: entry
            .allowed_containers
            .clone()
            .unwrap_or_else(|| config.targets.allowed_containers.clone()),
        kubernetes,
        runbook_dir: entry.runbook_dir.as_ref().map(PathBuf::from),
        max_concurrent_turns: entry
            .max_concurrent_turns
            .unwrap_or(config.runtime.max_concurrent_turns)
            .max(1),
    })
}

fn kubernetes_scope(entry: &WorkspaceConfigEntry) -> Option<KubernetesScope> {
    let kubeconfig_env = entry.kubeconfig_env.as_ref()?;
    if kubeconfig_env.trim().is_empty() {
        return None;
    }
    let allowed_kinds = entry.allowed_kinds.clone().unwrap_or_else(|| {
        DEFAULT_KINDS
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect()
    });
    Some(KubernetesScope {
        cluster_alias: entry
            .cluster_alias
            .clone()
            .unwrap_or_else(|| entry.id.clone()),
        kubeconfig_env: kubeconfig_env.clone(),
        context: entry.kube_context.clone(),
        allowed_namespaces: entry.allowed_namespaces.clone().unwrap_or_default(),
        allowed_kinds,
    })
}

pub fn deny_cross_workspace(
    expected: &WorkspaceId,
    actual: &WorkspaceId,
    resource: &str,
) -> Result<()> {
    if expected == actual {
        return Ok(());
    }
    Err(OpsCodexError::Policy(format!(
        "cross-workspace access denied: {resource} belongs to {} not {}",
        actual.as_str(),
        expected.as_str()
    )))
}
