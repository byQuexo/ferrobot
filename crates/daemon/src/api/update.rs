//! Version and self-update of source builds (`/v1/update`).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::update::UpdateInfo;

/// This daemon's version and, for a source build, the state of its update.
#[utoipa::path(get, path = "/v1/update", tag = "system",
    responses((status = 200, body = UpdateInfo)))]
pub(crate) async fn get_update(State(state): State<AppState>) -> Json<UpdateInfo> {
    Json(crate::update::info(&state))
}

/// Updates a source build (a debug build from a ferrobot checkout on a clean `main`): pulls,
/// rebuilds the UI, daemon and desktop shell, then re-executes the daemon with the same
/// arguments. Runs in the background; poll `GET /v1/update`.
#[utoipa::path(post, path = "/v1/update", tag = "system",
    responses(
        (status = 202, description = "Update started", body = UpdateInfo),
        (status = 404, description = "Not a source build", body = ErrorBody),
        (status = 409, description = "The checkout has changes or is not on main, or an update is running",
            body = ErrorBody)))]
pub(crate) async fn start_update(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<UpdateInfo>), ApiError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::update::start(&state).await?),
    ))
}
