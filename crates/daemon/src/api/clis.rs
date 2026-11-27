//! CLIs in the agents' computers (`/v1/clis`): Debian packages in the shared image, the rest in
//! their agents' home folders, and accounts whose tokens stay in the daemon.

use std::fmt::Write as _;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::question::{
    Approval, QuestionAnswer, QuestionAnswered, QuestionEvent, approves,
};
use ferrobot_core::{Account, AgentId, Cli, Install};
use ferrobot_sandbox::SandboxError;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use crate::AppState;
use crate::clis::{self, BuildStatus};
use crate::error::{ApiError, ErrorBody};
use crate::signin::{self, SignInView};

/// A CLI with what the daemon knows about it.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct CliView {
    #[serde(flatten)]
    cli: Cli,
    /// Whether its account's token is stored. The token itself is never returned.
    has_token: bool,
    /// Why its last home install failed, on which agent; null otherwise.
    error: Option<String>,
}

/// The CLIs and where their image build stands.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ClisView {
    /// Debian packages in the image, sorted (also in `clis`).
    packages: Vec<String>,
    clis: Vec<CliView>,
    /// Whether the image also has a desktop the operator watches and agents with the Desktop
    /// tools use.
    desktop: bool,
    status: BuildStatus,
    /// Why the last build failed; null otherwise.
    error: Option<String>,
    /// Whether the sandbox backend can build images (Docker only).
    supported: bool,
}

/// A new Debian package list (before 0.1.11; `PUT /v1/clis/{id}` adds any CLI).
#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct UpdateClis {
    /// Debian package names, at most 32; other CLIs stay. Absent: unchanged.
    #[serde(default)]
    packages: Option<Vec<String>>,
    /// Adds (`true`) or removes (`false`) the desktop; absent: unchanged.
    #[serde(default)]
    desktop: Option<bool>,
}

/// Most packages the operator lists; the desktop's come on top.
const MAX_CLI_PACKAGES: usize = 32;

/// A CLI as `PUT /v1/clis/{id}` takes it.
#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CliSpec {
    install: Install,
    /// The programs it brings, e.g. `["example"]`.
    #[serde(default)]
    programs: Vec<String>,
    account: Option<Account>,
    /// Agents that get it in their home folder and get its account.
    #[serde(default)]
    #[schema(value_type = Vec<String>)]
    agents: Vec<AgentId>,
}

/// A CLI's token.
#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CliToken {
    /// The service's token, e.g. a fine-grained GitHub token; `null` removes it.
    token: Option<String>,
}

async fn view(state: &AppState) -> ApiResult<ClisView> {
    let (status, error) = state.clis.get();
    let stored = clis::load(state).await?;
    let mut out = Vec::new();
    for cli in stored.clis {
        out.push(CliView {
            has_token: crate::logins::token(state, &cli.id).await.is_some(),
            error: state.clis.error_of(&cli.id),
            cli,
        });
    }
    let plain: Vec<Cli> = out.iter().map(|v| v.cli.clone()).collect();
    Ok(Json(ClisView {
        packages: clis::packages(&plain),
        clis: out,
        desktop: stored.desktop,
        status,
        error,
        supported: state.backend.supports_image_builds(),
    }))
}

async fn find(state: &AppState, id: &str) -> Result<Cli, ApiError> {
    clis::all(state)
        .await
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("CLI {id:?} not found")))
}

/// Every CLI in the agents' computers.
#[utoipa::path(get, path = "/v1/clis", tag = "clis",
    responses((status = 200, body = ClisView)))]
pub(crate) async fn get_clis(State(state): State<AppState>) -> ApiResult<ClisView> {
    view(&state).await
}

/// Replaces the plain Debian packages and builds their image in the background. New sandboxes
/// start from it; existing ones switch when they next wake, keeping their home directory.
#[utoipa::path(put, path = "/v1/clis", tag = "clis",
    request_body = UpdateClis,
    responses((status = 200, body = ClisView),
        (status = 400, description = "Not a Debian package name, or too many", body = ErrorBody),
        (status = 409, description = "A build is running", body = ErrorBody),
        (status = 501, description = "The sandbox backend cannot build images", body = ErrorBody)))]
pub(crate) async fn put_clis(
    State(state): State<AppState>,
    Json(req): Json<UpdateClis>,
) -> ApiResult<ClisView> {
    let packages = req
        .packages
        .map(|p| ferrobot_sandbox::package_set(&p))
        .transpose()
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if packages
        .as_ref()
        .is_some_and(|p| p.len() > MAX_CLI_PACKAGES)
    {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_CLI_PACKAGES} packages"
        )));
    }
    let wants_image = packages.as_ref().is_some_and(|p| !p.is_empty()) || req.desktop == Some(true);
    if wants_image && !state.backend.supports_image_builds() {
        return Err(SandboxError::NotImplemented {
            backend: state.backend.kind(),
            operation: "image builds",
        }
        .into());
    }
    clis::apply(&state, packages, req.desktop).await?;
    view(&state).await
}

