//! Queued-prompt steering delivery, restoration, and notice tests.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ag_contracts::{AgentError, MockAgentChannel};
use ag_telemetry::QueuedTrace;
use ag_worker::SessionRunClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{QueuedMessageSteering, SteeredMessage, SteeringDeliveries};
use crate::domain::session::{QueuedMessage, SessionId};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::infra::db::AppRepositories;

/// Shared state one steering delivery reads and updates.
struct SteeringFixture {
    /// The session's current-turn token; deliveries steer into the turn it
    /// holds when they are built.
    cancel_token: Arc<Mutex<CancellationToken>>,
    deliveries: Arc<SteeringDeliveries>,
    message_traces: Arc<Mutex<HashMap<(SessionId, u64), QueuedTrace>>>,
    queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    session_id: SessionId,
    transcript: Arc<Mutex<SessionTranscript>>,
    wakes: Arc<AtomicUsize>,
}

impl SteeringFixture {
    /// Creates state with `queued` prompts already waiting in the queue and
    /// traces for the `traced_orders` among them.
    fn new(traced_orders: &[u64], queued: Vec<QueuedMessage>) -> Self {
        let session_id = SessionId::from("session-1");
        let message_traces = traced_orders
            .iter()
            .map(|order| {
                (
                    (session_id.clone(), *order),
                    QueuedTrace::new("turn", Vec::new()),
                )
            })
            .collect();

        Self {
            cancel_token: Arc::default(),
            deliveries: Arc::default(),
            message_traces: Arc::new(Mutex::new(message_traces)),
            queued_messages: Arc::new(Mutex::new(VecDeque::from(queued))),
            session_id,
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
            wakes: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Builds a delivery through `channel` that shares this fixture's state.
    async fn steering(&self, channel: MockAgentChannel) -> QueuedMessageSteering {
        let wakes = Arc::clone(&self.wakes);
        let turn_cancel_token = self.cancel_token.lock().expect("cancel token").clone();

        QueuedMessageSteering {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::clone(&self.cancel_token),
            db: AppRepositories::in_memory().await.expect("repositories"),
            delivery: self.deliveries.begin(),
            message_traces: Arc::clone(&self.message_traces),
            queued_messages: Arc::clone(&self.queued_messages),
            session_id: self.session_id.clone(),
            session_run: SessionRunClient::from_channel(
                self.session_id.to_string(),
                Arc::new(channel),
            ),
            session_update_versions: Arc::default(),
            transcript: Arc::clone(&self.transcript),
            turn_cancel_token,
            wake_worker: Box::new(move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            }),
        }
    }

    /// Returns the queued prompt orders front to back.
    fn queued_orders(&self) -> Vec<u64> {
        self.queued_messages
            .lock()
            .expect("queue")
            .iter()
            .map(QueuedMessage::order)
            .collect()
    }

    /// Returns whether no prompt or notice reached the transcript.
    fn transcript_is_empty(&self) -> bool {
        self.transcript
            .lock()
            .expect("transcript")
            .messages()
            .is_empty()
    }

    /// Returns the orders that still hold a queued-turn trace.
    fn traced_orders(&self) -> Vec<u64> {
        let mut orders = self
            .message_traces
            .lock()
            .expect("traces")
            .keys()
            .map(|(_, order)| *order)
            .collect::<Vec<_>>();
        orders.sort_unstable();

        orders
    }
}

/// Builds one steerable queued prompt.
fn queued(order: u64, text: &str) -> QueuedMessage {
    QueuedMessage::new(order, text.into()).with_steering(true)
}

/// Builds one steerable prompt in flight that holds its queued-turn trace.
fn steered(order: u64, text: &str) -> SteeredMessage {
    SteeredMessage {
        message: queued(order, text),
        trace: Some(QueuedTrace::new("turn", Vec::new())),
    }
}

/// Builds a channel that accepts `accepted` prompts and rejects `rejected`
/// ones as delivered after the turn ended.
fn channel_with(
    accepted: &'static [&'static str],
    rejected: &'static [&'static str],
) -> MockAgentChannel {
    let mut channel = MockAgentChannel::new();
    for text in accepted {
        channel
            .expect_steer_turn()
            .withf(move |_, prompt| prompt.text == *text)
            .once()
            .returning(|_, _| Box::pin(async { Ok(()) }));
    }
    for text in rejected {
        channel
            .expect_steer_turn()
            .withf(move |_, prompt| prompt.text == *text)
            .once()
            .returning(|_, _| {
                Box::pin(async { Err(AgentError::Runtime("turn ended".to_string())) })
            });
    }

    channel
}

#[tokio::test]
async fn taken_prompts_leave_the_queue_with_their_traces_up_to_the_first_that_waits() {
    // Arrange
    let fixture = SteeringFixture::new(
        &[1, 2, 3, 4],
        vec![
            queued(1, "first"),
            queued(2, "second"),
            QueuedMessage::new(3, "must wait".into()),
            queued(4, "fourth"),
        ],
    );
    let steering = fixture.steering(channel_with(&[], &["first"])).await;

    // Act
    let messages = steering.take_steerable_messages();
    let queued_while_in_flight = fixture.queued_orders();
    let traced_while_in_flight = fixture.traced_orders();
    steering.run(messages).await;

    // Assert: the worker ends the traces of unqueued prompts it finds in
    // the map, so in-flight prompts hold theirs until they return.
    assert_eq!(queued_while_in_flight, [3, 4]);
    assert_eq!(traced_while_in_flight, [3, 4]);
    assert_eq!(fixture.queued_orders(), [1, 2, 3, 4]);
    assert_eq!(fixture.traced_orders(), [1, 2, 3, 4]);
}

