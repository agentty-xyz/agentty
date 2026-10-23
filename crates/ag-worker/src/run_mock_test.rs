use ag_contracts::OneShotError;
use async_trait::async_trait;

use crate::{RunInfo, RunRepository, RunState};

mockall::mock! {
    pub RunRepository {
        pub fn create(&self, run: &RunInfo) -> Result<(), OneShotError>;
        pub fn claim_queued(&self, run: &RunInfo) -> Result<bool, OneShotError>;
        pub fn close_session(&self, session_id: &str) -> Result<(), OneShotError>;
        pub fn transition(&self, id: &str, state: RunState, error: Option<String>) -> Result<(), OneShotError>;
        pub fn heartbeat(&self, id: &str) -> Result<(), OneShotError>;
        pub fn recover(&self) -> Result<(), OneShotError>;
    }
}

#[async_trait]
impl RunRepository for MockRunRepository {
    async fn create(&self, run: &RunInfo) -> Result<(), OneShotError> {
        self.create(run)
    }

    async fn claim_queued(&self, run: &RunInfo) -> Result<bool, OneShotError> {
        self.claim_queued(run)
    }

    async fn close_session(&self, session_id: &str) -> Result<(), OneShotError> {
        self.close_session(session_id)
    }

    async fn transition(
        &self,
        id: &str,
        state: RunState,
        error: Option<&str>,
    ) -> Result<(), OneShotError> {
        self.transition(id, state, error.map(str::to_string))
    }

    async fn heartbeat(&self, id: &str) -> Result<(), OneShotError> {
        self.heartbeat(id)
    }

    async fn recover(&self) -> Result<(), OneShotError> {
        self.recover()
    }
}
