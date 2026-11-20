//! OAuth sign-in for connectors, as the MCP authorization spec describes it:
//! metadata discovery, client registration (dynamic, or the operator's own
//! OAuth app), PKCE, refresh, and RFC 7009 revocation on sign-out. Built on
//! rmcp's OAuth client and the `oauth2` crate. The daemon is the OAuth client;
//! tokens live in the secret store, never in a sandbox.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ferrobot_core::Connector;
use oauth2::TokenResponse as _;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationMetadata, AuthorizationRequest,
    AuthorizationSession, CredentialRefreshGuard, CredentialStore, OAuthClientConfig,
    StoredCredentials,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::secrets::SecretStore;

/// How long a started sign-in waits for the browser to come back.
const PENDING_TTL: Duration = Duration::from_secs(600);
/// Revocation is best effort; signing out never waits long for it.
const REVOKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a connector's OAuth tokens (and dynamic registration) live.
pub fn secret_key(connector: Uuid) -> String {
    format!("connector-oauth:{connector}")
}

/// Where a connector's own OAuth app (client id, secret, scopes) lives.
pub fn client_key(connector: Uuid) -> String {
    format!("connector-client:{connector}")
}

/// The operator's own OAuth app for a connector, for servers without dynamic
/// client registration (Google, GitHub, Slack, ...), and scopes to ask for.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// The connector's own OAuth app, if the operator set one.
pub async fn client_config(state: &AppState, id: Uuid) -> Result<ClientConfig, String> {
    match state
        .settings
        .secrets
        .get(&client_key(id))
        .await
        .map_err(|e| e.to_string())?
    {
        Some(json) => serde_json::from_str(&json).map_err(|e| e.to_string()),
        None => Ok(ClientConfig::default()),
    }
}

/// Stores the connector's own OAuth app (or removes it when empty).
pub async fn set_client_config(
    state: &AppState,
    id: Uuid,
    config: &ClientConfig,
) -> Result<(), crate::error::ApiError> {
    let secrets = &state.settings.secrets;
    if *config == ClientConfig::default() {
        return secrets.delete(&client_key(id)).await;
    }
    let json = serde_json::to_string(config)
        .map_err(|e| crate::error::ApiError::Internal(e.to_string()))?;
    secrets.set(&client_key(id), &json).await
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

    /// The server revoked the tokens: keep the client, mark the sign-in expired.
    async fn on_refresh_token_rejected(
        &self,
        rejected: &StoredCredentials,
    ) -> Result<(), AuthError> {
        let tokens = |c: &StoredCredentials| serde_json::to_value(&c.token_response).ok();
        let Some(current) = self.load().await? else {
            return Ok(());
        };
        if tokens(&current) != tokens(rejected) {
            return Ok(());
        }
        let kept = StoredCredentials::new(current.client_id.clone(), None, Vec::new(), None)
            .with_issuer(current.issuer.clone());
        let mut json = serde_json::to_value(kept).map_err(store_err)?;
        json["expired"] = true.into();
        self.secrets
            .set(&self.key, &json.to_string())
            .await
            .map_err(store_err)
    }
}

/// Where a connector stands with OAuth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignIn {
    SignedOut,
    SignedIn,
    /// The server rejected the refresh token; sign in again.
    Expired,
}

/// Where the connector stands with OAuth.
pub async fn sign_in(state: &AppState, id: Uuid) -> SignIn {
    let Ok(Some(json)) = state.settings.secrets.get(&secret_key(id)).await else {
        return SignIn::SignedOut;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
        return SignIn::SignedOut;
    };
    if value["expired"] == true {
        return SignIn::Expired;
    }
    match serde_json::from_value::<StoredCredentials>(value) {
        Ok(c) if c.token_response.is_some() => SignIn::SignedIn,
        _ => SignIn::SignedOut,
    }
}

