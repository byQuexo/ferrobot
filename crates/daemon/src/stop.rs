//! The operator's Stop for a chain of work: an agent's turn, the turns of agents working on a
//! message from it (and from those, and so on), and the agent messages queued to or from any of
//! them. Messages from the operator, routines and webhooks stay queued.

use ferrobot_core::{AgentId, MailMessage, MailStatus, Sender};

use crate::AppState;
use crate::error::ApiError;

/// Stops `agent`'s chain; returns every agent stopped (`agent` first) and how many queued
/// messages were dropped.
pub(crate) async fn stop_chain(
    state: &AppState,
    agent: AgentId,
) -> Result<(Vec<AgentId>, usize), ApiError> {
    let open = state.store.open_messages().await?;
    let chain = chain(agent, &open);
    state.hub.mark_stopped(&chain);
    for agent in &chain {
        state.hub.interrupt(*agent);
    }
    let queued = open
        .iter()
        .filter(|m| m.status == MailStatus::Pending && between(m, &chain))
        .map(|m| m.id)
        .collect();
    let dropped = state
        .store
        .drop_messages(queued, "stopped by the operator".into())
        .await?;
    for id in &dropped {
        state.publish_message(*id).await;
    }
    Ok((chain, dropped.len()))
}

fn from_agent(m: &MailMessage) -> Option<AgentId> {
    match m.sender {
        Sender::Agent(a) => Some(a),
        _ => None,
    }
}

/// `agent`, then every agent working on a message from one already in the chain.
fn chain(agent: AgentId, open: &[MailMessage]) -> Vec<AgentId> {
    let mut chain = vec![agent];
    let mut grew = true;
    while grew {
        grew = false;
        for m in open.iter().filter(|m| m.status == MailStatus::InFlight) {
            if from_agent(m).is_some_and(|a| chain.contains(&a)) && !chain.contains(&m.recipient) {
                chain.push(m.recipient);
                grew = true;
            }
        }
    }
    chain
}

/// A message from an agent, to or from one in `chain`.
fn between(m: &MailMessage, chain: &[AgentId]) -> bool {
    from_agent(m).is_some_and(|a| chain.contains(&a) || chain.contains(&m.recipient))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> AgentId {
        format!("00000000-0000-4000-8000-0000000000{n:02}")
            .parse()
            .unwrap()
    }

    fn msg(sender: Sender, to: u8, status: MailStatus) -> MailMessage {
        MailMessage {
            id: 0,
            sender,
            recipient: id(to),
            body: String::new(),
            reply_to: None,
            priority: None,
            room_id: None,
            status,
            attempts: 0,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn the_chain_follows_work_on_messages_from_it() {
        let open = [
            // 2 works on 1's message, 3 on 2's: both are 1's chain, in any order.
            msg(Sender::Agent(id(2)), 3, MailStatus::InFlight),
            msg(Sender::Agent(id(1)), 2, MailStatus::InFlight),
            // 4 works on the operator's message, 5 only has 1's message queued.
            msg(Sender::User, 4, MailStatus::InFlight),
            msg(Sender::Agent(id(1)), 5, MailStatus::Pending),
        ];
        let chain = chain(id(1), &open);
        assert_eq!(chain, [id(1), id(2), id(3)]);
        let dropped: Vec<_> = [
            msg(Sender::Agent(id(1)), 5, MailStatus::Pending),
            msg(Sender::Agent(id(6)), 3, MailStatus::Pending),
            msg(Sender::Agent(id(6)), 7, MailStatus::Pending),
            msg(Sender::User, 1, MailStatus::Pending),
            msg(Sender::Webhook(1), 2, MailStatus::Pending),
        ]
        .iter()
        .map(|m| between(m, &chain))
        .collect();
        assert_eq!(dropped, [true, true, false, false, false]);
    }
}
