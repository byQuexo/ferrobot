//! Question cards an agent shows the operator (`ask_user`); the answer returns as a user message.
//! Maps 1:1 onto a subset of A2UI v1.0 (see `protocol/README.md`).

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::{AgentId, ConnectorAuth, ToolGroup};

/// Version of the question card format (`QuestionEvent.version`).
pub const QUESTION_CARD_VERSION: u32 = 1;
/// Upper bounds that keep a card readable.
pub const MAX_QUESTIONS: usize = 8;
pub const MAX_CHOICES: usize = 12;
const MAX_TEXT: usize = 500;
const MAX_ID: usize = 64;

/// How a question is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Pick exactly one choice (A2UI `ChoicePicker`, `mutuallyExclusive`).
    Single,
    /// Pick any number of choices (A2UI `ChoicePicker`, `multipleSelection`).
    Multi,
    /// Free text only (A2UI `TextField`).
    Text,
    /// Hidden text the agent never sees (A2UI `TextField`, `obscured`). Only on cards the
    /// daemon raises; `ask_user` refuses it.
    #[cfg_attr(feature = "mcp", schemars(skip))]
    Secret,
}

/// One selectable option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct Choice {
    /// Shown to the user.
    pub label: String,
    /// Stable value reported in the answer; defaults to the label.
    #[serde(default)]
    pub value: String,
}

/// One question on a card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct Question {
    /// Unique within the card; defaults to `q1`, `q2`, ...
    #[serde(default)]
    pub id: String,
    /// The question text.
    pub prompt: String,
    pub kind: QuestionKind,
    /// Options for `single` and `multi` (2..12). Empty for `text`.
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// `single`/`multi`: also offer a free-text "other" answer.
    #[serde(default)]
    pub allow_custom: bool,
}

/// A card with one or more questions, answered together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct QuestionCard {
    /// Optional heading.
    #[serde(default)]
    pub title: Option<String>,
    /// 1..8 questions, shown in order.
    pub questions: Vec<Question>,
}

/// The answer to one question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct QuestionAnswer {
    pub question_id: String,
    /// Selected choice values (`single`: at most one).
    #[serde(default)]
    pub values: Vec<String>,
    /// Free text (`text` questions, or "other" when `allow_custom`).
    #[serde(default)]
    pub custom: Option<String>,
}

/// What an approval card asks the operator to allow; the decision goes to the waiting tool
/// call, not to the agent's mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Approval {
    /// An agent wants to create another agent (`create_agent` tool).
    CreateAgent {
        /// Requested name of the new agent.
        name: String,
        /// Requested one-line description (role).
        description: String,
    },
    /// An agent wants to add a connector (`add_connector` tool). Approving creates it and gives
    /// it to the agent; a token, if it takes one, goes in under Settings, never on a card.
    AddConnector {
        name: String,
        url: String,
        /// A local server's program and arguments (it runs on the daemon's computer).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_env: Option<String>,
        auth: ConnectorAuth,
        /// The agent that asked: it gets the connector and a message with the outcome.
        requester: AgentId,
    },
    /// An agent wants a CLI in its computer (`propose_cli` tool). Approving adds it for that
    /// agent; a token, if it needs one, goes in under Settings, never on a card.
    AddCli { cli: crate::Cli, requester: AgentId },
    /// An agent offers the operator a slash command (`add_command` tool). Its prompt is later
    /// sent in the operator's name, so Auto mode never skips this card.
    AddCommand {
        name: String,
        description: String,
        prompt: String,
        as_task: bool,
        requester: AgentId,
    },
    /// An agent wants a routine of its own (`create_routine` tool): `prompt` reaches it on
    /// `schedule` (cron, the daemon's time zone).
    CreateRoutine {
        name: String,
        schedule: String,
        prompt: String,
        requester: AgentId,
    },
    /// An agent wants to give an agent it created more tools (`set_agent_tools` tool). The new
    /// sets replace the target's; `null` leaves that set as it is.
    SetAgentTools {
        agent: AgentId,
        tools: Option<Vec<ToolGroup>>,
        /// Connector ids.
        connectors: Option<Vec<uuid::Uuid>>,
        requester: AgentId,
    },
    /// The agent's model refused a request with tools (raised by the daemon). Approving turns
    /// off its tool groups and connectors so its waiting messages run as plain chat.
    UseWithoutTools { agent: AgentId, model: String },
    /// The tool-call classifier flagged a call as risky and Auto mode is off: the call waits
    /// for the operator.
    ToolCall {
        agent: AgentId,
        /// `shell` or the connector tool's name.
        tool: String,
        /// The command or the call's arguments, shortened.
        detail: String,
        /// What made the call risky, e.g. "pushes code".
        reason: String,
        /// A very risky step (money, deleting in a connected service, force-pushing,
        /// publishing): Auto mode still asks, and only the app answers it.
        #[serde(default)]
        critical: bool,
    },
}

