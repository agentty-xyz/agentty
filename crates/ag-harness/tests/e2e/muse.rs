//! Live Muse provider check.

use ag_harness::{MUSE_SPARK_1_2, ModelClient, MuseConfig};

use crate::{DynError, greeting};

const MODEL_API_BASE_URL: &str = "https://api.meta.ai/v1";

#[tokio::test]
#[ignore = "requires live Muse credentials"]
async fn test_muse() -> Result<(), DynError> {
    // Arrange
    let config = MuseConfig {
        api_key: std::env::var("MODEL_API_KEY")?,
        base_url: std::env::var("MODEL_API_BASE_URL")
            .unwrap_or_else(|_| MODEL_API_BASE_URL.to_string()),
        model: std::env::var("MODEL_API_MODEL").unwrap_or_else(|_| MUSE_SPARK_1_2.to_string()),
    };
    let client = ModelClient::muse(config)?;

    // Act and Assert
    greeting::request(client, "Muse").await
}
