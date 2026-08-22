use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use crate::model::{CompletionMetadata, ModelErrorType, ModelMetadata};

/// Stream-local identifier that correlates lifecycle events for one operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LifecycleId(u64);

impl LifecycleId {
    /// Returns the stream-local numeric identifier.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// One ordered, metadata-only harness lifecycle event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleEvent {
    kind: LifecycleEventKind,
    sequence: u64,
}

impl LifecycleEvent {
    /// Returns the event's zero-based position in its observer stream.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the typed lifecycle fact carried by this event.
    pub fn kind(&self) -> &LifecycleEventKind {
        &self.kind
    }
}

/// Typed metadata-only facts emitted while model turns execute.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LifecycleEventKind {
    /// A complete harness turn started.
    TurnStarted {
        /// Identifier shared by all events in the turn.
        turn_id: LifecycleId,
    },
    /// A complete harness turn finished successfully.
    TurnCompleted {
        /// Elapsed turn time.
        duration: Duration,
        /// Identifier shared by all events in the turn.
        turn_id: LifecycleId,
    },
    /// A complete harness turn failed or was cancelled.
    TurnFailed {
        /// Elapsed turn time.
        duration: Duration,
        /// Stable failure classification.
        error_type: TurnErrorType,
        /// Identifier shared by all events in the turn.
        turn_id: LifecycleId,
    },
    /// One provider-neutral model request started.
    ModelRequestStarted {
        /// Identifier shared by this request's lifecycle events.
        model_call_id: LifecycleId,
        /// Validated provider and requested-model identity, when available.
        model: Option<ModelMetadata>,
        /// Zero-based model-call position within a turn.
        request_index: u64,
        /// Owning turn, or `None` for a standalone model request.
        turn_id: Option<LifecycleId>,
    },
    /// One provider-neutral model request completed successfully.
    ModelRequestCompleted {
        /// Normalized provider completion metadata, when available.
        completion: Option<CompletionMetadata>,
        /// Elapsed model-request time.
        duration: Duration,
        /// Identifier shared by this request's lifecycle events.
        model_call_id: LifecycleId,
        /// Shape of the provider-neutral response.
        response_type: ModelResponseType,
        /// Owning turn, or `None` for a standalone model request.
        turn_id: Option<LifecycleId>,
    },
    /// One provider-neutral model request failed.
    ModelRequestFailed {
        /// Elapsed model-request time.
        duration: Duration,
        /// Stable failure classification.
        error_type: ModelErrorType,
        /// Identifier shared by this request's lifecycle events.
        model_call_id: LifecycleId,
        /// Owning turn, or `None` for a standalone model request.
        turn_id: Option<LifecycleId>,
    },
    /// One in-flight provider-neutral model request was cancelled.
    ModelRequestCancelled {
        /// Elapsed model-request time before cancellation.
        duration: Duration,
        /// Identifier shared by this request's lifecycle events.
        model_call_id: LifecycleId,
        /// Owning turn, or `None` for a standalone model request.
        turn_id: Option<LifecycleId>,
    },
    /// The model requested one tool operation.
    ToolRequested {
        /// Identifier shared by this tool operation's lifecycle events.
        tool_call_id: LifecycleId,
        /// Bounded built-in tool name.
        tool_name: String,
        /// Owning turn.
        turn_id: LifecycleId,
    },
    /// One allowed tool operation started execution.
    ToolStarted {
        /// Identifier shared by this tool operation's lifecycle events.
        tool_call_id: LifecycleId,
        /// Owning turn.
        turn_id: LifecycleId,
    },
    /// One allowed tool operation completed successfully.
    ToolCompleted {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Identifier shared by this tool operation's lifecycle events.
        tool_call_id: LifecycleId,
        /// Owning turn.
        turn_id: LifecycleId,
    },
    /// One requested tool was denied by policy.
    ToolDenied {
        /// Elapsed time before denial.
        duration: Duration,
        /// Identifier shared by this tool operation's lifecycle events.
        tool_call_id: LifecycleId,
        /// Owning turn.
        turn_id: LifecycleId,
    },
    /// One requested tool failed or was cancelled.
    ToolFailed {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Stable failure classification.
        error_type: ToolErrorType,
        /// Identifier shared by this tool operation's lifecycle events.
        tool_call_id: LifecycleId,
        /// Owning turn.
        turn_id: LifecycleId,
    },
}

/// Shape of a successful provider-neutral model response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelResponseType {
    /// Terminal, schema-validated structured output.
    Output,
    /// An intermediate native tool request.
    ToolCall,
}