#[tokio::test]
async fn delivered_prompts_join_the_transcript_and_rejections_return_to_the_queue() {
    // Arrange
    let fixture = SteeringFixture::new(&[], vec![queued(10, "later")]);
    let steering = fixture
        .steering(channel_with(&["first"], &["second"]))
        .await;

    // Act
    steering
        .run(vec![
            steered(1, "first"),
            steered(2, "second"),
            steered(3, "third"),
        ])
        .await;

    // Assert
    assert_eq!(fixture.queued_orders(), [2, 3, 10]);
    assert_eq!(fixture.traced_orders(), [2, 3]);
    assert_eq!(fixture.wakes.load(Ordering::SeqCst), 1);
    let transcript = fixture.transcript.lock().expect("transcript");
    let messages = transcript.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].kind, SessionMessageKind::UserPrompt);
    assert_eq!(messages[0].content, "first");
    assert_eq!(messages[1].kind, SessionMessageKind::WorkflowNotice);
    assert!(messages[1].content.contains(
        "[Queue Error] Could not send the queued message now: turn ended. It stays queued"
    ));
}

#[tokio::test]
async fn rejections_after_the_user_stopped_the_turn_are_discarded() {
    // Arrange
    let fixture = SteeringFixture::new(&[], Vec::new());
    let steering = fixture.steering(channel_with(&[], &["first"])).await;
    fixture.cancel_token.lock().expect("cancel token").cancel();

    // Act
    steering
        .run(vec![steered(1, "first"), steered(2, "second")])
        .await;

    // Assert
    assert_eq!(fixture.queued_orders(), Vec::<u64>::new());
    assert_eq!(fixture.traced_orders(), Vec::<u64>::new());
    assert_eq!(fixture.wakes.load(Ordering::SeqCst), 0);
    assert!(fixture.transcript_is_empty());
}

#[tokio::test]
async fn rejections_after_the_user_stopped_a_turn_with_a_newer_token_are_discarded() {
    // Arrange: the steered turn installed its own token after the delivery
    // was built.
    let fixture = SteeringFixture::new(&[], Vec::new());
    let steering = fixture.steering(channel_with(&[], &["first"])).await;
    let steered_turn = CancellationToken::new();
    *fixture.cancel_token.lock().expect("cancel token") = steered_turn.clone();
    steered_turn.cancel();

    // Act
    steering.run(vec![steered(1, "first")]).await;

    // Assert
    assert_eq!(fixture.queued_orders(), Vec::<u64>::new());
    assert_eq!(fixture.traced_orders(), Vec::<u64>::new());
    assert_eq!(fixture.wakes.load(Ordering::SeqCst), 0);
    assert!(fixture.transcript_is_empty());
}

#[tokio::test]
async fn prompts_behind_an_older_returned_prompt_stay_queued_without_steering() {
    // Arrange
    let fixture = SteeringFixture::new(&[1], vec![queued(1, "returned")]);
    let steering = fixture.steering(channel_with(&[], &[])).await;

    // Act
    steering.run(vec![steered(2, "newer")]).await;

    // Assert
    assert_eq!(fixture.queued_orders(), [1, 2]);
    assert_eq!(fixture.traced_orders(), [1, 2]);
    assert_eq!(fixture.wakes.load(Ordering::SeqCst), 1);
    assert!(fixture.transcript_is_empty());
}

#[tokio::test]
async fn deliveries_wait_for_the_earlier_delivery_of_the_session() {
    // Arrange
    let fixture = SteeringFixture::new(&[], Vec::new());
    let steering = fixture.steering(channel_with(&["first"], &[])).await;
    let earlier_delivery = fixture.deliveries.order.lock().await;

    // Act
    let delivery = tokio::spawn(steering.run(vec![steered(1, "first")]));
    tokio::task::yield_now().await;
    let delivered_while_locked = !fixture.transcript_is_empty();
    drop(earlier_delivery);
    delivery.await.expect("delivery task");

    // Assert
    assert!(!delivered_while_locked);
    let transcript = fixture.transcript.lock().expect("transcript");
    assert_eq!(transcript.messages()[0].content, "first");
}

#[tokio::test]
async fn deliveries_settle_once_every_begun_delivery_ended() {
    // Arrange
    let fixture = SteeringFixture::new(&[], Vec::new());
    let unused_steering = fixture.steering(channel_with(&[], &[])).await;
    let steering = fixture.steering(channel_with(&["first"], &[])).await;
    let deliveries = Arc::clone(&fixture.deliveries);
    let settled = tokio::spawn(async move { deliveries.settled().await });

    // Act
    tokio::task::yield_now().await;
    let settled_with_two_in_flight = settled.is_finished();
    drop(unused_steering);
    tokio::task::yield_now().await;
    let settled_with_one_in_flight = settled.is_finished();
    steering.run(vec![steered(1, "first")]).await;
    settled.await.expect("settled task");

    // Assert
    assert!(!settled_with_two_in_flight);
    assert!(!settled_with_one_in_flight);
}
