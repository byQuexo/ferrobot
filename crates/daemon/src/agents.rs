//! Creating agents. Shared by `POST /v1/agents` (the operator) and the
//! `create_agent` MCP tool (an agent, after the operator approved or with Auto mode on).

use std::fmt::Write as _;
use std::time::Duration;

use ferrobot_core::protocol::DaemonFrame;
use ferrobot_core::question::{
    APPROVE, Approval, DECLINE, QuestionAnswer, QuestionAnswered, QuestionEvent, approves,
};
use ferrobot_core::{Agent, AgentId, AgentName, AvatarCharacter, Connector, Sender, ToolGroup};
use ferrobot_mcp::{AgentRequestOutcome, AgentToolsOutcome, AgentToolsRequest};
use ferrobot_store::{AgentPatch, NewAgent};

use crate::api::check_len;
use crate::error::ApiError;
use crate::state::UiEvent;
use crate::{AppState, lifecycle};

/// Upper bound for the one-line description.
pub const MAX_DESCRIPTION_BYTES: usize = 280;
/// Most tags an agent can have.
pub const MAX_TAGS: usize = 5;
/// Longest tag, in characters.
pub const MAX_TAG_CHARS: usize = 20;
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
    /// The operator's labels; agents create agents without any.
    pub tags: Vec<String>,
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
            tags: Vec::new(),
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
    let tools = at_most(&own, spec.tools.clone().unwrap_or_else(|| own.clone()))?;
    let mine = state.store.agent_connectors(creator).await?;
    let connectors = match &spec.connectors {
        None => mine,
        Some(names) => pick(&mine, names)?,
    };
    Ok((tools, connectors))
}

/// `tools`, normalized, if `own` has every one of them.
fn at_most(own: &[ToolGroup], tools: Vec<ToolGroup>) -> Result<Vec<ToolGroup>, ApiError> {
    let tools = ToolGroup::normalize(tools);
    let wider: Vec<&str> = tools
        .iter()
        .filter(|g| !own.contains(g))
        .map(|g| g.as_str())
        .collect();
    if !wider.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "agents you create get at most your own tools; you do not have: {}",
            wider.join(", ")
        )));
    }
    Ok(tools)
}

/// The connectors of `mine` named in `names`.
fn pick(mine: &[Connector], names: &[String]) -> Result<Vec<Connector>, ApiError> {
    names
        .iter()
        .map(|n| {
            mine.iter()
                .find(|c| c.name == n.trim())
                .cloned()
                .ok_or_else(|| ApiError::BadRequest(format!("you have no connector {n}")))
        })
        .collect()
}

/// Replaces an agent's tool groups and connectors (`None`: unchanged) the way
/// `PATCH /v1/agents/{id}` and `PUT /v1/agents/{id}/connectors` do.
pub(crate) async fn set_profile(
    state: &AppState,
    id: AgentId,
    tools: Option<Vec<ToolGroup>>,
    connectors: Option<Vec<uuid::Uuid>>,
) -> Result<(), ApiError> {
    // Connectors first: one deleted since the card fails here, before the tools change.
    if let Some(ids) = connectors {
        state.store.set_agent_connectors(id, ids).await?;
    }
    if tools.is_some() {
        let patch = AgentPatch {
            tools,
            ..AgentPatch::default()
        };
        state.store.update_agent_profile(id, patch).await?;
    }
    reconfigure(state, id).await?;
    state.publish_agent(id).await;
    Ok(())
}

/// Names of the tool groups and connectors in the new sets (`None`: unchanged) that the agent
/// does not have yet.
fn added<'a>(
    tools: &[ToolGroup],
    connectors: &[Connector],
    new_tools: Option<&[ToolGroup]>,
    new_connectors: Option<&'a [Connector]>,
) -> Vec<&'a str> {
    let new_tools = new_tools.into_iter().flatten();
    let new_connectors = new_connectors.into_iter().flatten();
    new_tools
        .filter(|g| !tools.contains(g))
        .map(|g| g.as_str())
        .chain(
            new_connectors
                .filter(|c| !connectors.iter().any(|t| t.id == c.id))
                .map(|c| c.name.as_str()),
        )
        .collect()
}

fn list<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let names: Vec<&str> = names.into_iter().collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

