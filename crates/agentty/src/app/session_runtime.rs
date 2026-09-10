//! Foreground-owned session workflow actor and cloneable control handle.
//!
//! [`SessionRuntime`] owns the live [`SessionManager`] plus a bounded command
//! mailbox. The terminal runtime drives accepted commands on the foreground
//! task so reducer-owned render snapshots and session handles remain coherent,
//! while [`SessionRuntimeHandle`] gives background coordinators a cloneable
//! `Send + Sync` capability without sharing all of [`crate::app::App`] behind
//! a mutex.

use std::future::poll_fn;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

use ag_session::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CreateSessionRequest, ReviewRequest,
    Session, SessionError, SessionId,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::app::SessionManager;

/// Maximum number of accepted API commands waiting for foreground execution.
const SESSION_RUNTIME_COMMAND_CAPACITY: usize = 32;
/// Stable error returned after the foreground session runtime has stopped.
const SESSION_RUNTIME_UNAVAILABLE: &str = "Session runtime is unavailable";

/// One command accepted by the session runtime actor.
pub(crate) enum SessionRuntimeCommand {
    /// Creates one session from an explicit API request.
    Create {
        request: CreateSessionRequest,
        response_tx: oneshot::Sender<Result<SessionId, SessionError>>,
    },
    /// Loads one complete session aggregate.
    Get {
        response_tx: oneshot::Sender<Result<Option<Session>, SessionError>>,
        session_id: SessionId,
    },
    /// Sends one user message.
    SendMessage {
        access: SessionRuntimeAccess,
        message: String,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Submits one coordinator-owned turn directly on the session worker.
    SubmitCoordinatorMessage {
        request: CoordinatorMessageRequest,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Answers one complete clarification-question set.
    AnswerQuestions {
        access: SessionRuntimeAccess,
        request: AnswerQuestionsRequest,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Cancels one session.
    Cancel {
        access: SessionRuntimeAccess,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Enqueues one session for merge.
    Merge {
        access: SessionRuntimeAccess,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Queues publication of one session branch and creates or refreshes its
    /// review request.
    CreateReviewRequest {
        access: SessionRuntimeAccess,
        response_tx: oneshot::Sender<Result<ReviewRequest, SessionError>>,
        session_id: SessionId,
    },
}

/// Capability level attached to one programmatic session request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionRuntimeAccess {
    /// Ordinary frontend or public API request.
    User,
    /// Coordinator-owned request for a managed orchestration worker.
    Coordinator,
}

/// Cloneable control capability for the foreground session runtime.
#[derive(Clone)]
pub(crate) struct SessionRuntimeHandle {
    access: SessionRuntimeAccess,
    command_tx: mpsc::Sender<SessionRuntimeCommand>,
    consumer_state: Arc<SessionRuntimeConsumerState>,
}

impl SessionRuntimeHandle {
    /// Creates one session through the runtime actor.
    pub(crate) async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.request(|response_tx| SessionRuntimeCommand::Create {
            request,
            response_tx,
        })
        .await
    }

    /// Loads one complete session aggregate through the runtime actor.
    pub(crate) async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Session>, SessionError> {
        let session_id = session_id.clone();

        self.request(|response_tx| SessionRuntimeCommand::Get {
            response_tx,
            session_id,
        })
        .await
    }

    /// Sends one message through the runtime actor.
    pub(crate) async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request(|response_tx| SessionRuntimeCommand::SendMessage {
            access: self.access,
            message,
            response_tx,
            session_id,
        })
        .await
    }

    /// Submits one coordinator-owned turn through the runtime actor.
    pub(crate) async fn submit_coordinator_message(
        &self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request(
            |response_tx| SessionRuntimeCommand::SubmitCoordinatorMessage {
                request,
                response_tx,
                session_id,
            },
        )
        .await
    }

    /// Answers one complete clarification-question set through the runtime
    /// actor.
    pub(crate) async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request(|response_tx| SessionRuntimeCommand::AnswerQuestions {
            access: self.access,
            request,
            response_tx,
            session_id,
        })
        .await
    }

    /// Cancels one session through the runtime actor.
    pub(crate) async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request(|response_tx| SessionRuntimeCommand::Cancel {
            access: self.access,
            response_tx,
            session_id,
        })
        .await
    }