/// Choice value that approves an approval card; anything else declines.
pub const APPROVE: &str = "approve";
/// Choice value that declines an approval card.
pub const DECLINE: &str = "decline";

/// Payload of a `question` event: the card as shown in the chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct QuestionEvent {
    /// Card format version (currently 1).
    pub version: u32,
    pub card: QuestionCard,
    /// Set on approval cards raised by the daemon (e.g. for `create_agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Approval>,
    /// The turn that raised the card (its trace shows the wait); `null` outside a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

impl QuestionEvent {
    /// A one-question Approve/Decline card.
    pub fn approval(title: String, prompt: String, approval: Approval) -> Self {
        Self {
            version: QUESTION_CARD_VERSION,
            card: QuestionCard {
                title: Some(title),
                questions: vec![Question {
                    id: "q1".into(),
                    prompt,
                    kind: QuestionKind::Single,
                    choices: vec![
                        Choice {
                            label: "Approve".into(),
                            value: APPROVE.into(),
                        },
                        Choice {
                            label: "Decline".into(),
                            value: DECLINE.into(),
                        },
                    ],
                    allow_custom: false,
                }],
            },
            approval: Some(approval),
            turn_id: None,
        }
    }

    /// Renames an approval card's Approve and Decline choices; their values stay.
    #[must_use]
    pub fn with_labels(mut self, approve: &str, decline: &str) -> Self {
        for c in self.card.questions.iter_mut().flat_map(|q| &mut q.choices) {
            c.label = if c.value == APPROVE { approve } else { decline }.into();
        }
        self
    }

    /// Marks the card as raised by `turn`.
    #[must_use]
    pub fn in_turn(mut self, turn: Option<String>) -> Self {
        self.turn_id = turn;
        self
    }
}

/// Whether the answers to an approval card approve it.
pub fn approves(answers: &[QuestionAnswer]) -> bool {
    answers
        .first()
        .is_some_and(|a| a.values.iter().any(|v| v == APPROVE))
}

/// Payload of a `question_answered` event. A card is answered at most once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct QuestionAnswered {
    /// Id of the `question` event.
    pub question: i64,
    pub answers: Vec<QuestionAnswer>,
    /// The user message that carried the answer to the agent; `null` for
    /// approval cards (the decision goes to the waiting tool call).
    pub message_id: Option<i64>,
    /// No answer came in time: the daemon declined the approval card.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    /// The agent that answered: during the operator's task, its agent decides the risky steps
    /// of the agents working for it. Absent: the operator (or the timeout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<AgentId>,
    /// How risky that agent judged the step before deciding it. Absent when the operator
    /// answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<RiskAssessment>,
}

/// How risky a step is for the task at hand, in the judgment of the agent deciding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub enum Risk {
    /// Reads or changes nothing that matters, or only what the task is about.
    Low,
    /// Could cause harm or cost, but the task needs it and it can be undone.
    Medium,
    /// Could cause lasting harm, leak data or cost money: only the operator approves it.
    High,
}

/// An agent's rating of a teammate's risky step, given before it decides the card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct RiskAssessment {
    pub risk: Risk,
    /// Why, in a sentence.
    pub reason: String,
}

fn check_text(what: &str, s: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if s.len() > max {
        return Err(format!("{what} exceeds {max} bytes"));
    }
    Ok(())
}