/// Changes the tools of an agent `requester` created (`set_agent_tools`), at most to its own.
/// Narrowing applies at once; anything new needs the operator's card unless Auto mode is on.
pub async fn set_tools(
    state: &AppState,
    requester: AgentId,
    req: AgentToolsRequest,
) -> Result<AgentToolsOutcome, ApiError> {
    if req.tools.is_none() && req.connectors.is_none() {
        return Err(ApiError::BadRequest(
            "give tools, connectors or both".into(),
        ));
    }
    if req.agent == requester {
        return Err(ApiError::BadRequest(
            "you cannot change your own tools".into(),
        ));
    }
    let target = state.store.get_agent(req.agent).await?;
    if target.created_by != Some(requester) {
        return Err(ApiError::BadRequest(format!(
            "you can only change the tools of agents you created; you did not create {}",
            target.name
        )));
    }
    let me = state.store.get_agent(requester).await?;
    let tools = req.tools.map(|t| at_most(&me.tools, t)).transpose()?;
    let mine = state.store.agent_connectors(requester).await?;
    let connectors = req.connectors.map(|n| pick(&mine, &n)).transpose()?;
    let have = state.store.agent_connectors(target.id).await?;
    let new = added(
        &target.tools,
        &have,
        tools.as_deref(),
        connectors.as_deref(),
    );
    let ids: Option<Vec<uuid::Uuid>> = connectors
        .as_ref()
        .map(|cs| cs.iter().map(|c| c.id).collect());
    if new.is_empty() {
        set_profile(state, target.id, tools, ids).await?;
        return Ok(AgentToolsOutcome::Applied);
    }
    let chat = chat_of(state, requester).await?;
    // Unreadable settings fail closed: the operator gets the card.
    if crate::api::settings::load(state)
        .await
        .is_ok_and(|s| s.auto_mode)
    {
        set_profile(state, target.id, tools, ids).await?;
        let notice = serde_json::json!({
            "name": target.name, "id": target.id, "by": me.name, "auto": true
        });
        state
            .record(chat, None, "agent_tools_changed", notice)
            .await;
        return Ok(AgentToolsOutcome::Applied);
    }
    let mut prompt = format!("Change the tools of \"{}\"?", target.name);
    if let Some(tools) = &tools {
        let names = tools.iter().map(|g| g.as_str());
        let _ = write!(prompt, "\nTools: {}", list(names));
    }
    if let Some(connectors) = &connectors {
        let names = connectors.iter().map(|c| c.name.as_str());
        let _ = write!(prompt, "\nConnectors: {}", list(names));
    }
    let _ = write!(prompt, "\nNew for it: {}", list(new));
    let card = QuestionEvent::approval(
        format!("{} wants to give {} more tools", me.name, target.name),
        prompt,
        Approval::SetAgentTools {
            agent: target.id,
            tools,
            connectors: ids,
            requester,
        },
    )
    .in_turn(state.hub.current_turn(requester));
    let payload = serde_json::to_value(&card).map_err(|e| ApiError::Internal(e.to_string()))?;
    let event = state
        .store
        .append_event(chat, None, "question".into(), payload)
        .await?;
    let question_id = event.id;
    state.publish(UiEvent::Event { event });
    Ok(AgentToolsOutcome::Pending { question_id })
}

