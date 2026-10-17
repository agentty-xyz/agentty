//! Test-only access to app internals shared by sibling layer suites.

use std::sync::{Arc, Mutex};

use ag_contracts::OneShotError;
use ag_worker::{RunInfo, RunRepository, RunState};

pub(crate) use super::core::test_support::AppClients;
pub(crate) use super::review::{REVIEW_NO_DIFF_MESSAGE, diff_content_hash, review_loading_message};
pub(crate) use super::service::test_support::TestSessionRunFactory;
pub(crate) use super::session::{SyncMainOutcome, SyncSessionStartError};
pub(crate) use super::sync::{MockSyncMainRunner, ProjectSyncContext, SyncMainCompletion};

/// Builds a utility worker that answers every submission with `answer` and
/// records the purpose of each accepted run.
pub(crate) fn purpose_recording_run_worker(
    answer: &'static str,
) -> (ag_worker::RunWorker, Arc<Mutex<Vec<String>>>) {
    let repository = Arc::new(PurposeRecordingRuns::default());
    let mut runtime = ag_contracts::MockOneShotClient::new();
    runtime.expect_submit_cancellable().returning(move |_, _| {
        Ok(ag_contracts::OneShotSubmission {
            response: ag_protocol::AgentResponse::plain(answer),
            stats: ag_contracts::SessionStats::default(),
        })
    });
    let worker = ag_worker::RunWorker::with_client(
        Arc::new(runtime),
        Arc::clone(&repository) as Arc<dyn RunRepository>,
        Arc::new(ag_worker::HeartbeatClock),
        std::num::NonZeroUsize::MIN,
    );

    (worker, Arc::clone(&repository.purposes))
}

/// Accepts every run lifecycle change and records each run's purpose.
#[derive(Default)]
struct PurposeRecordingRuns {
    purposes: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl RunRepository for PurposeRecordingRuns {
    async fn create(&self, run: &RunInfo) -> Result<(), OneShotError> {
        self.purposes
            .lock()
            .expect("recorded purposes")
            .push(run.purpose.clone());

        Ok(())
    }

    async fn close_session(&self, _: &str) -> Result<(), OneShotError> {
        Ok(())
    }

    async fn transition(&self, _: &str, _: RunState, _: Option<&str>) -> Result<(), OneShotError> {
        Ok(())
    }

    async fn heartbeat(&self, _: &str) -> Result<(), OneShotError> {
        Ok(())
    }

    async fn recover(&self) -> Result<(), OneShotError> {
        Ok(())
    }
}
