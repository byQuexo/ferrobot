//! OAuth sign-in for connectors, as the MCP authorization spec describes it:
//! metadata discovery, dynamic client registration, PKCE, refresh. The daemon
//! is the OAuth client; tokens live in the secret store, never in a sandbox.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ferrobot_core::Connector;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession,
    CredentialStore, StoredCredentials,
};
use uuid::Uuid;

use crate::AppState;
use crate::secrets::SecretStore;

/// How long a started sign-in waits for the browser to come back.
const PENDING_TTL: Duration = Duration::from_secs(600);

/// Where a connector's OAuth client and tokens live in the secret store.
pub fn secret_key(connector: Uuid) -> String {
    format!("connector-oauth:{connector}")
}

/// rmcp's credential store, backed by the daemon's secret store.
struct Keychain {
    secrets: SecretStore,
    key: String,
}

fn store_err(e: impl std::fmt::Display) -> AuthError {
    AuthError::InternalError(format!("secret store: {e}"))
}

#[async_trait]
impl CredentialStore for Keychain {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        match self.secrets.get(&self.key).await.map_err(store_err)? {
            Some(json) => serde_json::from_str(&json).map(Some).map_err(store_err),
            None => Ok(None),
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&credentials).map_err(store_err)?;
        self.secrets.set(&self.key, &json).await.map_err(store_err)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.secrets.delete(&self.key).await.map_err(store_err)
    }
}

/// Sign-ins waiting for the browser, by their OAuth `state`.
#[derive(Default)]
pub struct Pending(tokio::sync::Mutex<HashMap<String, (Uuid, AuthorizationSession, Instant)>>);

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pending")
    }
}

/// Loopback and private hosts the operator chose themselves. Anything else
/// keeps rmcp's guard against metadata that points at internal addresses.
fn is_private(url: &str) -> bool {
    match url::Url::parse(url)
        .ok()
        .and_then(|u| u.host().map(|h| h.to_owned()))
    {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

async fn manager(state: &AppState, c: &Connector) -> Result<AuthorizationManager, AuthError> {
    let mut manager = if is_private(&c.url) {
        AuthorizationManager::new_allowing_private_addresses(c.url.as_str()).await?
    } else {
        AuthorizationManager::new(c.url.as_str()).await?
    };
    manager.set_credential_store(Keychain {
        secrets: state.settings.secrets.clone(),
        key: secret_key(c.id),
    });
    Ok(manager)
}

/// Starts a sign-in and returns the URL to open in the operator's browser.
pub async fn start(state: &AppState, c: &Connector) -> Result<String, String> {
    let fail = |e: AuthError| {
        tracing::warn!(connector = %c.name, error = %e, "cannot start OAuth sign-in");
        format!(
            "{} does not offer an OAuth sign-in ferrobot can use",
            c.name
        )
    };
    let mut manager = manager(state, c).await.map_err(fail)?;
    let resolution = manager.resolve_metadata().await.map_err(fail)?;
    manager.set_metadata(resolution.metadata);
    let redirect = format!("{}/v1/oauth/callback", state.settings.ui_url);
    let request = AuthorizationRequest::new(redirect).with_client_name("ferrobot");
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, e)| fail(e))?;
    let url = session.auth_url.clone();
    let csrf = url::Url::parse(&url)
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.into_owned())
        })
        .ok_or_else(|| "the authorization URL has no state".to_owned())?;
    let mut pending = state.oauth.0.lock().await;
    pending.retain(|_, (_, _, at)| at.elapsed() < PENDING_TTL);
    pending.insert(csrf, (c.id, session, Instant::now()));
    Ok(url)
}

/// Finishes the sign-in the browser came back for. Each `state` works once.
pub async fn finish(
    state: &AppState,
    csrf: &str,
    code: Option<&str>,
    issuer: Option<&str>,
) -> Result<Uuid, &'static str> {
    let entry = state.oauth.0.lock().await.remove(csrf);
    let (id, session, at) = entry.ok_or("This sign-in is unknown or was already used.")?;
    if at.elapsed() > PENDING_TTL {
        return Err("This sign-in expired. Start it again in ferrobot.");
    }
    let code = code.ok_or("The sign-in was cancelled.")?;
    if let Err(e) = session
        .handle_callback_with_issuer(code, csrf, issuer)
        .await
    {
        tracing::warn!(connector = %id, error = %e, "OAuth sign-in failed");
        return Err("The sign-in failed. Try again in ferrobot.");
    }
    state.connectors.evict_connector(id).await;
    Ok(id)
}

/// Whether the connector holds OAuth tokens.
pub async fn signed_in(state: &AppState, id: Uuid) -> bool {
    let Ok(Some(json)) = state.settings.secrets.get(&secret_key(id)).await else {
        return false;
    };
    serde_json::from_str::<StoredCredentials>(&json).is_ok_and(|c| c.token_response.is_some())
}

/// An HTTP client that sends the connector's OAuth token and refreshes it,
/// or `None` when the connector is not signed in.
pub async fn client(
    state: &AppState,
    c: &Connector,
) -> Result<Option<AuthClient<reqwest::Client>>, AuthError> {
    if !signed_in(state, c.id).await {
        return Ok(None);
    }
    let mut manager = manager(state, c).await?;
    if !manager.initialize_from_store().await? {
        return Ok(None);
    }
    Ok(Some(AuthClient::new(reqwest::Client::new(), manager)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_operator_chosen_local_hosts_skip_the_ssrf_guard() {
        assert!(is_private("http://localhost:3000/mcp"));
        assert!(is_private("http://127.0.0.1:3000/mcp"));
        assert!(is_private("http://192.168.1.20/mcp"));
        assert!(is_private("http://[::1]:3000/mcp"));
        assert!(!is_private("https://mcp.linear.app/mcp"));
        assert!(!is_private("https://8.8.8.8/mcp"));
        assert!(!is_private("not a url"));
    }
}
