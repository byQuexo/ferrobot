//! The operator's own slash commands for the chat's message box (`/v1/commands`). The app
//! expands them; the daemon only keeps the list.

use std::collections::HashSet;

use axum::Json;
use axum::extract::State;
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
}

/// What a new daemon starts with: an example the operator can change or delete.
fn seed() -> Vec<Command> {
    vec![Command {
        name: "goal".into(),
        description: "Keep working until this is reached, checked after every turn".into(),
        prompt: "$ARGUMENTS".into(),
        as_task: true,
    }]
}

fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The operator's slash commands; a daemon that never saved any lists the example `/goal`.
#[utoipa::path(get, path = "/v1/commands", tag = "settings",
    responses((status = 200, body = [Command])))]
pub(crate) async fn get_commands(State(state): State<AppState>) -> ApiResult<Vec<Command>> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => serde_json::from_str(&raw)
            .map(Json)
            .map_err(|e| ApiError::Internal(format!("commands: {e}"))),
        None => Ok(Json(seed())),
    }
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
        let bad = |why: String| Err(ApiError::BadRequest(format!("/{}: {why}", c.name)));
        if !valid_name(&c.name) {
            return bad(format!(
                "a name has 1-{MAX_NAME} of a-z, 0-9 and -, starting with a letter"
            ));
        }
        if BUILT_IN.contains(&c.name.as_str()) || !seen.insert(c.name.clone()) {
            return bad("that name is taken".into());
        }
        let description = c.description.trim().to_owned();
        if !is_printable(&description, MAX_DESCRIPTION) {
            return bad(format!(
                "a description is one line of 1-{MAX_DESCRIPTION} characters"
            ));
        }
        if c.prompt.trim().is_empty() || c.prompt.len() > MAX_PROMPT_BYTES {
            return bad(format!("a prompt has 1-{MAX_PROMPT_BYTES} bytes"));
        }
        out.push(Command { description, ..c });
    }
    save_json(&state, SETTING, &out).await?;
    Ok(Json(out))
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
