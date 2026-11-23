//! Files agents shared with the operator (`share_file`), read from storage.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::{AppState, files};

/// Largest file shown as text.
const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/agents/{id}/files/{file_id}", get(shared_file))
        .route("/artifacts/{id}/{file_id}", get(page))
        .route("/v1/previews", post(create_preview))
        .route("/artifacts/previews/{id}", get(preview))
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SharedFileView {
    name: String,
    size: u64,
    /// The text, for UTF-8 files up to 2 MiB; otherwise `null`.
    content: Option<String>,
    /// For .html and .htm files: the page (relative to the daemon), sandboxed, with
    /// no access to the app or the operator token.
    page_url: Option<String>,
    /// The file itself (relative to the daemon), a signed link that works for
    /// 30 minutes: pages and PDFs open inline, other files download. Add
    /// `&download=1` to always download.
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
    let bytes = files::read(&state, agent, field(&event.payload, "key")?)
        .await
        .map_err(|_| ApiError::NotFound(format!("{name} is no longer stored")))?;
    let url = files::fresh_link(&state, agent, file_id)
        .await
        .map_err(|e| ApiError::Internal(format!("file link: {e}")))?;
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
    /// Expiry of the link (unix seconds), from `url`.
    exp: Option<u64>,
    /// Signature of the link, from `url`.
    sig: Option<String>,
    /// Any value: send the file as a download.
    download: Option<String>,
}

/// A shared file by signed, expiring link (HMAC over agent, file and expiry), as frames cannot
/// send the operator token. Pages open sandboxed, a real PDF inline, anything else downloads.
#[utoipa::path(get, path = "/artifacts/{id}/{file_id}", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"),
        ("file_id" = i64, Path, description = "Id of the `file` event"),
        FileQuery),
    responses((status = 200, description = "The file", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 404, description = "Unknown file", body = ErrorBody)))]
pub(crate) async fn page(
    State(state): State<AppState>,
    Path((id, file_id)): Path<(String, i64)>,
    Query(q): Query<FileQuery>,
) -> Result<Response, ApiError> {
    let missing = || ApiError::NotFound("file not found, or the link expired".into());
    let agent = parse_id(&id).map_err(|_| missing())?;
    let (Some(exp), Some(sig)) = (q.exp, q.sig.as_deref()) else {
        return Err(missing());
    };
    if !files::link_valid(&state, agent, file_id, exp, sig).await {
        return Err(missing());
    }
    let event = state
        .store
        .get_event(agent, file_id)
        .await
        .map_err(|_| missing())?;
    if event.kind != "file" {
        return Err(missing());
    }
    let name = event.payload["name"].as_str().unwrap_or("file").to_owned();
    let bytes = files::read(&state, agent, field(&event.payload, "key")?)
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
    Ok(respond(content_type, disposition, csp, bytes))
}

fn respond(
    content_type: &'static str,
    disposition: HeaderValue,
    csp: &'static str,
    body: Vec<u8>,
) -> Response {
    (
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
        body,
    )
        .into_response()
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PreviewRequest {
    /// The block's HTML, at most 512 KiB.
    html: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct PreviewLink {
    /// The block as a sandboxed page (relative to the daemon), a signed link that works for
    /// 30 minutes.
    url: String,
}

/// Serves a HTML block from the chat as a page, so its inline styles and scripts run under
/// the shared pages' policy (no network) instead of being blocked by the app's.
#[utoipa::path(post, path = "/v1/previews", tag = "chat", request_body = PreviewRequest,
    responses((status = 200, body = PreviewLink),
        (status = 400, description = "Larger than 512 KiB", body = ErrorBody)))]
pub(crate) async fn create_preview(
    State(state): State<AppState>,
    Json(req): Json<PreviewRequest>,
) -> ApiResult<PreviewLink> {
    if req.html.len() > files::MAX_BLOCK_BYTES {
        return Err(ApiError::BadRequest(format!(
            "an HTML preview is at most {} KiB",
            files::MAX_BLOCK_BYTES / 1024
        )));
    }
    let id = state.previews.put(&req.html);
    let url = files::fresh_preview_link(&state, &id)
        .await
        .map_err(|e| ApiError::Internal(format!("preview link: {e}")))?;
    Ok(Json(PreviewLink { url }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct LinkQuery {
    /// Expiry of the link (unix seconds), from `url`.
    exp: Option<u64>,
    /// Signature of the link, from `url`.
    sig: Option<String>,
}

/// A HTML block by signed, expiring link, as a sandboxed page.
#[utoipa::path(get, path = "/artifacts/previews/{id}", tag = "chat",
    params(("id" = String, Path, description = "Block id, from `POST /v1/previews`"), LinkQuery),
    responses((status = 200, description = "The page", content_type = "text/html", body = String),
        (status = 404, description = "Unknown block, or the link expired", body = ErrorBody)))]
pub(crate) async fn preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LinkQuery>,
) -> Result<Response, ApiError> {
    let missing = || ApiError::NotFound("preview not found, or the link expired".into());
    let (Some(exp), Some(sig)) = (q.exp, q.sig.as_deref()) else {
        return Err(missing());
    };
    if !files::preview_link_valid(&state, &id, exp, sig).await {
        return Err(missing());
    }
    let html = state.previews.get(&id).ok_or_else(missing)?;
    Ok(respond(
        "text/html; charset=utf-8",
        HeaderValue::from_static("inline"),
        PAGE_CSP,
        html.as_bytes().to_vec(),
    ))
}

/// Shared pages: inline scripts, styles and data only. No remote origin at
/// all (an allowlist would be a channel out and a script supply chain).
const PAGE_CSP: &str = "sandbox allow-scripts; default-src 'none'; \
    script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
    img-src data: blob:; font-src data:; media-src data: blob:; \
    connect-src 'none'; form-action 'none'; base-uri 'none'";

/// Downloads: nothing runs even if a browser renders them.
const SANDBOX: &str = "sandbox; default-src 'none'";
