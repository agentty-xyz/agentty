use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context as TaskContext, Poll};
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
        /// Provider HTTP status used as `error.type`, when available.
        http_status: Option<u16>,
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
        /// Provider-assigned identifier for this tool call.
        provider_call_id: String,
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

/// Observable outcome of one provider-neutral model request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelResponseType {
    /// Terminal, schema-validated structured output.
    Output,
    /// Native continuation was unavailable and the request was replayed.
    ResumeUnavailable,
    /// An intermediate native tool request.
    ToolCall,
}

impl fmt::Display for ModelResponseType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output => formatter.write_str("output"),
            Self::ResumeUnavailable => formatter.write_str("resume unavailable"),
            Self::ToolCall => formatter.write_str("tool call"),
        }
    }
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
    /// Durable session coordination or persistence failed.
    Session,
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
            Self::Session => crate::telemetry::ERROR_SESSION,
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

    /// Enters ambient state for one poll of the identified operation.
    #[doc(hidden)]
    fn enter_operation(
        &self,
        _operation_id: LifecycleId,
    ) -> Option<Box<dyn LifecycleOperationGuard>> {
        None
    }
}

/// Guard for ambient state installed while an operation future is polled.
#[doc(hidden)]
pub trait LifecycleOperationGuard {}

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

    fn enter_operation(
        &self,
        operation_id: LifecycleId,
    ) -> Option<Box<dyn LifecycleOperationGuard>> {
        let guards = self
            .observers
            .iter()
            .filter_map(|observer| {
                catch_unwind(AssertUnwindSafe(|| observer.enter_operation(operation_id)))
                    .ok()
                    .flatten()
            })
            .collect::<Vec<_>>();
        (!guards.is_empty()).then(|| {
            Box::new(LifecycleOperationGuardSet { guards }) as Box<dyn LifecycleOperationGuard>
        })
    }
}

struct LifecycleOperationGuardSet {
    guards: Vec<Box<dyn LifecycleOperationGuard>>,
}

impl LifecycleOperationGuard for LifecycleOperationGuardSet {}

impl Drop for LifecycleOperationGuardSet {
    fn drop(&mut self) {
        while let Some(guard) = self.guards.pop() {
            drop_operation_guard(guard);
        }
    }
}

struct IsolatedLifecycleOperationGuard {
    guard: Option<Box<dyn LifecycleOperationGuard>>,
}

impl IsolatedLifecycleOperationGuard {
    fn new(guard: Option<Box<dyn LifecycleOperationGuard>>) -> Self {
        Self { guard }
    }
}

impl Drop for IsolatedLifecycleOperationGuard {
    fn drop(&mut self) {
        if let Some(guard) = self.guard.take() {
            drop_operation_guard(guard);
        }
    }
}

fn drop_operation_guard(guard: Box<dyn LifecycleOperationGuard>) {
    let _ = catch_unwind(AssertUnwindSafe(|| drop(guard)));
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
        provider_call_id: String,
        tool_name: String,
        turn_id: Option<LifecycleId>,
    ) -> Option<ToolLifecycle> {
        let turn_id = turn_id?;
        let tool_call_id = self.next_id()?;
        self.emit(LifecycleEventKind::ToolRequested {
            provider_call_id,
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

    fn scope<F>(&self, operation_id: LifecycleId, future: F) -> LifecycleOperation<F>
    where
        F: Future,
    {
        LifecycleOperation {
            future: Box::pin(future),
            observer: self.state.as_ref().map(|state| Arc::clone(&state.observer)),
            operation_id,
        }
    }
}

struct LifecycleOperation<F> {
    future: Pin<Box<F>>,
    observer: Option<Arc<dyn LifecycleObserver>>,
    operation_id: LifecycleId,
}

impl<F> Future for LifecycleOperation<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, task_context: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let operation = self.get_mut();
        let guard = operation.observer.as_ref().and_then(|observer| {
            catch_unwind(AssertUnwindSafe(|| {
                observer.enter_operation(operation.operation_id)
            }))
            .ok()
            .flatten()
        });
        let _guard = IsolatedLifecycleOperationGuard::new(guard);

        operation.future.as_mut().poll(task_context)
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
    pub(crate) fn scope<F>(&self, future: F) -> impl Future<Output = F::Output>
    where
        F: Future,
    {
        self.emitter.scope(self.model_call_id, future)
    }

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

    pub(crate) fn failed(mut self, error_type: ModelErrorType, http_status: Option<u16>) {
        self.active = false;
        self.emitter.emit(LifecycleEventKind::ModelRequestFailed {
            duration: self.started_at.elapsed(),
            error_type,
            http_status,
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
    pub(crate) fn scope<F>(&self, future: F) -> impl Future<Output = F::Output>
    where
        F: Future,
    {
        self.emitter.scope(self.tool_call_id, future)
    }

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
#[path = "lifecycle_test.rs"]
mod tests;
