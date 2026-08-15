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
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("operation timed out: {0}")]
    Timeout(String),
}

pub type Result<T> = std::result::Result<T, OpsCodexError>;
