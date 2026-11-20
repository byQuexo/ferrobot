//! The operator's voice call with agents. What an agent says reaches the others with the
//! operator's next message, so agents in a call never start each other's turns.

use std::collections::{HashMap, VecDeque};
use std::sync::{MutexGuard, PoisonError};

use ferrobot_core::{AgentId, Sender};

use crate::api::MAX_TEXT_BYTES;
use crate::{AppState, UiEvent};

/// Each operator message starts a turn for every agent in the call.
pub const MAX_CALL_AGENTS: usize = 4;
/// What one agent said, as passed on to the others (characters).
const MAX_LINE_CHARS: usize = 1500;
/// Lines waiting for one agent; the oldest go first (bytes).
const MAX_HEARD_BYTES: usize = 12 * 1024;
/// Longest operator message in a call, so the passed-on lines still fit.
pub const MAX_CALL_TEXT_BYTES: usize = MAX_TEXT_BYTES - MAX_HEARD_BYTES - 256;

#[derive(Debug, Default)]
pub struct Call {
    agents: Vec<AgentId>,
    /// What the others said since each agent's last operator message.
    heard: HashMap<AgentId, VecDeque<String>>,
}

impl Call {
    fn hear(&mut self, agent: AgentId, text: &str) {
        if !self.agents.contains(&agent) {
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
            while queue.iter().map(String::len).sum::<usize>() > MAX_HEARD_BYTES {
                queue.pop_front();
            }
        }
    }

    fn messages(&mut self, text: &str) -> Vec<(AgentId, String)> {
        let mut out = Vec::new();
        for agent in &self.agents {
            let heard = self.heard.remove(agent).unwrap_or_default();
            let body = if heard.is_empty() {
                text.to_owned()
            } else {
                let heard = Vec::from(heard).join("\n");
                format!("In the call since your last turn:\n{heard}\n\nThe operator: {text}")
            };
            out.push((*agent, body));
        }
        out
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
        }
        let body = format!(
            "{topic}\n\n(<@{host}> invited you into a voice call with the operator. Answer in \
             your reply: it is read aloud to everyone in the call, so keep it short and spoken. \
             Do not use send_message for the call. You hear the others with the operator's next \
             message.)"
        );
        match self
            .store
            .enqueue_message(Sender::Agent(host), guest, body, None)
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
        assert_eq!(out[1].1, "hi");
        assert!(call.heard.is_empty());
    }
}
