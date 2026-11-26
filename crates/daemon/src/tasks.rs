//! Tracked tasks: a delegate's answer goes back to the assigner as a message unless it already
//! answered; a finished task left without an answer gets one reminder from the supervisor.

use ferrobot_core::protocol::TurnOutcome;
use ferrobot_core::{AgentId, MailMessage, Sender};
use ferrobot_mcp::{MAX_MESSAGE_BYTES, MailboxNotifier};

use crate::AppState;
use crate::webhooks::WebhookEvent;

/// Reports the end of `agent`'s `turn` on `msg` to whoever gave it the task.
/// `text`: the turn's final answer, or the error of a failed turn.
pub(crate) async fn report(
    state: &AppState,
    (agent, turn): (AgentId, &str),
    msg: &MailMessage,
    outcome: TurnOutcome,
    text: Option<&str>,
) {
    let task = match state.store.task_of(agent, msg).await {
        Ok(Some(task)) => task,
        Ok(None) => return,
        Err(e) => {
            tracing::error!(error = %e, message = msg.id, "cannot look up the task");
            return;
        }
    };
    if task.message.id == msg.id && outcome == TurnOutcome::Completed {
        let summary = format!("finished task #{}", msg.id);
        let detail = text.map(str::to_owned);
        crate::webhooks::notify(state, agent, WebhookEvent::TaskDone, summary, detail);
    }
    if task.reported {
        return;
    }
    let Some(assigner) = task.assigner() else {
        return;
    };
    // The operator stopped the assigner too; the report would start its work again.
    if outcome == TurnOutcome::Interrupted && state.hub.recently_stopped(assigner) {
        return;
    }
    let id = task.message.id;
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    let body = match (outcome, text) {
        (TurnOutcome::Completed, Some(answer)) => format!("Task #{id} answer:\n\n{answer}"),
        // Nothing to pass on: the supervisor reminds the delegate once.
        (TurnOutcome::Completed, None) => return,
        (TurnOutcome::Interrupted, None) => format!("Task #{id} was stopped by the operator."),
        (TurnOutcome::Interrupted, Some(partial)) => {
            format!("Task #{id} was stopped by the operator. So far:\n\n{partial}")
        }
        (TurnOutcome::Failed, error) => {
            format!("Task #{id} failed: {}", error.unwrap_or("unknown error"))
        }
    };
    let mut body = state.normalize_mentions(&body).await;
    crate::webhooks::truncate(&mut body, MAX_MESSAGE_BYTES);
    match state
        .store
        .enqueue_message(Sender::Agent(agent), assigner, body, Some(id))
        .await
    {
        Ok(m) => {
            state.message_enqueued(&m);
            let payload = serde_json::json!({ "message_id": m.id, "to": assigner, "answer": true });
            if let Err(e) = state
                .store
                .append_event(agent, Some(turn.to_owned()), "handoff".into(), payload)
                .await
            {
                tracing::warn!(error = %e, task = id, "cannot record the task's answer");
            }
        }
        // The assigner may have been deleted since.
        Err(e) => tracing::warn!(error = %e, task = id, "cannot report the task"),
    }
}

/// Sends one reminder for each finished task left without an answer, once
/// its delegate is idle.
pub async fn nudge(state: &AppState) {
    let tasks = match state.store.unanswered_tasks().await {
        Ok(tasks) => tasks,
        Err(e) => {
            tracing::error!(error = %e, "cannot list unanswered tasks");
            return;
        }
    };
    for task in tasks {
        let delegate = task.message.recipient;
        if state.hub.is_working(delegate)
            // Deferred mail waits for a wake, so it must not hold the reminder back.
            || !matches!(state.store.has_waking_messages(delegate).await, Ok(false))
        {
            continue;
        }
        let Some(assigner) = task.assigner() else {
            continue;
        };
        let Ok(assigner) = state.store.get_agent(assigner).await else {
            continue;
        };
        let body = format!(
            "Automatic reminder: task #{} is still open, {} has no answer from you yet. \
             Finish it, or say why you can't; your answer goes back automatically.",
            task.message.id, assigner.name
        );
        match state.store.nudge_task(&task, body).await {
            Ok(m) => state.message_enqueued(&m),
            Err(e) => tracing::warn!(error = %e, task = task.message.id, "cannot nudge"),
        }
    }
}
