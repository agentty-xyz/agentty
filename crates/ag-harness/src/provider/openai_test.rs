use serde_json::json;

use super::{
    GPT_5_3_CODEX, GPT_5_6_TERRA, GPT_6_1_SOL, GPT_6_ASTRA, GPT_6_LUNA, OpenAiConfig, profile,
};
use crate::model::{Model, ModelClient};
use crate::schema_contract::OutputSchema;
use crate::store_fixture::image_input;

#[test]
fn profiles_keep_requests_under_each_documented_input_limit() {
    // Arrange
    let large = [GPT_6_1_SOL, GPT_6_ASTRA, GPT_6_LUNA, GPT_5_6_TERRA];

    // Act
    let codex = profile(GPT_5_3_CODEX).expect("GPT-5.3-Codex is cataloged");
    let unlisted = profile("gpt-unlisted");

    // Assert
    for model in large {
        let profile = profile(model).expect("GPT-6 family models are cataloged");
        assert_eq!(profile.context_window_tokens(), 1_050_000, "{model}");
        assert_eq!(profile.input_token_budget(), 647_017, "{model}");
        assert!(profile.input_token_budget() < 922_000, "{model}");
    }
    assert_eq!(codex.context_window_tokens(), 400_000);
    assert_eq!(codex.input_token_budget(), 190_877);
    assert!(codex.input_token_budget() < 272_000);
    assert_eq!(unlisted, None);
}

#[test]
fn client_constrains_output_natively_and_accepts_images() {
    // Arrange
    let object = OutputSchema::new(json!({"type": "object"})).expect("schema should be valid");
    let array = OutputSchema::new(json!({"type": "array"})).expect("schema should be valid");

    // Act
    let model = ModelClient::openai(OpenAiConfig {
        api_key: "sk-test".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        model: GPT_6_1_SOL.to_string(),
    })
    .expect("fixture configuration should be valid");

    // Assert
    assert_eq!(model.metadata().provider(), "openai");
    assert_eq!(model.metadata().model(), GPT_6_1_SOL);
    assert!(!Model::keeps_prefix_across_schemas(&model));
    assert!(Model::validate_schema(&model, &object).is_ok());
    assert!(Model::validate_schema(&model, &array).is_err());
    assert!(Model::validate_input(&model, &image_input("look", b"payload", "closely")).is_ok());
}
