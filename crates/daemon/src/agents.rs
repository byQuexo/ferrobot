//! Creating agents. Shared by `POST /v1/agents` (the operator) and the
//! `create_agent` MCP tool (an agent, after the operator approved).

use std::fmt::Write as _;
use std::time::Duration;

use ferrobot_core::question::{
    APPROVE, Approval, DECLINE, QuestionAnswer, QuestionAnswered, QuestionEvent,
};
use ferrobot_core::{Agent, AgentId, AgentName, AvatarCharacter};
use ferrobot_mcp::AgentRequestOutcome;
use ferrobot_store::{AgentPatch, NewAgent};

use crate::api::check_len;
use crate::error::ApiError;
use crate::state::UiEvent;
use crate::{AppState, lifecycle};

/// Upper bound for the one-line description.
pub const MAX_DESCRIPTION_BYTES: usize = 280;
/// Default for [`crate::Settings::max_agents`].
pub const DEFAULT_MAX_AGENTS: usize = 10;
/// Default for [`crate::Settings::approval_timeout`].
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// What to create.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub character: Option<AvatarCharacter>,
    /// `None` with `model` also `None`: the creating agent's choice (or the
    /// daemon default for the operator).
    pub provider_id: Option<uuid::Uuid>,
    pub model: Option<String>,
}

impl From<ferrobot_mcp::AgentRequest> for AgentSpec {
    fn from(r: ferrobot_mcp::AgentRequest) -> Self {
        Self {
            name: r.name,
            description: r.description,
            instructions: r.instructions,
            character: r.character,
            provider_id: None,
            model: None,
        }
    }
}

/// Checks that the provider exists and that a model is known where the
/// provider needs one. Returns the normalised model.
pub(crate) async fn check_model(
    state: &AppState,
    provider_id: Option<uuid::Uuid>,
    model: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    if model.is_some_and(|m| m.len() > 200) {
        return Err(ApiError::BadRequest("model exceeds 200 bytes".into()));
    }
    if let Some(id) = provider_id {
        let provider = state
            .store
            .get_provider(id)
            .await
            .map_err(|_| ApiError::BadRequest(format!("unknown provider {id}")))?;
        if provider.kind.driver() != "claude" && model.is_none() && provider.default_model.is_none()
        {
            return Err(ApiError::BadRequest(format!(
                "provider {} has no default model; pick a model",
                provider.name
            )));
        }
    }
    Ok(model.map(str::to_owned))
}

pub(crate) fn check_description(d: &str) -> Result<(), ApiError> {
    if d.len() > MAX_DESCRIPTION_BYTES {
        return Err(ApiError::BadRequest(format!(
            "description exceeds {MAX_DESCRIPTION_BYTES} bytes"
        )));
    }
    Ok(())
}

pub(crate) fn check_character(c: Option<&AvatarCharacter>) -> Result<(), ApiError> {
    match c {
        Some(c) if !c.is_valid() => {
            Err(ApiError::BadRequest("character hue must be 0..=359".into()))
        }
        _ => Ok(()),
    }
}

/// Validates a spec and checks the name is free and the limit not reached.
async fn check(state: &AppState, spec: &AgentSpec) -> Result<(AgentName, String), ApiError> {
    let name = AgentName::parse(spec.name.trim())?;
    let description = spec.description.trim().to_owned();
    check_description(&description)?;
    check_len("instructions", &spec.instructions)?;
    check_character(spec.character.as_ref())?;
    let agents = state.store.list_agents().await?;
    let max = state.settings.max_agents;
    if agents.len() >= max {
        return Err(ApiError::Conflict(format!(
            "agent limit reached: this host allows at most {max} agents \
             (FERROBOT_MAX_AGENTS); delete one first"
        )));
    }
    if agents.iter().any(|a| a.name == name) {
        return Err(ApiError::Conflict(format!("agent name {name} is taken")));
    }
    Ok((name, description))
}

/// Registers an agent and provisions its sandbox. `created_by`: the
/// creating agent, `None` for the operator.
pub async fn create(
    state: &AppState,
    spec: AgentSpec,
    created_by: Option<AgentId>,
) -> Result<Agent, ApiError> {
    let (provider_id, model) = match (created_by, spec.provider_id, &spec.model) {
        (Some(creator), None, None) => {
            let creator = state.store.get_agent(creator).await?;
            (creator.provider_id, creator.model)
        }
        _ => (spec.provider_id, spec.model.clone()),
    };
    let model = check_model(state, provider_id, model.as_deref()).await?;
    let mut agent = {
        // Serialize creations so the limit holds under concurrency.
        let _guard = state.creating.lock().await;
        let (name, description) = check(state, &spec).await?;
        state
            .store
            .create_agent(NewAgent {
                name,
                description,
                instructions: spec.instructions,
                created_by,
            })
            .await?
    };
    if spec.character.is_some() || provider_id.is_some() || model.is_some() {
        state
            .store
            .update_agent_profile(
                agent.id,
                AgentPatch {
                    character: Some(spec.character),
                    provider_id: Some(provider_id),
                    model: Some(model.clone()),
                    ..AgentPatch::default()
                },
            )
            .await?;
        agent.character = spec.character;
        agent.provider_id = provider_id;
        agent.model = model;
    }
    if let Some(creator) = created_by {
        let inherited = state.store.agent_connectors(creator).await?;
        let ids = inherited.into_iter().map(|c| c.id).collect();
        state.store.set_agent_connectors(agent.id, ids).await?;
    }
    lifecycle::provision(state, &agent).await?;
    Ok(agent)
}

