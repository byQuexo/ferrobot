use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Core(#[from] ferrobot_core::CoreError),
    #[error("{entity} not found: {key}")]
    NotFound { entity: &'static str, key: String },
    #[error("{entity} already exists: {key}")]
    Conflict { entity: &'static str, key: String },
    #[error("database lock poisoned")]
    Poisoned,
    /// Reading or writing a memory note in the Obsidian vault failed.
    #[error("vault: {0}")]
    Vault(String),
    #[error("blocking task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl StoreError {
    pub(crate) fn corrupt(e: impl std::fmt::Display) -> Self {
        Self::Corrupt(e.to_string())
    }
}
