//! CLIs in the agents' computers: how one is installed and, if it needs an account, which hosts
//! get its token and where the CLI looks for it. The token itself stays in the daemon.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::AgentId;

const MAX_ITEMS: usize = 10;
const MAX_SCRIPT: usize = 4096;
/// Variables the sandbox needs for itself; an account cannot take them over.
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

/// How a CLI gets into the computers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Install {
    /// A Debian package in the image every computer starts from.
    Debian { package: String },
    /// An npm package, installed into the home folder of each of its agents.
    Npm { package: String },
    /// A program downloaded into the home folder of each of its agents, as its first program.
    Download {
        /// `https://` address of the program itself, not an archive.
        url: String,
        /// SHA-256 of the file in hex; checked before it is installed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
    /// A shell script run in the home folder of each of its agents, as the agent's user, with
    /// `PREFIX=~/.local`; it should put the CLI in `~/.local/bin`.
    Script { script: String },
}

impl Install {
    /// Installed per agent into its home, rather than into the shared image.
    pub fn is_home(&self) -> bool {
        !matches!(self, Self::Debian { .. })
    }
}

/// A CLI's own browser sign-in that shows a one-time code (device login).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct CliSignIn {
    /// The login command, e.g. `["example", "auth", "login", "--web"]`.
    pub login: Vec<String>,
    /// The command that prints the token afterwards, e.g. `["example", "auth", "token"]`.
    pub token: Vec<String>,
}

/// Where a CLI's token goes: only requests to `hosts` get it; the CLI holds a stand-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct Account {
    /// API hosts whose requests get the real token, exact public names like `api.example.com`.
    pub hosts: Vec<String>,
    /// Environment variables the CLI reads its token from, e.g. `EXAMPLE_API_KEY`.
    pub env: Vec<String>,
    /// Ask before every command that runs one of the CLI's programs or names one of `env` or
    /// `hosts`, unless Auto mode is on.
    #[serde(default)]
    pub confirm: bool,
    /// The CLI's own device sign-in; without it the operator pastes a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<CliSignIn>,
}

/// A CLI in the agents' computers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct Cli {
    /// 1-63 chars of `a-z0-9.+-`, starting with a letter or digit, e.g. `example`.
    pub id: String,
    pub install: Install,
    /// The programs it brings, e.g. `["example"]`.
    #[serde(default)]
    pub programs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    /// Agents that get it in their home folder and get its account. A Debian package is in
    /// every computer.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schema(value_type = Vec<String>))]
    pub agents: Vec<AgentId>,
}

impl Cli {
    /// Returns a human-readable reason when a field is invalid; `others` are the other CLIs.
    pub fn validate(&self, others: &[Cli]) -> Result<(), String> {
        let id = self.id.as_bytes();
        if id.is_empty()
            || id.len() > 63
            || !(id[0].is_ascii_lowercase() || id[0].is_ascii_digit())
            || !id
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+.-".contains(b))
        {
            return Err(
                "a name is 1-63 chars of a-z, 0-9, +, . and -, starting with a letter or digit"
                    .into(),
            );
        }
        if self.programs.len() > MAX_ITEMS || !self.programs.iter().all(|p| program_name(p)) {
            return Err(format!("up to {MAX_ITEMS} program names like example"));
        }
        match &self.install {
            Install::Debian { .. } => {}
            Install::Npm { package } => {
                if package.is_empty()
                    || package.len() > 214
                    || package.starts_with('-')
                    || !package
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"@/._~^-".contains(&b))
                {
                    return Err(format!("{package:?} is not an npm package name"));
                }
            }
            Install::Download { url, sha256 } => {
                if !url.starts_with("https://")
                    || url.len() > 2048
                    || url.chars().any(|c| c.is_whitespace() || c.is_control())
                {
                    return Err("a download needs an https:// address".into());
                }
                if sha256
                    .as_ref()
                    .is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return Err("sha256 is 64 hex characters".into());
                }
                if self.programs.is_empty() {
                    return Err("a download needs the program's name".into());
                }
            }
            Install::Script { script } => {
                if script.trim().is_empty() || script.len() > MAX_SCRIPT || script.contains('\0') {
                    return Err(format!("a script has 1-{MAX_SCRIPT} bytes"));
                }
            }
        }
        let Some(account) = &self.account else {
            return Ok(());
        };
        if account.hosts.is_empty() || account.hosts.len() > MAX_ITEMS {
            return Err(format!("an account has 1-{MAX_ITEMS} hosts"));
        }
        if let Some(bad) = account.hosts.iter().find(|h| !public_name(h)) {
            return Err(format!(
                "{bad:?} is not a public host name like api.example.com"
            ));
        }
        if account.env.is_empty() || account.env.len() > MAX_ITEMS {
            return Err(format!(
                "an account has 1-{MAX_ITEMS} environment variables"
            ));
        }
        if let Some(bad) = account.env.iter().find(|v| !env_name(v)) {
            return Err(format!(
                "{bad:?} cannot be a token variable: use a name like EXAMPLE_API_KEY that the sandbox does not use itself"
            ));
        }
        if let Some(s) = &account.sign_in {
            let argv = |a: &[String]| {
                (1..=16).contains(&a.len())
                    && a.iter()
                        .all(|w| !w.is_empty() && w.len() <= 256 && !w.contains('\0'))
            };
            if !argv(&s.login) || !argv(&s.token) {
                return Err("sign-in commands are 1-16 words".into());
            }
        }
        for other in others.iter().filter(|o| o.id != self.id) {
            let Some(theirs) = &other.account else {
                continue;
            };
            if let Some(h) = account.hosts.iter().find(|h| theirs.hosts.contains(h)) {
                return Err(format!("{h} already belongs to {}", other.id));
            }
            if let Some(v) = account.env.iter().find(|v| theirs.env.contains(v)) {
                return Err(format!("{v} already belongs to {}", other.id));
            }
        }
        Ok(())
    }
}

