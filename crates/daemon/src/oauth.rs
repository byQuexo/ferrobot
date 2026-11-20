//! OAuth sign-in for connectors, as the MCP authorization spec describes it:
//! metadata discovery, dynamic client registration, PKCE, refresh. The daemon
//! is the OAuth client; tokens live in the secret store, never in a sandbox.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ferrobot_core::Connector;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession,
    CredentialRefreshGuard, CredentialStore, StoredCredentials,
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
    refresh: Arc<tokio::sync::Mutex<()>>,
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

    // Servers that rotate refresh tokens revoke them all when one is used twice.
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        let guard = Arc::clone(&self.refresh).lock_owned().await;
        Ok(Some(CredentialRefreshGuard::new(guard)))
    }

    /// The server revoked the tokens: show the connector as signed out.
    async fn on_refresh_token_rejected(
        &self,
        rejected: &StoredCredentials,
    ) -> Result<(), AuthError> {
        let tokens = |c: &StoredCredentials| serde_json::to_value(&c.token_response).ok();
        match self.load().await? {
            Some(current) if tokens(&current) == tokens(rejected) => self.clear().await,
            _ => Ok(()),
        }
    }
}

type Waiting = (Uuid, AuthorizationSession, Instant);

/// Sign-ins waiting for the browser (by their OAuth `state`) and one refresh
/// lock per connector.
#[derive(Default)]
pub struct OAuth {
    pending: tokio::sync::Mutex<HashMap<String, Waiting>>,
    refresh: std::sync::Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for OAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuth")
    }
}

impl OAuth {
    fn refresh_lock(&self, connector: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.refresh.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(connector).or_default())
    }

    /// Cancels the connector's open sign-ins (deleted, signed out, URL changed).
    pub async fn forget(&self, connector: Uuid) {
        self.pending
            .lock()
            .await
            .retain(|_, (id, ..)| *id != connector);
    }
}

/// Private LAN addresses the operator chose themselves. rmcp already allows
/// loopback for loopback connectors and keeps its guard for everything else,
/// so metadata cannot point the daemon into the LAN.
fn is_private(url: &str) -> bool {
    match url::Url::parse(url)
        .ok()
        .and_then(|u| u.host().map(|h| h.to_owned()))
    {
        Some(url::Host::Ipv4(ip)) => ip.is_private(),
        _ => false,
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
        refresh: state.oauth.refresh_lock(c.id),
    });
    Ok(manager)
}

/// Published OAuth metadata only. rmcp's fallback guesses endpoints on the
/// MCP server itself, with no issuer to check stored tokens against.
async fn discover(manager: &mut AuthorizationManager) -> Result<(), AuthError> {
    let resolution = manager.resolve_metadata().await?;
    if !resolution.source.is_discovered() {
        return Err(AuthError::InternalError(
            "the server publishes no OAuth metadata".into(),
        ));
    }
    manager.set_metadata(resolution.metadata);
    Ok(())
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
    discover(&mut manager).await.map_err(fail)?;
    let redirect = format!("{}/v1/oauth/callback", state.settings.ui_url);
    let request = AuthorizationRequest::new(redirect).with_client_name("ferrobot");
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, e)| fail(e))?;
    let url = session.auth_url.clone();
    let parsed =
        url::Url::parse(&url).map_err(|_| "the authorization URL is invalid".to_owned())?;
    // Opened in the operator's browser: no javascript:, data: or file: links.
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err(format!(
            "{} sent an authorization URL that is not http(s)",
            c.name
        ));
    }
    // A `state` baked into the server's endpoint must not take over the sign-in.
    let states: Vec<String> = parsed
        .query_pairs()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .collect();
    let [csrf] = <[String; 1]>::try_from(states)
        .map_err(|_| "the authorization URL needs exactly one state".to_owned())?;
    let mut pending = state.oauth.pending.lock().await;
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
    let entry = state.oauth.pending.lock().await.remove(csrf);
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
        // A forged callback must not cancel the real one still on its way.
        state
            .oauth
            .pending
            .lock()
            .await
            .insert(csrf.to_owned(), (id, session, at));
        return Err("The sign-in failed. Try again in ferrobot.");
    }
    if state.store.get_connector(id).await.is_err() {
        let _ = state.settings.secrets.delete(&secret_key(id)).await;
        return Err("This connector was deleted in the meantime.");
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
    discover(&mut manager).await?;
    if !manager.initialize_from_store().await? {
        return Ok(None);
    }
    Ok(Some(AuthClient::new(reqwest::Client::new(), manager)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refreshes_take_turns_and_a_revoked_token_signs_out() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let store = |lock: &Arc<tokio::sync::Mutex<()>>| Keychain {
            secrets: SecretStore::default(),
            key: "k".into(),
            refresh: Arc::clone(lock),
        };
        let (a, b) = (store(&lock), store(&lock));
        let held = a.acquire_refresh_guard().await.unwrap();
        let waiting = tokio::time::timeout(Duration::from_millis(50), b.acquire_refresh_guard());
        assert!(
            waiting.await.is_err(),
            "a second refresh waits for the first"
        );
        drop(held);
        assert!(b.acquire_refresh_guard().await.unwrap().is_some());

        let creds = StoredCredentials::new("client".into(), None, vec![], None);
        a.save(creds.clone()).await.unwrap();
        a.on_refresh_token_rejected(&creds).await.unwrap();
        assert!(a.load().await.unwrap().is_none());
    }

    #[test]
    fn only_operator_chosen_lan_hosts_skip_the_ssrf_guard() {
        assert!(is_private("http://192.168.1.20/mcp"));
        assert!(is_private("http://10.0.0.5:8080/mcp"));
        assert!(!is_private("http://localhost:3000/mcp"));
        assert!(!is_private("http://127.0.0.1:3000/mcp"));
        assert!(!is_private("http://[::1]:3000/mcp"));
        assert!(!is_private("https://mcp.linear.app/mcp"));
        assert!(!is_private("https://8.8.8.8/mcp"));
        assert!(!is_private("not a url"));
    }
}
