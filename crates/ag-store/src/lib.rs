//! Reusable repository contracts and `SQLite` persistence for Agentty sessions.

mod activity;
mod connection;
mod error;
mod execution_graph;
mod operation;
mod project;
mod remote_device;
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
mod task_plan;
mod timestamp;
mod usage;

pub use activity::ActivityRepository;
pub use connection::{BACKGROUND_BUSY_TIMEOUT, Database, INTERACTIVE_BUSY_TIMEOUT};
pub use error::DbError;
pub(crate) use error::DbResultExt;
pub use execution_graph::{
    GRAPH_NODE_ACTIVITY_OPERATION_KIND, GraphEvidence, GraphExecutionRecord, GraphNodeHistory,
    GraphOutputReplyRecord, GraphProposalReplyRecord, GraphProposalRepository, GraphRepository,
    StoredGraphEvidence, StoredGraphResult,
};
#[cfg(any(test, feature = "test-utils"))]
pub use execution_graph::{MockGraphProposalRepository, MockGraphRepository};
pub use operation::{OperationRepository, SessionOperationRow};
pub use project::{ProjectListRow, ProjectRepository, ProjectRow};
#[cfg(any(test, feature = "test-utils"))]
pub use remote_device::MockRemoteDeviceRepository;
pub use remote_device::{RemoteDeviceRepository, RemoteDeviceRow};
pub use repository::AppRepositories;
pub use review::{
    NewSessionReviewCommentResolution, ReviewRepository, SessionReviewCommentResolutionRow,
    SessionReviewRequestRow, SessionReviewWatchRow,
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
pub use usage::{SessionUsageContextRow, SessionUsageRow, UsageRepository};

#[cfg(test)]
#[path = "test_support_test.rs"]
mod test_support;
