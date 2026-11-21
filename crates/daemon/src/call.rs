//! The operator's voice call with agents. The operator's words reach every agent in the call;
//! what an agent says to another agent by name reaches it as a new turn, at most
//! [`MAX_AGENT_TURNS`] times in a row, then the rest waits for the operator's next words.
//! Chats show only what was said; the call note and what an agent missed go to its model.

use std::collections::{HashMap, VecDeque};
use std::sync::{MutexGuard, PoisonError};

use ferrobot_core::{AgentId, Sender};

use crate::api::MAX_TEXT_BYTES;
use crate::{AppState, UiEvent};

/// Each operator message starts a turn for every agent in the call.
pub const MAX_CALL_AGENTS: usize = 4;
/// Turns agents start for each other in a row before the call waits for the operator.
const MAX_AGENT_TURNS: usize = 4;
/// What an agent replies when it has nothing to add; never passed on (nor spoken by the UI).
const PASS: &str = "(pass)";
/// Leads every call message, so agents talk like on the phone.
pub(crate) const CALL_NOTE: &str = "(Voice call: your reply is read aloud. Talk like a person on \
    the phone: one to three short spoken sentences, the solution first, no lists, links or \
    markdown. Never repeat what you or others already said. To ask another agent in the call, \
    address it as @name; answer an agent only when it asked you. If something needs research or \
    longer thinking, say so in one sentence and do it after the call in your chat. Reply exactly \
    (pass) when you have nothing to add. Call hang_up when the conversation is done.)\n\n";
/// Model-facing call messages waiting for their turn; more than this forgets the oldest.
const MAX_PROMPTS: usize = 256;
/// What one agent said, as passed on to the others (characters).
const MAX_LINE_CHARS: usize = 1500;
/// Lines waiting for one agent; the oldest go first (bytes).
const MAX_HEARD_BYTES: usize = 12 * 1024;
/// Longest operator message in a call, so the passed-on lines still fit.
pub const MAX_CALL_TEXT_BYTES: usize = MAX_TEXT_BYTES - MAX_HEARD_BYTES - CALL_NOTE.len() - 256;
const HEARD_INTRO: &str = "In the call since your last turn:\n";
const OPERATOR_INTRO: &str = "\n\nThe operator: ";

#[derive(Debug, Default)]
pub struct Call {
    agents: Vec<AgentId>,
    /// What the others said since each agent's last turn.
    heard: HashMap<AgentId, VecDeque<String>>,
    /// Turns agents started for each other since the operator last spoke.
    agent_turns: usize,
    /// Agents that hung up; they go when their turn ends, so their goodbye is heard.
    leaving: Vec<AgentId>,
    /// (host, guest) invited during the host's running turn: the guest has the invite already.
    just_invited: Vec<(AgentId, AgentId)>,
    /// (recipient, words in the chat, message for its model), oldest first, until handed over.
    prompts: VecDeque<(AgentId, String, String)>,
}

fn is_pass(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case(PASS)
}

impl Call {
    fn hear(&mut self, agent: AgentId, text: &str) {
        if !self.agents.contains(&agent) || is_pass(text) {
            return;
        }
        let text: String = text.chars().take(MAX_LINE_CHARS).collect();
        let line = format!("<@{agent}>: {text}");
        for other in &self.agents {
            if *other == agent {
                continue;
            }
            let queue = self.heard.entry(*other).or_default();
            queue.push_back(line.clone());
            // Counted as `messages` assembles them: a newline per line, plus the intros.
            let size = |q: &VecDeque<String>| {
                HEARD_INTRO.len()
                    + OPERATOR_INTRO.len()
                    + q.iter().map(|l| l.len() + 1).sum::<usize>()
            };
            while size(queue) > MAX_HEARD_BYTES {
                queue.pop_front();
            }
        }
    }

    /// The operator's words as each agent's model gets them.
    fn messages(&mut self, text: &str) -> Vec<(AgentId, String)> {
        self.agent_turns = 0;
        let mut out = Vec::new();
        for agent in &self.agents {
            let heard = self.heard.remove(agent).unwrap_or_default();
            let body = if heard.is_empty() {
                format!("{CALL_NOTE}{}", OPERATOR_INTRO.trim_start())
            } else {
                let heard = Vec::from(heard).join("\n");
                format!("{CALL_NOTE}{HEARD_INTRO}{heard}{OPERATOR_INTRO}")
            };
            out.push((*agent, body + text));
        }
        out
    }

