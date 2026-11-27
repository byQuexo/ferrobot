//! The operator's own slash commands for the chat's message box (`/v1/commands`). The app
//! expands them; the daemon keeps the list and the cards of commands agents offer.

use std::collections::HashSet;

use axum::Json;
use axum::extract::State;
use ferrobot_core::AgentId;
use ferrobot_core::question::{
    Approval, QuestionAnswer, QuestionAnswered, QuestionEvent, approves,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::settings::save_json;
use super::{ApiResult, is_printable};
use crate::AppState;
use crate::error::{ApiError, ErrorBody};

const SETTING: &str = "commands";
const MAX_COMMANDS: usize = 50;
const MAX_NAME: usize = 32;
const MAX_DESCRIPTION: usize = 100;
const MAX_PROMPT_BYTES: usize = 4000;
/// Commands the app has itself.
const BUILT_IN: [&str; 1] = ["task"];

/// A slash command: `/name rest` sends its prompt with `rest` filled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Command {
    /// What follows the `/`: 1-32 of `a-z`, `0-9` and `-`, starting with a letter; not a
    /// built-in command (`task`).
    pub name: String,
    /// One short line for the menu, 1-100 characters.
    pub description: String,
    /// What is sent, up to 4000 bytes. `$ARGUMENTS` becomes the text after the command;
    /// without it, that text follows after a blank line.
    pub prompt: String,
    /// Send it as a task with a goal, like `/task`.
    #[serde(default)]
    pub as_task: bool,
    /// The agent that offered it on a card the operator approved; `null`: the operator's own.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub added_by: Option<AgentId>,
}

/// What a new daemon starts with: an example the operator can change or delete.
fn seed() -> Vec<Command> {
    vec![Command {
        name: "goal".into(),
        description: "Keep working until this is reached, checked after every turn".into(),
        prompt: "$ARGUMENTS".into(),
        as_task: true,
        added_by: None,
    }]
}

fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A command as saved (description trimmed), or why it is not valid. Names are checked
/// against each other by the caller.
fn check(c: Command) -> Result<Command, ApiError> {
    let bad = |why: String| ApiError::BadRequest(format!("/{}: {why}", c.name));
    if !valid_name(&c.name) {
        return Err(bad(format!(
            "a name has 1-{MAX_NAME} of a-z, 0-9 and -, starting with a letter"
        )));
    }
    if BUILT_IN.contains(&c.name.as_str()) {
        return Err(bad("that name is taken".into()));
    }
    let description = c.description.trim().to_owned();
    if !is_printable(&description, MAX_DESCRIPTION) {
        return Err(bad(format!(
            "a description is one line of 1-{MAX_DESCRIPTION} characters"
        )));
    }
    if c.prompt.trim().is_empty() || c.prompt.len() > MAX_PROMPT_BYTES {
        return Err(bad(format!("a prompt has 1-{MAX_PROMPT_BYTES} bytes")));
    }
    Ok(Command { description, ..c })
}

async fn load(state: &AppState) -> Result<Vec<Command>, ApiError> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|e| ApiError::Internal(format!("commands: {e}")))
        }
        None => Ok(seed()),
    }
}

/// The operator's slash commands; a daemon that never saved any lists the example `/goal`.
#[utoipa::path(get, path = "/v1/commands", tag = "settings",
    responses((status = 200, body = [Command])))]
pub(crate) async fn get_commands(State(state): State<AppState>) -> ApiResult<Vec<Command>> {
    Ok(Json(load(&state).await?))
}

/// Replaces the slash commands: adding, editing and deleting all send the whole list.
#[utoipa::path(put, path = "/v1/commands", tag = "settings",
    request_body = [Command],
    responses((status = 200, body = [Command]),
        (status = 400, description = "Too many, a bad or taken name, or a bad description or prompt", body = ErrorBody)))]
pub(crate) async fn put_commands(
    State(state): State<AppState>,
    Json(list): Json<Vec<Command>>,
) -> ApiResult<Vec<Command>> {
    if list.len() > MAX_COMMANDS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_COMMANDS} commands"
        )));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for c in list {
        if !seen.insert(c.name.clone()) {
            return Err(ApiError::BadRequest(format!(
                "/{}: that name is taken",
                c.name
            )));
        }
        out.push(check(c)?);
    }
    save_json(&state, SETTING, &out).await?;
    Ok(Json(out))
}

/// An agent's `add_command`: always a card, Auto mode or not, since the prompt is later sent
/// in the operator's name. Returns the card's id.
pub(crate) async fn request(
    state: &AppState,
    requester: AgentId,
    req: ferrobot_mcp::CommandRequest,
) -> Result<i64, ApiError> {
    let command = check(Command {
        name: req.name,
        description: req.description,
        prompt: req.prompt,
        as_task: req.as_task,
        added_by: Some(requester),
    })?;
    let list = load(state).await?;
    if list.iter().any(|c| c.name == command.name) {
        return Err(ApiError::Conflict(format!(
            "/{} already exists; pick another name",
            command.name
        )));
    }
    if list.len() >= MAX_COMMANDS {
        return Err(ApiError::Conflict(format!(
            "the operator already has {MAX_COMMANDS} commands"
        )));
    }
    let who = state.store.get_agent(requester).await?.name;
    let how = if command.as_task {
        "as a task: the agent keeps working until it is done"
    } else {
        "as your message"
    };
    let prompt = format!(
        "Add /{} to your message box?\n{}\nSends, {how}:\n\n{}",
        command.name, command.description, command.prompt
    );
    let card = QuestionEvent::approval(
        format!("{who} offers a command"),
        prompt,
        Approval::AddCommand {
            name: command.name,
            description: command.description,
            prompt: command.prompt,
            as_task: command.as_task,
            requester,
        },
    )
    .in_turn(state.hub.current_turn(requester));
    let chat = crate::agents::chat_of(state, requester).await?;
    crate::agents::post_card(state, chat, &card).await
}

/// The operator's answer to a command card: approved, the command is added unless its name
/// was taken meanwhile (`409`, the card stays open); either way the agent gets a message.
pub(crate) async fn answer_card(
    state: &AppState,
    chat: AgentId,
    qid: i64,
    approval: &Approval,
    answers: Vec<QuestionAnswer>,
) -> Result<QuestionAnswered, ApiError> {
    let Approval::AddCommand {
        name,
        description,
        prompt,
        as_task,
        requester,
    } = approval
    else {
        return Err(ApiError::Internal("not a command card".into()));
    };
    let note = if approves(&answers) {
        let mut list = load(state).await?;
        if list.iter().any(|c| &c.name == name) {
            return Err(ApiError::Conflict(format!(
                "/{name} exists now: rename or delete it first, or decline"
            )));
        }
        list.push(Command {
            name: name.clone(),
            description: description.clone(),
            prompt: prompt.clone(),
            as_task: *as_task,
            added_by: Some(*requester),
        });
        save_json(state, SETTING, &list).await?;
        format!("The operator added your command /{name}.")
    } else {
        format!("The operator declined your command /{name}.")
    };
    let msg = crate::agents::tell(state, *requester, note).await?;
    let answered = QuestionAnswered {
        question: qid,
        answers,
        message_id: Some(msg.id),
        timed_out: false,
    };
    crate::agents::record_answer(state, chat, answered).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_short_lowercase_words() {
        for ok in ["goal", "fix-tests", "a1", &"a".repeat(32)] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "1st",
            "-x",
            "Goal",
            "goal now",
            "ziel/",
            &"a".repeat(33),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