/// Stable, low-cardinality reason that a harness turn failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TurnErrorType {
    /// The turn future was dropped before completion.
    Cancelled,
    /// A model request failed.
    Model(ModelErrorType),
    /// A repository-scoped tool failed.
    Tool,
    /// A requested tool was denied by policy.
    ToolDenied,
    /// The turn exceeded its configured tool-call limit.
    ToolCallLimit,
    /// A repository-scoped tool was enabled without a repository root.
    RepositoryRequired,
}

impl TurnErrorType {
    /// Returns the stable value intended for telemetry attributes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => crate::telemetry::ERROR_CANCELLED,
            Self::Model(error_type) => error_type.as_str(),
            Self::Tool => crate::telemetry::ERROR_TOOL_EXECUTION,
            Self::ToolDenied => crate::telemetry::ERROR_TOOL_DENIED,
            Self::ToolCallLimit => crate::telemetry::ERROR_TOOL_CALL_LIMIT,
            Self::RepositoryRequired => crate::telemetry::ERROR_REPOSITORY_REQUIRED,
        }
    }
}

/// Stable, low-cardinality reason that a tool operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolErrorType {
    /// The turn future was dropped during tool execution.
    Cancelled,
    /// The configured per-turn tool-call limit was reached.
    CallLimit,
    /// The allowed tool failed while executing or encoding its result.
    Execution,
}

impl ToolErrorType {
    /// Returns the stable value intended for telemetry attributes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => crate::telemetry::ERROR_CANCELLED,
            Self::CallLimit => crate::telemetry::ERROR_TOOL_CALL_LIMIT,
            Self::Execution => crate::telemetry::ERROR_TOOL_EXECUTION,
        }
    }
}

/// Synchronous destination for ordered harness lifecycle events.
///
/// Callback entry follows sequence order across threads and permits same-thread
/// reentrancy. Observer panics never change the model or turn result.
pub trait LifecycleObserver: Send + Sync {
    /// Receives one event before the operation continues.
    fn observe(&self, event: LifecycleEvent);
}

impl<Observe> LifecycleObserver for Observe
where
    Observe: Fn(LifecycleEvent) + Send + Sync,
{
    fn observe(&self, event: LifecycleEvent) {
        self(event);
    }
}

/// Ordered fan-out to multiple lifecycle observers.
///
/// Observers run in registration order. A panic in one observer does not
/// prevent later observers from receiving the event. Same-thread reentrant
/// events are queued until every observer receives the current event, keeping
/// each observer's stream in sequence order.
pub struct LifecycleObserverSet {
    available: Condvar,
    observers: Vec<Arc<dyn LifecycleObserver>>,
    state: Mutex<ObserverSetState>,
}

impl LifecycleObserverSet {
    /// Creates a fan-out containing `observer`.
    pub fn new(observer: impl LifecycleObserver + 'static) -> Self {
        Self {
            available: Condvar::new(),
            observers: vec![Arc::new(observer)],
            state: Mutex::new(ObserverSetState::default()),
        }
    }

    /// Appends an observer to the fan-out.
    #[must_use]
    pub fn with_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.observers.push(Arc::new(observer));

        self
    }

    fn deliver(&self, event: &LifecycleEvent) {
        for observer in &self.observers {
            let event = event.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| observer.observe(event)));
        }
    }
}

impl LifecycleObserver for LifecycleObserverSet {
    fn observe(&self, event: LifecycleEvent) {
        let current_thread = thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.owner == Some(current_thread) {
            state.pending.push_back(event);

            return;
        }
        state = self
            .available
            .wait_while(state, |state| state.owner.is_some())
            .unwrap_or_else(PoisonError::into_inner);
        state.owner = Some(current_thread);
        drop(state);

        let mut event = event;
        loop {
            self.deliver(&event);

            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(next_event) = state.pending.pop_front() else {
                state.owner = None;
                self.available.notify_one();

                return;
            };
            drop(state);
            event = next_event;
        }
    }
}

#[derive(Default)]
struct ObserverSetState {
    owner: Option<ThreadId>,
    pending: VecDeque<LifecycleEvent>,
}

#[derive(Clone, Default)]
pub(crate) struct LifecycleEmitter {
    state: Option<Arc<LifecycleState>>,
}

