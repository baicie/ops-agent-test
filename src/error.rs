use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum OpsCodexError {
    #[error("model error: {0}")]
    Model(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("policy error: {0}")]
    Policy(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("turn cancelled")]
    Cancelled,
    #[error("maximum agent steps exceeded")]
    MaxStepsExceeded,
    #[error("thread already has an active turn")]
    TurnAlreadyRunning,
    #[error("needs reconciliation: {0}")]
    NeedsReconciliation(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("operation timed out: {0}")]
    Timeout(String),
    #[error("{class}: {message}")]
    Connector {
        class: ConnectorClass,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorClass {
    Auth,
    RateLimit,
    Timeout,
    Unavailable,
    InvalidQuery,
    MalformedData,
    Policy,
}

impl ConnectorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::RateLimit => "rate_limit",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::InvalidQuery => "invalid_query",
            Self::MalformedData => "malformed_data",
            Self::Policy => "policy",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(self, Self::RateLimit | Self::Timeout | Self::Unavailable)
    }
}

impl std::fmt::Display for ConnectorClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl OpsCodexError {
    pub fn connector(class: ConnectorClass, message: impl Into<String>) -> Self {
        Self::Connector {
            class,
            message: message.into(),
        }
    }

    pub fn connector_class(&self) -> ConnectorClass {
        match self {
            Self::Timeout(_) => ConnectorClass::Timeout,
            Self::Policy(_) => ConnectorClass::Policy,
            Self::Connector { class, .. } => *class,
            Self::Cancelled => ConnectorClass::Unavailable,
            _ => ConnectorClass::Unavailable,
        }
    }

    pub fn retryable(&self) -> bool {
        self.connector_class().retryable()
    }
}

pub type Result<T> = std::result::Result<T, OpsCodexError>;