impl QuestionCard {
    /// Checks limits and fills defaults (question ids, choice values).
    pub fn normalize(&mut self) -> Result<(), String> {
        if let Some(t) = &self.title {
            if t.trim().is_empty() {
                self.title = None;
            } else {
                check_text("title", t, MAX_TEXT)?;
            }
        }
        if self.questions.is_empty() || self.questions.len() > MAX_QUESTIONS {
            return Err(format!("a card needs 1..{MAX_QUESTIONS} questions"));
        }
        let mut ids = Vec::new();
        for (i, q) in self.questions.iter_mut().enumerate() {
            if q.id.trim().is_empty() {
                q.id = format!("q{}", i + 1);
            }
            check_text("question id", &q.id, MAX_ID)?;
            if ids.contains(&q.id) {
                return Err(format!("duplicate question id {:?}", q.id));
            }
            ids.push(q.id.clone());
            check_text("prompt", &q.prompt, MAX_TEXT)?;
            match q.kind {
                QuestionKind::Secret => {
                    return Err(format!(
                        "{}: secret fields are only on cards ferrobot shows",
                        q.id
                    ));
                }
                QuestionKind::Text => {
                    if !q.choices.is_empty() {
                        return Err(format!("{}: text questions take no choices", q.id));
                    }
                }
                QuestionKind::Single | QuestionKind::Multi => {
                    if q.choices.len() < 2 || q.choices.len() > MAX_CHOICES {
                        return Err(format!("{}: needs 2..{MAX_CHOICES} choices", q.id));
                    }
                    let mut values: Vec<&str> = Vec::new();
                    for c in &mut q.choices {
                        check_text("choice label", &c.label, MAX_TEXT)?;
                        if c.value.trim().is_empty() {
                            c.value.clone_from(&c.label);
                        }
                    }
                    for c in &q.choices {
                        if values.contains(&c.value.as_str()) {
                            return Err(format!("{}: duplicate choice {:?}", q.id, c.value));
                        }
                        values.push(&c.value);
                    }
                }
            }
        }
        Ok(())
    }

    /// Validates a complete set of answers (one per question, in any order).
    pub fn check_answers(&self, answers: &[QuestionAnswer]) -> Result<(), String> {
        if answers.len() != self.questions.len() {
            return Err("answer every question exactly once".into());
        }
        for q in &self.questions {
            let mut mine = answers.iter().filter(|a| a.question_id == q.id);
            let (Some(a), None) = (mine.next(), mine.next()) else {
                return Err(format!("answer {:?} exactly once", q.id));
            };
            let custom = a.custom.as_deref().map(str::trim).filter(|c| !c.is_empty());
            // Optional here: whoever raised the card decides when a secret is needed.
            if q.kind == QuestionKind::Secret {
                if !a.values.is_empty() {
                    return Err(format!("{}: takes no choices", q.id));
                }
                continue;
            }
            if let Some(c) = custom {
                check_text("answer", c, MAX_ANSWER)?;
            }
            for v in &a.values {
                if !q.choices.iter().any(|c| &c.value == v) {
                    return Err(format!("{}: unknown choice {v:?}", q.id));
                }
            }
            let picked = a.values.len();
            match q.kind {
                QuestionKind::Text if picked > 0 || custom.is_none() => {
                    return Err(format!("{}: needs a text answer", q.id));
                }
                QuestionKind::Single if picked + usize::from(custom.is_some()) != 1 => {
                    return Err(format!("{}: pick exactly one answer", q.id));
                }
                QuestionKind::Multi if picked == 0 && custom.is_none() => {
                    return Err(format!("{}: pick at least one answer", q.id));
                }
                _ => {}
            }
            if custom.is_some() && q.kind != QuestionKind::Text && !q.allow_custom {
                return Err(format!("{}: free text is not allowed", q.id));
            }
        }
        Ok(())
    }

    /// The answer as the message text the agent receives.
    pub fn answer_text(&self, answers: &[QuestionAnswer]) -> String {
        let mut out = match &self.title {
            Some(t) => format!("Answers to \"{t}\":"),
            None => "Answers to your questions:".to_owned(),
        };
        for q in &self.questions {
            let Some(a) = answers
                .iter()
                .find(|a| a.question_id == q.id && q.kind != QuestionKind::Secret)
            else {
                continue;
            };
            let mut parts: Vec<String> = a
                .values
                .iter()
                .map(|v| {
                    q.choices
                        .iter()
                        .find(|c| &c.value == v)
                        .map_or_else(|| v.clone(), |c| c.label.clone())
                })
                .collect();
            if let Some(c) = a.custom.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                parts.push(c.to_owned());
            }
            let _ = write!(out, "\n- {} {}", q.prompt.trim(), parts.join(", "));
        }
        out
    }
}

/// Takes the typed secret out of `answers`, so what is stored or broadcast holds none.
pub fn take_secret(card: &QuestionCard, answers: &mut [QuestionAnswer]) -> Option<String> {
    let mut secret = None;
    for a in answers {
        let hidden = card
            .questions
            .iter()
            .any(|q| q.id == a.question_id && q.kind == QuestionKind::Secret);
        if let (true, Some(value)) = (hidden, a.custom.take()) {
            let value = value.trim();
            if secret.is_none() && !value.is_empty() {
                secret = Some(value.to_owned());
            }
        }
    }
    secret
}