    /// After `agent` finished a turn: each other agent it addressed by name gets what it has not
    /// heard yet as a turn, unless agents already took [`MAX_AGENT_TURNS`] in a row. Returns
    /// (agent, words for the chat, message for its model).
    fn passed_on(&mut self, agent: AgentId) -> Vec<(AgentId, String, String)> {
        let invited: Vec<AgentId> = self
            .just_invited
            .iter()
            .filter(|(host, _)| *host == agent)
            .map(|(_, guest)| *guest)
            .collect();
        self.just_invited.retain(|(host, _)| *host != agent);
        if !self.agents.contains(&agent) || self.agent_turns >= MAX_AGENT_TURNS {
            return Vec::new();
        }
        let own = format!("<@{agent}>: ");
        let mut out = Vec::new();
        for other in self.agents.clone() {
            let mention = format!("<@{other}>");
            let addressed = |q: &VecDeque<String>| {
                q.iter()
                    .any(|l| l.strip_prefix(&own).is_some_and(|t| t.contains(&mention)))
            };
            if other == agent
                || invited.contains(&other)
                || !self.heard.get(&other).is_some_and(addressed)
            {
                continue;
            }
            let heard = self.heard.remove(&other).unwrap_or_default();
            let words: Vec<&str> = heard.iter().filter_map(|l| l.strip_prefix(&own)).collect();
            let words = words.join("\n");
            let heard = Vec::from(heard).join("\n");
            out.push((other, words, format!("{CALL_NOTE}{HEARD_INTRO}{heard}")));
        }
        if !out.is_empty() {
            self.agent_turns += 1;
        }
        out
    }

    fn remember(&mut self, agent: AgentId, words: &str, prompt: String) {
        if self.prompts.len() >= MAX_PROMPTS {
            self.prompts.pop_front();
        }
        self.prompts.push_back((agent, words.to_owned(), prompt));
    }

    fn take_prompt(&mut self, agent: AgentId, words: &str) -> Option<String> {
        let at = self
            .prompts
            .iter()
            .position(|(a, w, _)| *a == agent && w == words)?;
        self.prompts.remove(at).map(|(_, _, p)| p)
    }
}

impl AppState {
    fn call(&self) -> MutexGuard<'_, Call> {
        self.call.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Who is in the call; empty when there is none.
    pub fn call_agents(&self) -> Vec<AgentId> {
        self.call().agents.clone()
    }

    /// Replaces who is in the call; empty ends it.
    pub fn set_call(&self, wanted: Vec<AgentId>) -> Result<(), String> {
        let mut agents = Vec::new();
        for agent in wanted {
            if !agents.contains(&agent) {
                agents.push(agent);
            }
        }
        if agents.len() > MAX_CALL_AGENTS {
            return Err(format!("a call has at most {MAX_CALL_AGENTS} agents"));
        }
        {
            let mut call = self.call();
            call.heard.retain(|a, _| agents.contains(a));
            call.leaving.retain(|a| agents.contains(a));
            if agents.is_empty() {
                call.agent_turns = 0;
                call.just_invited.clear();
                call.prompts.clear();
            }
            call.agents.clone_from(&agents);
        }
        self.publish(UiEvent::Call { agents });
        Ok(())
    }

    /// Keeps what `agent` said for the others in the call.
    pub fn heard_in_call(&self, agent: AgentId, text: &str) {
        self.call().hear(agent, text);
    }

    /// The operator's message for each agent in the call, after what that
    /// agent has not heard yet.
    pub fn call_messages(&self, text: &str) -> Vec<(AgentId, String)> {
        self.call().messages(text)
    }

    /// `agent` hangs up when its turn ends: the host (the agent the operator called) ends the
    /// call, an invited agent leaves. `Ok(true)` for the host.
    pub fn hang_up(&self, agent: AgentId) -> Result<bool, String> {
        let mut call = self.call();
        if !call.agents.contains(&agent) {
            return Err("you are not in a voice call".into());
        }
        if !call.leaving.contains(&agent) {
            call.leaving.push(agent);
        }
        Ok(call.agents.first() == Some(&agent))
    }

    /// Keeps what the call message `words` to `agent` says to its model; call before queuing it.
    pub fn remember_call_prompt(&self, agent: AgentId, words: &str, prompt: String) {
        self.call().remember(agent, words, prompt);
    }

    /// The model's version of the message `words` to `agent`, if it is a call message.
    pub fn call_prompt(&self, agent: AgentId, words: &str) -> Option<String> {
        self.call().take_prompt(agent, words)
    }

    /// At the end of `agent`'s turn: applies its hang-up, or (for a completed turn) what the
    /// agents it addressed should hear now as a turn of their own: (agent, words, prompt).
    pub fn call_turn_done(
        &self,
        agent: AgentId,
        completed: bool,
    ) -> Vec<(AgentId, String, String)> {
        let rest = {
            let mut call = self.call();
            if !call.leaving.contains(&agent) {
                return if completed {
                    call.passed_on(agent)
                } else {
                    Vec::new()
                };
            }
            if call.agents.first() == Some(&agent) {
                Vec::new()
            } else {
                call.agents
                    .iter()
                    .copied()
                    .filter(|a| *a != agent)
                    .collect()
            }
        };
        // Fewer agents than before always fit.
        let _ = self.set_call(rest);
        Vec::new()
    }

