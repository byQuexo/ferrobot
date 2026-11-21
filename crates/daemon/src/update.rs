//! Self-update status. Release builds update through the desktop app's updater; a debug build
//! started from a ferrobot checkout pulls, rebuilds and restarts itself (`source`).

use serde::Serialize;
use utoipa::ToSchema;

#[cfg(debug_assertions)]
mod source;
#[cfg(debug_assertions)]
pub use source::{Commands, Runner, Source};
#[cfg(debug_assertions)]
pub(crate) use source::{info, start};

/// Where a source update stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    /// No update has run since the daemon started.
    #[default]
    Idle,
    /// Steps are running.
    Running,
    /// The daemon already runs the latest commit; nothing was rebuilt.
    UpToDate,
    /// A step failed; see `step`, `error` and `log`.
    Failed,
    /// Rebuilt; the daemon is replacing itself with the new binary.
    Restarting,
}

/// This daemon's version and self-update status.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct UpdateInfo {
    /// Daemon version.
    pub version: String,
    /// A debug build from a ferrobot checkout, which `POST /v1/update` can pull, rebuild and
    /// restart. Release builds update through the desktop app.
    pub source_build: bool,
    /// The checkout's commit when this daemon started (source builds).
    pub commit: Option<String>,
    pub state: UpdateState,
    /// The command that is running or failed.
    pub step: Option<String>,
    /// Why the update failed.
    pub error: Option<String>,
    /// The last lines of the steps' output.
    pub log: Vec<String>,
}

#[cfg(not(debug_assertions))]
pub(crate) fn info(_: &crate::AppState) -> UpdateInfo {
    UpdateInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        ..UpdateInfo::default()
    }
}

#[cfg(not(debug_assertions))]
#[allow(clippy::unused_async, reason = "same signature as the source build's")]
pub(crate) async fn start(_: &crate::AppState) -> Result<UpdateInfo, crate::error::ApiError> {
    Err(crate::error::ApiError::NotFound(
        "not a source build".into(),
    ))
}