/// Adds or replaces a CLI. A Debian package rebuilds the image; other installs go into the home
/// folders of its agents (running ones now, others when they next wake). Agents whose account
/// setup changes restart their computers; their files stay.
#[utoipa::path(put, path = "/v1/clis/{id}", tag = "clis",
    params(("id" = String, Path, description = "The CLI's name, e.g. `example`")),
    request_body = CliSpec,
    responses((status = 200, body = ClisView),
        (status = 400, description = "An invalid field or an unknown agent", body = ErrorBody),
        (status = 409, description = "A build is running", body = ErrorBody)))]
pub(crate) async fn put_cli(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CliSpec>,
) -> ApiResult<ClisView> {
    let agents = super::known_agents(&state, req.agents)
        .await
        .map_err(|_| ApiError::BadRequest("an unknown agent".into()))?;
    let words = |list: Vec<String>, lower: bool| {
        let mut out: Vec<String> = Vec::new();
        for w in list {
            let w = if lower {
                w.trim().to_ascii_lowercase()
            } else {
                w.trim().to_owned()
            };
            if !w.is_empty() && !out.contains(&w) {
                out.push(w);
            }
        }
        out
    };
    let account = req.account.map(|a| Account {
        hosts: words(a.hosts, true),
        env: words(a.env, false),
        ..a
    });
    let cli = Cli {
        id,
        install: req.install,
        programs: words(req.programs, false),
        account,
        agents,
    };
    clis::put(&state, cli).await?;
    view(&state).await
}

/// Removes a CLI, its token and its CA, from the image or its agents' home folders.
#[utoipa::path(delete, path = "/v1/clis/{id}", tag = "clis",
    params(("id" = String, Path, description = "The CLI's name")),
    responses((status = 204, description = "Removed"),
        (status = 404, description = "No such CLI", body = ErrorBody),
        (status = 409, description = "A build is running", body = ErrorBody)))]
pub(crate) async fn delete_cli(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    signin::cancel(&state, &id);
    clis::remove(&state, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Stores or removes a CLI's token. Kept in the daemon's secret store, never returned.
#[utoipa::path(put, path = "/v1/clis/{id}/token", tag = "clis",
    params(("id" = String, Path, description = "The CLI's name")),
    request_body = CliToken,
    responses((status = 200, body = ClisView),
        (status = 400, description = "Not a token, or the CLI has no account", body = ErrorBody),
        (status = 404, description = "No such CLI", body = ErrorBody)))]
pub(crate) async fn put_cli_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CliToken>,
) -> ApiResult<ClisView> {
    let cli = find(&state, &id).await?;
    if cli.account.is_none() {
        return Err(ApiError::BadRequest(format!("{id} has no account")));
    }
    let token = req
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    if token
        .is_some_and(|t| t.len() > 1024 || t.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(ApiError::BadRequest(
            "a token is one word of up to 1024 characters".into(),
        ));
    }
    super::providers::set_secret(
        &state,
        &crate::logins::secret_key(&id),
        token.map(str::to_owned),
    )
    .await?;
    view(&state).await
}

/// Starts a CLI's own device sign-in in a throwaway container; the token it yields goes to the
/// secret store. Poll `GET` for the code to enter.
#[utoipa::path(post, path = "/v1/clis/{id}/sign-in", tag = "clis",
    params(("id" = String, Path, description = "The CLI's name")),
    responses((status = 200, body = SignInView),
        (status = 400, description = "The CLI has no sign-in", body = ErrorBody),
        (status = 404, description = "No such CLI", body = ErrorBody),
        (status = 409, description = "Its image is still building", body = ErrorBody)))]
pub(crate) async fn start_sign_in(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<SignInView> {
    let cli = find(&state, &id).await?;
    if matches!(cli.install, Install::Debian { .. }) && state.clis.get().0 == BuildStatus::Building
    {
        return Err(ApiError::Conflict(format!(
            "the image with {id} is still building; sign in when it is done"
        )));
    }
    Ok(Json(signin::start(&state, cli)?))
}

/// Where a CLI's sign-in stands: the address and code to enter while it waits.
#[utoipa::path(get, path = "/v1/clis/{id}/sign-in", tag = "clis",
    params(("id" = String, Path, description = "The CLI's name")),
    responses((status = 200, body = SignInView)))]
