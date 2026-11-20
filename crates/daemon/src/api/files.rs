//! Files agents shared with the operator (`share_file`), read from storage.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

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
    /// For .html files: the page (relative to the daemon), sandboxed, with
    /// no access to the app or the operator token.
    page_url: Option<String>,
    /// The file itself (relative to the daemon): pages and PDFs open inline,
    /// other files download. Add `?download=1` to always download.
    url: String,
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
    let view = event.payload["view"].as_str().unwrap_or_default();
    let url = format!("/artifacts/{agent}/{file_id}/{view}");
    let content = (bytes.len() <= MAX_PREVIEW_BYTES)
        .then(|| String::from_utf8(bytes.clone()).ok())
        .flatten();
    Ok(Json(SharedFileView {
        page_url: files::is_page(&name).then(|| url.clone()),
        name,
        size: bytes.len() as u64,
        content,
        url,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct FileQuery {
    /// Any value: send the file as a download.
    download: Option<String>,
}

/// A file an agent shared. Needs no operator token: frames and the system
/// browser cannot send one, and the random `view` key is the capability.
/// .html opens as a sandboxed page (opaque origin, no network requests),
/// a real PDF opens inline, anything else (or `?download`) downloads.
#[utoipa::path(get, path = "/artifacts/{id}/{file_id}/{view}", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("file_id" = i64, Path, description = "Id of the `file` event"),
        ("view" = String, Path, description = "The file's view key (from `url`)"),
        FileQuery),
    responses((status = 200, description = "The file", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 404, description = "Unknown file", body = ErrorBody)))]
pub(crate) async fn page(
    State(state): State<AppState>,
    Path((id, file_id, view)): Path<(String, i64, String)>,
    Query(q): Query<FileQuery>,
) -> Result<Response, ApiError> {
    let missing = || ApiError::NotFound("file not found".into());
    let agent = parse_id(&id).map_err(|_| missing())?;
    let event = state
        .store
        .get_event(agent, file_id)
        .await
        .map_err(|_| missing())?;
    let expected = event.payload["view"].as_str().unwrap_or_default();
    if event.kind != "file" || expected.is_empty() || !crate::auth::tokens_equal(&view, expected) {
        return Err(missing());
    }
    let name = event.payload["name"].as_str().unwrap_or("file").to_owned();
    let bytes = files::read(&state, field(&event.payload, "key")?)
        .await
        .map_err(|_| missing())?;
    let pdf = files::is_pdf(&name) && bytes.starts_with(b"%PDF-");
    let (content_type, disposition, csp) = if q.download.is_some() {
        ("application/octet-stream", "attachment", SANDBOX)
    } else if files::is_page(&name) {
        ("text/html; charset=utf-8", "inline", PAGE_CSP)
    } else if pdf {
        // A sandbox would stop browsers' PDF viewers; nosniff keeps it a PDF.
        ("application/pdf", "inline", "default-src 'none'")
    } else {
        ("application/octet-stream", "attachment", SANDBOX)
    };
    let filename = name.replace(['"', '\\', '\r', '\n'], "_");
    let disposition = HeaderValue::from_str(&format!("{disposition}; filename=\"{filename}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(csp),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        bytes,
    )
        .into_response())
}

/// Shared pages: scripts and styles inline or from https, no requests out.
const PAGE_CSP: &str = "sandbox allow-scripts; default-src 'none'; \
    script-src 'unsafe-inline' https:; style-src 'unsafe-inline' https:; \
    img-src data: blob: https:; font-src data: https:; media-src data: blob: https:; \
    connect-src 'none'; form-action 'none'; base-uri 'none'";

/// Downloads: nothing runs even if a browser renders them.
const SANDBOX: &str = "sandbox; default-src 'none'";