/// The operator's answer to a `set_agent_tools` card: approved, the change is made; either way
/// the agent that asked gets a message.
pub(crate) async fn answer_tools_card(
    state: &AppState,
    chat: AgentId,
    qid: i64,
    approval: &Approval,
    answers: Vec<QuestionAnswer>,
) -> Result<QuestionAnswered, ApiError> {
    let Approval::SetAgentTools {
        agent,
        tools,
        connectors,
        requester,
    } = approval
    else {
        return Err(ApiError::Internal("not a tools card".into()));
    };
    let note = match state.store.get_agent(*agent).await {
        Err(_) => "The agent whose tools you asked to change no longer exists.".to_owned(),
        Ok(target) if approves(&answers) => {
            set_profile(state, *agent, tools.clone(), connectors.clone()).await?;
            format!("The operator approved the new tools for {}.", target.name)
        }
        Ok(target) => format!("The operator declined the new tools for {}.", target.name),
    };
    let message_id = if state.store.get_agent(*requester).await.is_ok() {
        let msg = state
            .store
            .enqueue_message(Sender::User, *requester, note, None)
            .await?;
        state.message_arrived(*requester);
        let id = msg.id;
        state.publish(UiEvent::Message { message: msg });
        Some(id)
    } else {
        None
    };
    let answered = QuestionAnswered {
        question: qid,
        answers,
        message_id,
        timed_out: false,
    };
    let payload = serde_json::to_value(&answered).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.record(chat, None, "question_answered", payload).await;
    Ok(answered)
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
/// Why an agent without a provider cannot reach a model on this daemon.
pub(crate) const NO_DAEMON_KEY: &str = "the daemon has no Anthropic key: pick a model provider \
    (add one in Settings, Model providers, with its key in Vault)";

pub(crate) async fn check_model(
    state: &AppState,
    provider_id: Option<uuid::Uuid>,
    model: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    if model.is_some_and(|m| m.len() > 200) {
        return Err(ApiError::BadRequest("model exceeds 200 bytes".into()));
    }
    if provider_id.is_none()
        && state.settings.driver == "claude"
        && state.settings.anthropic_api_key.is_none()
    {
        return Err(ApiError::BadRequest(NO_DAEMON_KEY.into()));
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

/// The tags trimmed, if there are at most [`MAX_TAGS`] distinct ones of 1 to [`MAX_TAG_CHARS`]
/// characters.
pub(crate) fn check_tags(tags: Vec<String>) -> Result<Vec<String>, ApiError> {
    if tags.len() > MAX_TAGS {
        return Err(ApiError::BadRequest(format!("at most {MAX_TAGS} tags")));
    }
    let mut out: Vec<String> = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_owned();
        let chars = tag.chars().count();
        if chars == 0 || chars > MAX_TAG_CHARS || tag.chars().any(char::is_control) {
            return Err(ApiError::BadRequest(format!(
                "tags must be 1-{MAX_TAG_CHARS} characters"
            )));
        }
        if out.contains(&tag) {
            return Err(ApiError::BadRequest(format!("tag {tag:?} is there twice")));
        }
        out.push(tag);
    }
    Ok(out)
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
    let tags = check_tags(spec.tags.clone())?;
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
                tags: Some(tags.clone()),
                ..AgentPatch::default()
            },
        )
        .await?;
    agent.tags = tags;
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

/// Puts an approval card in `chat` and waits for the operator, at most
/// [`crate::Settings::approval_timeout`]. `None`: no answer in time; the card is then locked as
/// declined.
pub(crate) async fn await_card(
    state: &AppState,
    chat: AgentId,
    card: &QuestionEvent,
) -> Result<Option<bool>, ApiError> {
    let payload = serde_json::to_value(card).map_err(|e| ApiError::Internal(e.to_string()))?;
    let event = state
        .store
        .append_event(chat, None, "question".into(), payload)
        .await?;
    let qid = event.id;
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    state.approvals_lock().insert(qid, tx);
    state.publish(UiEvent::Event { event });
    state.publish_agent(chat).await;
    if let Ok(r) = tokio::time::timeout(state.settings.approval_timeout, &mut rx).await {
        return Ok(Some(r.unwrap_or(false)));
    }
    let _guard = state.answers.lock().await;
    if state.approvals_lock().remove(&qid).is_none() {
        // The answer raced the timeout and is already in the channel.
        return Ok(Some(rx.try_recv().unwrap_or(false)));
    }
    let answered = QuestionAnswered {
        question: qid,
        answers: vec![QuestionAnswer {
            question_id: "q1".into(),
            values: vec![DECLINE.into()],
            custom: None,
        }],
        message_id: None,
        timed_out: true,
    };
    let payload = serde_json::to_value(&answered).map_err(|e| ApiError::Internal(e.to_string()))?;
    state.record(chat, None, "question_answered", payload).await;
    Ok(None)
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
    )
    .in_turn(state.hub.current_turn(requester));
    let Some(approved) = await_card(state, chat, &card).await? else {
        return Ok(AgentRequestOutcome::Declined {
            reason: "the operator did not answer in time".into(),
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use ToolGroup::{Memory, Team, Web};

    #[test]
    fn only_what_the_agent_lacks_is_new() {
        let connector = |name: &str| Connector {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            url: "https://example.com/mcp".into(),
            command: None,
            token_env: None,
            tools: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let (gh, db) = (connector("github"), connector("db"));
        let have = [Team, Memory];
        let current = [gh.clone()];
        let new_db = [db];
        for (tools, connectors, new) in [
            (None, None, vec![]),
            (Some(&[Team][..]), Some(&[][..]), vec![]),
            (Some(&[Team, Web][..]), None, vec!["web"]),
            (None, Some(&new_db[..]), vec!["db"]),
            (Some(&[Web][..]), Some(&new_db[..]), vec!["web", "db"]),
        ] {
            assert_eq!(added(&have, &current, tools, connectors), new);
        }
        let err = at_most(&have, vec![Web, Team]).unwrap_err().to_string();
        assert!(err.ends_with("you do not have: web"), "{err}");
        assert_eq!(at_most(&have, vec![Memory, Team, Team]).unwrap(), have);
        assert!(pick(&current, &["slack".into()]).is_err());
        assert_eq!(pick(&current, &[" github".into()]).unwrap(), [gh]);
    }
}
