//! The operator's task with a goal (`/task`): the agent is sent back to work after every turn
//! until it calls `finish_task`, has reported, and a check of its work finds the goal met.

use std::collections::BTreeMap;
use std::time::Duration;

use ferrobot_core::protocol::TurnOutcome;
use ferrobot_core::{Agent, AgentId, MailMessage, Provider, ProviderKind, Sender};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::api::settings::{load_json, save_json};
use crate::error::ApiError;
use crate::usage::Family;
use crate::{AppState, UiEvent};

const SETTING: &str = "goals";
pub(crate) const DEFAULT_MAX_CONTINUATIONS: u32 = 20;
pub(crate) const MAX_CONTINUATIONS: u32 = 100;
/// How much of the agent's turn the check reads.
const MAX_CHECK_INPUT: usize = 6000;
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);
const CHECK_FAILED: &str = "the check could not run";

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// An agent's task with a goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct TaskGoal {
    /// What the operator wants done.
    pub goal: String,
    /// How often the agent was sent back to work so far.
    pub continuations: u32,
    /// After this many, the agent gives up.
    pub max_continuations: u32,
    /// The agent's `finish_task` summary, until its turn is checked.
    #[serde(default)]
    pub summary: Option<String>,
    /// The continuation waiting in the mailbox, if any.
    #[serde(default)]
    pub pending: Option<i64>,
}

type Goals = BTreeMap<AgentId, TaskGoal>;

async fn load(state: &AppState) -> Result<Goals, ApiError> {
    load_json(state, SETTING, "goals").await
}

pub(crate) async fn get(state: &AppState, agent: AgentId) -> Option<TaskGoal> {
    load(state).await.ok()?.remove(&agent)
}

const HOW_TO_FINISH: &str = "When it is done, call finish_task with a one-line summary, then end \
    your turn with your report to your operator.";

/// Gives `agent` the task: it goes out as the operator's message. Replaces an earlier task.
pub(crate) async fn set(
    state: &AppState,
    agent: AgentId,
    goal: String,
    max_continuations: u32,
) -> Result<TaskGoal, ApiError> {
    let _lock = LOCK.lock().await;
    let mut goals = load(state).await?;
    if let Some(old) = goals.remove(&agent) {
        drop_pending(state, &old).await;
    }
    let body = format!(
        "{goal}\n\n[ferrobot] This is a task with a goal: keep working until it is done; you are \
         sent back to it whenever a turn ends before. {HOW_TO_FINISH}"
    );
    let message = state
        .store
        .enqueue_message(Sender::User, agent, body, None)
        .await?;
    let task = TaskGoal {
        goal,
        continuations: 0,
        max_continuations,
        summary: None,
        pending: None,
    };
    goals.insert(agent, task.clone());
    save_json(state, SETTING, &goals).await?;
    let event = json!({ "state": "set", "goal": task.goal, "note_id": message.id });
    state.record(agent, None, "goal", event).await;
    state.message_arrived(agent);
    state.publish(UiEvent::Message { message });
    state.publish(UiEvent::GoalChanged { agent_id: agent });
    Ok(task)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    Achieved,
    Stopped,
    GaveUp,
}

impl End {
    fn as_str(self) -> &'static str {
        match self {
            Self::Achieved => "achieved",
            Self::Stopped => "stopped",
            Self::GaveUp => "gave_up",
        }
    }
}

/// Ends the agent's task, if it has one, and drops a waiting continuation.
pub(crate) async fn end(state: &AppState, agent: AgentId, how: End, note: &str) {
    let _lock = LOCK.lock().await;
    end_locked(state, agent, how, note).await;
}

async fn end_locked(state: &AppState, agent: AgentId, how: End, note: &str) {
    let Ok(mut goals) = load(state).await else {
        return;
    };
    let Some(goal) = goals.remove(&agent) else {
        return;
    };
    drop_pending(state, &goal).await;
    if let Err(e) = save_json(state, SETTING, &goals).await {
        tracing::error!(error = %e, %agent, "cannot end the task");
        return;
    }
    let event = json!({ "state": how.as_str(), "goal": goal.goal, "note": note });
    state.record(agent, None, "goal", event).await;
    state.publish(UiEvent::GoalChanged { agent_id: agent });
}

