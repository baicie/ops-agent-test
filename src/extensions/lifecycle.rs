use serde::{Deserialize, Serialize};

use crate::extensions::CapabilitySummary;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionState {
    Discovered,
    Validated,
    Configured,
    Enabled,
    Healthy,
    Degraded,
    Disabled,
}

impl ExtensionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Validated => "validated",
            Self::Configured => "configured",
            Self::Enabled => "enabled",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionHealth {
    pub state: ExtensionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub restart_count: u32,
}

impl ExtensionHealth {
    pub fn disabled(detail: impl Into<String>) -> Self {
        Self {
            state: ExtensionState::Disabled,
            detail: Some(detail.into()),
            restart_count: 0,
        }
    }

    pub fn healthy() -> Self {
        Self {
            state: ExtensionState::Healthy,
            detail: None,
            restart_count: 0,
        }
    }

    pub fn degraded(detail: impl Into<String>, restart_count: u32) -> Self {
        Self {
            state: ExtensionState::Degraded,
            detail: Some(detail.into()),
            restart_count,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionSummary {
    pub id: String,
    pub kind: String,
    pub version: String,
    pub hash: String,
    pub enabled: bool,
    pub health: ExtensionHealth,
    pub workspaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilitySummary>,
}
