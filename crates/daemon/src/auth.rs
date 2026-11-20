//! Operator bearer token for the UI API. `GET` may use `?access_token=` instead, since browsers
//! cannot set headers on WebSocket upgrades or `<img>` requests.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::ApiError;

/// File name of the generated token inside the data directory.
pub const TOKEN_FILE: &str = "api-token";
/// Query parameter accepted on `GET` requests (WebSocket, images).
pub const QUERY_PARAM: &str = "access_token";

/// Checks a user-supplied token: long enough and safe in a URL query.
pub fn validate(token: &str) -> Result<(), String> {
    if token.len() < 16 {
        return Err("API token must be at least 16 characters".into());
    }
    if !token
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
    {
        return Err("API token may only contain A-Z a-z 0-9 - . _ ~".into());
    }
    Ok(())
}

/// Path of the token file for a data directory.
pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE)
}

/// Reads the token file, creating it with a fresh random token (mode 0600)
/// if it does not exist. Tightens the permissions of an existing file.
pub fn load_or_create(data_dir: &Path) -> io::Result<String> {
    let path = token_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            restrict(&path)?;
            let token = raw.trim().to_owned();
            validate(&token).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?;
            Ok(token)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&path)?;
            writeln!(file, "{token}")?;
            Ok(token)
        }
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn restrict(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        tracing::warn!(path = %path.display(), "API token file was readable by others; setting mode 0600");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Constant-time comparison (of SHA-256 digests, so lengths never leak).
pub fn tokens_equal(given: &str, expected: &str) -> bool {
    let a = Sha256::digest(given.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn presented(req: &Request) -> Option<&str> {
    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if header.is_some() || req.method() != Method::GET {
        return header;
    }
    req.uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix(QUERY_PARAM)?.strip_prefix('='))
}

/// Middleware: rejects `/v1` requests without the operator token.
pub async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.settings.api_token.as_deref() else {
        return next.run(req).await;
    };
    // The OAuth callback is a browser redirect without the token; its single-use `state` authorizes it.
    // Shared pages load in a frame, which cannot send it; their random view key authorizes them.
    let open = ["/v1/health", "/v1/oauth/callback"];
    let path = req.uri().path();
    if req.method() == Method::OPTIONS || open.contains(&path) || path.starts_with("/artifacts/") {
        return next.run(req).await;
    }
    match presented(&req) {
        Some(token) if tokens_equal(token.trim(), expected) => next.run(req).await,
        _ => {
            let mut res = ApiError::Unauthorized.into_response();
            res.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Bearer"),
            );
            res
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens() {
        assert!(tokens_equal("abcdefghijklmnop", "abcdefghijklmnop"));
        assert!(!tokens_equal("abcdefghijklmnoq", "abcdefghijklmnop"));
        assert!(!tokens_equal("", "abcdefghijklmnop"));
    }

    #[test]
    fn validates_tokens() {
        assert!(validate("0123456789abcdef").is_ok());
        assert!(validate("short").is_err());
        assert!(validate("0123456789abcdef&x=1").is_err());
    }

    #[test]
    fn creates_token_file_once_with_private_mode() {
        let dir = std::env::temp_dir().join(format!("ferrobot-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = load_or_create(&dir).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(load_or_create(&dir).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = token_path(&dir);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            load_or_create(&dir).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
