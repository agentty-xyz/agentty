use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio_util::sync::CancellationToken;

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
    receiver: mpsc::UnboundedReceiver<H::Command>,
) {
    run_until_stopped(
        host,
        wakeup,
        receiver,
        CancellationToken::new(),
        Arc::default(),
    )
    .await;
}

/// Explicit retirement finishes the current workflow but abandons queued work
/// instead of draining it into a runtime the host is about to replace.
pub(crate) async fn run_until_stopped<H: WorkerHost>(
    host: H,
    wakeup: Arc<Notify>,
    mut receiver: mpsc::UnboundedReceiver<H::Command>,
    stop: CancellationToken,
    execution: Arc<Mutex<()>>,
) {
    let mut commands = VecDeque::new();
    loop {
        let execution_guard = execution.lock().await;
        let mut drained = 0;
        while drained < MAILBOX_DRAIN_BATCH_SIZE && !stop.is_cancelled() {
            let Ok(command) = receiver.try_recv() else {
                break;
            };
            commands.push_back(command);
            drained += 1;
        }
        if stop.is_cancelled() {
            break;
        }
        if let Some(work) = next_work(&host, &mut commands) {
            // Select from the oldest buffered command and message before
            // admitting another batch. Unread commands follow the buffered
            // ones in submission order, so neither queue can be starved by
            // producers keeping the mailbox full.
            host.execute(work).await;
            drop(execution_guard);
            tokio::task::yield_now().await;
            continue;
        }
        drop(execution_guard);
        if drained == MAILBOX_DRAIN_BATCH_SIZE {
            // Paused work still needs cooperative draining to find commands
            // that may run while paused, without delaying cancellation.
            tokio::task::yield_now().await;
            continue;
        }
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            command = receiver.recv() => {
                let Some(command) = command else { break; };
                commands.push_back(command);
            }
            () = wakeup.notified() => {}
        }
    }
    receiver.close();
    while let Ok(command) = receiver.try_recv() {
        commands.push_back(command);
    }
    let _execution_guard = execution.lock().await;
    for command in commands {
        host.abandon(ScheduledWork::Command(command)).await;
    }
    while let Some(message) = host.pop_message() {
        host.abandon(ScheduledWork::Message(message)).await;
    }
    host.shutdown().await;
}

/// Bound synchronous polling so callers can request retirement or a pause.
const MAILBOX_DRAIN_BATCH_SIZE: usize = 64;

#[cfg(test)]
#[path = "scheduler_test.rs"]
mod tests;
