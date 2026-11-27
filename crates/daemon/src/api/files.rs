//! Files agents shared with the operator (`share_file`), read from storage, and files the
//! operator uploads into an agent's workspace.

use std::collections::HashSet;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ferrobot_sandbox::{ExecRequest, SandboxError, SandboxPath};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ApiResult, parse_id};
use crate::error::{ApiError, ErrorBody};
use crate::{AppState, files, lifecycle};

const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/agents/{id}/files/{file_id}", get(shared_file))
        .route(
            "/v1/agents/{id}/uploads",
            post(upload).layer(upload_limit()),
        )
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
    let bytes = if csp == PAGE_CSP {
        thin_scrollbars(bytes)
    } else {
        bytes
    };
    Ok(respond(content_type, disposition, csp, bytes))
}

/// Thin scrollbars for a page, as Windows draws classic ones with arrows in the app's frames.
/// Least specific (`:where`), so the page's own rules win; a page that mentions scrollbars at all
/// is left as it is, since a set `scrollbar-color` turns off its `::-webkit-scrollbar` styles.
fn thin_scrollbars(page: Vec<u8>) -> Vec<u8> {
    const STYLE: &[u8] = b"<style>:where(:root){scrollbar-width:thin;\
        scrollbar-color:rgba(128,128,128,.5) transparent}</style>";
    if page
        .windows(b"scrollbar".len())
        .any(|w| w.eq_ignore_ascii_case(b"scrollbar"))
    {
        return page;
    }
    // After a BOM and a doctype, which must come first.
    let mut at = if page.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    let rest = &page[at..];
    let start = rest.len() - rest.trim_ascii_start().len();
    let doctype = rest[start..]
        .get(..9)
        .is_some_and(|d| d.eq_ignore_ascii_case(b"<!doctype"));
    if doctype && let Some(end) = rest[start..].iter().position(|&b| b == b'>') {
        at += start + end + 1;
    }
    let mut out = Vec::with_capacity(page.len() + STYLE.len());
    out.extend_from_slice(&page[..at]);
    out.extend_from_slice(STYLE);
    out.extend_from_slice(&page[at..]);
    out
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
        thin_scrollbars(html.as_bytes().to_vec()),
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

const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// The upload route's body limit: above [`MAX_UPLOAD_BYTES`], so a file just over it gets the
/// handler's JSON error.
pub(crate) fn upload_limit() -> axum::extract::DefaultBodyLimit {
    axum::extract::DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1024 * 1024)
}

/// Query of an upload.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct UploadQuery {
    /// The file's name. Only letters, digits, `.`, `-` and `_` are kept (others become `_`);
    /// a name already in `uploads/` gets `-2`, `-3`, ... before its extension.
    name: String,
}

/// A file the operator put into an agent's workspace.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Upload {
    /// Where the file is, relative to the agent's workspace, e.g. `uploads/photo.png`.
    path: String,
    /// Bytes written.
    size: u64,
}

/// Puts a file from the operator into the agent's workspace folder `uploads/`, waking its
/// computer if it sleeps. Body: the raw file, at most 25 MiB.
#[utoipa::path(post, path = "/v1/agents/{id}/uploads", tag = "chat",
    params(("id" = String, Path, description = "Agent id (UUID)"), UploadQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream",
        description = "The file, at most 25 MiB"),
    responses((status = 201, body = Upload),
        (status = 400, description = "Larger than 25 MiB", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 409, description = "No sandbox", body = ErrorBody),
        (status = 413, description = "Far larger than 25 MiB (plain text)"),
        (status = 502, description = "Sandbox backend error", body = ErrorBody)))]