    /// Enqueues one session for merge through the runtime actor.
    pub(crate) async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request(|response_tx| SessionRuntimeCommand::Merge {
            access: self.access,
            response_tx,
            session_id,
        })
        .await
    }

    /// Queues creation or refresh of one review request through the runtime
    /// actor.
    pub(crate) async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        let session_id = session_id.clone();

        self.request(|response_tx| SessionRuntimeCommand::CreateReviewRequest {
            access: self.access,
            response_tx,
            session_id,
        })
        .await
    }

    /// Enqueues one typed command and waits for its per-command response.
    async fn request<ResultValue>(
        &self,
        command: impl FnOnce(
            oneshot::Sender<Result<ResultValue, SessionError>>,
        ) -> SessionRuntimeCommand,
    ) -> Result<ResultValue, SessionError> {
        let mut consumer_rx = self.consumer_state.subscribe();
        let consumer_is_active = *consumer_rx.borrow_and_update();
        if !consumer_is_active {
            return Err(SessionError::Operation(
                SESSION_RUNTIME_UNAVAILABLE.to_string(),
            ));
        }

        let (response_tx, response_rx) = oneshot::channel();
        tokio::select! {
            biased;
            _ = consumer_rx.wait_for(|consumer_is_active| !*consumer_is_active) => {
                return Err(SessionError::Operation(
                    SESSION_RUNTIME_UNAVAILABLE.to_string(),
                ));
            }
            result = self.command_tx.send(command(response_tx)) => {
                result.map_err(|_| {
                    SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
                })?;
            }
        }

        tokio::select! {
            biased;
            result = response_rx => {
                result.map_err(|_| {
                    SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
                })?
            }
            _ = consumer_rx.wait_for(|consumer_is_active| !*consumer_is_active) => {
                Err(SessionError::Operation(
                    SESSION_RUNTIME_UNAVAILABLE.to_string(),
                ))
            }
        }
    }
}

/// Shared foreground-consumer lifecycle observed by runtime handles.
struct SessionRuntimeConsumerState {
    active_count: AtomicUsize,
    active_tx: watch::Sender<bool>,
}

impl SessionRuntimeConsumerState {
    /// Creates an inactive consumer lifecycle signal.
    fn new() -> Self {
        let (active_tx, _active_rx) = watch::channel(false);

        Self {
            active_count: AtomicUsize::new(0),
            active_tx,
        }
    }

    /// Registers one foreground consumer until the returned guard drops.
    fn enter(self: &Arc<Self>) -> SessionRuntimeConsumerGuard {
        if self.active_count.fetch_add(1, Ordering::AcqRel) == 0 {
            self.active_tx.send_replace(true);
        }

        SessionRuntimeConsumerGuard {
            state: Arc::clone(self),
        }
    }

    /// Subscribes to foreground-consumer availability changes.
    fn subscribe(&self) -> watch::Receiver<bool> {
        self.active_tx.subscribe()
    }
}

/// Registration guard for one foreground session-command consumer.
pub(crate) struct SessionRuntimeConsumerGuard {
    state: Arc<SessionRuntimeConsumerState>,
}

impl Drop for SessionRuntimeConsumerGuard {
    fn drop(&mut self) {
        if self.state.active_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.state.active_tx.send_replace(false);
        }
    }
}

/// Session workflow owner and foreground-driven actor mailbox.
pub(crate) struct SessionRuntime {
    command_rx: mpsc::Receiver<SessionRuntimeCommand>,
    command_tx: mpsc::Sender<SessionRuntimeCommand>,
    consumer_state: Arc<SessionRuntimeConsumerState>,
    manager: SessionManager,
}

impl SessionRuntime {
    /// Wraps the loaded session manager and creates its bounded actor mailbox.
    pub(crate) fn new(manager: SessionManager) -> Self {
        let (command_tx, command_rx) = mpsc::channel(SESSION_RUNTIME_COMMAND_CAPACITY);

        Self {
            command_rx,
            command_tx,
            consumer_state: Arc::new(SessionRuntimeConsumerState::new()),
            manager,
        }
    }

    /// Returns a cloneable control handle for background or frontend callers.
    pub(crate) fn handle(&self) -> SessionRuntimeHandle {
        SessionRuntimeHandle {
            access: SessionRuntimeAccess::User,
            command_tx: self.command_tx.clone(),
            consumer_state: Arc::clone(&self.consumer_state),
        }
    }

    /// Returns a handle authorized to mutate coordinator-owned workers.
    pub(crate) fn coordinator_handle(&self) -> SessionRuntimeHandle {
        SessionRuntimeHandle {
            access: SessionRuntimeAccess::Coordinator,
            command_tx: self.command_tx.clone(),
            consumer_state: Arc::clone(&self.consumer_state),
        }
    }

    /// Registers a foreground command consumer for the guard's lifetime.
    pub(crate) fn foreground_consumer(&self) -> SessionRuntimeConsumerGuard {
        self.consumer_state.enter()
    }

    /// Waits for the next accepted actor command.
    ///
    /// The receiver cannot close while the runtime is alive because the
    /// runtime retains its own sender alongside the public handles.
    pub(crate) async fn next_command(&mut self) -> SessionRuntimeCommand {
        poll_fn(|context| match self.command_rx.poll_recv(context) {
            Poll::Ready(Some(command)) => Poll::Ready(command),
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        })
        .await
    }
}

impl From<SessionManager> for SessionRuntime {
    fn from(manager: SessionManager) -> Self {
        Self::new(manager)
    }
}

impl Deref for SessionRuntime {
    type Target = SessionManager;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

impl DerefMut for SessionRuntime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.manager
    }
}

#[cfg(test)]
#[path = "session_runtime_test.rs"]
mod tests;
