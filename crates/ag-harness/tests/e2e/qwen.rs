//! Live Qwen provider check.

use ag_harness::{ModelClient, QwenConfig};

use crate::{DynError, greeting};

#[tokio::test]
#[ignore = "requires live Qwen credentials"]
async fn test_qwen() -> Result<(), DynError> {
    // Arrange
    let config = QwenConfig {
        api_key: std::env::var("DASHSCOPE_API_KEY")?,
        base_url: std::env::var("DASHSCOPE_BASE_URL")?,
        model: "qwen-plus".to_string(),
    };
    let client = ModelClient::qwen(config)?;

    // Act and Assert
    greeting::request(client, "Qwen").await
}
