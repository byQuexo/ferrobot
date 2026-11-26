//! Cards in a bridged channel: an approval takes a ✅ or ❌ reaction, a single question a
//! reply. Only allowed users answer, through the same route as the app. Cards with a secret
//! field or several questions, an `add_connector` approval and very risky steps stay in the app.

use std::fmt::Write as _;

use axum::Json;
use axum::extract::{Path, State};
use ferrobot_core::AgentId;
use ferrobot_core::question::{
    APPROVE, Approval, DECLINE, Question, QuestionAnswer, QuestionEvent, QuestionKind,
};
use ferrobot_store::StoredEvent;
use reqwest::Method;
use serde_json::Value;

use super::{MAX_CHARS, SECRET, channels_of, content, get, lock, post, react, request};
use crate::AppState;
use crate::api::chat::{AnswerQuestion, answer_question};
use crate::error::ApiError;

const APPROVED: &str = "✅";
const DECLINED: &str = "❌";
const GONE: &str = "No longer waiting: answered in ferrobot or timed out.";

/// A card posted to a channel and still waiting there.
#[derive(Debug, Clone)]
pub(super) struct Posted {
    agent: AgentId,
    qid: i64,
    channel: String,
    message: String,
    /// What was posted, without the line on how to answer.
    text: String,
    question: Question,
    approval: bool,
}

/// A new card goes to the agent's first bridged channel while the bridge is on.
pub(crate) fn question(state: &AppState, event: &StoredEvent) {
    let Ok(card) = serde_json::from_value::<QuestionEvent>(event.payload.clone()) else {
        return;
    };
    let (state, agent, qid) = (state.clone(), event.agent_id, event.id);
    tokio::spawn(async move {
        if let Err(e) = post_card(&state, agent, qid, &card).await {
            tracing::warn!(error = %e, %agent, qid, "discord bridge could not post a card");
        }
    });
}

async fn post_card(
    state: &AppState,
    agent: AgentId,
    qid: i64,
    card: &QuestionEvent,
) -> Result<(), String> {
    let Some(channel) = channels_of(state, agent).await.into_iter().next() else {
        return Ok(());
    };
    let Ok(Some(token)) = state.settings.secrets.get(SECRET).await else {
        return Ok(());
    };
    let name = state
        .store
        .get_agent(agent)
        .await
        .map_err(|e| e.to_string())?
        .name;
    let text = describe(name.as_str(), card);
    let answerable = in_discord(card);
    let how = answerable
        .as_ref()
        .map_or("Answer it in ferrobot.".to_owned(), how_to);
    let message = send(state, &token, &channel, &format!("{text}\n\n{how}")).await?;
    let Some(question) = answerable else {
        return Ok(());
    };
    let approval = card.approval.is_some();
    if approval {
        for emoji in [APPROVED, DECLINED] {
            react(state, &token, (&channel, &message), emoji, true).await?;
        }
    }
    let posted = Posted {
        agent,
        qid,
        channel,
        message,
        text,
        question,
        approval,
    };
    cards(state).push(posted);
    Ok(())
}

/// The question Discord may answer, or `None` when the card stays in the app.
fn in_discord(card: &QuestionEvent) -> Option<Question> {
    if matches!(
        card.approval,
        Some(Approval::AddConnector { .. } | Approval::ToolCall { critical: true, .. })
    ) {
        return None;
    }
    let [question] = card.card.questions.as_slice() else {
        return None;
    };
    (question.kind != QuestionKind::Secret).then(|| question.clone())
}

fn describe(agent: &str, card: &QuestionEvent) -> String {
    let mut text = if card.approval.is_some() {
        format!("🛂 **{agent}** asks for your approval")
    } else {
        format!("❓ **{agent}** asks you")
    };
    if let Some(title) = &card.card.title {
        let _ = write!(text, "\n**{title}**");
    }
    for q in &card.card.questions {
        let _ = write!(text, "\n{}", q.prompt);
        if card.approval.is_none() {
            for (n, choice) in q.choices.iter().enumerate() {
                let _ = write!(text, "\n{}. {}", n + 1, choice.label);
            }
        }
    }
    // Room for the answer line and the outcome added later.
    text.chars().take(MAX_CHARS - 200).collect()
}

