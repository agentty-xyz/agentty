//! Live ordered text/image input checks for every built-in provider.

use std::sync::Arc;

use ag_harness::model::{ModelCapabilities, ModelClient, ModelRegistry};
use ag_harness::provider::{
    KIMI_K2_6, KimiConfig, MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, MuseConfig, QWEN_PLUS,
    QwenConfig,
};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::MemoryStore;
use ag_harness::{Harness, SessionError};

use crate::{DynError, vision};

const MODEL_API_BASE_URL: &str = "https://api.meta.ai/v1";
const QWEN_TEXT_MODEL: &str = "qwen3-max";
const QWEN_VL_MODEL: &str = "qwen3-vl-plus";

fn kimi(model: &str) -> Result<ModelClient, DynError> {
    let client = ModelClient::kimi(KimiConfig {
        api_key: std::env::var("KIMI_API_KEY")?,
        base_url: std::env::var("KIMI_BASE_URL")?,
        model: model.to_string(),
    })?;

    Ok(client)
}

fn muse(model: &str) -> Result<ModelClient, DynError> {
    let client = ModelClient::muse(MuseConfig {
        api_key: std::env::var("MODEL_API_KEY")?,
        base_url: std::env::var("MODEL_API_BASE_URL")
            .unwrap_or_else(|_| MODEL_API_BASE_URL.to_string()),
        model: model.to_string(),
    })?;

    Ok(client)
}

fn qwen(model: &str) -> Result<ModelClient, DynError> {
    let client = ModelClient::qwen(QwenConfig {
        api_key: std::env::var("DASHSCOPE_API_KEY")?,
        base_url: std::env::var("DASHSCOPE_BASE_URL")?,
        model: model.to_string(),
    })?;

    Ok(client)
}

#[tokio::test]
#[ignore = "requires live Kimi credentials"]
async fn test_kimi_k2_6_images() -> Result<(), DynError> {
    vision::describes_images(kimi(KIMI_K2_6)?, KIMI_K2_6).await
}

#[tokio::test]
#[ignore = "requires live Kimi credentials"]
async fn test_kimi_k2_7_code_images() -> Result<(), DynError> {
    vision::describes_images(kimi("kimi-k2.7-code")?, "kimi-k2.7-code").await
}

#[tokio::test]
#[ignore = "requires live Kimi credentials"]
async fn test_kimi_k2_7_code_highspeed_images() -> Result<(), DynError> {
    vision::describes_images(
        kimi("kimi-k2.7-code-highspeed")?,
        "kimi-k2.7-code-highspeed",
    )
    .await
}

#[tokio::test]
#[ignore = "requires live Kimi credentials"]
async fn test_kimi_k3_images() -> Result<(), DynError> {
    vision::describes_images(kimi("kimi-k3")?, "kimi-k3").await
}

#[tokio::test]
#[ignore = "requires live Muse credentials"]
async fn test_muse_images() -> Result<(), DynError> {
    for model in [MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR] {
        vision::describes_images(muse(model)?, model).await?;
    }

    Ok(())
}

#[tokio::test]
#[ignore = "requires live Qwen credentials"]
async fn test_qwen_images() -> Result<(), DynError> {
    for model in [
        QWEN_PLUS,
        "qwen3.8-27b",
        "qwen3.8-flash",
        "qwen3.8-max",
        "qwen-vl-max",
        "qwen-vl-plus",
        "qwen3-vl-flash",
        QWEN_VL_MODEL,
    ] {
        vision::describes_images(qwen(model)?, model).await?;
    }

    Ok(())
}

/// `qwen3-max` accepts image parts over the wire but invents their content,
/// so the harness must reject them before any request.
#[tokio::test]
#[ignore = "requires live Qwen credentials"]
async fn test_qwen_text_model_rejects_images() -> Result<(), DynError> {
    vision::rejects_images(qwen(QWEN_TEXT_MODEL)?, QWEN_TEXT_MODEL).await
}

/// Image history follows a switch to an image-capable registration and blocks
/// a switch to a text-only one.
#[tokio::test]
#[ignore = "requires live Qwen and Muse credentials"]
async fn test_image_history_switches_between_providers() -> Result<(), DynError> {
    // Arrange
    let vision_capabilities = ModelCapabilities {
        context_budget: None,
        image_input: true,
        native_continuation: false,
        tool_calls: true,
    };
    let mut registry = ModelRegistry::new();
    registry.register(
        ExecutionIdentity::new("qwen-vl", "1")?,
        qwen(QWEN_VL_MODEL)?,
        vision_capabilities,
    )?;
    registry.register(
        ExecutionIdentity::new("qwen-text", "1")?,
        qwen(QWEN_TEXT_MODEL)?,
        vision_capabilities,
    )?;
    registry.register(
        ExecutionIdentity::new("muse", "1")?,
        muse(MUSE_SPARK_1_3)?,
        vision_capabilities,
    )?;
    let harness = Harness::from_registry(&registry, "qwen-vl")?.store(Arc::new(MemoryStore::new()));
    let mut session = harness
        .session("switch", vision::color_schema()?)
        .create()
        .await?;
    session
        .send(vision::colored_images(vision::REMEMBER)?)
        .await?;

    // Act
    let text_only = session.switch_model(&registry, "qwen-text").await;
    session.switch_model(&registry, "muse").await?;
    let follow_up = session.send(vision::RECALL).await?;

    // Assert
    if !matches!(text_only, Err(SessionError::UnsupportedModelHistory { .. })) {
        return Err(format!("text-only target must reject image history: {text_only:?}").into());
    }

    vision::assert_colors(follow_up.output(), "Muse after switch")
}
