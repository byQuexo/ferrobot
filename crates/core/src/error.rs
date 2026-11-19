use thiserror::Error;

/// Validation errors for core domain values.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("invalid agent name {name:?}: {reason}")]
    InvalidAgentName { name: String, reason: &'static str },
    #[error("invalid agent id {0:?}")]
    InvalidAgentId(String),
    #[error("invalid sender {0:?}")]
    InvalidSender(String),
    #[error("unknown {kind} value {value:?}")]
    UnknownVariant { kind: &'static str, value: String },
}