async fn drop_pending(state: &AppState, goal: &TaskGoal) {
    let Some(id) = goal.pending else { return };
    if let Ok(dropped) = state
        .store
        .drop_messages(vec![id], "the task ended".into())
        .await
    {
        for id in dropped {
            state.publish_message(id).await;
        }
    }
}

/// The agent's `finish_task`. `Err` carries a message for the agent.
pub(crate) async fn finish(
    state: &AppState,
    agent: AgentId,
    summary: String,
) -> Result<(), String> {
    let _lock = LOCK.lock().await;
    let mut goals = load(state).await.map_err(|e| e.to_string())?;
    let goal = goals
        .get_mut(&agent)
        .ok_or("you have no task from your operator to finish")?;
    goal.summary = Some(summary);
    save_json(state, SETTING, &goals)
        .await
        .map_err(|e| e.to_string())
}

/// After a turn on one of the operator's messages: ends, continues or checks the agent's task.
pub(crate) fn turn_ended(
    state: &AppState,
    (agent, turn): (AgentId, &str),
    msg: &MailMessage,
    outcome: TurnOutcome,
    reply: Option<String>,
) {
    if msg.sender != Sender::User || msg.room_id.is_some() {
        return;
    }
    let (state, turn) = (state.clone(), turn.to_owned());
    tokio::spawn(async move { check(&state, (agent, &turn), outcome, reply).await });
}

async fn check(
    state: &AppState,
    (agent, turn): (AgentId, &str),
    outcome: TurnOutcome,
    reply: Option<String>,
) {
    let (goal, summary) = {
        let _lock = LOCK.lock().await;
        let Ok(mut goals) = load(state).await else {
            return;
        };
        let Some(goal) = goals.get_mut(&agent) else {
            return;
        };
        match outcome {
            TurnOutcome::Completed => {}
            TurnOutcome::Interrupted => {
                return end_locked(state, agent, End::Stopped, "the turn was stopped").await;
            }
            TurnOutcome::Failed => {
                return end_locked(state, agent, End::Stopped, "a turn failed").await;
            }
        }
        let summary = goal.summary.take();
        let text = goal.goal.clone();
        if summary.is_some() && save_json(state, SETTING, &goals).await.is_err() {
            return;
        }
        (text, summary)
    };
    let reply = reply.unwrap_or_default();
    let Some(summary) = summary else {
        return continue_with(state, agent, &goal, None).await;
    };
    if reply.trim().is_empty() {
        let nudge = "you called finish_task, but your turn ended without a report";
        return continue_with(state, agent, &goal, Some(nudge)).await;
    }
    match judge(state, (agent, turn), &goal, &summary, &reply).await {
        None | Some(Verdict::Met) => end(state, agent, End::Achieved, &summary).await,
        Some(Verdict::NotMet(reason)) => continue_with(state, agent, &goal, Some(&reason)).await,
    }
}

