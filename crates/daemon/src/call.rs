//! The operator's voice call with agents. With one agent it happens in that agent's chat; once a
//! second agent joins it moves into a room. There every line (the operator's words or an agent's
//! reply) reaches every other member as a turn, and each decides to speak or `(pass)`. At most
//! [`MAX_AGENT_LINES`] agent lines in a row start turns; then the room waits for the operator.
//! Chats and rooms show only the words; the call note and what an agent missed go to its model.

use std::collections::{HashMap, HashSet, VecDeque};

use ferrobot_core::{AgentId, MailMessage, Sender};
use tokio::sync::MutexGuard;

use crate::api::MAX_TEXT_BYTES;
use crate::{AppState, UiEvent};

/// Each operator message starts a turn for every agent in the call.
pub const MAX_CALL_AGENTS: usize = 4;
/// Agent lines in a row that start turns for the others before the room waits for the operator.
const MAX_AGENT_LINES: usize = 4;
/// What an agent replies when it has nothing to add; never a line (nor spoken by the UI).
const PASS: &str = "(pass)";
/// Leads every call message, so agents talk like on the phone.
pub(crate) const CALL_NOTE: &str = "(Voice call: your reply is read aloud. Talk like a person on \
    the phone: one to three short spoken sentences, the solution first, no lists, links or \
    markdown. Everyone in the call hears every line. Speak when you are asked or add something \
    new, otherwise reply exactly (pass); never repeat what was said. Talk to the others in your \
    reply, not with send_message. Longer work: say so in one sentence, do it after the call in \
    your chat. Call hang_up when the conversation is done.)\n\n";
/// Only the agent the operator called gets this.
const HOST_NOTE: &str = "(You host the call. When a task needs a specialist or deeper work, \
    propose it in one sentence and wait for the operator's yes. Then create_agent it and \
    invite_to_call it for feedback, or give it a task to research in the background; its answer \
    comes back to you.)\n\n";
/// What an invited agent hears first, after the host's name.
const INVITED: &str = " invited you into this call. Listen first: say at most a short hello or \
    (pass), then answer what you are asked; use what you know when someone asks, not as an opening.";
/// Model-facing call messages waiting for their turn; more than this forgets the oldest.
const MAX_PROMPTS: usize = 256;
/// What one agent said, as the others hear it (characters).
const MAX_LINE_CHARS: usize = 1500;
/// Room for what others said before the operator's words.
const MAX_HEARD_BYTES: usize = 12 * 1024;
/// Longest operator message in a call, so the lines said before it still fit.
pub const MAX_CALL_TEXT_BYTES: usize =
    MAX_TEXT_BYTES - MAX_HEARD_BYTES - CALL_NOTE.len() - HOST_NOTE.len() - 256;
const HEARD_INTRO: &str = "In the call since your last turn:\n";
/// Room lines waiting for one agent; the oldest go first. The newest always fits.
const MAX_ROOM_HEARD_BYTES: usize =
    MAX_TEXT_BYTES - CALL_NOTE.len() - HOST_NOTE.len() - HEARD_INTRO.len();
const OPERATOR: &str = "The operator: ";

#[derive(Debug, Default)]
pub struct Call {
    agents: Vec<AgentId>,
    /// Set once a second agent joins; the call keeps it until it ends.
    room: Option<i64>,
    /// Room lines each member has not heard yet.
    heard: HashMap<AgentId, VecDeque<String>>,
    /// Members with a room turn queued that has not started; new lines join that turn.
    waiting: HashSet<AgentId>,
    /// Agent lines since the operator last spoke.
    agent_lines: usize,
    /// Agents that hung up; they go when their turn ends, so their goodbye is heard.
    leaving: Vec<AgentId>,
    /// Running room turns: turn id -> (agent, its text blocks so far).
    turns: HashMap<String, (AgentId, Vec<String>)>,
    /// Calls without a room: (recipient, words in the chat, message for its model), oldest first.
    prompts: VecDeque<(AgentId, String, String)>,
}

/// `(pass)`, also as `pass` or with a full stop: models drop the brackets.
fn is_pass(text: &str) -> bool {
    text.trim()
        .trim_end_matches(['.', '!'])
        .trim_start_matches('(')
        .trim_end_matches(')')
        .eq_ignore_ascii_case(PASS.trim_matches(['(', ')']))
}

fn line_of(agent: AgentId, text: &str) -> String {
    let text: String = text.chars().take(MAX_LINE_CHARS).collect();
    format!("<@{agent}>: {text}")
}

impl Call {
    fn note(&self, agent: AgentId) -> String {
        if self.agents.first() == Some(&agent) {
            format!("{CALL_NOTE}{HOST_NOTE}")
        } else {
            CALL_NOTE.to_owned()
        }
    }

