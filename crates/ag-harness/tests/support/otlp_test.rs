use super::{METRICS_PATH, OtlpCollector, TRACES_PATH};

#[tokio::test]
async fn exposes_signal_endpoints() {
    // Arrange
    let collector = OtlpCollector::start().await;

    // Act
    let metrics_endpoint = collector.metrics_endpoint();
    let traces_endpoint = collector.traces_endpoint();

    // Assert
    assert!(metrics_endpoint.ends_with(METRICS_PATH));
    assert!(traces_endpoint.ends_with(TRACES_PATH));
}
