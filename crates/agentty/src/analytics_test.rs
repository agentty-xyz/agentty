use uuid::Uuid;

use super::Analytics;
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn posthog_sender_uses_bundled_destination_and_stored_installation_id() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("repositories");

    // Act
    let analytics = Analytics::posthog(&repositories)
        .await
        .expect("bundled destination");

    // Assert
    assert_eq!(analytics.endpoint, "https://us.i.posthog.com/i/v0/e/");
    assert!(analytics.token.starts_with("phc_"));
    assert_eq!(
        repositories
            .settings()
            .get_setting(SettingName::TelemetryInstallationId)
            .await
            .expect("stored installation ID"),
        Some(analytics.distinct_id)
    );
}

#[tokio::test]
async fn installation_id_is_created_once_and_reused() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("repositories");

    // Act
    let first_id = Analytics::installation_id(&repositories).await;
    let second_id = Analytics::installation_id(&repositories).await;

    // Assert
    assert!(Uuid::parse_str(&first_id).is_ok());
    assert_eq!(first_id, second_id);
}

#[tokio::test]
async fn empty_installation_id_is_replaced() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("repositories");
    repositories
        .settings()
        .upsert_setting(SettingName::TelemetryInstallationId, "")
        .await
        .expect("empty installation ID");

    // Act
    let installation_id = Analytics::installation_id(&repositories).await;

    // Assert
    assert!(Uuid::parse_str(&installation_id).is_ok());
    assert_eq!(
        repositories
            .settings()
            .get_setting(SettingName::TelemetryInstallationId)
            .await
            .expect("stored installation ID"),
        Some(installation_id)
    );
}
