use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};

/// Scheduling metadata supplied by an application command.
pub trait ScheduledCommand: Send {
    /// Shared submission order, or `None` for an immediate command.
    fn order(&self) -> Option<u64>;
    /// Whether this command may execute while normal scheduling is paused.
    fn can_run_while_paused(&self) -> bool;
}

/// One item selected from command and message queues.
pub enum ScheduledWork<C, M> {
    /// A host workflow command.
    Command(C),
    /// A queued conversational message.
    Message(M),
}

/// Host policies and effects surrounding serial run execution.
///
/// The host owns message persistence and product-specific pause semantics.
/// Commands and messages must use the same monotonically increasing order.
/// Execution includes any ordered post-processing; model completion alone
/// does not allow the next item to overtake that work.
pub trait WorkQueue: Send + Sync {
    /// Workflow command accepted by the mailbox.
    type Command: ScheduledCommand;
    /// A queued conversational message.
    type Message: Send;

    /// Whether ordinary work must wait for a host state change.
    fn paused(&self) -> bool;
    /// Submission order of the next queued message, if any.
    fn message_order(&self) -> Option<u64>;
    /// Removes the next queued message for execution.
    fn pop_message(&self) -> Option<Self::Message>;
}

/// Host effects invoked by the serial worker after scheduling.
#[async_trait]
pub trait WorkerHost: WorkQueue {
    /// Executes one selected item, including ordered post-processing.
    async fn execute(&self, work: ScheduledWork<Self::Command, Self::Message>);
    /// Finalizes accepted work that cannot run when the mailbox closes.
    /// Hosts must settle its persistence and notify any waiting callers.
    async fn abandon(&self, work: ScheduledWork<Self::Command, Self::Message>);
    /// Releases resources after runnable work drains and paused work is
    /// abandoned.
    async fn shutdown(&self);
}

/// Selects the oldest runnable work, allowing immediate commands during pauses.
pub fn next_work<H: WorkQueue>(
    host: &H,
    commands: &mut VecDeque<H::Command>,
) -> Option<ScheduledWork<H::Command, H::Message>> {
    if host.paused() {
        let index = commands
            .iter()
            .position(ScheduledCommand::can_run_while_paused)?;

        return commands.remove(index).map(ScheduledWork::Command);
    }
    if commands
        .front()
        .is_some_and(|command| command.order().is_none())
    {
        return commands.pop_front().map(ScheduledWork::Command);
    }

    let command_order = commands.front().and_then(ScheduledCommand::order);
    let message_order = host.message_order();
    if command_order.is_some_and(|order| message_order.is_none_or(|message| order <= message)) {
        return commands.pop_front().map(ScheduledWork::Command);
    }

    host.pop_message().map(ScheduledWork::Message)
}

/// Drives a single session's mailbox without borrowing a frontend.
///
/// Hosts notify `wakeup` after changing pause state or appending messages.
/// Separate invocations may execute different sessions concurrently.
/// Closing the mailbox drains runnable work, then explicitly abandons paused
/// commands and messages before shutting down.
pub async fn run<H: WorkerHost>(
    host: H,
    wakeup: Arc<Notify>,
    mut receiver: mpsc::UnboundedReceiver<H::Command>,
) {
    let mut commands = VecDeque::new();
    loop {
        while let Ok(command) = receiver.try_recv() {
            commands.push_back(command);
        }
        if let Some(work) = next_work(&host, &mut commands) {
            host.execute(work).await;
            continue;
        }
        tokio::select! {
            command = receiver.recv() => {
                let Some(command) = command else { break; };
                commands.push_back(command);
            }
            () = wakeup.notified() => {}
        }
    }
    for command in commands {
        host.abandon(ScheduledWork::Command(command)).await;
    }
    while let Some(message) = host.pop_message() {
        host.abandon(ScheduledWork::Message(message)).await;
    }
    host.shutdown().await;
}

#[cfg(test)]
#[path = "scheduler_test.rs"]
mod tests;
