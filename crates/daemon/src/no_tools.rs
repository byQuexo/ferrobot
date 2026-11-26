//! Models that can't use tools: the model proxy finds them, the agent's turns wait as on a
//! credential problem, and one card asks the operator whether to run the agent without tools.

use axum::http::StatusCode;
use ferrobot_core::question::{
    Approval, QuestionAnswer, QuestionAnswered, QuestionEvent, approves,
};
use ferrobot_core::{Agent, AgentId};
use serde_json::json;

use crate::error::ApiError;
use crate::views::ToolsUnsupported;
use crate::{AppState, credentials};

/// The card's `approval.kind`.
const CARD: &str = "use_without_tools";

/// Whether a model request asks for tools: a non-empty `tools` list.
pub(crate) fn sends_tools(body: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Request {
        tools: Option<Vec<serde::de::IgnoredAny>>,
    }
    serde_json::from_slice::<Request>(body).is_ok_and(|r| r.tools.is_some_and(|t| !t.is_empty()))
}

/// Whether an upstream error says the model can't use tools, as `OpenRouter`, Ollama and
/// OpenAI-compatible servers word it.
pub(crate) fn refuses_tools(status: StatusCode, body: &[u8]) -> bool {
    if !matches!(status.as_u16(), 400 | 404 | 422) {
        return false;
    }
    let text = String::from_utf8_lossy(body).to_lowercase();
    text.contains("endpoints found that support tool use")
        || ["tools", "tool use", "tool calling", "function calling"]
            .iter()
            .any(|t| {
                [
                    format!("not support {t}"),
                    format!("n't support {t}"),
                    format!("{t} not supported"),
                    format!("{t} is not supported"),
                    format!("{t} are not supported"),
                ]
                .iter()
                .any(|p| text.contains(p.as_str()))
            })
}

/// Holds the agent's turns if the upstream refused its request with tools because `model` can't
/// use them. Reads an error's body and hands back an equal response.
pub(crate) async fn check(
    state: &AppState,
    agent: &Agent,
    model: &str,
    response: reqwest::Response,
) -> reqwest::Response {
    let status = response.status();
    if !status.is_client_error() {
        return response;
    }
    let headers = response.headers().clone();
    // shortcut: the whole error body is read; error bodies are small, cap it if one is not.
    let body = response.bytes().await.unwrap_or_default();
    if refuses_tools(status, &body) {
        set(state, agent, model).await;
    }
    let mut copy = axum::http::Response::new(body);
    *copy.status_mut() = status;
    *copy.headers_mut() = headers;
    copy.into()
}

/// Holds the agent's turns; the first report leaves a notice and, unless one is open, the card.
/// Synchronous with the proxy's answer, so the failing turn already sees it when it settles.
async fn set(state: &AppState, agent: &Agent, model: &str) {
    let provider = match agent.provider_id {
        Some(id) => state.store.get_provider(id).await.map(|p| p.name),
        None => Ok("Anthropic".to_owned()),
    };
    let provider = provider.unwrap_or_else(|_| "Its provider".to_owned());
    let model = crate::agents::shorten(model, 100);
    let reason = format!("{provider} can't run {model} with tools");
    let problem = ToolsUnsupported {
        model: model.clone(),
        reason: reason.clone(),
        since: time::OffsetDateTime::now_utc(),
    };
    if !state.hub.set_tools_unsupported(agent.id, problem) {
        return;
    }
    tracing::warn!(agent = %agent.name, "model can't use tools, turns held");
    let turn = state.hub.current_turn(agent.id);
    let payload = json!({ "model": model, "reason": reason, "turn_id": turn });
    state
        .record(agent.id, None, "tools_unsupported", payload)
        .await;
    state.publish_agent(agent.id).await;
    if let Err(e) = card(state, agent, &model, &reason).await {
        tracing::error!(agent = %agent.name, error = %e, "could not show the tools card");
    }
}

async fn card(state: &AppState, agent: &Agent, model: &str, reason: &str) -> Result<(), ApiError> {
    if state.store.open_approval(agent.id, CARD).await?.is_some() {
        return Ok(());
    }
    let name = &agent.name;
    let card = QuestionEvent::approval(
        format!("{name}'s model can't use tools"),
        format!(
            "{reason}. Use {name} without tools? Then it can only chat: no shell, files, \
             messages to other agents, memory or connectors."
        ),
        Approval::UseWithoutTools {
            agent: agent.id,
            model: model.to_owned(),
        },
    )
    .with_labels("Use without tools", "Keep tools")
    .in_turn(state.hub.current_turn(agent.id));
    crate::agents::post_card(state, agent.id, &card).await?;
    Ok(())
}

/// The operator's answer to the card. Approved while the hold stands, the agent loses its tool
/// groups and connectors and its waiting messages run; otherwise nothing changes.
pub(crate) async fn answer_card(
    state: &AppState,
    chat: AgentId,
    qid: i64,
    approval: &Approval,
    answers: Vec<QuestionAnswer>,
) -> Result<QuestionAnswered, ApiError> {
    let Approval::UseWithoutTools { agent, .. } = approval else {
        return Err(ApiError::Internal("not a use-without-tools card".into()));
    };
    // A card left open after a model change must not strip an agent that works now.
    if approves(&answers) && state.hub.tools_unsupported(*agent).is_some() {
        crate::agents::set_profile(state, *agent, Some(Vec::new()), Some(Vec::new())).await?;
        credentials::clear(state, *agent).await;
    }
    let answered = QuestionAnswered {
        question: qid,
        answers,
        message_id: None,
        timed_out: false,
    };
    crate::agents::record_answer(state, chat, answered).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tool_refusals_match() {
        let openrouter = r#"{"error":{"message":"No endpoints found that support tool use. Try disabling \"bash\". To learn more about provider routing, visit: https://openrouter.ai/docs/provider-routing","code":404}}"#;
        let ollama = r#"{"error":{"message":"registry.ollama.ai/library/gemma:2b does not support tools","type":"api_error"}}"#;
        for (status, body, refused) in [
            (404, openrouter, true),
            (400, ollama, true),
            (400, "This model doesn't support tools.", true),
            (
                422,
                "Function calling is not supported for this model",
                true,
            ),
            (400, "tool use is not supported", true),
            (500, ollama, false),
            (401, ollama, false),
            (404, "No endpoints found that support image input", false),
            (404, "No endpoints found matching your data policy", false),
            (404, r#"{"error":{"message":"model 'x' not found"}}"#, false),
            (
                400,
                "tools[0].function.parameters: unsupported keyword $defs",
                false,
            ),
        ] {
            let status = StatusCode::from_u16(status).unwrap();
            assert_eq!(
                refuses_tools(status, body.as_bytes()),
                refused,
                "{status} {body}"
            );
        }
    }

    #[test]
    fn only_a_non_empty_tools_list_counts() {
        for (body, tools) in [
            (r#"{"model":"m","tools":[{"type":"function"}]}"#, true),
            (r#"{"model":"m","tools":[]}"#, false),
            (r#"{"model":"m","tools":null}"#, false),
            (r#"{"model":"m"}"#, false),
            ("not json", false),
        ] {
            assert_eq!(sends_tools(body.as_bytes()), tools, "{body}");
        }
    }
}
