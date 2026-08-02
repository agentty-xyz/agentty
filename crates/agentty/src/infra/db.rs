//! Database layer for persisting session metadata using `SQLite` via `SQLx`.

mod activity;
mod connection;
mod error;
mod operation;
mod orchestration;
mod project;
mod repository;
mod review;
mod session;
mod session_message;
mod session_snapshot;
mod setting;
mod status;
mod usage;

pub use activity::ActivityRepository;
pub(crate) use activity::SqliteActivityRepository;
pub use connection::{DB_DIR, DB_FILE, Database};
pub use error::DbError;
pub(crate) use error::DbResultExt;
pub(crate) use operation::SqliteOperationRepository;
pub use operation::{OperationRepository, SessionOperationRow};
#[cfg(test)]
pub(crate) use orchestration::MockOrchestrationRepository;
pub(crate) use orchestration::SqliteOrchestrationRepository;
pub use orchestration::{
    OrchestrationRepository, PersistedOrchestrationTask, SessionOrchestrationMetadataRow,
    SessionOrchestrationRow, SessionOrchestrationTaskRow,
};
pub(crate) use project::SqliteProjectRepository;
pub use project::{ProjectListRow, ProjectRepository, ProjectRow};
pub use repository::AppRepositories;
pub(crate) use review::SqliteReviewRepository;
pub use review::{ReviewRepository, SessionReviewRequestRow};
pub(crate) use session::SqliteSessionRepository;
pub use session::{
    ForkSessionSnapshot, PersistedSessionCreation, SessionAgentModelRow, SessionDetailRow,
    SessionFocusedReviewRow, SessionListRow, SessionMessageRow, SessionRepository, SessionRow,
    SessionTurnMetadata,
};
pub(crate) use setting::{SettingRepository, SqliteSettingRepository};
pub(crate) use usage::SqliteUsageRepository;
pub use usage::{SessionUsageRow, UsageRepository};
