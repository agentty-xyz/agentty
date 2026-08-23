//! Live Kimi provider check.

use ag_harness::{KimiConfig, ModelClient};

use crate::{DynError, greeting};

#[tokio::test]
#[ignore = "requires live Kimi credentials"]
async fn test_kimi() -> Result<(), DynError> {
    // Arrange
    let config = KimiConfig {
        api_key: std::env::var("KIMI_API_KEY")?,
        base_url: std::env::var("KIMI_BASE_URL")?,
        model: std::env::var("KIMI_MODEL")?,
    };
    let client = ModelClient::kimi(config)?;

    // Act and Assert
    greeting::request(client, "Kimi").await
}
