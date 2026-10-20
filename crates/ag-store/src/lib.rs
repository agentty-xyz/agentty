//! Reusable repository contracts and `SQLite` persistence for Agentty sessions.

mod activity;
mod connection;
mod error;
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
mod timestamp;
mod usage;

pub use activity::ActivityRepository;
pub use connection::Database;
pub use error::DbError;
pub(crate) use error::DbResultExt;
pub use operation::{OperationRepository, SessionOperationRow};
pub use project::{ProjectListRow, ProjectRepository, ProjectRow};
pub use repository::AppRepositories;
pub use review::{
    NewSessionReviewCommentResolution, ReviewRepository, SessionReviewCommentResolutionRow,
    SessionReviewRequestRow,
};
pub use session::{
    ForkSessionSnapshot, PersistedSessionCreation, SessionAgentModelRow, SessionDetailRow,
    SessionFocusedReviewRow, SessionListRow, SessionMessageRow, SessionRepository,
    SessionReviewAuditRow, SessionRow, SessionTurnMetadata,
};
pub use session_preparation::{
    SessionPreparationRepository, SessionPreparationRow, SessionPreparationState,
};
pub use setting::SettingRepository;
pub use task::TaskRepository;
pub use task_agent::TaskAgentContextRow;
pub use timestamp::TimestampSource;
pub use usage::{SessionUsageRow, UsageRepository};

#[cfg(test)]
#[path = "test_support_test.rs"]
mod test_support;
