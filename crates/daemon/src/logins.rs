//! Logins for CLIs in the sandboxes, set up by the operator (GitHub as a preset in the app).
//! The token stays in the daemon's secret store. An agent lent a login gets a stand-in token and
//! `HTTPS_PROXY` pointing at the daemon, which swaps in the real token for that login's hosts
//! only (`egress.rs`).

use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;

use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;

const SETTING: &str = "logins";
/// Where the CAs and the CA bundle that trusts them live in the sandbox, under its home.
pub const CA_FILE: &str = ".ferrobot/login-ca.pem";
pub const BUNDLE_FILE: &str = ".ferrobot/ca-bundle.pem";
/// The system bundle the sandbox's own trust starts from.
pub const SYSTEM_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
const MAX_LOGINS: usize = 32;
const MAX_ITEMS: usize = 10;
/// Variables the sandbox needs for itself; a login cannot take them over.
const RESERVED: [&str; 17] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "GIT_SSL_CAINFO",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "TERM",
];

/// A login for CLIs: which hosts get the real token and where the CLIs look for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Login {
    /// 1-32 chars of `a-z0-9-`, starting with a letter, e.g. `example`.
    pub id: String,
    /// API hosts whose requests get the real token, exact public names like `api.example.com`.
    pub hosts: Vec<String>,
    /// Environment variables the CLIs read their token from; they get a stand-in.
    pub env: Vec<String>,
    /// Program names that use this login, e.g. `example`; see `confirm`.
    #[serde(default)]
    pub programs: Vec<String>,
    /// Ask before every command that runs one of `programs` or names one of `env` or `hosts`,
    /// unless Auto mode is on.
    #[serde(default)]
    pub confirm: bool,
    /// The agents whose CLIs use this login.
    #[serde(default)]
    #[schema(value_type = Vec<String>)]
    pub agents: Vec<AgentId>,
}

impl Login {
    pub fn secret_key(&self) -> String {
        secret_key(&self.id)
    }

    /// What the sandbox holds instead of the token; unique per login.
    pub fn stand_in(&self) -> String {
        format!("ferrobot-stand-in-{}", self.id)
    }

    /// Returns a human-readable reason when a field is invalid; `others` are the other logins.
    pub fn validate(&self, others: &[Login]) -> Result<(), String> {
        let id = self.id.as_bytes();
        if id.is_empty()
            || id.len() > 32
            || !id[0].is_ascii_lowercase()
            || !id
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        {
            return Err("id must be 1-32 chars of a-z, 0-9 and -, starting with a letter".into());
        }
        if self.hosts.is_empty() || self.hosts.len() > MAX_ITEMS {
            return Err(format!("a login has 1-{MAX_ITEMS} hosts"));
        }
        if let Some(bad) = self.hosts.iter().find(|h| !public_name(h)) {
            return Err(format!(
                "{bad:?} is not a public host name like api.example.com"
            ));
        }
        if self.env.is_empty() || self.env.len() > MAX_ITEMS {
            return Err(format!("a login has 1-{MAX_ITEMS} environment variables"));
        }
        if let Some(bad) = self.env.iter().find(|v| !env_name(v)) {
            return Err(format!(
                "{bad:?} cannot be a login's variable: use a name like EXAMPLE_API_KEY that the sandbox does not use itself"
            ));
        }
        if self.programs.len() > MAX_ITEMS
            || !self.programs.iter().all(|p| {
                !p.is_empty()
                    && p.len() <= 64
                    && p.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+')
                    })
            })
        {
            return Err(format!("up to {MAX_ITEMS} program names like example"));
        }
        for other in others.iter().filter(|o| o.id != self.id) {
            if let Some(h) = self.hosts.iter().find(|h| other.hosts.contains(h)) {
                return Err(format!("{h} already belongs to the {} login", other.id));
            }
            if let Some(v) = self.env.iter().find(|v| other.env.contains(v)) {
                return Err(format!("{v} already belongs to the {} login", other.id));
            }
        }
        Ok(())
    }
}

