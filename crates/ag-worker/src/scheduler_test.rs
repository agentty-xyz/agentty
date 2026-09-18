use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::scheduler::run_until_stopped;
use crate::{ScheduledCommand, ScheduledWork, WorkQueue, WorkerHost, next_work, run};

struct Command(Option<u64>, bool);
impl ScheduledCommand for Command {
    fn order(&self) -> Option<u64> {
        self.0
    }

    fn can_run_while_paused(&self) -> bool {
        self.1
    }
}

#[derive(Default)]
struct Host {
    abandoned: Mutex<Vec<u64>>,
    paused: AtomicBool,
    messages: Mutex<VecDeque<u64>>,
    completed: Mutex<Vec<u64>>,
    stopped: AtomicBool,
    observed: Notify,
}

impl WorkQueue for Arc<Host> {
    type Command = Command;
    type Message = u64;

    fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn message_order(&self) -> Option<u64> {
        self.messages
            .lock()
            .expect("test operation succeeds")
            .front()
            .copied()
    }

    fn pop_message(&self) -> Option<u64> {
        self.messages
            .lock()
            .expect("test operation succeeds")
            .pop_front()
    }
}

#[async_trait]
impl WorkerHost for Arc<Host> {
    async fn execute(&self, work: ScheduledWork<Command, u64>) {
        let value = match work {
            ScheduledWork::Command(command) => command.0.unwrap_or(0),
            ScheduledWork::Message(message) => message,
        };
        self.completed
            .lock()
            .expect("test operation succeeds")
            .push(value);
        self.observed.notify_one();
    }

    async fn abandon(&self, work: ScheduledWork<Command, u64>) {
        let value = match work {
            ScheduledWork::Command(command) => command.0.unwrap_or(0),
            ScheduledWork::Message(message) => message,
        };
        assert!(!self.stopped.load(Ordering::SeqCst));
        self.abandoned.lock().expect("abandoned work").push(value);
    }