    /// Keeps `line` for every member but `from`.
    fn hear(&mut self, from: Option<AgentId>, line: &str) {
        for other in &self.agents {
            if from == Some(*other) {
                continue;
            }
            let queue = self.heard.entry(*other).or_default();
            queue.push_back(line.to_owned());
            // Counted as `room_prompt` joins them.
            while queue.iter().map(|l| l.len() + 1).sum::<usize>() > MAX_ROOM_HEARD_BYTES {
                queue.pop_front();
            }
        }
    }

    /// The operator spoke in the room: everyone gets a turn for it and the agents' count restarts.
    fn operator_said(&mut self, text: &str) {
        self.agent_lines = 0;
        self.hear(None, &format!("{OPERATOR}{text}"));
        self.waiting.extend(self.agents.iter().copied());
    }

    /// `agent` said `text` in the room. Returns who gets a new turn for it: nobody past
    /// [`MAX_AGENT_LINES`], and nobody whose queued turn will hear it anyway.
    fn agent_said(&mut self, agent: AgentId, text: &str) -> Vec<AgentId> {
        self.hear(Some(agent), &line_of(agent, text));
        self.agent_lines += 1;
        if self.agent_lines > MAX_AGENT_LINES {
            return Vec::new();
        }
        let wake: Vec<AgentId> = self
            .agents
            .iter()
            .copied()
            .filter(|a| *a != agent && !self.waiting.contains(a))
            .collect();
        self.waiting.extend(&wake);
        wake
    }

    /// What `agent`'s model gets as a room turn starts: what it has not heard yet. `None`:
    /// nothing new, e.g. an earlier turn heard it all.
    fn room_prompt(&mut self, agent: AgentId) -> Option<String> {
        self.waiting.remove(&agent);
        let heard = self.heard.remove(&agent).filter(|h| !h.is_empty())?;
        let heard = Vec::from(heard).join("\n");
        Some(format!("{}{HEARD_INTRO}{heard}", self.note(agent)))
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
    async fn call(&self) -> MutexGuard<'_, Call> {
        self.call.lock().await
    }

    /// Who is in the call (empty: no call) and its room, once it has one.
    pub async fn call_view(&self) -> (Vec<AgentId>, Option<i64>) {
        let call = self.call().await;
        (call.agents.clone(), call.room)
    }

    /// Replaces who is in the call; empty ends it.
    pub async fn set_call(&self, wanted: Vec<AgentId>) -> Result<(), String> {
        let mut agents = Vec::new();
        for agent in wanted {
            if !agents.contains(&agent) {
                agents.push(agent);
            }
        }
        if agents.len() > MAX_CALL_AGENTS {
            return Err(format!("a call has at most {MAX_CALL_AGENTS} agents"));
        }
        let mut call = self.call().await;
        self.apply(&mut call, agents).await
    }

    /// Puts `agents` in the call (empty ends it) and keeps its room in step: it opens with the
    /// second agent, newcomers become members, and it ends with the call.
    async fn apply(&self, call: &mut Call, agents: Vec<AgentId>) -> Result<(), String> {
        let room = match call.room {
            Some(id) if agents.is_empty() => Some(self.store.end_room(id).await),
            Some(id) => {
                let mut room = None;
                for agent in agents.iter().filter(|a| !call.agents.contains(a)) {
                    room = Some(self.store.add_room_member(id, *agent).await);
                }
                room
            }
            None if agents.len() > 1 => {
                let room = self
                    .store
                    .create_room(agents.clone())
                    .await
                    .map_err(|e| format!("the call room could not be saved: {e}"))?;
                call.room = Some(room.id);
                Some(Ok(room))
            }
            None => None,
        };
        match room {
            Some(Ok(room)) => self.publish(UiEvent::Room { room }),
            Some(Err(e)) => tracing::warn!(error = %e, "call room not updated"),
            None => {}
        }
        if agents.is_empty() {
            *call = Call::default();
        } else {
            call.heard.retain(|a, _| agents.contains(a));
            call.waiting.retain(|a| agents.contains(a));
            call.leaving.retain(|a| agents.contains(a));
            call.agents = agents;
        }
        self.publish(UiEvent::Call {
            agents: call.agents.clone(),
            room_id: call.room,
        });
        Ok(())
    }

    /// The operator's words to everyone in the call, a turn each; `None`: there is no call.
    pub async fn say_in_call(&self, text: &str) -> Option<Vec<MailMessage>> {
        let mut call = self.call().await;
        if call.agents.is_empty() {
            return None;
        }
        let room = call.room;
        if let Some(room) = room {
            self.add_line(room, Sender::User, text).await;
            call.operator_said(text);
        } else {
            for agent in call.agents.clone() {
                let prompt = format!("{}{OPERATOR}{text}", call.note(agent));
                call.remember(agent, text, prompt);
            }
        }
        let mut sent = Vec::new();
        for agent in call.agents.clone() {
            sent.extend(self.queue(Sender::User, agent, text, room).await);
        }
        Some(sent)
    }