pub fn secret_key(id: &str) -> String {
    format!("login:{id}")
}

/// A name the daemon may send a token to: lowercase DNS labels, at least two, no IP address and
/// none of the names that point at this computer or the local network.
fn public_name(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    host.len() <= 253
        && labels.len() >= 2
        && host.parse::<IpAddr>().is_err()
        && !labels
            .last()
            .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !matches!(
            labels.last(),
            Some(&("localhost" | "local" | "internal" | "lan" | "home"))
        )
}

fn env_name(var: &str) -> bool {
    let b = var.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_uppercase() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
        && !RESERVED.contains(&var)
        && !var.starts_with("FERROBOT_")
        && !var.starts_with("GIT_CONFIG_")
}

pub async fn all(state: &AppState) -> Vec<Login> {
    match state.store.get_setting(SETTING).await {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::error!(error = %e, "cannot read the logins");
            Vec::new()
        }
    }
}

pub async fn save(state: &AppState, logins: &[Login]) -> Result<(), ApiError> {
    if logins.len() > MAX_LOGINS {
        return Err(ApiError::BadRequest(format!("at most {MAX_LOGINS} logins")));
    }
    let raw = serde_json::to_string(logins).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.store.put_setting(SETTING, raw).await?;
    Ok(())
}

pub async fn token(state: &AppState, id: &str) -> Option<String> {
    state
        .settings
        .secrets
        .get(&secret_key(id))
        .await
        .ok()
        .flatten()
}

/// The login lent to `agent` that covers `host`, and its real token.
pub async fn for_host(state: &AppState, agent: AgentId, host: &str) -> Option<(Login, String)> {
    let login = all(state)
        .await
        .into_iter()
        .find(|l| l.agents.contains(&agent) && l.hosts.iter().any(|h| h == host))?;
    let token = token(state, &login.id).await?;
    Some((login, token))
}

/// The logins lent to `agent`.
pub async fn of_agent(state: &AppState, agent: AgentId) -> Vec<Login> {
    all(state)
        .await
        .into_iter()
        .filter(|l| l.agents.contains(&agent))
        .collect()
}

/// Whether `command` uses one of `agent`'s logins that asks first: it runs one of its programs
/// or names one of its variables or hosts. A heuristic for a card, not a boundary: a script can
/// still reach the login without naming it.
pub async fn asks_first(state: &AppState, agent: AgentId, command: &str) -> bool {
    of_agent(state, agent)
        .await
        .iter()
        .filter(|l| l.confirm)
        .any(|l| uses(l, command))
}