pub(crate) async fn upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<Upload>), ApiError> {
    if body.len() > MAX_UPLOAD_BYTES {
        return Err(ApiError::BadRequest(
            "the file is larger than 25 MiB".into(),
        ));
    }
    let id = parse_id(&id)?;
    lifecycle::ensure_awake(&state, id).await?;
    state.hub.touch(id);
    let agent = state.store.get_agent(id).await?;
    let handle = lifecycle::sandbox_handle(&agent)?;
    let home = state.backend.home();
    // One exec makes the folder and lists it, so an upload never replaces an earlier one.
    let mut list = ExecRequest::new([
        "sh",
        "-c",
        "mkdir -p workspace/uploads && ls -A workspace/uploads",
    ]);
    // A relative path in the home: the local backend maps only the workdir to its sandbox dir.
    list.workdir = Some(home.clone());
    let listing = state.backend.exec(&handle, &list).await?;
    if listing.exit_code != 0 {
        let why = format!("cannot create uploads/: {}", listing.stderr.trim());
        return Err(SandboxError::Other(why).into());
    }
    let taken: HashSet<&str> = listing.stdout.lines().collect();
    let name = free_name(&safe_name(&q.name), &taken);
    let path = SandboxPath::parse(&format!(
        "{}/workspace/uploads/{name}",
        home.trim_end_matches('/')
    ))?;
    state.backend.write_file(&handle, &path, &body).await?;
    Ok((
        StatusCode::CREATED,
        Json(Upload {
            path: format!("uploads/{name}"),
            size: body.len() as u64,
        }),
    ))
}

/// The last component of `raw` with only `[A-Za-z0-9._-]` (others become `_`), no leading
/// dot, at most 80 characters before a short extension; `file` if nothing is left.
fn safe_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.');
    let (stem, ext) = match clean.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 10 => {
            (stem, Some(ext))
        }
        _ => (clean, None),
    };
    let stem = if stem.is_empty() { "file" } else { stem };
    let stem = &stem[..stem.len().min(80)];
    ext.map_or_else(|| stem.to_owned(), |ext| format!("{stem}.{ext}"))
}

/// `name`, or `name-2`, `name-3`, ... (before the extension) if `taken` has it.
fn free_name(name: &str, taken: &HashSet<&str>) -> String {
    let (stem, ext) = name
        .rsplit_once('.')
        .map_or((name, String::new()), |(s, e)| (s, format!(".{e}")));
    // One more candidate than `taken` holds, so one is free.
    std::iter::once(name.to_owned())
        .chain((2..=taken.len() + 1).map(|n| format!("{stem}-{n}{ext}")))
        .find(|candidate| !taken.contains(candidate.as_str()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_get_thin_scrollbars_after_their_doctype() {
        let thin = |page: &str| String::from_utf8(thin_scrollbars(page.into())).unwrap();
        let style = String::from_utf8(thin_scrollbars(Vec::new())).unwrap();
        assert!(style.starts_with("<style>") && style.contains("scrollbar-width:thin"));
        assert_eq!(
            thin("\n<!DOCTYPE html><p>hi"),
            format!("\n<!DOCTYPE html>{style}<p>hi")
        );
        assert_eq!(thin("<p>hi"), format!("{style}<p>hi"));
        assert_eq!(
            thin("\u{feff}<!doctype html>x"),
            format!("\u{feff}<!doctype html>{style}x")
        );
        let own = "<style>::-webkit-scrollbar{width:4px}</style>";
        assert_eq!(thin(own), own, "a page's own scrollbars stay");
    }

    #[test]
    fn upload_names_are_safe_and_free() {
        for (raw, safe) in [
            ("photo.png", "photo.png"),
            ("../../etc/passwd", "passwd"),
            (
                "C:\\Users\\me\\Report 2026 (final).pdf",
                "Report_2026__final_.pdf",
            ),
            ("..hidden", "hidden"),
            ("Übersicht.md", "_bersicht.md"),
            ("", "file"),
            ("...", "file"),
            (".png", "png"),
            ("$(rm -rf ~).sh", "__rm_-rf___.sh"),
        ] {
            assert_eq!(safe_name(raw), safe, "{raw:?}");
        }
        assert_eq!(safe_name(&"a".repeat(300)).len(), 80);
        assert_eq!(safe_name(&format!("{}.tar", "b".repeat(300))).len(), 84);
        let taken = HashSet::from(["photo.png", "photo-2.png", "notes"]);
        assert_eq!(free_name("photo.png", &taken), "photo-3.png");
        assert_eq!(free_name("notes", &taken), "notes-2");
        assert_eq!(free_name("new.txt", &taken), "new.txt");
    }
}