    /// `agent` hangs up when its turn ends: the host (the agent the operator called) ends the
    /// call, an invited agent leaves. `Ok(true)` for the host.
    pub async fn hang_up(&self, agent: AgentId) -> Result<bool, String> {
        let mut call = self.call().await;
        if !call.agents.contains(&agent) {
            return Err("you are not in a voice call".into());
        }
        if !call.leaving.contains(&agent) {
            call.leaving.push(agent);
        }
        Ok(call.agents.first() == Some(&agent))
    }

    /// What `agent`'s model gets for `msg` in turn `turn`: a call message with the call note
    /// and what it has not heard yet, anything else as it is. `None`: skip the message (a room
    /// turn with nothing new, or one of a room that ended).
    pub async fn turn_body(&self, agent: AgentId, msg: &MailMessage, turn: &str) -> Option<String> {
        let mut call = self.call().await;
        let Some(room) = msg.room_id else {
            return Some(
                call.take_prompt(agent, &msg.body)
                    .unwrap_or_else(|| msg.body.clone()),
            );
        };
        if call.room != Some(room) {
            return None;
        }
        let prompt = call.room_prompt(agent)?;
        call.turns
            .entry(turn.to_owned())
            .or_insert_with(|| (agent, Vec::new()));
        Some(prompt)
    }

    /// Keeps a text block of room turn `turn` for the line it becomes.
    pub async fn call_text(&self, turn: &str, text: &str) {
        if is_pass(text) || text.trim().is_empty() {
            return;
        }
        if let Some((_, said)) = self.call().await.turns.get_mut(turn) {
            said.push(text.to_owned());
        }
    }

    /// At the end of `agent`'s turn `turn`: a completed room turn's words become a line, which
    /// the others hear in turns of their own; then a hang-up takes effect.
    pub async fn call_turn_done(&self, agent: AgentId, turn: &str, completed: bool) {
        let mut call = self.call().await;
        let said = call.turns.remove(turn).map(|(_, said)| said.join("\n\n"));
        let leaving = call.leaving.contains(&agent);
        let host = call.agents.first() == Some(&agent);
        if let (Some(room), Some(words), true) = (call.room, said, completed)
            && !words.is_empty()
        {
            self.add_line(room, Sender::Agent(agent), &words).await;
            let wake = call.agent_said(agent, &words);
            // The host's goodbye ends the call: nobody needs a turn for it.
            if !(leaving && host) {
                for to in wake {
                    self.queue(Sender::Agent(agent), to, &words, Some(room))
                        .await;
                }
            }
        }
        if leaving {
            let rest = if host {
                Vec::new()
            } else {
                call.agents
                    .iter()
                    .copied()
                    .filter(|a| *a != agent)
                    .collect()
            };
            // Fewer agents than before always fit.
            let _ = self.apply(&mut call, rest).await;
        }
    }

    /// Brings `guest` into the call `host` is in. The call moves into its room (it opens with
    /// the second agent), the topic is a line there, and the guest hears it in its first turn.
    pub async fn invite_to_call(
        &self,
        host: AgentId,
        guest: AgentId,
        topic: &str,
    ) -> Result<(), String> {
        let mut call = self.call().await;
        if !call.agents.contains(&host) {
            return Err("you are not in a voice call with the operator".into());
        }
        if call.agents.contains(&guest) {
            return Err("that agent is already in the call".into());
        }
        if call.agents.len() >= MAX_CALL_AGENTS {
            return Err(format!("a call has at most {MAX_CALL_AGENTS} agents"));
        }
        let mut agents = call.agents.clone();
        agents.push(guest);
        self.apply(&mut call, agents).await?;
        let room = call.room;
        if let Some(room) = room {
            self.add_line(room, Sender::Agent(host), topic).await;
            call.heard
                .entry(guest)
                .or_default()
                .push_back(format!("<@{host}>{INVITED}"));
            call.hear(Some(host), &line_of(host, topic));
            call.waiting.insert(guest);
        }
        if self
            .queue(Sender::Agent(host), guest, topic, room)
            .await
            .is_none()
        {
            let rest = call
                .agents
                .iter()
                .copied()
                .filter(|a| *a != guest)
                .collect();
            let _ = self.apply(&mut call, rest).await;
            return Err("the invite could not be delivered".into());
        }
        Ok(())
    }

