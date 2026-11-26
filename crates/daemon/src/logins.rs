//! The token side of CLIs with an account (`clis.rs`): an agent lent one gets a stand-in token and
//! `HTTPS_PROXY` pointing at the daemon, which swaps in the real token for that CLI's hosts only
//! (`egress.rs`). The token stays in the daemon's secret store.

use std::collections::{BTreeMap, HashSet};

use ferrobot_core::AgentId;

use crate::AppState;

/// Where the CAs and the CA bundle that trusts them live in the sandbox, under its home.
pub const CA_FILE: &str = ".ferrobot/login-ca.pem";
pub const BUNDLE_FILE: &str = ".ferrobot/ca-bundle.pem";
/// The system bundle the sandbox's own trust starts from.
pub const SYSTEM_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// A CLI's account as the proxy and the sandbox see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub id: String,
    pub hosts: Vec<String>,
    pub env: Vec<String>,
    pub programs: Vec<String>,
    pub confirm: bool,
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
}

pub fn secret_key(id: &str) -> String {
    format!("login:{id}")
}

/// The CLIs with an account.
pub async fn all(state: &AppState) -> Vec<Login> {
    crate::clis::all(state)
        .await
        .into_iter()
        .filter_map(|c| {
            let account = c.account?;
            Some(Login {
                id: c.id,
                hosts: account.hosts,
                env: account.env,
                programs: c.programs,
                confirm: account.confirm,
                agents: c.agents,
            })
        })
        .collect()
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
