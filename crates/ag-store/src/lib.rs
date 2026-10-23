//! Reusable repository contracts and `SQLite` persistence for Agentty sessions.

mod activity;
mod connection;
mod error;
mod execution_graph;
mod operation;
mod project;
mod repository;
mod review;
mod run;
mod session;
mod session_message;
mod session_preparation;
mod session_snapshot;
mod setting;
mod status;
mod task;
mod task_agent;
mod task_agent_turn;
mod task_chain;
mod task_plan;
mod timestamp;
mod usage;

pub use activity::ActivityRepository;
pub use connection::Database;
pub use error::DbError;
pub(crate) use error::DbResultExt;
#[cfg(any(test, feature = "test-utils"))]
pub use execution_graph::MockGraphRepository;
pub use execution_graph::{
    GraphEvidence, GraphExecutionRecord, GraphRepository, StoredGraphEvidence, StoredGraphResult,
};
pub use operation::{OperationRepository, SessionOperationRow};
pub use project::{ProjectListRow, ProjectRepository, ProjectRow};
pub use repository::AppRepositories;
pub use review::{
    NewSessionReviewCommentResolution, ReviewRepository, SessionReviewCommentResolutionRow,
    SessionReviewRequestRow,
};
pub use session::{
    ForkSessionSnapshot, LEGACY_SESSION_AGENT_CLEAR_MARKER, PersistedSessionCreation,
    SessionAgentModelRow, SessionDetailRow, SessionFocusedReviewRow, SessionListRow,
    SessionMessageRow, SessionRepository, SessionReviewAuditRow, SessionRow, SessionTurnMetadata,
};
pub use session_preparation::{
    SessionPreparationRepository, SessionPreparationRow, SessionPreparationState,
};
pub use setting::SettingRepository;
#[cfg(any(test, feature = "test-utils"))]
pub use task::MockTaskRepository;
pub use task::TaskRepository;
pub use task_agent::TaskAgentContextRow;
pub use task_agent_turn::{TaskAgentHistoryRow, TaskAgentTurnMetadata, TaskAgentTurnRow};
pub use timestamp::TimestampSource;
pub use usage::{SessionUsageRow, UsageRepository};

#[cfg(test)]
#[path = "test_support_test.rs"]
mod test_support;