fn shorten(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// The agent at the top of `id`'s creation chain, created by the operator:
/// its chat is where the operator talks.
async fn chat_of(state: &AppState, id: AgentId) -> Result<AgentId, ApiError> {
    let mut current = id;
    for _ in 0..16 {
        match state.store.get_agent(current).await?.created_by {
            Some(parent) if state.store.get_agent(parent).await.is_ok() => current = parent,
            _ => break,
        }
    }
    Ok(current)
}

/// Shows the operator an approval card in the chat they use for the
/// requester (its top creator's), waits for the decision (or
/// [`crate::Settings::approval_timeout`]) and creates the agent if approved.
pub async fn request(
    state: &AppState,
    requester: AgentId,
    spec: AgentSpec,
) -> Result<AgentRequestOutcome, ApiError> {
    let (name, description) = check(state, &spec).await?;
    let chat = chat_of(state, requester).await?;
    let mut prompt = format!("Create the agent \"{name}\"?");
    if !description.is_empty() {
        let _ = write!(prompt, "\nRole: {description}");
    }
    // Everything the new agent gets, so nothing is approved unseen.
    if !spec.instructions.trim().is_empty() {
        let _ = write!(
            prompt,
            "\nInstructions: {}",
            shorten(&spec.instructions, 600)
        );
    }
    let inherited = state.store.agent_connectors(requester).await?;
    if !inherited.is_empty() {
        let names: Vec<&str> = inherited.iter().map(|c| c.name.as_str()).collect();
        let _ = write!(prompt, "\nConnectors: {}", names.join(", "));
    }
    let title = if chat == requester {
        "Approval needed".to_owned()
    } else {
        let who = state.store.get_agent(requester).await?.name;
        format!("{who} wants to create an agent")
    };
    let card = QuestionEvent::approval(
        title,
        prompt,
        Approval::CreateAgent {
            name: name.to_string(),
            description,
        },
    );
    let payload = serde_json::to_value(&card).map_err(|e| ApiError::Internal(e.to_string()))?;
    let event = state
        .store
        .append_event(chat, None, "question".into(), payload)
        .await?;
    let qid = event.id;
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    state.approvals_lock().insert(qid, tx);
    state.publish(UiEvent::Event { event });

    let approved =
        if let Ok(r) = tokio::time::timeout(state.settings.approval_timeout, &mut rx).await {
            r.unwrap_or(false)
        } else {
            let _guard = state.answers.lock().await;
            if state.approvals_lock().remove(&qid).is_some() {
                // Nobody answered: lock the card as declined.
                let answered = QuestionAnswered {
                    question: qid,
                    answers: vec![QuestionAnswer {
                        question_id: "q1".into(),
                        values: vec![DECLINE.into()],
                        custom: None,
                    }],
                    message_id: None,
                };
                let payload = serde_json::to_value(&answered)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                state.record(chat, None, "question_answered", payload).await;
                return Ok(AgentRequestOutcome::Declined {
                    reason: "the operator did not answer in time".into(),
                });
            }
            // The answer raced the timeout and is already in the channel.
            rx.try_recv().unwrap_or(false)
        };
    if !approved {
        return Ok(AgentRequestOutcome::Declined {
            reason: "the operator declined".into(),
        });
    }
    let agent = create(state, spec, Some(requester)).await?;
    state.publish_agent(agent.id).await;
    Ok(AgentRequestOutcome::Created {
        id: agent.id,
        name: agent.name.to_string(),
    })
}

/// Delivers an operator's decision on an approval card to the waiting
/// request. `false` if nothing waits any more (timed out, daemon restarted).
pub(crate) fn decide(state: &AppState, qid: i64, answers: &[QuestionAnswer]) -> bool {
    let Some(tx) = state.approvals_lock().remove(&qid) else {
        return false;
    };
    let approved = answers
        .first()
        .is_some_and(|a| a.values.iter().any(|v| v == APPROVE));
    tx.send(approved).is_ok()
}
