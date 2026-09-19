//! Total elapsed-time budget for focused-review provider submissions.

use std::time::Duration;

use ag_contracts::{OneShotError, OneShotRequest, OneShotSubmission};
use ag_worker::RunClient;
use async_trait::async_trait;
use tokio::time::{Instant, timeout_at};

/// Applies one deadline to every summary, batch, repair, and cross-file call.
pub(crate) struct ReviewDeadlineClient<'a> {
    client: &'a dyn RunClient,
    deadline: Instant,
}

impl<'a> ReviewDeadlineClient<'a> {
    /// Starts a total time budget. Tests supply short budgets through the same
    /// boundary used for the production review deadline.
    pub(crate) fn new(client: &'a dyn RunClient, duration: Duration) -> Self {
        Self {
            client,
            deadline: Instant::now() + duration,
        }
    }

    fn expired() -> OneShotError {
        OneShotError::new("Focused review deadline exceeded. Press `f` to retry.")
    }
}

#[async_trait]
impl RunClient for ReviewDeadlineClient<'_> {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        if Instant::now() >= self.deadline {
            return Err(Self::expired());
        }

        timeout_at(self.deadline, self.client.submit(request))
            .await
            .map_err(|_| Self::expired())?
    }
}

#[cfg(test)]
#[path = "review_deadline_test.rs"]
mod tests;