/// Upper bound for one free-text answer (bytes).
pub const MAX_ANSWER: usize = 4000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_keeps_the_deciding_agents_rating() {
        // Answers recorded before ratings existed still read.
        let old: QuestionAnswered = serde_json::from_value(serde_json::json!({
            "question": 3, "answers": [], "message_id": null
        }))
        .unwrap();
        assert_eq!(old.assessment, None);
        let rated = QuestionAnswered {
            assessment: Some(RiskAssessment {
                risk: Risk::Medium,
                reason: "the task needs it".into(),
            }),
            ..old.clone()
        };
        let json = serde_json::to_value(&rated).unwrap();
        assert_eq!(json["assessment"]["risk"], "medium");
        assert!(
            serde_json::to_value(&old)
                .unwrap()
                .get("assessment")
                .is_none()
        );
    }

    fn card() -> QuestionCard {
        serde_json::from_value(serde_json::json!({
            "title": "Release",
            "questions": [
                {"prompt": "Which day?", "kind": "single",
                 "choices": [{"label": "Thursday"}, {"label": "Friday"}], "allow_custom": true},
                {"prompt": "Channels?", "kind": "multi",
                 "choices": [{"label": "Email", "value": "email"}, {"label": "Chat", "value": "chat"}]},
                {"prompt": "Anything else?", "kind": "text"}
            ]
        }))
        .unwrap()
    }

    fn ans(id: &str, values: &[&str], custom: Option<&str>) -> QuestionAnswer {
        QuestionAnswer {
            question_id: id.into(),
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            custom: custom.map(Into::into),
        }
    }

    #[test]
    fn normalizes_and_formats() {
        let mut c = card();
        c.normalize().unwrap();
        assert_eq!(c.questions[2].id, "q3");
        assert_eq!(c.questions[0].choices[1].value, "Friday");
        let answers = [
            ans("q1", &[], Some("Monday")),
            ans("q2", &["email", "chat"], None),
            ans("q3", &[], Some("keep it short")),
        ];
        c.check_answers(&answers).unwrap();
        assert_eq!(
            c.answer_text(&answers),
            "Answers to \"Release\":\n- Which day? Monday\n- Channels? Email, Chat\n- Anything else? keep it short"
        );
    }

    #[test]
    fn rejects_bad_cards_and_answers() {
        let mut c = card();
        c.questions[1].choices.truncate(1);
        assert!(c.normalize().is_err());
        let mut c = card();
        c.normalize().unwrap();
        let two = [
            ans("q1", &["Thursday", "Friday"], None),
            ans("q2", &["email"], None),
            ans("q3", &[], Some("x")),
        ];
        assert!(c.check_answers(&two).is_err());
        let custom = [
            ans("q1", &["Thursday"], None),
            ans("q2", &[], Some("fax")),
            ans("q3", &[], Some("x")),
        ];
        assert!(c.check_answers(&custom).is_err());
        assert!(c.check_answers(&[ans("q1", &["Thursday"], None)]).is_err());
    }

    #[test]
    fn secret_fields_stay_out_of_agent_cards_and_answers() {
        let mut c = card();
        c.questions[2].kind = QuestionKind::Secret;
        assert!(c.normalize().unwrap_err().contains("secret"));

        let mut event = QuestionEvent::approval(
            "Add a key?".into(),
            "p".into(),
            Approval::AddConnector {
                name: "github".into(),
                url: "https://api.example.com/mcp".into(),
                command: None,
                token_env: None,
                auth: ConnectorAuth::Token,
                requester: AgentId::new(),
            },
        );
        event.card.questions.push(Question {
            id: "token".into(),
            prompt: "Token".into(),
            kind: QuestionKind::Secret,
            choices: vec![],
            allow_custom: false,
        });
        let card = &event.card;
        let declined = [ans("q1", &[DECLINE], None), ans("token", &[], None)];
        card.check_answers(&declined).unwrap();
        let mut approved = [
            ans("q1", &[APPROVE], None),
            ans("token", &[], Some(" sk-1 ")),
        ];
        card.check_answers(&approved).unwrap();
        assert!(!card.answer_text(&approved).contains("sk-1"));
        assert_eq!(take_secret(card, &mut approved).as_deref(), Some("sk-1"));
        assert_eq!(approved[1].custom, None);
        assert_eq!(approved[0].values, [APPROVE]);
    }
}
