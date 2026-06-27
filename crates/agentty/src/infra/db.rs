//! Database layer for persisting session metadata using `SQLite` via `SQLx`.

mod activity;
mod connection;
mod error;
mod operation;
mod project;
mod repository;
mod review;
mod session;
mod setting;
mod usage;

pub use activity::ActivityRepository;
pub(crate) use activity::SqliteActivityRepository;
pub(crate) use connection::unix_timestamp_now;
pub use connection::{DB_DIR, DB_FILE, Database};
pub use error::DbError;
pub(crate) use operation::SqliteOperationRepository;
pub use operation::{OperationRepository, SessionOperationRow};
pub(crate) use project::SqliteProjectRepository;
pub use project::{ProjectListRow, ProjectRepository, ProjectRow};
pub use repository::AppRepositories;
pub(crate) use review::SqliteReviewRepository;
pub use review::{ReviewRepository, SessionReviewRequestRow};
#[cfg(test)]
pub(crate) use session::SessionJoinRow;
pub(crate) use session::SqliteSessionRepository;
pub use session::{
    PersistedSessionAgentModel, SessionDetailRow, SessionFocusedReviewRow, SessionFollowUpTaskRow,
    SessionListRow, SessionMessageRow, SessionRepository, SessionRow, SessionTurnMetadata,
};
pub(crate) use setting::{SettingRepository, SqliteSettingRepository};
pub(crate) use usage::SqliteUsageRepository;
pub use usage::{SessionUsageRow, UsageRepository};
