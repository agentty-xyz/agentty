//! Test-only access to app internals shared by sibling layer suites.

use std::sync::Arc;

use ag_contracts::{OneShotError, OneShotRequest, OneShotSubmission};
use ag_worker::RunRepository;

pub(crate) use super::core::test_support::AppClients;
pub(crate) use super::project_page::tests::{
    add_listed_project, git_status_updated, hide_project, hold_background_polls,
    insert_loadable_session,
};
pub(crate) use super::remote_control::tests::start_fake_listener;
pub(crate) use super::review::{REVIEW_NO_DIFF_MESSAGE, diff_content_hash, review_loading_message};
pub(crate) use super::service::test_support::{SteerableSessionChannel, TestSessionRunFactory};
pub(crate) use super::session::{SyncMainOutcome, SyncSessionStartError};
pub(crate) use super::sync::{MockSyncMainRunner, ProjectSyncContext, SyncMainCompletion};

/// Builds a utility worker that persists its runs, and the usage they record,
/// through `runs` while `answer` resolves every submission.
pub(crate) fn persisting_run_worker(
    runs: Arc<dyn RunRepository>,
    answer: impl Fn(OneShotRequest) -> Result<OneShotSubmission, OneShotError> + Send + 'static,
) -> Arc<ag_worker::RunWorker> {
    let mut runtime = ag_contracts::MockOneShotClient::new();
    runtime
        .expect_submit_cancellable()
        .returning(move |request, _| answer(request));
    runtime.expect_close().return_const(());

    Arc::new(ag_worker::RunWorker::with_client(
        Arc::new(runtime),
        runs,
        Arc::new(ag_worker::HeartbeatClock),
        std::num::NonZeroUsize::MIN,
    ))
}
