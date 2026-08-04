//! Frontend-neutral session management API and shared session models.
//!
//! Host applications implement [`SessionBackend`] to connect the stable
//! programmatic API to their persistence, agent, Git, and forge workflows.
//! Callers such as future orchestrator sessions use [`SessionService`] without
//! depending on terminal UI state.

mod error;
mod message;
mod model;
mod service;

pub use error::SessionError;
pub use message::{
    SessionMessage, SessionMessageKind, SessionMessageKindParseError, SessionTranscript,
    normalized_message_content, stored_message_content,
};
pub use model::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, Session, SessionId,
    SessionRole, SessionSettings, SessionStatus, SpeedMode, activity_day_key_with_offset,
};
pub use service::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CoordinatorMessageVisibility,
    CreateSessionMode, CreateSessionRequest, QuestionAnswer, SessionBackend, SessionService,
};
