//! Command-line tools in every agent's sandbox: one image, the base image plus Debian packages.
//! A sandbox picks it up when it is next created or woken (`lifecycle::wake`).

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;

const SETTING: &str = "clis";
const BUILD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The operator's packages and the image last built from them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Stored {
    pub packages: Vec<String>,
    /// Last successful build; `None`: the base image.
    pub image: Option<String>,
}

/// Where the image build stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// No CLIs: sandboxes run the base image.
    #[default]
    Idle,
    Building,
    /// New and woken sandboxes start from the image with these CLIs.
    Ready,
    /// The last build failed; sandboxes keep the previous image.
    Failed,
}

/// Build status and error, in memory: a restart rebuilds, usually from the cache.
#[derive(Debug, Default)]
pub struct Builds(Mutex<(BuildStatus, Option<String>)>);

impl Builds {
    fn lock(&self) -> MutexGuard<'_, (BuildStatus, Option<String>)> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn get(&self) -> (BuildStatus, Option<String>) {
        self.lock().clone()
    }

    fn set(&self, status: BuildStatus, error: Option<String>) {
        *self.lock() = (status, error);
    }

    /// Marks a build as started; `false` if one is running.
    fn start(&self) -> bool {
        let mut current = self.lock();
        if current.0 == BuildStatus::Building {
            return false;
        }
        *current = (BuildStatus::Building, None);
        true
    }
}

pub(crate) async fn load(state: &AppState) -> Result<Stored, ApiError> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|e| ApiError::Internal(format!("CLI settings: {e}")))
        }
        None => Ok(Stored::default()),
    }
}

async fn save(state: &AppState, stored: &Stored) -> Result<(), ApiError> {
    let raw = serde_json::to_string(stored).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.store.put_setting(SETTING, raw).await?;
    Ok(())
}

/// The image new sandboxes start from; `None`: the backend's default.
pub(crate) async fn image(state: &AppState) -> Result<Option<String>, ApiError> {
    Ok(load(state).await?.image)
}

/// Saves validated `packages` and builds their image in the background.
pub(crate) async fn apply(state: &AppState, packages: Vec<String>) -> Result<(), ApiError> {
    if !state.clis.start() {
        return Err(ApiError::Conflict(
            "the CLI image is still building; try again when it is done".into(),
        ));
    }
    let saved = async {
        let mut stored = load(state).await?;
        stored.packages = packages;
        save(state, &stored).await
    }
    .await;
    if let Err(e) = saved {
        state.clis.set(BuildStatus::Failed, Some(e.to_string()));
        return Err(e);
    }
    let state = state.clone();
    tokio::spawn(async move { build(&state).await });
    Ok(())
}

/// Rebuilds on startup, so a rebuilt base image reaches agents with CLIs (a new tag).
pub fn resume(state: &AppState) {
    if !state.backend.supports_image_builds() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        match load(&state).await {
            Ok(stored) if !stored.packages.is_empty() && state.clis.start() => build(&state).await,
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "cannot read the CLI settings"),
        }
    });
}

async fn build(state: &AppState) {
    let outcome = async {
        let mut stored = load(state).await?;
        stored.image =
            tokio::time::timeout(BUILD_TIMEOUT, state.backend.build_image(&stored.packages))
                .await
                .map_err(|_| {
                    ApiError::Upstream("the build took longer than 15 minutes".into())
                })??;
        save(state, &stored).await?;
        Ok::<_, ApiError>(stored)
    }
    .await;
    match outcome {
        Ok(stored) if stored.packages.is_empty() => state.clis.set(BuildStatus::Idle, None),
        Ok(_) => state.clis.set(BuildStatus::Ready, None),
        Err(e) => {
            tracing::warn!(error = %e, "CLI image build failed");
            let message = match e {
                ApiError::Sandbox(e) => e.to_string(),
                other => other.to_string(),
            };
            state.clis.set(BuildStatus::Failed, Some(message));
        }
    }
}