/// Sends the agent back to work, or gives up at the limit.
async fn continue_with(state: &AppState, agent: AgentId, text: &str, reason: Option<&str>) {
    let _lock = LOCK.lock().await;
    let Ok(mut goals) = load(state).await else {
        return;
    };
    let Some(goal) = goals.get_mut(&agent).filter(|g| g.goal == text) else {
        return;
    };
    // An operator's message ending a turn as well must not queue a second one.
    if let Some(id) = goal.pending
        && state
            .store
            .get_message(id)
            .await
            .is_ok_and(|m| m.status == ferrobot_core::MailStatus::Pending)
    {
        return;
    }
    if goal.continuations >= goal.max_continuations {
        let note = format!(
            "still not done after {} continuations",
            goal.max_continuations
        );
        return end_locked(state, agent, End::GaveUp, &note).await;
    }
    goal.continuations += 1;
    let because = reason.map_or_else(String::new, |r| format!(" ({r})"));
    let body = format!(
        "[ferrobot] Not done yet{because}: {text}\n\nContinue working on it. {HOW_TO_FINISH}"
    );
    let message = match state
        .store
        .enqueue_message(Sender::User, agent, body, None)
        .await
    {
        Ok(m) => m,
        Err(e) => return tracing::error!(error = %e, %agent, "cannot continue the task"),
    };
    goal.pending = Some(message.id);
    let step = goal.continuations;
    if let Err(e) = save_json(state, SETTING, &goals).await {
        tracing::error!(error = %e, %agent, "cannot save the task");
    }
    let event = json!({
        "state": "continued", "step": step, "reason": reason, "note_id": message.id,
    });
    // Before the message is published, so the chat hides it from the start.
    state.record(agent, None, "goal", event).await;
    state.message_arrived(agent);
    state.publish(UiEvent::Message { message });
    state.publish(UiEvent::GoalChanged { agent_id: agent });
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Met,
    NotMet(String),
}

/// Asks a model whether the goal is met. `None`: no check for this agent's provider, so its
/// `finish_task` stands. A check that cannot run counts as not met.
async fn judge(
    state: &AppState,
    (agent, turn): (AgentId, &str),
    goal: &str,
    summary: &str,
    reply: &str,
) -> Option<Verdict> {
    let agent = state.store.get_agent(agent).await.ok()?;
    let provider = crate::agents::provider_of(state, &agent).await;
    let prompt = prompt(state, (agent.id, turn), goal, summary, reply).await;
    let answer = match provider.as_ref().map(|p| p.kind) {
        Some(ProviderKind::ClaudeCode) => {
            crate::claude_code::ask(state, "haiku", &prompt, CHECK_TIMEOUT).await
        }
        None | Some(ProviderKind::Anthropic) => {
            ask_api(
                state,
                (&agent, provider.as_ref(), turn),
                Family::Anthropic,
                &prompt,
            )
            .await?
        }
        Some(
            ProviderKind::Openai
            | ProviderKind::Gemini
            | ProviderKind::Openrouter
            | ProviderKind::Ollama
            | ProviderKind::OpenaiCompatible,
        ) => {
            let family = Family::ChatCompletions;
            ask_api(state, (&agent, provider.as_ref(), turn), family, &prompt).await?
        }
        Some(_) => return None,
    };
    Some(answer.and_then(|a| verdict(&a)).unwrap_or_else(|e| {
        tracing::warn!(agent = %agent.id, error = %e, "task check failed");
        Verdict::NotMet(CHECK_FAILED.into())
    }))
}

async fn prompt(
    state: &AppState,
    (agent, turn): (AgentId, &str),
    goal: &str,
    summary: &str,
    reply: &str,
) -> String {
    let events = state
        .store
        .turn_events(agent, turn.to_owned())
        .await
        .unwrap_or_default();
    let steps: Vec<String> = events
        .iter()
        .filter_map(|e| match e.kind.as_str() {
            "tool_use" => Some(format!("- used {}", e.payload["name"].as_str()?)),
            "assistant_text" => Some(format!("- wrote: {}", e.payload["text"].as_str()?)),
            _ => None,
        })
        .collect();
    let half = MAX_CHECK_INPUT / 2;
    format!(
        "You check whether an AI agent got its operator's task done. Judge only from what is \
         shown here and do not assume work that is not described. Answer with JSON only: \
         {{\"met\": true or false, \"reason\": \"one short sentence\"}}.\n\n\
         The task:\n{}\n\nThe agent's one-line summary:\n{}\n\n\
         What the agent did in its last turn:\n{}\n\nIts final report to the operator:\n{}",
        clip(goal, half),
        clip(summary, 500),
        clip_end(&steps.join("\n"), half),
        clip(reply, half),
    )
}

