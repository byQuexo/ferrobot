//! Logins for CLIs in the sandboxes (GitHub first). The token stays in the daemon's secret store.
//! An agent assigned a login gets a stand-in token and `HTTPS_PROXY` pointing at the daemon,
//! which swaps in the real token for that service's hosts only (`egress.rs`).

use std::collections::BTreeMap;

use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;

/// What a CLI in the sandbox holds instead of the real token.
pub const STAND_IN: &str = "ferrobot-stand-in-token";
const SETTING: &str = "logins";
/// Where the CA and the CA bundle that trusts it live in the sandbox, under its home.
pub const CA_FILE: &str = ".ferrobot/login-ca.pem";
pub const BUNDLE_FILE: &str = ".ferrobot/ca-bundle.pem";
/// The system bundle the sandbox's own trust starts from.
pub const SYSTEM_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoginKind {
    /// `gh` and `git` over HTTPS.
    Github,
}

impl LoginKind {
    pub const ALL: [Self; 1] = [Self::Github];

    /// Hosts whose requests get the real token.
    pub fn hosts(self) -> &'static [&'static str] {
        match self {
            Self::Github => &["github.com", "api.github.com", "uploads.github.com"],
        }
    }

    /// The domain the local CA may issue certificates under (its name constraint).
    pub fn domain(self) -> &'static str {
        match self {
            Self::Github => "github.com",
        }
    }

    pub fn secret_key(self) -> &'static str {
        match self {
            Self::Github => "login:github",
        }
    }

    /// Variables the CLIs read their token from; they get the stand-in.
    fn token_vars(self) -> &'static [&'static str] {
        match self {
            Self::Github => &["GH_TOKEN", "GITHUB_TOKEN"],
        }
    }
}

/// The agents each login is lent to.
pub type Assignments = BTreeMap<LoginKind, Vec<AgentId>>;

pub async fn assignments(state: &AppState) -> Assignments {
    match state.store.get_setting(SETTING).await {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        Ok(None) => Assignments::new(),
        Err(e) => {
            tracing::error!(error = %e, "cannot read the logins");
            Assignments::new()
        }
    }
}

pub async fn save(state: &AppState, assigned: &Assignments) -> Result<(), crate::error::ApiError> {
    let raw = serde_json::to_string(assigned)
        .map_err(|e| crate::error::ApiError::Internal(e.to_string()))?;
    state.store.put_setting(SETTING, raw).await?;
    Ok(())
}

pub async fn token(state: &AppState, kind: LoginKind) -> Option<String> {
    state
        .settings
        .secrets
        .get(kind.secret_key())
        .await
        .ok()
        .flatten()
}

/// The real token for `agent`'s request to `host`, if a login lent to it covers that host.
pub async fn for_host(state: &AppState, agent: AgentId, host: &str) -> Option<String> {
    let assigned = assignments(state).await;
    for kind in LoginKind::ALL {
        if kind.hosts().contains(&host) && assigned.get(&kind).is_some_and(|a| a.contains(&agent)) {
            return token(state, kind).await;
        }
    }
    None
}

/// The logins lent to `agent`.
pub async fn of_agent(state: &AppState, agent: AgentId) -> Vec<LoginKind> {
    assignments(state)
        .await
        .into_iter()
        .filter(|(_, agents)| agents.contains(&agent))
        .map(|(kind, _)| kind)
        .collect()
}

/// The sandbox environment of an agent with `kinds`: the proxy (the agent token as its password),
/// stand-in tokens, git's credentials and the CA bundle. Empty without a login.
pub fn env(
    kinds: &[LoginKind],
    daemon_url: &str,
    agent_token: &str,
    home: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let Ok(mut proxy) = url::Url::parse(daemon_url) else {
        return env;
    };
    if kinds.is_empty()
        || proxy.set_username("ferrobot").is_err()
        || proxy.set_password(Some(agent_token)).is_err()
    {
        return env;
    }
    let host = proxy.host_str().unwrap_or_default().to_owned();
    let proxy = proxy.as_str().trim_end_matches('/').to_owned();
    let no_proxy = format!("{host},localhost,127.0.0.1");
    let bundle = format!("{home}/{BUNDLE_FILE}");
    let mut set = |k: &str, v: &str| env.insert(k.to_owned(), v.to_owned());
    for (k, v) in [
        ("HTTPS_PROXY", proxy.as_str()),
        ("https_proxy", proxy.as_str()),
        ("NO_PROXY", no_proxy.as_str()),
        ("no_proxy", no_proxy.as_str()),
        ("SSL_CERT_FILE", bundle.as_str()),
        ("GIT_SSL_CAINFO", bundle.as_str()),
        ("CURL_CA_BUNDLE", bundle.as_str()),
        ("REQUESTS_CA_BUNDLE", bundle.as_str()),
    ] {
        set(k, v);
    }
    set("NODE_EXTRA_CA_CERTS", &format!("{home}/{CA_FILE}"));
    for kind in kinds {
        for var in kind.token_vars() {
            set(var, STAND_IN);
        }
    }
    if kinds.contains(&LoginKind::Github) {
        // git asks this helper for credentials; it answers with the stand-in.
        set("GIT_CONFIG_COUNT", "1");
        set("GIT_CONFIG_KEY_0", "credential.https://github.com.helper");
        set(
            "GIT_CONFIG_VALUE_0",
            &format!("!f() {{ echo username=x-access-token; echo password={STAND_IN}; }}; f"),
        );
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_agents_with_a_login_get_the_proxy_and_stand_ins() {
        let none = env(&[], "http://host.docker.internal:7879", "tok", "/home/node");
        assert!(none.is_empty());
        let env = env(
            &[LoginKind::Github],
            "http://host.docker.internal:7879",
            "tok",
            "/home/node",
        );
        assert_eq!(
            env["HTTPS_PROXY"],
            "http://ferrobot:tok@host.docker.internal:7879"
        );
        assert_eq!(env["NO_PROXY"], "host.docker.internal,localhost,127.0.0.1");
        assert_eq!(env["GH_TOKEN"], STAND_IN);
        assert_eq!(env["SSL_CERT_FILE"], "/home/node/.ferrobot/ca-bundle.pem");
        assert!(env["GIT_CONFIG_VALUE_0"].contains(STAND_IN));
        assert!(!env.values().any(|v| v.contains("ghp_")), "no real token");
    }
}