    async fn add_line(&self, room: i64, speaker: Sender, body: &str) {
        match self
            .store
            .add_room_line(room, speaker, body.to_owned())
            .await
        {
            Ok(line) => self.publish(UiEvent::RoomLine { line }),
            Err(e) => tracing::warn!(error = %e, "call room line not saved"),
        }
    }

    /// Queues a call message and wakes its recipient; a failure (e.g. deleted mid-call) is logged.
    async fn queue(
        &self,
        sender: Sender,
        to: AgentId,
        body: &str,
        room: Option<i64>,
    ) -> Option<MailMessage> {
        let body = body.to_owned();
        let queued = match room {
            Some(room) => {
                self.store
                    .enqueue_room_message(sender, to, body, room)
                    .await
            }
            None => self.store.enqueue_message(sender, to, body, None).await,
        };
        match queued {
            Ok(message) => {
                self.message_arrived(to);
                self.publish(UiEvent::Message {
                    message: message.clone(),
                });
                Some(message)
            }
            Err(e) => {
                tracing::warn!(agent = %to, error = %e, "call message not queued");
                None
            }
        }
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

    fn room_of(agents: Vec<AgentId>) -> Call {
        Call {
            agents,
            room: Some(1),
            ..Call::default()
        }
    }

    #[test]
    fn lines_are_cut_and_the_oldest_dropped() {
        let (a, b) = (id(1), id(2));
        let mut call = room_of(vec![a, b]);
        for i in 0..40 {
            call.agent_said(b, &format!("{i} {}", "x".repeat(3000)));
        }
        let queue = &call.heard[&a];
        assert!(queue.iter().all(|l| l.len() <= MAX_LINE_CHARS + 41));
        assert!(queue.iter().map(|l| l.len() + 1).sum::<usize>() <= MAX_ROOM_HEARD_BYTES);
        assert!(queue.back().unwrap().contains(": 39 "), "the newest stays");
        assert!(!call.heard.contains_key(&b), "nobody hears themselves");
        let prompt = call.room_prompt(a).unwrap();
        assert!(prompt.starts_with(&format!("{CALL_NOTE}{HOST_NOTE}{HEARD_INTRO}")));
        assert!(prompt.len() <= MAX_TEXT_BYTES, "{}", prompt.len());
        assert_eq!(call.room_prompt(a), None, "heard once");
    }

    #[test]
    fn everyone_hears_each_line_and_the_room_waits_for_the_operator_after_the_cap() {
        let (a, b, c) = (id(1), id(2), id(3));
        let mut call = room_of(vec![a, b, c]);
        call.operator_said("ok");
        assert!(call.room_prompt(a).unwrap().contains(HOST_NOTE), "a hosts");
        assert_eq!(
            call.agent_said(a, "early"),
            [],
            "the others have a turn queued"
        );
        let prompt = call.room_prompt(b).unwrap();
        assert!(prompt.starts_with(CALL_NOTE) && !prompt.contains(HOST_NOTE));
        assert!(
            prompt.ends_with(&format!("{OPERATOR}ok\n<@{a}>: early")),
            "{prompt}"
        );
        call.room_prompt(c);
        for n in 2..=MAX_AGENT_LINES {
            let (from, others) = if n % 2 == 0 { (b, [a, c]) } else { (a, [b, c]) };
            let mut woken = call.agent_said(from, &format!("line {n}"));
            woken.sort();
            assert_eq!(woken, others, "line {n} reaches everyone else");
            for other in others {
                call.room_prompt(other);
            }
        }
        assert_eq!(call.agent_said(c, "one too many"), [], "past the cap");
        assert!(call.waiting.is_empty());
        call.operator_said("go on");
        let prompt = call.room_prompt(a).unwrap();
        assert!(
            prompt.ends_with(&format!("<@{c}>: one too many\n{OPERATOR}go on")),
            "kept for the operator's next words: {prompt}"
        );
        call.room_prompt(b);
        assert_eq!(call.agent_said(b, "again").len(), 1, "the count restarts");
    }

    #[test]
    fn passes_say_nothing() {
        for pass in ["(pass)", "pass", "Pass.", " (PASS)! "] {
            assert!(is_pass(pass), "{pass}");
        }
        assert!(!is_pass("passing on that"));
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
    fn many_tiny_lines_keep_the_room_prompt_within_the_limit() {
        let (a, b) = (id(1), id(2));
        let mut call = room_of(vec![a, b]);
        for _ in 0..3000 {
            call.agent_said(b, "x");
        }
        call.operator_said(&"y".repeat(MAX_CALL_TEXT_BYTES));
        let prompt = call.room_prompt(a).unwrap();
        assert!(prompt.len() <= MAX_TEXT_BYTES, "{}", prompt.len());
        assert!(prompt.ends_with('y'), "the operator's words stay whole");
    }
}