impl LifecycleEmitter {
    pub(crate) fn new(observer: impl LifecycleObserver + 'static) -> Self {
        Self {
            state: Some(Arc::new(LifecycleState {
                delivery: DeliveryCoordinator::default(),
                next_id: AtomicU64::new(0),
                observer: Arc::new(observer),
            })),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    pub(crate) fn start_turn(&self) -> Option<TurnLifecycle> {
        let turn_id = self.next_id()?;
        self.emit(LifecycleEventKind::TurnStarted { turn_id });

        Some(TurnLifecycle {
            active: true,
            emitter: self.clone(),
            started_at: Instant::now(),
            turn_id,
        })
    }

    pub(crate) fn start_model_request(
        &self,
        model: Option<ModelMetadata>,
        request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Option<ModelRequestLifecycle> {
        let model_call_id = self.next_id()?;
        self.emit(LifecycleEventKind::ModelRequestStarted {
            model_call_id,
            model,
            request_index,
            turn_id,
        });

        Some(ModelRequestLifecycle {
            active: true,
            emitter: self.clone(),
            model_call_id,
            started_at: Instant::now(),
            turn_id,
        })
    }

    pub(crate) fn request_tool(
        &self,
        tool_name: String,
        turn_id: Option<LifecycleId>,
    ) -> Option<ToolLifecycle> {
        let turn_id = turn_id?;
        let tool_call_id = self.next_id()?;
        self.emit(LifecycleEventKind::ToolRequested {
            tool_call_id,
            tool_name,
            turn_id,
        });

        Some(ToolLifecycle {
            active: true,
            emitter: self.clone(),
            started_at: Instant::now(),
            tool_call_id,
            turn_id,
        })
    }

    fn next_id(&self) -> Option<LifecycleId> {
        self.state
            .as_ref()
            .map(|state| LifecycleId(state.next_id.fetch_add(1, Ordering::Relaxed)))
    }

    fn emit(&self, kind: LifecycleEventKind) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let delivery = state.delivery.enter();
        let event = LifecycleEvent {
            kind,
            sequence: delivery.sequence(),
        };

        let _ = catch_unwind(AssertUnwindSafe(|| state.observer.observe(event)));
    }
}

struct LifecycleState {
    delivery: DeliveryCoordinator,
    next_id: AtomicU64,
    observer: Arc<dyn LifecycleObserver>,
}

#[derive(Default)]
struct DeliveryCoordinator {
    available: Condvar,
    state: Mutex<DeliveryState>,
}

impl DeliveryCoordinator {
    fn enter(&self) -> DeliveryPermit<'_> {
        let current_thread = thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state = self
            .available
            .wait_while(state, |state| {
                state
                    .owner
                    .as_ref()
                    .is_some_and(|owner| *owner != current_thread)
            })
            .unwrap_or_else(PoisonError::into_inner);
        state.depth += 1;
        state.owner = Some(current_thread);
        let sequence = state.next_sequence;
        state.next_sequence += 1;

        DeliveryPermit {
            coordinator: self,
            sequence,
        }
    }

    fn exit(&self) {
        let current_thread = thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        debug_assert_eq!(state.owner, Some(current_thread));
        state.depth -= 1;
        if state.depth == 0 {
            state.owner = None;
            self.available.notify_one();
        }
    }
}

#[derive(Default)]
struct DeliveryState {
    depth: usize,
    next_sequence: u64,
    owner: Option<ThreadId>,
}

struct DeliveryPermit<'coordinator> {
    coordinator: &'coordinator DeliveryCoordinator,
    sequence: u64,
}

impl DeliveryPermit<'_> {
    fn sequence(&self) -> u64 {
        self.sequence
    }
}

impl Drop for DeliveryPermit<'_> {
    fn drop(&mut self) {
        self.coordinator.exit();
    }
}

pub(crate) struct TurnLifecycle {
    active: bool,
    emitter: LifecycleEmitter,
    started_at: Instant,
    turn_id: LifecycleId,
}

impl TurnLifecycle {
    pub(crate) fn id(&self) -> LifecycleId {
        self.turn_id
    }

    pub(crate) fn completed(mut self) {
        self.active = false;
        self.emitter.emit(LifecycleEventKind::TurnCompleted {
            duration: self.started_at.elapsed(),
            turn_id: self.turn_id,
        });
    }

    pub(crate) fn failed(mut self, error_type: TurnErrorType) {
        self.active = false;
        self.emitter.emit(LifecycleEventKind::TurnFailed {
            duration: self.started_at.elapsed(),
            error_type,
            turn_id: self.turn_id,
        });
    }
}