fn program_name(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 64
        && !p.starts_with('.')
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(id: &str, hosts: &[&str], env: &[&str]) -> Cli {
        Cli {
            id: id.into(),
            install: Install::Npm {
                package: format!("@scope/{id}"),
            },
            programs: vec![id.into()],
            account: Some(Account {
                hosts: hosts.iter().map(|h| (*h).into()).collect(),
                env: env.iter().map(|v| (*v).into()).collect(),
                confirm: true,
                sign_in: None,
            }),
            agents: Vec::new(),
        }
    }

    #[test]
    fn a_token_goes_only_to_public_names_and_leaves_the_sandbox_variables_alone() {
        let ok = cli("example", &["api.example.com"], &["EXAMPLE_API_KEY"]);
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
                cli("x", &[host], &["X_KEY"]).validate(&[]).is_err(),
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
                cli("x", &["api.x.com"], &[var]).validate(&[]).is_err(),
                "{var}"
            );
        }
        let taken = cli("other", &["api.example.com"], &["OTHER_KEY"]);
        assert!(ok.validate(&[taken]).is_err(), "one host, one CLI");
        let same_var = cli("other", &["api.other.com"], &["EXAMPLE_API_KEY"]);
        assert!(ok.validate(&[same_var]).is_err(), "one variable, one CLI");
    }

    #[test]
    fn installs_are_checked_before_they_reach_a_computer() {
        let with = |install: Install, programs: &[&str]| Cli {
            id: "example".into(),
            install,
            programs: programs.iter().map(|p| (*p).into()).collect(),
            account: None,
            agents: Vec::new(),
        };
        let download = |url: &str, sha256: Option<&str>| Install::Download {
            url: url.into(),
            sha256: sha256.map(Into::into),
        };
        let hash = "ab".repeat(32);
        assert!(
            with(download("https://example.com/x", Some(&hash)), &["x"])
                .validate(&[])
                .is_ok()
        );
        for bad in [
            with(download("http://example.com/x", None), &["x"]),
            with(download("https://example.com/x", Some("abc")), &["x"]),
            with(download("https://example.com/x", None), &[]),
            with(
                Install::Npm {
                    package: "x; rm -rf ~".into(),
                },
                &[],
            ),
            with(
                Install::Script {
                    script: "  ".into(),
                },
                &[],
            ),
            with(
                Install::Debian {
                    package: "jq".into(),
                },
                &["../bin"],
            ),
        ] {
            assert!(bad.validate(&[]).is_err(), "{bad:?}");
        }
        assert!(
            Cli {
                id: "Bad Name".into(),
                ..with(
                    Install::Debian {
                        package: "jq".into()
                    },
                    &[]
                )
            }
            .validate(&[])
            .is_err()
        );
    }
}
