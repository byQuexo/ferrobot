//! A CLI's own device sign-in, in a throwaway container from the agents' image: install the CLI,
//! start its login, show the operator its code, then read the token into the secret store and
//! remove the container. The token never reaches an agent's computer, a log or a response.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrobot_core::{Cli, CliSignIn};
use ferrobot_sandbox::{ExecRequest, SandboxHandle, SandboxSpec};
use serde::Serialize;
use tokio::sync::Notify;
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;

const SIGN_IN_FOR: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_secs(1);
const LOG: &str = "/tmp/ferrobot-sign-in.log";
const EXIT: &str = "/tmp/ferrobot-sign-in.exit";

/// Where a sign-in stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SignInView {
    #[default]
    Idle,
    /// Installing the CLI and starting its login.
    Starting,
    /// The CLI waits for the operator: open `url` and enter `code`.
    Waiting {
        url: Option<String>,
        code: Option<String>,
    },
    /// The token is in the secret store.
    Done,
    Failed {
        error: String,
    },
}

#[derive(Debug, Default)]
pub struct SignIns(Mutex<HashMap<String, (SignInView, Arc<Notify>)>>);

impl SignIns {
    fn set(&self, id: &str, view: SignInView) {
        if let Some(entry) = crate::lock(&self.0).get_mut(id) {
            entry.0 = view;
        }
    }
}

pub(crate) fn view(state: &AppState, id: &str) -> SignInView {
    crate::lock(&state.clis.sign_ins.0)
        .get(id)
        .map(|(view, _)| view.clone())
        .unwrap_or_default()
}

/// Stops a running sign-in; its container goes with it.
pub(crate) fn cancel(state: &AppState, id: &str) {
    if let Some((_, cancel)) = crate::lock(&state.clis.sign_ins.0).remove(id) {
        cancel.notify_one();
    }
}

/// Starts `cli`'s sign-in in the background, replacing one still running.
pub(crate) fn start(state: &AppState, cli: Cli) -> Result<SignInView, ApiError> {
    let Some(sign_in) = cli.account.as_ref().and_then(|a| a.sign_in.clone()) else {
        return Err(ApiError::BadRequest(format!(
            "{} has no sign-in; paste its token instead",
            cli.id
        )));
    };
    if !state.backend.supports_image_builds() {
        return Err(ApiError::BadRequest("CLIs need the Docker backend".into()));
    }
    cancel(state, &cli.id);
    let notify = Arc::new(Notify::new());
    crate::lock(&state.clis.sign_ins.0)
        .insert(cli.id.clone(), (SignInView::Starting, notify.clone()));
    let state = state.clone();
    tokio::spawn(async move {
        let created = Mutex::new(None);
        let outcome = tokio::select! {
            r = run(&state, &cli, &sign_in, &created) => Some(r),
            () = notify.notified() => None,
        };
        let handle = crate::lock(&created).take();
        match handle {
            Some(handle) => {
                if let Err(e) = state.backend.destroy(&handle, true).await {
                    tracing::warn!(cli = %cli.id, error = %e, "sign-in container not removed");
                }
            }
            None => remove(&state, &cli.id).await,
        }
        match outcome {
            Some(Ok(())) => state.clis.sign_ins.set(&cli.id, SignInView::Done),
            Some(Err(error)) => state
                .clis
                .sign_ins
                .set(&cli.id, SignInView::Failed { error }),
            None => {}
        }
    });
    Ok(SignInView::Starting)
}

/// A name no agent can have (agent names have no dot).
fn sandbox_name(id: &str) -> String {
    format!("signin.{id}")
}

/// Removes the sign-in container of `id`, if any.
async fn remove(state: &AppState, id: &str) {
    // shortcut: assumes the default `ferrobot-` container prefix, which is the only one the daemon sets.
    let handle = SandboxHandle {
        backend: state.backend.kind().into(),
        id: format!("ferrobot-{}", sandbox_name(id)),
    };
    if let Err(e) = state.backend.destroy(&handle, true).await {
        tracing::debug!(cli = %id, error = %e, "no sign-in container to remove");
    }
}

/// Removes sign-in containers a stopped daemon left behind; one may hold a token.
pub(crate) async fn sweep(state: &AppState, clis: &[Cli]) {
    for cli in clis
        .iter()
        .filter(|c| c.account.as_ref().is_some_and(|a| a.sign_in.is_some()))
    {
        remove(state, &cli.id).await;
    }
}