impl Drop for TurnLifecycle {
    fn drop(&mut self) {
        if self.active {
            self.emitter.emit(LifecycleEventKind::TurnFailed {
                duration: self.started_at.elapsed(),
                error_type: TurnErrorType::Cancelled,
                turn_id: self.turn_id,
            });
        }
    }
}

pub(crate) struct ModelRequestLifecycle {
    active: bool,
    emitter: LifecycleEmitter,
    model_call_id: LifecycleId,
    started_at: Instant,
    turn_id: Option<LifecycleId>,
}

impl ModelRequestLifecycle {
    pub(crate) fn completed(
        mut self,
        completion: Option<CompletionMetadata>,
        response_type: ModelResponseType,
    ) {
        self.active = false;
        self.emitter
            .emit(LifecycleEventKind::ModelRequestCompleted {
                completion,
                duration: self.started_at.elapsed(),
                model_call_id: self.model_call_id,
                response_type,
                turn_id: self.turn_id,
            });
    }

    pub(crate) fn failed(mut self, error_type: ModelErrorType) {
        self.active = false;
        self.emitter.emit(LifecycleEventKind::ModelRequestFailed {
            duration: self.started_at.elapsed(),
            error_type,
            model_call_id: self.model_call_id,
            turn_id: self.turn_id,
        });
    }
}

impl Drop for ModelRequestLifecycle {
    fn drop(&mut self) {
        if self.active {
            self.emitter
                .emit(LifecycleEventKind::ModelRequestCancelled {
                    duration: self.started_at.elapsed(),
                    model_call_id: self.model_call_id,
                    turn_id: self.turn_id,
                });
        }
    }
}

pub(crate) struct ToolLifecycle {
    active: bool,
    emitter: LifecycleEmitter,
    started_at: Instant,
    tool_call_id: LifecycleId,
    turn_id: LifecycleId,
}

impl ToolLifecycle {
    pub(crate) fn started(&mut self) {
        self.emitter.emit(LifecycleEventKind::ToolStarted {
            tool_call_id: self.tool_call_id,
            turn_id: self.turn_id,
        });
        self.started_at = Instant::now();
    }

    pub(crate) fn completed(mut self) {
        self.active = false;
        self.emitter.emit(LifecycleEventKind::ToolCompleted {
            duration: self.started_at.elapsed(),
            tool_call_id: self.tool_call_id,
            turn_id: self.turn_id,
        });
    }

    pub(crate) fn denied(mut self) {
        self.active = false;
        self.emitter.emit(LifecycleEventKind::ToolDenied {
            duration: self.started_at.elapsed(),
            tool_call_id: self.tool_call_id,
            turn_id: self.turn_id,
        });
    }

    pub(crate) fn failed(mut self, error_type: ToolErrorType) {
        self.active = false;
        self.emitter.emit(LifecycleEventKind::ToolFailed {
            duration: self.started_at.elapsed(),
            error_type,
            tool_call_id: self.tool_call_id,
            turn_id: self.turn_id,
        });
    }
}