    async fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

fn take(host: &Arc<Host>, commands: &mut VecDeque<Command>) -> Option<u64> {
    next_work(host, commands).map(|work| match work {
        ScheduledWork::Command(command) => command.0.unwrap_or(0),
        ScheduledWork::Message(message) => message,
    })
}

#[test]
fn orders_messages_commands_and_immediate_work() {
    // Arrange
    let host = Arc::new(Host::default());
    host.messages
        .lock()
        .expect("test operation succeeds")
        .extend([1, 3]);
    let mut commands = VecDeque::from([
        Command(None, true),
        Command(Some(2), false),
        Command(Some(3), false),
        Command(Some(4), false),
    ]);
    // Act
    let result: Vec<_> = (0..7).map(|_| take(&host, &mut commands)).collect();
    // Assert
    assert_eq!(
        result,
        [Some(0), Some(1), Some(2), Some(3), Some(3), Some(4), None]
    );
}

#[test]
fn pause_allows_answers_without_consuming_waiting_work() {
    // Arrange
    let host = Arc::new(Host::default());
    host.paused.store(true, Ordering::SeqCst);
    host.messages
        .lock()
        .expect("test operation succeeds")
        .push_back(1);
    let mut commands = VecDeque::from([Command(Some(2), false), Command(Some(3), true)]);
    // Act
    let answer = take(&host, &mut commands);
    let waiting = take(&host, &mut commands);
    host.paused.store(false, Ordering::SeqCst);
    let resumed = take(&host, &mut commands);
    // Assert
    assert_eq!(answer, Some(3));
    assert_eq!(waiting, None);
    assert_eq!(resumed, Some(1));
    assert_eq!(take(&host, &mut commands), Some(2));
}

#[tokio::test]
async fn headless_worker_observes_mailbox_wakeups_and_shutdown() {
    // Arrange
    let host = Arc::new(Host::default());
    let wakeup = Arc::new(Notify::new());
    let (sender, receiver) = mpsc::unbounded_channel();
    let worker = tokio::spawn(run(Arc::clone(&host), Arc::clone(&wakeup), receiver));
    // Act
    tokio::task::yield_now().await;
    sender
        .send(Command(Some(1), false))
        .expect("test operation succeeds");
    host.observed.notified().await;
    host.messages
        .lock()
        .expect("test operation succeeds")
        .push_back(2);
    wakeup.notify_one();
    host.observed.notified().await;
    sender
        .send(Command(Some(3), false))
        .expect("test operation succeeds");
    drop(sender);
    worker.await.expect("test operation succeeds");
    // Assert
    assert_eq!(
        *host.completed.lock().expect("test operation succeeds"),
        [1, 2, 3]
    );
    assert!(host.stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn closed_mailbox_drains_runnable_work_and_abandons_paused_work() {
    // Arrange
    let host = Arc::new(Host::default());
    host.paused.store(true, Ordering::SeqCst);
    host.messages.lock().expect("messages").push_back(4);
    let (sender, receiver) = mpsc::unbounded_channel();
    for command in [
        Command(Some(1), false),
        Command(Some(2), true),
        Command(Some(3), false),
    ] {
        sender.send(command).expect("accepted command");
    }
    drop(sender);

    // Act
    run(Arc::clone(&host), Arc::new(Notify::new()), receiver).await;

    // Assert
    assert_eq!(*host.completed.lock().expect("completed"), [2]);
    assert_eq!(*host.abandoned.lock().expect("abandoned"), [1, 3, 4]);
    assert!(host.messages.lock().expect("messages").is_empty());
    assert!(host.stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancellation_interrupts_a_replenished_mailbox_before_any_work_starts() {
    // Arrange: more ready input than one cooperative scheduling slice.
    let host = Arc::new(Host::default());
    host.paused.store(true, Ordering::SeqCst);
    let (sender, receiver) = mpsc::unbounded_channel();
    for order in 0..1_000 {
        sender
            .send(Command(Some(order), false))
            .expect("queued command");
    }
    let stop = CancellationToken::new();
    let mut worker = Box::pin(run_until_stopped(
        host.clone(),
        Arc::default(),
        receiver,
        stop.clone(),
        Arc::default(),
    ));

    // Act: the drain must yield to its caller even though the receiver is
    // ready.
    poll_fn(|context| {
        assert!(worker.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    for order in 1_000..2_000 {
        sender
            .send(Command(Some(order), false))
            .expect("replenished command");
    }
    stop.cancel();
    worker.await;

    // Assert: all accepted work is settled exactly once, without execution,
    // and a retained sender cannot delay cleanup.
    assert_eq!(
        *host.completed.lock().expect("completed"),
        Vec::<u64>::new()
    );
    assert_eq!(
        *host.abandoned.lock().expect("abandoned"),
        (0..2_000).collect::<Vec<_>>()
    );
    assert!(host.stopped.load(Ordering::SeqCst));
    assert!(sender.send(Command(Some(2_000), false)).is_err());
}

#[tokio::test]
async fn continuously_replenished_mailbox_executes_commands_and_messages_in_order() {
    // Arrange: messages on both sides of a command batch boundary.
    let host = Arc::new(Host::default());
    let messages = [1, 65, 129];
    host.messages.lock().expect("messages").extend(messages);
    let (sender, receiver) = mpsc::unbounded_channel();
    let stop = CancellationToken::new();
    let mut worker = Box::pin(run_until_stopped(
        host.clone(),
        Arc::default(),
        receiver,
        stop.clone(),
        Arc::default(),
    ));
    let mut next_order = 0;
    let mut expected = Vec::new();

    // Act: replenish faster than the worker drains, before every poll.
    for order in (0..=140).filter(|order| order % 2 == 0 || messages.contains(order)) {
        for _ in 0..128 {
            sender
                .send(Command(Some(next_order), false))
                .expect("replenished command");
            next_order += 2;
        }
        poll_fn(|context| {
            assert!(worker.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        // Assert: each slice executes the next item despite unread commands.
        expected.push(order);
        assert_eq!(*host.completed.lock().expect("completed"), expected);
        tokio::task::yield_now().await;
    }
    stop.cancel();
    worker.await;
    assert!(host.stopped.load(Ordering::SeqCst));
}