fn how_to(q: &Question) -> String {
    match q.kind {
        _ if q.choices.iter().any(|c| c.value == APPROVE) => {
            format!("React {APPROVED} to approve or {DECLINED} to decline.")
        }
        QuestionKind::Text | QuestionKind::Secret => {
            "Reply to this message with your answer.".into()
        }
        QuestionKind::Single | QuestionKind::Multi => {
            let numbers = if q.kind == QuestionKind::Multi {
                "numbers, separated by commas"
            } else {
                "a number"
            };
            let own = if q.allow_custom {
                ", or your own answer"
            } else {
                ""
            };
            format!("Reply to this message with {numbers}{own}.")
        }
    }
}

/// The waiting question card that message `m` replies to.
pub(super) fn replied(state: &AppState, m: &Value) -> Option<Posted> {
    let to = m["message_reference"]["message_id"].as_str()?;
    cards(state)
        .iter()
        .find(|c| c.message == to && !c.approval)
        .cloned()
}

/// An allowed user's reply to a question card answers it.
pub(super) async fn answer_reply(
    state: &AppState,
    token: &str,
    card: Posted,
    text: &str,
    who: &str,
) {
    if let Some((values, custom)) = parse_reply(&card.question, text) {
        finish(state, token, &card, values, custom, who).await;
    } else {
        let hint = format!(
            "That matches no option; reply with a number from 1 to {}.",
            card.question.choices.len()
        );
        post(state, token, &card.channel, Some(&card.message), &hint).await;
    }
}

fn parse_reply(q: &Question, text: &str) -> Option<(Vec<String>, Option<String>)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if q.kind == QuestionKind::Text {
        return Some((Vec::new(), Some(text.to_owned())));
    }
    let picks: Vec<&str> = if q.kind == QuestionKind::Multi {
        text.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect()
    } else {
        vec![text]
    };
    match picks.iter().map(|p| choice(q, p)).collect() {
        Some(values) => Some((values, None)),
        None if q.allow_custom => Some((Vec::new(), Some(text.to_owned()))),
        None => None,
    }
}

fn choice(q: &Question, pick: &str) -> Option<String> {
    let c = pick
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| q.choices.get(i))
        .or_else(|| {
            q.choices
                .iter()
                .find(|c| c.label.eq_ignore_ascii_case(pick))
        })?;
    Some(if c.value.is_empty() {
        c.label.clone()
    } else {
        c.value.clone()
    })
}

/// Each round: an approval an allowed user reacted to is answered (❌ wins over ✅), and a
/// card answered elsewhere or timed out says so and is dropped.
pub(super) async fn check(state: &AppState, token: &str, users: &[String], channel: &str) {
    let waiting: Vec<Posted> = cards(state)
        .iter()
        .filter(|c| c.channel == channel)
        .cloned()
        .collect();
    for card in waiting {
        if state
            .store
            .question_answered(card.agent, card.qid)
            .await
            .unwrap_or(false)
        {
            close(state, token, &card, GONE).await;
            continue;
        }
        if !card.approval {
            continue;
        }
        for (emoji, value) in [(DECLINED, DECLINE), (APPROVED, APPROVE)] {
            if let Some(who) = reacted(state, token, &card, emoji, users).await {
                finish(state, token, &card, vec![value.to_owned()], None, &who).await;
                break;
            }
        }
    }
}

async fn reacted(
    state: &AppState,
    token: &str,
    card: &Posted,
    emoji: &str,
    users: &[String],
) -> Option<String> {
    let path = format!(
        "/channels/{}/messages/{}/reactions/{emoji}",
        card.channel, card.message
    );
    let list = get(state, token, &path).await.ok()?;
    let user = list.as_array()?.iter().find(|u| {
        u["id"]
            .as_str()
            .is_some_and(|id| users.iter().any(|x| x == id))
    })?;
    Some(user["username"].as_str().unwrap_or("someone").to_owned())
}