impl Drop for ToolLifecycle {
    fn drop(&mut self) {
        if self.active {
            self.emitter.emit(LifecycleEventKind::ToolFailed {
                duration: self.started_at.elapsed(),
                error_type: ToolErrorType::Cancelled,
                tool_call_id: self.tool_call_id,
                turn_id: self.turn_id,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Barrier, Mutex, mpsc};

    use super::*;

    fn completion_metadata() -> CompletionMetadata {
        CompletionMetadata::new("stop".to_string(), None, None, None, None)
    }

    fn recording_emitter() -> (LifecycleEmitter, Arc<Mutex<Vec<LifecycleEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let emitter = LifecycleEmitter::new(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });

        (emitter, events)
    }

    #[test]
    fn observer_set_preserves_order_during_reentrant_delivery() {
        // Arrange
        let emitter_holder = Arc::new(Mutex::new(None::<LifecycleEmitter>));
        let first_emitter = Arc::clone(&emitter_holder);
        let deliveries = Arc::new(Mutex::new(Vec::new()));
        let first_deliveries = Arc::clone(&deliveries);
        let second_deliveries = Arc::clone(&deliveries);
        let observers = LifecycleObserverSet::new(move |event: LifecycleEvent| {
            let sequence = event.sequence();
            first_deliveries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((0, sequence));
            if sequence == 0 {
                first_emitter
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .expect("emitter should be available to its observer")
                    .emit(LifecycleEventKind::TurnStarted {
                        turn_id: LifecycleId(1),
                    });
            }
        })
        .with_observer(move |event: LifecycleEvent| {
            second_deliveries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((1, event.sequence()));
        });
        let emitter = LifecycleEmitter::new(observers);
        *emitter_holder
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(emitter.clone());

        // Act
        emitter.emit(LifecycleEventKind::TurnStarted {
            turn_id: LifecycleId(0),
        });

        // Assert
        assert_eq!(
            *deliveries.lock().unwrap_or_else(PoisonError::into_inner),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );
    }

    #[test]
    fn observer_set_isolates_each_observer_panic() {
        // Arrange
        let deliveries = Arc::new(Mutex::new(Vec::new()));
        let recorded_deliveries = Arc::clone(&deliveries);
        let observers = LifecycleObserverSet::new(|_| {
            std::panic::resume_unwind(Box::new("observer failed"));
        })
        .with_observer(move |event: LifecycleEvent| {
            recorded_deliveries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event.sequence());
        });
        let emitter = LifecycleEmitter::new(observers);

        // Act
        emitter.emit(LifecycleEventKind::TurnStarted {
            turn_id: LifecycleId(0),
        });
        emitter.emit(LifecycleEventKind::TurnStarted {
            turn_id: LifecycleId(1),
        });

        // Assert
        assert_eq!(
            *deliveries.lock().unwrap_or_else(PoisonError::into_inner),
            vec![0, 1]
        );
    }

    #[test]
    fn emits_ordered_terminal_events_for_every_operation() {
        // Arrange
        let (emitter, events) = recording_emitter();
        let turn_id = LifecycleId(41);

        // Act
        emitter
            .start_turn()
            .expect("turn should be observed")
            .completed();
        emitter
            .start_turn()
            .expect("turn should be observed")
            .failed(TurnErrorType::RepositoryRequired);
        drop(emitter.start_turn().expect("turn should be observed"));
        emitter
            .start_model_request(None, 0, Some(turn_id))
            .expect("model request should be observed")
            .completed(Some(completion_metadata()), ModelResponseType::Output);
        emitter
            .start_model_request(None, 1, Some(turn_id))
            .expect("model request should be observed")
            .failed(ModelErrorType::Provider);
        drop(
            emitter
                .start_model_request(None, 2, Some(turn_id))
                .expect("model request should be observed"),
        );
        let mut completed_tool = emitter
            .request_tool("read".to_string(), Some(turn_id))
            .expect("tool should be observed");
        completed_tool.started();
        completed_tool.completed();
        emitter
            .request_tool("read".to_string(), Some(turn_id))
            .expect("tool should be observed")
            .denied();
        emitter
            .request_tool("read".to_string(), Some(turn_id))
            .expect("tool should be observed")
            .failed(ToolErrorType::CallLimit);
        drop(
            emitter
                .request_tool("read".to_string(), Some(turn_id))
                .expect("tool should be observed"),
        );

        // Assert
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert_eq!(
            events
                .iter()
                .map(LifecycleEvent::sequence)
                .collect::<Vec<_>>(),
            (0..events.len() as u64).collect::<Vec<_>>()
        );
        assert_eq!(
            events[0].kind(),
            &LifecycleEventKind::TurnStarted {
                turn_id: LifecycleId(0),
            }
        );
        assert!(matches!(
            events[2].kind(),
            LifecycleEventKind::TurnStarted { turn_id } if turn_id.get() == 1
        ));
        assert!(matches!(
            events[5].kind(),
            LifecycleEventKind::TurnFailed {
                error_type: TurnErrorType::Cancelled,
                ..
            }
        ));
        assert!(matches!(
            events[11].kind(),
            LifecycleEventKind::ModelRequestCancelled { .. }
        ));
        assert!(matches!(
            events[14].kind(),
            LifecycleEventKind::ToolCompleted { .. }
        ));
        assert!(matches!(
            events[16].kind(),
            LifecycleEventKind::ToolDenied { .. }
        ));
        assert!(matches!(
            events[18].kind(),
            LifecycleEventKind::ToolFailed {
                error_type: ToolErrorType::CallLimit,
                ..
            }
        ));
        assert!(matches!(
            events[20].kind(),
            LifecycleEventKind::ToolFailed {
                error_type: ToolErrorType::Cancelled,
                ..
            }
        ));
    }

    #[test]
    fn disabled_emitter_allocates_no_lifecycle_state() {
        // Arrange
        let emitter = LifecycleEmitter::default();

        // Act
        let turn = emitter.start_turn();
        let model_request = emitter.start_model_request(None, 0, None);
        let tool = emitter.request_tool("read".to_string(), None);
        emitter.emit(LifecycleEventKind::TurnStarted {
            turn_id: LifecycleId(0),
        });

        // Assert
        assert!(!emitter.is_enabled());
        assert!(turn.is_none());
        assert!(model_request.is_none());
        assert!(tool.is_none());
    }

    #[test]
    fn observer_panics_do_not_change_control_flow() {
        // Arrange
        let emitter = LifecycleEmitter::new(|_| {
            std::panic::resume_unwind(Box::new("observer failed"));
        });

        // Act
        let turn = emitter.start_turn().expect("turn should still start");
        turn.completed();

        // Assert
        assert!(emitter.is_enabled());
    }

    #[test]
    fn serializes_concurrent_observer_delivery() {
        // Arrange
        let release_first = Arc::new((Mutex::new(false), Condvar::new()));
        let observer_release = Arc::clone(&release_first);
        let (delivered, delivery) = mpsc::channel();
        let emitter = LifecycleEmitter::new(move |event: LifecycleEvent| {
            delivered
                .send(event.sequence())
                .expect("delivery receiver should remain available");
            if event.sequence() == 0 {
                let (released, ready) = &*observer_release;
                let released = released.lock().unwrap_or_else(PoisonError::into_inner);
                let _released = ready
                    .wait_while(released, |released| !*released)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        });
        let first_emitter = emitter.clone();
        let second_emitter = emitter;
        let second_ready = Arc::new(Barrier::new(2));
        let thread_ready = Arc::clone(&second_ready);

        // Act
        let first = thread::spawn(move || {
            first_emitter.emit(LifecycleEventKind::TurnStarted {
                turn_id: LifecycleId(0),
            });
        });
        assert_eq!(
            delivery
                .recv_timeout(Duration::from_secs(1))
                .expect("first event should enter the observer"),
            0
        );
        let second = thread::spawn(move || {
            thread_ready.wait();
            second_emitter.emit(LifecycleEventKind::TurnStarted {
                turn_id: LifecycleId(1),
            });
        });
        second_ready.wait();
        let concurrent_delivery = delivery.recv_timeout(Duration::from_millis(100));
        let (released, ready) = &*release_first;
        *released.lock().unwrap_or_else(PoisonError::into_inner) = true;
        ready.notify_one();
        first.join().expect("first emitter should finish");
        second.join().expect("second emitter should finish");

        // Assert
        assert!(concurrent_delivery.is_err());
        assert_eq!(
            delivery
                .recv_timeout(Duration::from_secs(1))
                .expect("second event should follow the first"),
            1
        );
    }

    #[test]
    fn supports_reentrant_observer_delivery() {
        // Arrange
        let emitter_holder = Arc::new(Mutex::new(None::<LifecycleEmitter>));
        let observer_emitter = Arc::clone(&emitter_holder);
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer_events = Arc::clone(&events);
        let emitter = LifecycleEmitter::new(move |event: LifecycleEvent| {
            let sequence = event.sequence();
            observer_events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(sequence);
            if sequence == 0 {
                observer_emitter
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .expect("emitter should be available to its observer")
                    .emit(LifecycleEventKind::TurnStarted {
                        turn_id: LifecycleId(1),
                    });
            }
        });
        *emitter_holder
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(emitter.clone());

        // Act
        emitter.emit(LifecycleEventKind::TurnStarted {
            turn_id: LifecycleId(0),
        });

        // Assert
        assert_eq!(
            *events.lock().unwrap_or_else(PoisonError::into_inner),
            vec![0, 1]
        );
    }

    #[test]
    fn starts_tool_execution_timer_after_started_observer() {
        // Arrange
        let observer_time = Arc::new(Mutex::new(None));
        let recorded_time = Arc::clone(&observer_time);
        let emitter = LifecycleEmitter::new(move |event: LifecycleEvent| {
            if matches!(event.kind(), LifecycleEventKind::ToolStarted { .. }) {
                *recorded_time.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Instant::now());
            }
        });
        let turn_id = LifecycleId(0);
        let mut tool = emitter
            .request_tool("read".to_string(), Some(turn_id))
            .expect("tool should be observed");

        // Act
        tool.started();
        let execution_started_at = tool.started_at;
        tool.completed();

        // Assert
        let observer_time = observer_time
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .expect("started observer should record its time");
        assert!(execution_started_at >= observer_time);
    }
}
