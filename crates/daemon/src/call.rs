//! The operator's voice call with agents. With one agent it happens in that agent's chat; once a
//! second agent joins it moves into a room. There every line (the operator's words or an agent's
//! reply) is due for every other member, and they take it one at a time: the floor goes host
//! first, then in join order, and each speaks or `(pass)`es having heard the one before. At most
//! [`MAX_AGENT_LINES`] agent lines in a row make others due; then the room waits for the operator.
//! Chats and rooms show only the words; the call note and what an agent missed go to its model.

use std::collections::{HashMap, VecDeque};

use ferrobot_core::protocol::TurnOutcome;
use ferrobot_core::{AgentId, MailMessage, Sender};
use tokio::sync::MutexGuard;

use crate::api::MAX_TEXT_BYTES;
use crate::{AppState, UiEvent};

/// Each operator message starts a turn for every agent in the call.
pub const MAX_CALL_AGENTS: usize = 4;
/// Agent lines in a row that make the others due before the room waits for the operator.
const MAX_AGENT_LINES: usize = 4;
/// What an agent replies when it has nothing to add; never a line (nor spoken by the UI).
const PASS: &str = "(pass)";
/// Leads every call message, so agents talk like on the phone.
pub(crate) const CALL_NOTE: &str = "(Voice call: your reply is read aloud. Talk like a person on \
    the phone: one to three short spoken sentences, the solution first, no lists, links or \
    markdown. Speak the operator's language. Everyone in the call hears every line. Speak when \
    you are asked or add something new, otherwise reply exactly (pass); never repeat what was \
    said. Talk to the others in your reply, not with send_message; ask the operator out loud, \
    never with ask_user. Longer work: say so in one sentence, do it after the call in your chat. \
    Call hang_up when the conversation is done.)\n\n";
const LANGUAGE: &str = "the operator's language";
/// What naming the voice settings' language adds to the note at most: ` (fil)`.
const MAX_LANGUAGE_BYTES: usize = 6;
/// Only the agent the operator called gets this.
const HOST_NOTE: &str = "(You host the call. When a task needs a specialist or deeper work, \
    propose it in one sentence and wait for the operator's yes. Then say an approval may show in \
    your chat, create_agent it (tell it to speak this call's language) and invite_to_call it for \
    feedback, or give it a task to research in the background; its answer comes back to you.)\n\n";
/// What an invited agent hears first, after the host's name.
const INVITED: &str = " invited you into this call. Listen first: say at most a short hello or \
    (pass), then answer what you are asked; use what you know when someone asks, not as an opening.";
/// Model-facing call messages waiting for their turn; more than this forgets the oldest.
const MAX_PROMPTS: usize = 256;
/// What one agent said, as the others hear it (characters).
const MAX_LINE_CHARS: usize = 1500;
/// Room for what others said before the operator's words.
const MAX_HEARD_BYTES: usize = 12 * 1024;
const MAX_NOTE_BYTES: usize = CALL_NOTE.len() + MAX_LANGUAGE_BYTES + HOST_NOTE.len();
/// Longest operator message in a call, so the lines said before it still fit.
pub const MAX_CALL_TEXT_BYTES: usize = MAX_TEXT_BYTES - MAX_HEARD_BYTES - MAX_NOTE_BYTES - 256;
const HEARD_INTRO: &str = "In the call since your last turn:\n";
/// Room lines waiting for one agent; the oldest go first. The newest always fits.
const MAX_ROOM_HEARD_BYTES: usize = MAX_TEXT_BYTES - MAX_NOTE_BYTES - HEARD_INTRO.len();
const OPERATOR: &str = "The operator: ";

#[derive(Debug, Default)]
pub struct Call {
    agents: Vec<AgentId>,
    /// Set once a second agent joins; the call keeps it until it ends.
    room: Option<i64>,
    /// The voice settings' language code when the call started, named in the note.
    language: Option<String>,
    /// Room lines each member has not heard yet.
    heard: HashMap<AgentId, VecDeque<String>>,
    /// Members due a room turn, next first.
    order: VecDeque<AgentId>,
    /// Whose turn it is in the room; nobody else gets one meanwhile.
    floor: Option<Floor>,
    /// The latest line (who, words), carried by the message that hands the floor on.
    last: Option<(Sender, String)>,
    /// Agent lines since the operator last spoke.
    agent_lines: usize,
    /// Agents that hung up; they go when their turn ends, so their goodbye is heard.
    leaving: Vec<AgentId>,
    /// Calls without a room: (recipient, words in the chat, message for its model), oldest first.
    prompts: VecDeque<(AgentId, String, String)>,
}