async fn run(
    state: &AppState,
    cli: &Cli,
    sign_in: &CliSignIn,
    created: &Mutex<Option<SandboxHandle>>,
) -> Result<(), String> {
    remove(state, &cli.id).await;
    let home = state.backend.home();
    let spec = SandboxSpec {
        name: sandbox_name(&cli.id),
        env: BTreeMap::from([(
            "PATH".to_owned(),
            format!("{home}/.local/bin:{}", crate::clis::DEFAULT_PATH),
        )]),
        labels: BTreeMap::from([("dev.ferrobot.sign-in".to_owned(), cli.id.clone())]),
        image: crate::clis::image(state).await.map_err(|e| e.to_string())?,
        command: Some(vec!["sleep".into(), "infinity".into()]),
    };
    let handle = state
        .backend
        .create(&spec)
        .await
        .map_err(|e| e.to_string())?;
    *crate::lock(created) = Some(handle.clone());
    let exec = |req: ExecRequest| {
        let handle = handle.clone();
        async move {
            state
                .backend
                .exec(&handle, &req)
                .await
                .map_err(|e| e.to_string())
        }
    };
    if let Some(req) = crate::clis::install_command(cli, &home) {
        let out = exec(req).await?;
        if out.exit_code != 0 {
            return Err(format!("installing {} failed", cli.id));
        }
    }
    // The login runs on after this exec returns; its output and exit code go to files.
    let mut start = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        format!("( \"$@\"; echo $? > {EXIT} ) > {LOG} 2>&1 < /dev/null &"),
        "sh".to_owned(),
    ];
    start.extend(sign_in.login.iter().cloned());
    exec(ExecRequest::new(start)).await?;
    let deadline = tokio::time::Instant::now() + SIGN_IN_FOR;
    let poll = ExecRequest::new([
        "sh",
        "-c",
        &format!("cat {LOG} 2>/dev/null; printf '\\036'; cat {EXIT} 2>/dev/null"),
    ]);
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err("no sign-in within 10 minutes".into());
        }
        let out = exec(poll.clone()).await?;
        let (log, exit) = out.stdout.split_once('\u{1e}').unwrap_or((&out.stdout, ""));
        let (url, code) = device_prompt(log);
        if url.is_some() || code.is_some() {
            state
                .clis
                .sign_ins
                .set(&cli.id, SignInView::Waiting { url, code });
        }
        match exit.trim() {
            "" => tokio::time::sleep(POLL).await,
            "0" => break,
            _ => {
                let last = log
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                return Err(format!(
                    "the sign-in ended without a token: {}",
                    last.chars().take(200).collect::<String>()
                ));
            }
        }
    }
    let out = exec(ExecRequest::new(sign_in.token.clone())).await?;
    let token = out.stdout.trim();
    if out.exit_code != 0
        || token.is_empty()
        || token.len() > 4096
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("{} did not print a token", cli.id));
    }
    state
        .settings
        .secrets
        .set(&crate::logins::secret_key(&cli.id), token)
        .await
        .map_err(|e| e.to_string())
}

/// The address and one-time code a device sign-in prints, e.g. `https://example.com/device` and
/// `ABCD-1234`.
fn device_prompt(log: &str) -> (Option<String>, Option<String>) {
    let words = || log.split_whitespace();
    let url = words()
        .find(|w| w.starts_with("https://"))
        .map(|w| w.trim_end_matches(['.', ',', ')', '"', '\'']).to_owned());
    let code = words()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .find(|w| {
            w.split_once('-').is_some_and(|(a, b)| {
                [a, b].iter().all(|p| {
                    (4..=8).contains(&p.len())
                        && p.bytes()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                })
            })
        })
        .map(str::to_owned);
    (url, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_device_address_and_code_in_a_login_s_output() {
        let log = "\n! First copy your one-time code: F94D-C257\nOpen this URL to continue in your \
                   web browser: https://example.com/login/device\n";
        assert_eq!(
            device_prompt(log),
            (
                Some("https://example.com/login/device".into()),
                Some("F94D-C257".into())
            )
        );
        assert_eq!(device_prompt("starting..."), (None, None));
        assert_eq!(device_prompt("see my-file and x-y").1, None, "not a code");
    }
}
