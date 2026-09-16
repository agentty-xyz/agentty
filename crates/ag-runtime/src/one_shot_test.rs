use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use super::{OneShotClient, OneShotError, OneShotRequest, OneShotSubmission};

struct StatelessClient(AtomicUsize);

#[async_trait]
impl OneShotClient for StatelessClient {
    async fn submit(&self, _: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        self.0.fetch_add(1, Ordering::SeqCst);

        Err(OneShotError::new("unexpected submission"))
    }
}

#[tokio::test]
async fn default_close_is_repeatable_without_submitting_work() {
    // Arrange
    let client = StatelessClient(AtomicUsize::new(0));

    // Act
    let cleanup = tokio::time::timeout(Duration::from_secs(1), async {
        client.close().await;
        client.close().await;
    })
    .await;

    // Assert
    cleanup.expect("stateless cleanup completes without provider work");
    assert_eq!(client.0.load(Ordering::SeqCst), 0);
}