fn uses(login: &Login, command: &str) -> bool {
    let words = command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`'));
    let runs = words.map(|w| w.trim_matches(['"', '\''])).any(|w| {
        login
            .programs
            .iter()
            .any(|p| w.rsplit('/').next() == Some(p))
    });
    runs || login.env.iter().any(|v| command.contains(v.as_str()))
        || login.hosts.iter().any(|h| command.contains(h.as_str()))
}

/// The sandbox environment of an agent with `logins`: the proxy (the agent token as its
/// password), stand-in tokens, git's credentials and the CA bundle. Empty without a login.
pub fn env(
    logins: &[Login],
    daemon_url: &str,
    agent_token: &str,
    home: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let Ok(mut proxy) = url::Url::parse(daemon_url) else {
        return env;
    };
    if logins.is_empty()
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
    let mut seen = HashSet::new();
    for login in logins {
        for var in login.env.iter().filter(|v| seen.insert(v.as_str())) {
            set(var, &login.stand_in());
        }
    }
    if let Some(github) = logins
        .iter()
        .find(|l| l.hosts.iter().any(|h| h == "github.com"))
    {
        // git asks this helper for credentials; it answers with the stand-in.
        set("GIT_CONFIG_COUNT", "1");
        set("GIT_CONFIG_KEY_0", "credential.https://github.com.helper");
        set(
            "GIT_CONFIG_VALUE_0",
            &format!(
                "!f() {{ echo username=x-access-token; echo password={}; }}; f",
                github.stand_in()
            ),
        );
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(id: &str, hosts: &[&str], env: &[&str]) -> Login {
        Login {
            id: id.into(),
            hosts: hosts.iter().map(|h| (*h).into()).collect(),
            env: env.iter().map(|v| (*v).into()).collect(),
            programs: vec![id.into()],
            confirm: true,
            agents: Vec::new(),
        }
    }

    #[test]
    fn only_agents_with_a_login_get_the_proxy_and_stand_ins() {
        let none = env(&[], "http://host.docker.internal:7879", "tok", "/home/node");
        assert!(none.is_empty());
        let github = login(
            "github",
            &["github.com", "api.github.com"],
            &["GH_TOKEN", "GITHUB_TOKEN"],
        );
        let example = login("example", &["api.example.com"], &["EXAMPLE_API_KEY"]);
        let env = env(
            &[github, example],
            "http://host.docker.internal:7879",
            "tok",
            "/home/node",
        );
        assert_eq!(
            env["HTTPS_PROXY"],
            "http://ferrobot:tok@host.docker.internal:7879"
        );
        assert_eq!(env["NO_PROXY"], "host.docker.internal,localhost,127.0.0.1");
        assert_eq!(env["GH_TOKEN"], "ferrobot-stand-in-github");
        assert_eq!(env["EXAMPLE_API_KEY"], "ferrobot-stand-in-example");
        assert_eq!(env["SSL_CERT_FILE"], "/home/node/.ferrobot/ca-bundle.pem");
        assert!(env["GIT_CONFIG_VALUE_0"].contains("ferrobot-stand-in-github"));
    }

    #[test]
    fn a_login_sends_its_token_only_to_public_names_and_leaves_the_sandbox_variables_alone() {
        let ok = login("example", &["api.example.com"], &["EXAMPLE_API_KEY"]);
        assert_eq!(ok.validate(&[]), Ok(()));
        for host in [
            "localhost",
            "127.0.0.1",
            "10.0.0.1",
            "intranet",
            "host.docker.internal",
            "printer.local",
            "*.example.com",
            "API.example.com",
            "[::1]",
            "1.2.3.4.5",
        ] {
            assert!(
                login("x", &[host], &["X_KEY"]).validate(&[]).is_err(),
                "{host}"
            );
        }
        for var in [
            "HTTPS_PROXY",
            "PATH",
            "FERROBOT_AGENT_TOKEN",
            "GIT_CONFIG_COUNT",
            "SSL_CERT_FILE",
            "lower",
        ] {
            assert!(
                login("x", &["api.x.com"], &[var]).validate(&[]).is_err(),
                "{var}"
            );
        }
        let taken = login("other", &["api.example.com"], &["OTHER_KEY"]);
        assert!(ok.validate(&[taken]).is_err(), "one host, one login");
        let same_var = login("other", &["api.other.com"], &["EXAMPLE_API_KEY"]);
        assert!(ok.validate(&[same_var]).is_err(), "one variable, one login");
    }

    #[test]
    fn asking_first_looks_for_the_program_its_variables_and_hosts() {
        let l = login("example", &["api.example.com"], &["EXAMPLE_API_KEY"]);
        for cmd in [
            "example alert list",
            "cd x && /usr/local/bin/example incidents",
            "curl -H \"Authorization: Bearer $EXAMPLE_API_KEY\" https://example.com",
            "curl https://api.example.com/api/alerts",
        ] {
            assert!(uses(&l, cmd), "{cmd}");
        }
        for cmd in ["ls -la", "echo examples", "cat notes/example-cli.md"] {
            assert!(!uses(&l, cmd), "{cmd}");
        }
    }
}