/// Whether the connector holds OAuth tokens.
pub async fn signed_in(state: &AppState, id: Uuid) -> bool {
    sign_in(state, id).await == SignIn::SignedIn
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
async fn discover(manager: &mut AuthorizationManager) -> Result<AuthorizationMetadata, AuthError> {
    let resolution = manager.resolve_metadata().await?;
    if !resolution.source.is_discovered() {
        return Err(AuthError::InternalError(
            "the server publishes no OAuth metadata".into(),
        ));
    }
    manager.set_metadata(resolution.metadata.clone());
    Ok(resolution.metadata)
}

/// Where the browser comes back after signing in (register this with the
/// provider when using an own OAuth app).
pub fn redirect_uri(state: &AppState) -> String {
    format!("{}/v1/oauth/callback", state.settings.ui_url)
}

/// Google issues a refresh token only for offline access, and only with
/// fresh consent; without it every sign-in would last an hour.
fn with_provider_params(mut url: url::Url) -> url::Url {
    if url.host_str() == Some("accounts.google.com") {
        url.query_pairs_mut()
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
    }
    url
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
    let config = client_config(state, c.id).await.map_err(|e| {
        tracing::error!(connector = %c.name, error = %e, "cannot read the OAuth app");
        "the secret store is unavailable".to_owned()
    })?;
    let mut manager = manager(state, c).await.map_err(fail)?;
    discover(&mut manager).await.map_err(fail)?;
    let mut request = AuthorizationRequest::new(redirect_uri(state)).with_client_name("ferrobot");
    if let Some(id) = &config.client_id {
        request = request.with_preregistered_client(id.clone());
        if let Some(secret) = &config.client_secret {
            request = request.with_client_secret(secret.clone());
        }
    }
    if !config.scopes.is_empty() {
        request = request.with_scopes(config.scopes.clone());
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, e)| fail(e))?;
    let parsed = url::Url::parse(&session.auth_url)
        .map_err(|_| "the authorization URL is invalid".to_owned())?;
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
    let url = with_provider_params(parsed).to_string();
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

/// An HTTP client that sends the connector's OAuth token and refreshes it,
/// or `None` when the connector is not signed in.
pub async fn client(
    state: &AppState,
    c: &Connector,
) -> Result<Option<AuthClient<reqwest::Client>>, AuthError> {
    if !signed_in(state, c.id).await {
        return Ok(None);
    }
    let config = client_config(state, c.id).await.map_err(store_err)?;
    let mut manager = manager(state, c).await?;
    discover(&mut manager).await?;
    if !manager.initialize_from_store().await? {
        return Ok(None);
    }
    // rmcp stores no client secret; a confidential client needs it to refresh.
    if let (Some(id), Some(secret)) = (&config.client_id, &config.client_secret) {
        let mut client = OAuthClientConfig::new(id.clone(), redirect_uri(state))
            .with_client_secret(secret.clone());
        if !config.scopes.is_empty() {
            client = client.with_scopes(config.scopes.clone());
        }
        manager.configure_client(client)?;
    }
    Ok(Some(AuthClient::new(reqwest::Client::new(), manager)))
}

/// Signs the connector out: asks the server to revoke its tokens (RFC 7009)
/// when it offers that, then forgets them here either way.
pub async fn sign_out(state: &AppState, c: &Connector) -> Result<(), crate::error::ApiError> {
    if let Err(e) = revoke(state, c).await {
        tracing::warn!(connector = %c.name, error = %e, "could not revoke OAuth tokens");
    }
    state.settings.secrets.delete(&secret_key(c.id)).await?;
    state.oauth.forget(c.id).await;
    state.connectors.evict_connector(c.id).await;
    Ok(())
}

/// The token goes only to the token endpoint's own origin, over https
/// (`oauth2` refuses anything else as well).
fn revocation_allowed(revocation: &url::Url, token_endpoint: &str) -> bool {
    let Ok(token) = url::Url::parse(token_endpoint) else {
        return false;
    };
    revocation.scheme() == "https" && revocation.origin() == token.origin()
}

async fn revoke(state: &AppState, c: &Connector) -> Result<(), String> {
    let Some(json) = state
        .settings
        .secrets
        .get(&secret_key(c.id))
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let stored: StoredCredentials = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let Some(tokens) = stored.token_response else {
        return Ok(());
    };
    let mut manager = manager(state, c).await.map_err(|e| e.to_string())?;
    let metadata = discover(&mut manager).await.map_err(|e| e.to_string())?;
    let Some(endpoint) = metadata
        .additional_fields
        .get("revocation_endpoint")
        .and_then(|v| v.as_str())
    else {
        return Ok(());
    };
    let revocation = url::Url::parse(endpoint).map_err(|e| e.to_string())?;
    if !revocation_allowed(&revocation, &metadata.token_endpoint) {
        return Err(format!("refusing revocation endpoint {endpoint}"));
    }
    let config = client_config(state, c.id).await?;
    let client_id = config.client_id.unwrap_or(stored.client_id);
    let mut client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(client_id))
        .set_revocation_url(oauth2::RevocationUrl::from_url(revocation));
    if let Some(secret) = config.client_secret {
        client = client.set_client_secret(oauth2::ClientSecret::new(secret));
    }
    // Revoking the refresh token ends the whole grant; else the access token.
    let token = match tokens.refresh_token() {
        Some(refresh) => oauth2::StandardRevocableToken::RefreshToken(refresh.clone()),
        None => oauth2::StandardRevocableToken::AccessToken(tokens.access_token().clone()),
    };
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REVOKE_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let send = move |request: oauth2::HttpRequest| {
        let http = http.clone();
        async move {
            let (parts, body) = request.into_parts();
            let response = http
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .map_err(std::io::Error::other)?;
            let status = response.status();
            let bytes = response.bytes().await.map_err(std::io::Error::other)?;
            oauth2::http::Response::builder()
                .status(status)
                .body(bytes.to_vec())
                .map_err(std::io::Error::other)
        }
    };
    client
        .revoke_token(token)
        .map_err(|e| e.to_string())?
        .request_async(&send)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refreshes_take_turns_and_a_revoked_token_expires_the_sign_in() {
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
        let SecretStore::Memory(map) = &a.secrets else {
            unreachable!()
        };
        let json = map.lock().unwrap()["k"].clone();
        assert!(json.contains("\"expired\":true"), "{json}");
        let kept = a.load().await.unwrap().unwrap();
        assert_eq!(kept.client_id, "client", "the client registration is kept");
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

    #[test]
    fn google_sign_ins_ask_for_offline_access() {
        let google =
            url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth?state=s").unwrap();
        let q: HashMap<_, _> = with_provider_params(google)
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(
            (q["access_type"].as_str(), q["prompt"].as_str()),
            ("offline", "consent")
        );
        let other = url::Url::parse("https://auth.example.com/authorize?state=s").unwrap();
        assert_eq!(with_provider_params(other.clone()), other);
    }

    #[test]
    fn tokens_are_revoked_only_at_the_token_endpoints_origin() {
        let url = |s: &str| url::Url::parse(s).unwrap();
        let token = "https://oauth2.googleapis.com/token";
        assert!(revocation_allowed(
            &url("https://oauth2.googleapis.com/revoke"),
            token
        ));
        assert!(!revocation_allowed(
            &url("https://evil.example/revoke"),
            token
        ));
        assert!(!revocation_allowed(
            &url("http://oauth2.googleapis.com/revoke"),
            "http://oauth2.googleapis.com/token"
        ));
        assert!(!revocation_allowed(
            &url("http://127.0.0.1:9/revoke"),
            "http://127.0.0.1:9/token"
        ));
    }
}