    /// Brings `guest` into the call `host` is in, with mail from the
    /// host that starts its first turn.
    pub async fn invite_to_call(
        &self,
        host: AgentId,
        guest: AgentId,
        topic: &str,
    ) -> Result<(), String> {
        {
            let mut call = self.call();
            if !call.agents.contains(&host) {
                return Err("you are not in a voice call with the operator".into());
            }
            if call.agents.contains(&guest) {
                return Err("that agent is already in the call".into());
            }
            if call.agents.len() >= MAX_CALL_AGENTS {
                return Err(format!("a call has at most {MAX_CALL_AGENTS} agents"));
            }
            call.agents.push(guest);
            call.just_invited.push((host, guest));
        }
        let prompt = format!(
            "{CALL_NOTE}{topic}\n\n(<@{host}> invited you into this voice call with the operator. \
             Talk to them and the others directly in your reply; do not use send_message for the \
             call. What the others say to you reaches you as new messages.)"
        );
        self.remember_call_prompt(guest, topic, prompt);
        match self
            .store
            .enqueue_message(Sender::Agent(host), guest, topic.to_owned(), None)
            .await
        {
            Ok(message) => {
                self.message_arrived(guest);
                self.publish(UiEvent::Message { message });
            }
            Err(e) => {
                self.call().agents.retain(|a| *a != guest);
                return Err(e.to_string());
            }
        }
        self.publish(UiEvent::Call {
            agents: self.call_agents(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> AgentId {
        format!("00000000-0000-4000-8000-0000000000{n:02}")
            .parse()
            .unwrap()
    }

    #[test]
    fn lines_are_cut_and_the_oldest_dropped() {
        let (a, b) = (id(1), id(2));
        let mut call = Call {
            agents: vec![a, b],
            ..Call::default()
        };
        call.hear(id(3), "not in the call");
        for i in 0..20 {
            call.hear(b, &format!("{i} {}", "x".repeat(3000)));
        }
        let queue = &call.heard[&a];
        assert!(queue.iter().all(|l| l.len() <= MAX_LINE_CHARS + 41));
        assert!(queue.iter().map(String::len).sum::<usize>() <= MAX_HEARD_BYTES);
        assert!(queue.back().unwrap().contains(": 19 "), "the newest stays");
        assert!(!call.heard.contains_key(&b), "nobody hears themselves");
        let out = call.messages("hi");
        assert!(out[0].1.ends_with("\n\nThe operator: hi"));
        assert_eq!(out[1].1, format!("{CALL_NOTE}The operator: hi"));
        assert!(call.heard.is_empty());
    }

    #[test]
    fn agents_answer_each_other_a_few_times_then_wait_for_the_operator() {
        let (a, b) = (id(1), id(2));
        let mut call = Call {
            agents: vec![a, b],
            ..Call::default()
        };
        call.hear(a, PASS);
        assert!(call.passed_on(a).is_empty(), "a pass is not passed on");
        call.hear(a, "Nice weather.");
        assert!(call.passed_on(a).is_empty(), "nobody was addressed");
        call.messages("ok");
        for turn in 0..MAX_AGENT_TURNS {
            let (from, to) = if turn % 2 == 0 { (a, b) } else { (b, a) };
            let said = format!("line {turn}, <@{to}>?");
            call.hear(from, &said);
            let out = call.passed_on(from);
            assert_eq!(out.len(), 1);
            let (who, words, prompt) = &out[0];
            assert_eq!(
                (*who, words.as_str()),
                (to, said.as_str()),
                "the chat shows the words"
            );
            assert!(prompt.starts_with(CALL_NOTE), "the model gets the note");
            assert!(prompt.ends_with(&format!("<@{from}>: {said}")));
        }
        call.hear(a, &format!("one too many, <@{b}>"));
        assert!(
            call.passed_on(a).is_empty(),
            "the call waits for the operator"
        );
        let out = call.messages("go on");
        assert!(
            out[1].1.contains("one too many"),
            "kept for the operator's next words"
        );
        call.hear(b, &format!("again, <@{a}>"));
        assert_eq!(
            call.passed_on(b).len(),
            1,
            "the operator's words reset the count"
        );

        call.just_invited.push((a, b));
        call.hear(a, &format!("<@{b}> joins us now."));
        assert!(
            call.passed_on(a).is_empty(),
            "the guest has the invite already"
        );
        assert_eq!(call.just_invited, []);
    }

    #[test]
    fn prompts_wait_for_their_message_once() {
        let (a, b) = (id(1), id(2));
        let mut call = Call::default();
        call.remember(a, "hi", "note + hi".into());
        call.remember(a, "hi", "note + hi again".into());
        assert_eq!(call.take_prompt(b, "hi"), None);
        assert_eq!(call.take_prompt(a, "hi").as_deref(), Some("note + hi"));
        assert_eq!(
            call.take_prompt(a, "hi").as_deref(),
            Some("note + hi again")
        );
        assert_eq!(call.take_prompt(a, "hi"), None);
    }

    #[test]
    fn many_tiny_lines_keep_the_call_body_within_the_limit() {
        let (a, b) = (id(1), id(2));
        let mut call = Call {
            agents: vec![a, b],
            ..Call::default()
        };
        for _ in 0..1000 {
            call.hear(b, "x");
        }
        let out = call.messages(&"y".repeat(MAX_CALL_TEXT_BYTES));
        assert!(out[0].1.len() <= MAX_TEXT_BYTES, "{}", out[0].1.len());
    }
}