pub(crate) async fn get_sign_in(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<SignInView> {
    Ok(Json(signin::view(&state, &id)))
}

/// Cancels a CLI's sign-in and removes its container.
#[utoipa::path(delete, path = "/v1/clis/{id}/sign-in", tag = "clis",
    params(("id" = String, Path, description = "The CLI's name")),
    responses((status = 204, description = "Cancelled")))]
pub(crate) async fn cancel_sign_in(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> StatusCode {
    signin::cancel(&state, &id);
    StatusCode::NO_CONTENT
}

/// Shows the operator a card to add the CLI an agent proposed (`propose_cli`), with its exact
/// install and the hosts its token would go to; Auto mode never skips it.
pub(crate) async fn request_card(
    state: &AppState,
    requester: AgentId,
    request: ferrobot_mcp::CliRequest,
) -> Result<i64, ApiError> {
    let mut cli = request.cli;
    // An agent never picks the commands that run next to the real token at sign-in.
    if let Some(account) = &mut cli.account {
        account.sign_in = None;
    }
    let all = clis::all(state).await;
    if all.iter().any(|c| c.id == cli.id) {
        return Err(ApiError::Conflict(format!(
            "a CLI named {} is already set up; ask your operator to give it to you",
            cli.id
        )));
    }
    cli.validate(&all).map_err(ApiError::BadRequest)?;
    if let Install::Debian { package } = &cli.install {
        ferrobot_sandbox::package_set(std::slice::from_ref(package))
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    }
    let who = state.store.get_agent(requester).await?.name;
    let mut prompt = format!("Add the CLI \"{}\" to {who}'s computer?\n", cli.id);
    match &cli.install {
        Install::Debian { package } => {
            let _ = write!(
                prompt,
                "Debian package {package}, in every computer's image."
            );
        }
        Install::Npm { package } => {
            let _ = write!(prompt, "npm package {package}, in {who}'s home folder.");
        }
        Install::Download { url, sha256 } => {
            let check = sha256
                .as_ref()
                .map_or("no checksum".to_owned(), |h| format!("SHA-256 {h}"));
            let _ = write!(prompt, "Downloaded from {url} ({check}).");
        }
        Install::Script { script } => {
            let _ = write!(prompt, "Runs this script in {who}'s home folder:\n{script}");
        }
    }
    if !cli.programs.is_empty() {
        let _ = write!(prompt, "\nPrograms: {}.", cli.programs.join(", "));
    }
    if let Some(account) = &cli.account {
        let _ = write!(
            prompt,
            "\nIts token goes only to {}, read from {}; add it under Settings → Computers after \
             adding it. {who} never sees it.{}",
            account.hosts.join(", "),
            account.env.join(", "),
            if account.confirm {
                " Every command that uses it asks first."
            } else {
                ""
            }
        );
    }
    if !request.reason.is_empty() {
        let _ = write!(prompt, "\nWhy: {}", request.reason);
    }
    let card = QuestionEvent::approval(
        format!("{who} wants a CLI"),
        prompt,
        Approval::AddCli { cli, requester },
    )
    .in_turn(state.hub.current_turn(requester));
    let chat = crate::agents::chat_of(state, requester).await?;
    crate::agents::post_card(state, chat, &card).await
}

/// The operator's answer to a proposed CLI: approved, it is added for the agent that asked.
pub(crate) async fn answer_card(
    state: &AppState,
    chat: AgentId,
    qid: i64,
    approval: &Approval,
    answers: Vec<QuestionAnswer>,
) -> Result<QuestionAnswered, ApiError> {
    let Approval::AddCli { cli, requester } = approval else {
        return Err(ApiError::Internal("not an add-CLI card".into()));
    };
    let note = if approves(&answers) {
        // Approved later than asked: a CLI set up meanwhile keeps its name, setup and token.
        if clis::load(state).await?.clis.iter().any(|c| c.id == cli.id) {
            return Err(ApiError::Conflict(format!(
                "a CLI named {} was set up after this card; this card can no longer add it",
                cli.id
            )));
        }
        state
            .settings
            .secrets
            .delete(&crate::logins::secret_key(&cli.id))
            .await?;
        clis::put(state, cli.clone()).await?;
        if cli.account.is_some() {
            format!(
                "The operator added the CLI \"{}\". Its account works once they add its token \
                 under Settings; never ask for it.",
                cli.id
            )
        } else {
            format!(
                "The operator added the CLI \"{}\". It is in your computer in a moment.",
                cli.id
            )
        }
    } else {
        format!("The operator declined the CLI \"{}\".", cli.id)
    };
    let message_id = if state.store.get_agent(*requester).await.is_ok() {
        Some(crate::agents::tell(state, *requester, note).await?.id)
    } else {
        None
    };
    let answered = QuestionAnswered {
        question: qid,
        answers,
        message_id,
        timed_out: false,
    };
    crate::agents::record_answer(state, chat, answered).await
}