fn clip(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// Keeps the end: the newest steps.
fn clip_end(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    format!("…{}", s.chars().skip(n - max).collect::<String>())
}

/// Reads `{"met": .., "reason": ..}` from a model's answer, which may wrap it in other text.
fn verdict(answer: &str) -> Result<Verdict, String> {
    let json = answer
        .find('{')
        .zip(answer.rfind('}'))
        .filter(|(a, b)| a < b)
        .and_then(|(a, b)| serde_json::from_str::<Value>(&answer[a..=b]).ok())
        .ok_or("the check gave no verdict")?;
    let reason = json["reason"].as_str().unwrap_or_default().trim();
    match json["met"].as_bool() {
        Some(true) => Ok(Verdict::Met),
        Some(false) if reason.is_empty() => Ok(Verdict::NotMet("the check found it unmet".into())),
        Some(false) => Ok(Verdict::NotMet(clip(reason, 300))),
        None => Err("the check gave no verdict".into()),
    }
}

/// One check through the agent's provider (or the daemon's Anthropic key), with the provider's
/// key, from the daemon; recorded in Usage under the turn `task-check:<turn>`. `None`: no
/// model or key to check with.
async fn ask_api(
    state: &AppState,
    (agent, provider, turn): (&Agent, Option<&Provider>, &str),
    family: Family,
    prompt: &str,
) -> Option<Result<String, String>> {
    let model = agent
        .model
        .clone()
        .or_else(|| provider.and_then(|p| p.default_model.clone()))
        .or_else(|| state.settings.model.clone())?;
    let (upstream, key) = match provider {
        None => (
            state.settings.anthropic_upstream.clone(),
            Some(state.settings.anthropic_api_key.clone()?),
        ),
        Some(p) => {
            let key = state
                .settings
                .secrets
                .get(&crate::proxy::secret_key(p.id))
                .await
                .ok()?;
            if key.is_none() && p.kind.needs_key() {
                return None;
            }
            (p.upstream()?, key)
        }
    };
    let message = json!([{ "role": "user", "content": prompt }]);
    let request = if family == Family::Anthropic {
        let req = state
            .http
            .post(format!("{upstream}/v1/messages"))
            .header("anthropic-version", "2023-06-01")
            .json(&json!({ "model": model, "max_tokens": 300, "messages": message }));
        match &key {
            Some(k) => req.header("x-api-key", k),
            None => req,
        }
    } else {
        let req = state
            .http
            .post(format!("{upstream}/chat/completions"))
            .json(&json!({ "model": model, "max_tokens": 300, "messages": message }));
        match &key {
            Some(k) => req.bearer_auth(k),
            None => req,
        }
    };
    let mut call = crate::usage::Call::new(state, agent, provider, "");
    call.model = model;
    call.turn_id = Some(format!("task-check:{turn}"));
    let response = match request.timeout(CHECK_TIMEOUT).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            call.record_failure();
            return Some(Err(format!("the model answered {}", r.status())));
        }
        Err(e) => {
            call.record_failure();
            return Some(Err(format!("cannot reach the model: {e}")));
        }
    };
    let Ok(body) = response.json::<Value>().await else {
        call.record_failure();
        return Some(Err("the model's answer was not JSON".into()));
    };
    let openrouter = provider.is_some_and(|p| p.kind == ProviderKind::Openrouter);
    call.record_body(family, &body, openrouter);
    let text = if family == Family::Anthropic {
        body.pointer("/content/0/text")
    } else {
        body.pointer("/choices/0/message/content")
    };
    Some(
        text.and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "the model gave no text".into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_are_read_from_wrapped_answers() {
        assert_eq!(
            verdict(r#"{"met": true, "reason": "ok"}"#),
            Ok(Verdict::Met)
        );
        assert_eq!(
            verdict("```json\n{\"met\": false, \"reason\": \"tests still fail\"}\n```"),
            Ok(Verdict::NotMet("tests still fail".into()))
        );
        assert!(verdict("I think it is done").is_err());
        assert!(verdict(r#"{"met": "maybe"}"#).is_err());
    }

    #[test]
    fn clipping_keeps_the_newest_steps() {
        assert_eq!(clip_end("abcdef", 3), "…def");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("abc", 3), "abc");
    }
}
