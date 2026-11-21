//! Creating agents. Shared by `POST /v1/agents` (the operator) and the
//! `create_agent` MCP tool (an agent, after the operator approved or with Auto mode on).

use std::fmt::Write as _;
use std::time::Duration;

use ferrobot_core::protocol::DaemonFrame;
use ferrobot_core::question::{
    APPROVE, Approval, DECLINE, QuestionAnswer, QuestionAnswered, QuestionEvent,
};
use ferrobot_core::{Agent, AgentId, AgentName, AvatarCharacter, Connector, ToolGroup};
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
    /// `None`: every group from the operator, the creator's own from an agent.
    pub tools: Option<Vec<ToolGroup>>,
    /// Names of the creator's connectors; `None`: all of them. The operator assigns
    /// connectors separately.
    pub connectors: Option<Vec<String>>,
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
            tools: r.tools,
            connectors: r.connectors,
        }
    }
}

/// The new agent's tools and connectors. From an agent they are at most its own, checked
/// here on every path (approval card or Auto mode) so a request can never widen them.
async fn profile(
    state: &AppState,
    spec: &AgentSpec,
    creator: Option<AgentId>,
) -> Result<(Vec<ToolGroup>, Vec<Connector>), ApiError> {
    let Some(creator) = creator else {
        let tools = spec.tools.clone().unwrap_or_else(ToolGroup::all);
        return Ok((ToolGroup::normalize(tools), Vec::new()));
    };
    let own = state.store.get_agent(creator).await?.tools;
    let tools = ToolGroup::normalize(spec.tools.clone().unwrap_or_else(|| own.clone()));
    let wider: Vec<&str> = tools
        .iter()
        .filter(|g| !own.contains(g))
        .map(|g| g.as_str())
        .collect();
    if !wider.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "a new agent gets at most your own tools; you do not have: {}",
            wider.join(", ")
        )));
    }
    let mine = state.store.agent_connectors(creator).await?;
    let connectors = match &spec.connectors {
        None => mine,
        Some(names) => names
            .iter()
            .map(|n| {
                mine.iter()
                    .find(|c| c.name == n.trim())
                    .cloned()
                    .ok_or_else(|| ApiError::BadRequest(format!("you have no connector {n}")))
            })
            .collect::<Result<_, _>>()?,
    };
    Ok((tools, connectors))
}

/// The agent's instructions as its runner gets them: with the operator's naming convention
/// when it may create agents.
pub(crate) async fn runner_instructions(state: &AppState, agent: &Agent) -> String {
    let names = crate::api::settings::load(state)
        .await
        .ok()
        .and_then(|s| s.agent_names);
    match names {
        Some(names) if agent.tools.contains(&ToolGroup::CreateAgents) => {
            let lead = if agent.instructions.trim().is_empty() {
                String::new()
            } else {
                format!("{}\n\n", agent.instructions.trim_end())
            };
            format!(
                "{lead}Name the agents you create like this (the operator's convention): {names}"
            )
        }
        _ => agent.instructions.clone(),
    }
}

/// Sends every agent that may create agents its instructions with the current naming
/// convention.
pub(crate) async fn reconfigure_creators(state: &AppState) {
    let Ok(agents) = state.store.list_agents().await else {
        return;
    };
    for agent in agents
        .into_iter()
        .filter(|a| a.tools.contains(&ToolGroup::CreateAgents))
    {
        let instructions = runner_instructions(state, &agent).await;
        state.hub.send(
            agent.id,
            DaemonFrame::Configure {
                agent_name: agent.name,
                instructions,
                tools: agent.tools,
            },
        );
    }
}

