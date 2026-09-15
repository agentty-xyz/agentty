use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};

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
