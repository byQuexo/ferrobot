use thiserror::Error;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("{backend} backend does not implement {operation} yet")]
    NotImplemented {
        backend: &'static str,
        operation: &'static str,
    },
    #[error("sandbox {0} not found")]
    NotFound(String),
    #[error("invalid sandbox path {path:?}: {reason}")]
    InvalidPath { path: String, reason: &'static str },
    #[error("handle belongs to backend {actual:?}, expected {expected:?}")]
    WrongBackend {
        expected: &'static str,
        actual: String,
    },
    #[error("docker: {0}")]
    Docker(#[from] bollard::errors::Error),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("object store: {0}")]
    Storage(#[from] object_store::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}
