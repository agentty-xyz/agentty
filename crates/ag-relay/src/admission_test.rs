use super::{AdmissionPolicy, AdmitAll};

#[tokio::test]
async fn default_policy_admits_every_instance() {
    // Arrange
    let instance_id = "0000000000000000000000000000000000000000000000000000000000000000";

    // Act
    let admitted = AdmitAll.admit(instance_id).await;

    // Assert
    assert_eq!(admitted, Ok(true));
}