/// The member whose turn it is in the room.
#[derive(Debug)]
struct Floor {
    agent: AgentId,
    /// Its turn once started; until then its message waits.
    turn: Option<String>,
    /// What it said so far, its line once the turn completes. `None`: the host's turn outside the
    /// room that invited someone, which speaks first but makes no line.
    said: Option<Vec<String>>,
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
        let note = match &self.language {
            Some(code) => CALL_NOTE.replacen(LANGUAGE, &format!("{LANGUAGE} ({code})"), 1),
            None => CALL_NOTE.to_owned(),
        };
        if self.agents.first() == Some(&agent) {
            note + HOST_NOTE
        } else {
            note
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
            // Counted as `start_turn` joins them.
            while queue.iter().map(|l| l.len() + 1).sum::<usize>() > MAX_ROOM_HEARD_BYTES {
                queue.pop_front();
            }
        }
    }

    /// The operator spoke in the room: everyone is due it, host first, and the agents' count
    /// restarts.
    fn operator_said(&mut self, text: &str) {
        self.agent_lines = 0;
        self.hear(None, &format!("{OPERATOR}{text}"));
        self.order = self.agents.iter().copied().collect();
        self.last = Some((Sender::User, text.to_owned()));
    }

    /// `agent` said `text` in the room: the others are due it, unless past [`MAX_AGENT_LINES`].
    fn agent_said(&mut self, agent: AgentId, text: &str) {
        self.hear(Some(agent), &line_of(agent, text));
        self.last = Some((Sender::Agent(agent), text.to_owned()));
        self.agent_lines += 1;
        if self.agent_lines > MAX_AGENT_LINES {
            return;
        }
        let due: Vec<AgentId> = self
            .agents
            .iter()
            .copied()
            .filter(|a| *a != agent && !self.order.contains(a))
            .collect();
        self.order.extend(due);
    }

    /// Gives a free floor to the next member due.
    fn next_speaker(&mut self) -> Option<AgentId> {
        if self.floor.is_some() {
            return None;
        }
        let agent = self.order.pop_front()?;
        self.floor = Some(Floor {
            agent,
            turn: None,
            said: Some(Vec::new()),
        });
        Some(agent)
    }

    /// Starts `agent`'s room turn `turn` if the floor is its: its model gets all it has not
    /// heard yet. `None`: not its turn, or nothing new (then the floor is free again).
    fn start_turn(&mut self, agent: AgentId, turn: &str) -> Option<String> {
        let floor = self
            .floor
            .as_mut()
            .filter(|f| f.agent == agent && f.turn.is_none())?;
        let Some(heard) = self.heard.remove(&agent).filter(|h| !h.is_empty()) else {
            self.floor = None;
            return None;
        };
        floor.turn = Some(turn.to_owned());
        let heard = Vec::from(heard).join("\n");
        Some(format!("{}{HEARD_INTRO}{heard}", self.note(agent)))
    }

    /// Keeps a text block of `turn` if it holds the floor.
    fn text(&mut self, turn: &str, text: &str) {
        if let Some(said) = self
            .floor
            .as_mut()
            .filter(|f| f.turn.as_deref() == Some(turn))
            .and_then(|f| f.said.as_mut())
        {
            said.push(text.to_owned());
        }
    }

    /// Ends `turn` if it holds the floor and frees it: a completed turn's words come back as its
    /// line. An interrupted one means the operator cut in, so nobody is due until they speak.
    fn end_turn(&mut self, turn: &str, outcome: TurnOutcome) -> Option<(AgentId, String)> {
        if self.floor.as_ref()?.turn.as_deref() != Some(turn) {
            return None;
        }
        let floor = self.floor.take()?;
        match outcome {
            TurnOutcome::Completed => {
                let words = floor.said?.join("\n\n");
                (!words.is_empty()).then_some((floor.agent, words))
            }
            TurnOutcome::Interrupted => {
                self.order.clear();
                None
            }
            TurnOutcome::Failed => None,
        }
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

    /// Whether `agent` is in the operator's voice call.
    pub async fn in_call(&self, agent: AgentId) -> bool {
        self.call().await.agents.contains(&agent)
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
        self.apply(&mut call, agents, None).await
    }

    /// Puts `agents` in the call (empty ends it) and keeps its room in step: it opens with the
    /// second agent, newcomers become members, and it ends with the call. `hung_up`: the agent
    /// whose hang-up this is.
    async fn apply(
        &self,
        call: &mut Call,
        agents: Vec<AgentId>,
        hung_up: Option<AgentId>,
    ) -> Result<(), String> {
        let room = match call.room {
            Some(id) if agents.is_empty() => {
                self.silence(call, id).await;
                Some(self.store.end_room(id).await)
            }
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
            if call.agents.is_empty() {
                call.language = crate::api::voice::load(self)
                    .await
                    .ok()
                    .and_then(|v| v.language);
            }
            call.heard.retain(|a, _| agents.contains(a));
            call.order.retain(|a| agents.contains(a));
            call.leaving.retain(|a| agents.contains(a));
            call.agents = agents;
        }
        self.publish(UiEvent::Call {
            agents: call.agents.clone(),
            room_id: call.room,
            hung_up,
        });
        Ok(())
    }

    /// The call ends: the room turn holding the floor is cut off and queued ones never run.
    async fn silence(&self, call: &Call, room: i64) {
        if let Some(Floor {
            agent,
            turn: Some(turn),
            said: Some(_),
        }) = &call.floor
        {
            self.hub.interrupt_turn(*agent, turn);
        }
        match self.store.cancel_room_messages(room).await {
            Ok(ids) => {
                for id in ids {
                    self.publish_message(id).await;
                }
            }
            Err(e) => tracing::warn!(error = %e, "queued call turns not cancelled"),
        }
    }

    /// Hands a free floor to the next member due, with the message that starts its turn.
    async fn hand_on(&self, call: &mut Call) -> Option<MailMessage> {
        let room = call.room?;
        let (sender, words) = call.last.clone()?;
        while let Some(next) = call.next_speaker() {
            if let Some(message) = self.queue(sender, next, &words, Some(room)).await {
                return Some(message);
            }
            call.floor = None;
        }
        None
    }

    /// The operator's words to the call; `None`: there is no call. Without a room each agent
    /// gets them as a turn; in a room they are a line, and the next agent due gets the floor.
    pub async fn say_in_call(&self, text: &str) -> Option<Vec<MailMessage>> {
        let mut call = self.call().await;
        if call.agents.is_empty() {
            return None;
        }
        let Some(room) = call.room else {
            let mut sent = Vec::new();
            for agent in call.agents.clone() {
                let prompt = format!("{}{OPERATOR}{text}", call.note(agent));
                call.remember(agent, text, prompt);
                sent.extend(self.queue(Sender::User, agent, text, None).await);
            }
            return Some(sent);
        };
        self.add_line(room, Sender::User, text, None).await;
        call.operator_said(text);
        Some(self.hand_on(&mut call).await.into_iter().collect())
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
    /// message whose agent does not hold the floor, has nothing new, or whose room ended).
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
        let prompt = call.start_turn(agent, turn);
        if prompt.is_none() {
            self.hand_on(&mut call).await;
        }
        prompt
    }

    /// Keeps a text block of room turn `turn` for the line it becomes.
    pub async fn call_text(&self, turn: &str, text: &str) {
        if !is_pass(text) && !text.trim().is_empty() {
            self.call().await.text(turn, text);
        }
    }

    /// At the end of `agent`'s turn `turn`: a completed room turn's words become a line, a
    /// hang-up takes effect, and the floor goes to the next member due.
    pub async fn call_turn_done(&self, agent: AgentId, turn: &str, outcome: TurnOutcome) {
        let mut call = self.call().await;
        if let (Some(room), Some((speaker, words))) = (call.room, call.end_turn(turn, outcome)) {
            self.add_line(room, Sender::Agent(speaker), &words, None)
                .await;
            call.agent_said(speaker, &words);
        }
        if call.leaving.contains(&agent) {
            let rest = if call.agents.first() == Some(&agent) {
                Vec::new()
            } else {
                call.agents
                    .iter()
                    .copied()
                    .filter(|a| *a != agent)
                    .collect()
            };
            // Fewer agents than before always fit.
            let _ = self.apply(&mut call, rest, Some(agent)).await;
        }
        self.hand_on(&mut call).await;
    }

    /// Brings `guest` into the call `host` is in. The call moves into its room (it opens with
    /// the second agent), the topic is a line there, and the guest speaks next, after the host's
    /// running turn.
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
        self.apply(&mut call, agents, None).await?;
        let Some(room) = call.room else {
            return Err("the call has no room".into());
        };
        self.add_line(room, Sender::Agent(host), topic, Some(guest))
            .await;
        call.heard
            .entry(guest)
            .or_default()
            .push_back(format!("<@{host}>{INVITED}"));
        call.hear(Some(host), &line_of(host, topic));
        call.last = Some((Sender::Agent(host), topic.to_owned()));
        call.order.retain(|a| *a != guest);
        call.order.push_front(guest);
        // The host's announcement in the turn that invited comes first.
        if call.floor.is_none()
            && let Some(turn) = self.hub.current_turn(host)
        {
            call.floor = Some(Floor {
                agent: host,
                turn: Some(turn),
                said: None,
            });
        }
        self.hand_on(&mut call).await;
        Ok(())
    }

    async fn add_line(&self, room: i64, speaker: Sender, body: &str, invited: Option<AgentId>) {
        match self
            .store
            .add_room_line(room, speaker, body.to_owned(), invited)
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

    /// Plays the next turn in the room: `text` (empty: a pass) as its reply. Returns who spoke
    /// and what its model got.
    fn take_turn(call: &mut Call, text: &str) -> (AgentId, String) {
        let agent = call.next_speaker().expect("someone is due");
        let prompt = call.start_turn(agent, "t").expect("something to hear");
        if !text.is_empty() {
            call.text("t", text);
        }
        if let Some((speaker, words)) = call.end_turn("t", TurnOutcome::Completed) {
            call.agent_said(speaker, &words);
        }
        (agent, prompt)
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
        let (agent, prompt) = take_turn(&mut call, "");
        assert_eq!(agent, a);
        assert!(prompt.starts_with(&format!("{}{HEARD_INTRO}", call.note(a))));
        assert!(prompt.len() <= MAX_TEXT_BYTES, "{}", prompt.len());
        call.floor = Some(Floor {
            agent: a,
            turn: None,
            said: Some(Vec::new()),
        });
        assert_eq!(call.start_turn(a, "t2"), None, "heard once");
        assert!(call.floor.is_none(), "nothing new frees the floor");
    }

    #[test]
    fn agents_take_the_floor_one_at_a_time_up_to_the_cap() {
        let (a, b, c) = (id(1), id(2), id(3));
        let mut call = room_of(vec![a, b, c]);
        call.operator_said("ok");
        assert_eq!(call.next_speaker(), Some(a), "the host first");
        assert_eq!(call.next_speaker(), None, "one at a time");
        assert_eq!(call.start_turn(b, "x"), None, "not b's turn");
        assert!(call.start_turn(a, "t").unwrap().contains(HOST_NOTE));
        call.text("t", "early");
        let (speaker, words) = call.end_turn("t", TurnOutcome::Completed).unwrap();
        call.agent_said(speaker, &words);
        let (who, prompt) = take_turn(&mut call, "");
        assert_eq!(who, b, "then in join order");
        assert!(!prompt.contains(HOST_NOTE));
        assert!(
            prompt.ends_with(&format!("{OPERATOR}ok\n<@{a}>: early")),
            "{prompt}"
        );
        assert_eq!(take_turn(&mut call, "line 2").0, c, "a pass is no line");
        let mut spoke = vec![];
        for n in 3..=6 {
            spoke.push(take_turn(&mut call, &format!("line {n}")).0);
        }
        assert_eq!(spoke, [a, b, c, a], "past the cap nobody new is due");
        assert_eq!(call.next_speaker(), None, "the room waits for the operator");
        call.operator_said("go on");
        assert_eq!(take_turn(&mut call, "").0, a);
        let (who, prompt) = take_turn(&mut call, "");
        assert_eq!(who, b);
        assert!(
            prompt.ends_with(&format!("<@{a}>: line 6\n{OPERATOR}go on")),
            "kept for the operator's next words: {prompt}"
        );
    }

    #[test]
    fn an_interrupted_turn_makes_no_line_and_waits_for_the_operator() {
        let (a, b) = (id(1), id(2));
        let mut call = room_of(vec![a, b]);
        call.operator_said("ok");
        call.next_speaker();
        call.start_turn(a, "t");
        call.text("t", "cut");
        assert_eq!(call.end_turn("other", TurnOutcome::Completed), None);
        assert_eq!(call.end_turn("t", TurnOutcome::Interrupted), None);
        assert_eq!(call.next_speaker(), None);
    }

    #[test]
    fn the_note_names_the_language_and_only_the_host_gets_the_host_note() {
        let (a, b) = (id(1), id(2));
        let mut call = room_of(vec![a, b]);
        assert!(call.note(b).contains("Speak the operator's language. "));
        call.language = Some("de".into());
        let host = call.note(a);
        assert!(host.contains("Speak the operator's language (de). ") && host.contains(HOST_NOTE));
        assert!(!call.note(b).contains(HOST_NOTE));
        call.language = Some("fil".into());
        assert_eq!(call.note(a).len(), MAX_NOTE_BYTES, "the longest note");
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
        call.language = Some("fil".into());
        for _ in 0..3000 {
            call.agent_said(b, "x");
        }
        call.operator_said(&"y".repeat(MAX_CALL_TEXT_BYTES));
        let (_, prompt) = take_turn(&mut call, "");
        assert!(prompt.len() <= MAX_TEXT_BYTES, "{}", prompt.len());
        assert!(prompt.ends_with('y'), "the operator's words stay whole");
    }
}
