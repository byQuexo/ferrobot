//! Command-line tools for every agent's sandbox (`/v1/clis`).

use axum::Json;
use axum::extract::State;
use ferrobot_sandbox::SandboxError;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use crate::AppState;
use crate::clis::{self, BuildStatus};
use crate::error::{ApiError, ErrorBody};

/// The CLI packages and where their image build stands.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ClisView {
    /// Debian packages added to the base image, sorted.
    packages: Vec<String>,
    status: BuildStatus,
    /// Why the last build failed; null otherwise.
    error: Option<String>,
    /// Whether the sandbox backend can build images (Docker only).
    supported: bool,
}

/// A new CLI package list.
#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct UpdateClis {
    /// Debian package names, at most 32; empty: the plain base image.
    packages: Vec<String>,
}

async fn view(state: &AppState) -> ApiResult<ClisView> {
    let (status, error) = state.clis.get();
    Ok(Json(ClisView {
        packages: clis::load(state).await?.packages,
        status,
        error,
        supported: state.backend.supports_image_builds(),
    }))
}

/// The CLI packages in every agent's sandbox.
#[utoipa::path(get, path = "/v1/clis", tag = "clis",
    responses((status = 200, body = ClisView)))]
pub(crate) async fn get_clis(State(state): State<AppState>) -> ApiResult<ClisView> {
    view(&state).await
}

/// Replaces the CLI packages and builds their image in the background. New sandboxes start
/// from it; existing ones switch when they next wake, keeping their home directory.
#[utoipa::path(put, path = "/v1/clis", tag = "clis",
    request_body = UpdateClis,
    responses((status = 200, body = ClisView),
        (status = 400, description = "Not a Debian package name, or too many", body = ErrorBody),
        (status = 409, description = "A build is running", body = ErrorBody),
        (status = 501, description = "The sandbox backend cannot build images", body = ErrorBody)))]
pub(crate) async fn put_clis(
    State(state): State<AppState>,
    Json(req): Json<UpdateClis>,
) -> ApiResult<ClisView> {
    let packages = ferrobot_sandbox::package_set(&req.packages)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if !packages.is_empty() && !state.backend.supports_image_builds() {
        return Err(SandboxError::NotImplemented {
            backend: state.backend.kind(),
            operation: "image builds",
        }
        .into());
    }
    clis::apply(&state, packages).await?;
    view(&state).await
}
