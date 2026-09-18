use std::num::NonZeroUsize;
use std::sync::Arc;

use super::RunWorker;
use crate::{Clock, RunRepository};

impl RunWorker {
    /// Injects a scripted runtime for deterministic worker tests.
    pub fn with_client(
        runtime: Arc<dyn ag_contracts::OneShotClient>,
        repository: Arc<dyn RunRepository>,
        clock: Arc<dyn Clock>,
        concurrency: NonZeroUsize,
    ) -> Self {
        Self::from_runtime(
            ag_runtime::UtilityRuntime::from_client(runtime),
            repository,
            clock,
            concurrency,
        )
    }
}
