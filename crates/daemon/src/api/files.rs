//! Files agents shared with the operator (`share_file`), read from storage.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use super::{ApiResult, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::{AppState, files};

/// Largest file shown as text.
const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SharedFileView {
    name: String,
    size: u64,
    /// The text, for UTF-8 files up to 2 MiB; otherwise `null`.
    content: Option<String>,
    /// For .html files: where the page opens (relative to the daemon), in a
    /// sandboxed frame without access to the app or the operator token.
    page_url: Option<String>,
}

fn field<'a>(payload: &'a serde_json::Value, name: &str) -> Result<&'a str, ApiError> {
    payload[name]
        .as_str()
        .ok_or_else(|| ApiError::NotFound("shared file not found".into()))
}

/// A file the agent shared (`file` event id), from storage.
#[utoipa::path(get, path = "/v1/agents/{id}/files/{file_id}", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("file_id" = i64, Path, description = "Id of the `file` event")),
    responses((status = 200, body = SharedFileView),
        (status = 404, description = "Unknown agent or file", body = ErrorBody)))]
pub(crate) async fn shared_file(
    State(state): State<AppState>,
    Path((id, file_id)): Path<(String, i64)>,
) -> ApiResult<SharedFileView> {
    let agent = parse_id(&id)?;
    let event = state.store.get_event(agent, file_id).await?;
    if event.kind != "file" {
        return Err(ApiError::NotFound("shared file not found".into()));
    }
    let name = field(&event.payload, "name")?.to_owned();
    let bytes = files::read(&state, field(&event.payload, "key")?)
        .await
        .map_err(|_| ApiError::NotFound(format!("{name} is no longer stored")))?;
    let page_url = files::is_page(&name).then(|| {
        let view = event.payload["view"].as_str().unwrap_or_default();
        format!("/artifacts/{agent}/{file_id}/{view}")
    });
    let content = (bytes.len() <= MAX_PREVIEW_BYTES)
        .then(|| String::from_utf8(bytes.clone()).ok())
        .flatten();
    Ok(Json(SharedFileView {
        name,
        size: bytes.len() as u64,
        content,
        page_url,
    }))
}

/// An .html file an agent shared, as a page. Needs no operator token: a
/// frame cannot send one, and the random `view` key is the capability. The
/// page runs sandboxed (opaque origin: no app, cookies or token) and cannot
/// make network requests.
#[utoipa::path(get, path = "/artifacts/{id}/{file_id}/{view}", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("file_id" = i64, Path, description = "Id of the `file` event"),
        ("view" = String, Path, description = "The file's view key (from `page_url`)")),
    responses((status = 200, description = "The page", content_type = "text/html", body = String),
        (status = 404, description = "Unknown page", body = ErrorBody)))]
pub(crate) async fn page(
    State(state): State<AppState>,
    Path((id, file_id, view)): Path<(String, i64, String)>,
) -> Result<Response, ApiError> {
    let missing = || ApiError::NotFound("page not found".into());
    let agent = parse_id(&id).map_err(|_| missing())?;
    let event = state
        .store
        .get_event(agent, file_id)
        .await
        .map_err(|_| missing())?;
    let expected = event.payload["view"].as_str().unwrap_or_default();
    let name = event.payload["name"].as_str().unwrap_or_default();
    if event.kind != "file"
        || expected.is_empty()
        || !crate::auth::tokens_equal(&view, expected)
        || !files::is_page(name)
    {
        return Err(missing());
    }
    let bytes = files::read(&state, field(&event.payload, "key")?)
        .await
        .map_err(|_| missing())?;
    let csp = "sandbox allow-scripts; default-src 'none'; \
               script-src 'unsafe-inline' https:; style-src 'unsafe-inline' https:; \
               img-src data: blob: https:; font-src data: https:; media-src data: blob: https:; \
               connect-src 'none'; form-action 'none'; base-uri 'none'";
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_SECURITY_POLICY, csp),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Html(String::from_utf8_lossy(&bytes).into_owned()),
    )
        .into_response())
}