/// Lets the agent's connector sessions and runner pick up a changed profile.
pub(crate) async fn reconfigure(state: &AppState, id: AgentId) -> Result<(), ApiError> {
    state.connectors.evict_agent(id).await;
    let agent = state.store.get_agent(id).await?;
    let old_runner = state
        .hub
        .runner_protocol(id)
        .is_some_and(|v| v < ferrobot_core::PROTOCOL_VERSION)
        || state.hub.is_refused(id);
    if old_runner && !crate::runner_ws::fits_old_runner(&agent.tools) {
        // An older runner would ignore the tools: a new computer gets the current runner.
        return lifecycle::recreate(state, id).await;
    }
    let instructions = runner_instructions(state, &agent).await;
    state.hub.send(
        id,
        DaemonFrame::Configure {
            agent_name: agent.name,
            instructions,
            tools: agent.tools,
        },
    );
    Ok(())
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
    let (tools, connectors) = profile(state, &spec, created_by).await?;
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
    // Before provisioning: the agent has no token yet, so nothing runs with every tool.
    state
        .store
        .update_agent_profile(
            agent.id,
            AgentPatch {
                character: Some(spec.character),
                provider_id: Some(provider_id),
                model: Some(model.clone()),
                tools: Some(tools.clone()),
                ..AgentPatch::default()
            },
        )
        .await?;
    agent.character = spec.character;
    agent.provider_id = provider_id;
    agent.model = model;
    agent.tools = tools;
    if !connectors.is_empty() {
        let ids = connectors.into_iter().map(|c| c.id).collect();
        state.store.set_agent_connectors(agent.id, ids).await?;
    }
    lifecycle::provision(state, &agent).await?;
    Ok(agent)
}

pub(crate) fn shorten(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// The agent at the top of `id`'s creation chain, created by the operator:
/// its chat is where the operator talks.
pub(crate) async fn chat_of(state: &AppState, id: AgentId) -> Result<AgentId, ApiError> {
    let mut current = id;
    for _ in 0..16 {
        match state.store.get_agent(current).await?.created_by {
            Some(parent) if state.store.get_agent(parent).await.is_ok() => current = parent,
            _ => break,
        }
    }
    Ok(current)
}

/// The approval card's text: everything the new agent gets, so nothing is approved unseen.
fn card_prompt(
    name: &AgentName,
    description: &str,
    instructions: &str,
    tools: &[ToolGroup],
    connectors: &[Connector],
) -> String {
    let mut prompt = format!("Create the agent \"{name}\"?");
    if !description.is_empty() {
        let _ = write!(prompt, "\nRole: {description}");
    }
    if !instructions.trim().is_empty() {
        let _ = write!(prompt, "\nInstructions: {}", shorten(instructions, 600));
    }
    let tools: Vec<&str> = tools.iter().map(|g| g.as_str()).collect();
    let tools = if tools.is_empty() {
        "none".to_owned()
    } else {
        tools.join(", ")
    };
    let _ = write!(prompt, "\nTools: {tools}");
    if !connectors.is_empty() {
        let names: Vec<&str> = connectors.iter().map(|c| c.name.as_str()).collect();
        let _ = write!(prompt, "\nConnectors: {}", names.join(", "));
    }
    prompt
}

/// Shows the operator an approval card in the chat they use for the
/// requester (its top creator's), waits for the decision (or
/// [`crate::Settings::approval_timeout`]) and creates the agent if approved.
/// With Auto mode on it creates the agent at once and leaves a notice there.
pub async fn request(
    state: &AppState,
    requester: AgentId,
    mut spec: AgentSpec,
) -> Result<AgentRequestOutcome, ApiError> {
    let (name, description) = check(state, &spec).await?;
    // Pinned now, so the agent gets what the card shows even if the requester changes meanwhile.
    let (tools, connectors) = profile(state, &spec, Some(requester)).await?;
    spec.tools = Some(tools.clone());
    spec.connectors = Some(connectors.iter().map(|c| c.name.clone()).collect());
    let chat = chat_of(state, requester).await?;
    // Unreadable settings fail closed: the operator gets the card.
    if crate::api::settings::load(state)
        .await
        .is_ok_and(|s| s.auto_mode)
    {
        let by = state.store.get_agent(requester).await?.name;
        let agent = create(state, spec, Some(requester)).await?;
        let notice =
            serde_json::json!({ "name": agent.name, "id": agent.id, "by": by, "auto": true });
        state.record(chat, None, "agent_created", notice).await;
        state.publish_agent(agent.id).await;
        return Ok(AgentRequestOutcome::Created {
            id: agent.id,
            name: agent.name.to_string(),
        });
    }
    let prompt = card_prompt(&name, &description, &spec.instructions, &tools, &connectors);
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