async fn finish(
    state: &AppState,
    token: &str,
    card: &Posted,
    values: Vec<String>,
    custom: Option<String>,
    who: &str,
) {
    let outcome = if card.approval {
        if values.iter().any(|v| v == APPROVE) {
            format!("{APPROVED} Approved by {who}.")
        } else {
            format!("{DECLINED} Declined by {who}.")
        }
    } else {
        let labels: Vec<&str> = values
            .iter()
            .map(|v| {
                card.question
                    .choices
                    .iter()
                    .find(|c| &c.value == v)
                    .map_or(v.as_str(), |c| c.label.as_str())
            })
            .collect();
        let said = custom.clone().unwrap_or_else(|| labels.join(", "));
        format!("{APPROVED} {who} answered: {said}")
    };
    let answers = vec![QuestionAnswer {
        question_id: card.question.id.clone(),
        values,
        custom,
    }];
    let path = Path((card.agent.to_string(), card.qid));
    match answer_question(State(state.clone()), path, Json(AnswerQuestion { answers })).await {
        Ok(_) => close(state, token, card, &outcome).await,
        Err(ApiError::Conflict(_)) => close(state, token, card, GONE).await,
        Err(ApiError::BadRequest(e)) => {
            post(state, token, &card.channel, Some(&card.message), &e).await;
        }
        Err(e) => {
            tracing::warn!(error = %e, qid = card.qid, "discord bridge could not answer a card");
        }
    }
}

/// Drops the card and writes the outcome under it in Discord.
async fn close(state: &AppState, token: &str, card: &Posted, outcome: &str) {
    cards(state).retain(|c| c.message != card.message);
    let path = format!("/channels/{}/messages/{}", card.channel, card.message);
    let text = format!("{}\n\n{outcome}", card.text);
    let sent = request(state, Method::PATCH, token, &path)
        .json(&content(&text))
        .send()
        .await;
    if let Err(e) = sent {
        tracing::warn!(error = %e.without_url(), "discord bridge could not update a card");
    }
}

/// Posts `text` and returns the new message's id.
async fn send(state: &AppState, token: &str, channel: &str, text: &str) -> Result<String, String> {
    let path = format!("/channels/{channel}/messages");
    let res = request(state, Method::POST, token, &path)
        .json(&content(text))
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if !res.status().is_success() {
        return Err(format!("posting a card: Discord answered {}", res.status()));
    }
    let body: Value = res
        .json()
        .await
        .map_err(|_| "Discord sent no JSON".to_owned())?;
    body["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "Discord sent no message id".to_owned())
}

fn cards(state: &AppState) -> std::sync::MutexGuard<'_, Vec<Posted>> {
    lock(&state.discord.cards)
}

#[cfg(test)]
mod tests {
    use ferrobot_core::question::{Choice, QuestionCard};

    use super::*;

    fn single(allow_custom: bool) -> Question {
        let choices = ["Monday", "Friday"]
            .map(|l| Choice {
                label: l.into(),
                value: l.to_lowercase(),
            })
            .to_vec();
        Question {
            id: "q1".into(),
            prompt: "Which day?".into(),
            kind: QuestionKind::Single,
            choices,
            allow_custom,
        }
    }

    #[test]
    fn replies_pick_by_number_or_label() {
        let q = single(false);
        assert_eq!(parse_reply(&q, " 2 "), Some((vec!["friday".into()], None)));
        assert_eq!(
            parse_reply(&q, "monday"),
            Some((vec!["monday".into()], None))
        );
        assert_eq!(parse_reply(&q, "3"), None);
        assert_eq!(
            parse_reply(&single(true), "Sunday"),
            Some((vec![], Some("Sunday".into())))
        );
        let multi = Question {
            kind: QuestionKind::Multi,
            ..single(false)
        };
        assert_eq!(
            parse_reply(&multi, "1, Friday"),
            Some((vec!["monday".into(), "friday".into()], None))
        );
    }

    #[test]
    fn some_cards_stay_in_the_app() {
        let card = |approval: Option<Approval>, questions: Vec<Question>| QuestionEvent {
            version: 1,
            card: QuestionCard {
                title: None,
                questions,
            },
            approval,
            turn_id: None,
        };
        assert!(in_discord(&card(None, vec![single(false)])).is_some());
        assert!(in_discord(&card(None, vec![single(false), single(false)])).is_none());
        let secret = Question {
            kind: QuestionKind::Secret,
            choices: vec![],
            ..single(false)
        };
        assert!(in_discord(&card(None, vec![secret])).is_none());
        let call = |critical| Approval::ToolCall {
            agent: AgentId::new(),
            tool: "shell".into(),
            detail: "git push".into(),
            reason: "pushes code".into(),
            critical,
        };
        assert!(in_discord(&card(Some(call(false)), vec![single(false)])).is_some());
        assert!(in_discord(&card(Some(call(true)), vec![single(false)])).is_none());
    }
}
