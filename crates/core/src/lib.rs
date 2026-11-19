//! Core domain types shared by every ferrobot crate.
//!
//! This crate is deliberately dependency-light: no I/O, no async runtime.
//! It defines identifiers, the agent and mailbox models, and the JSON wire
//! protocol spoken between the daemon and the in-sandbox agent runner.

pub mod agent;
pub mod error;
pub mod mail;
pub mod protocol;

pub use agent::{Agent, AgentId, AgentName, AgentStatus};
pub use error::CoreError;
pub use mail::{MailMessage, MailStatus, Sender};

/// Human-readable project name (also set in the workspace `Cargo.toml` and README).
pub const PROJECT_NAME: &str = "ferrobot";

/// Version of the daemon <-> runner protocol. Bumped on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;
