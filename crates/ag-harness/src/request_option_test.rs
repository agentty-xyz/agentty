//! Output-mode capability checks and generation-parameter defaults.

use std::num::NonZeroU32;

use serde_json::json;

use crate::model::{MockModel, ModelError, ModelErrorType};
use crate::request_option::{GenerationParams, OutputMode};
use crate::schema_contract::OutputSchema;

/// Returns a model that supports text output when `text` is set and
/// rejects every schema.
fn model(text: bool) -> MockModel {
    let mut model = MockModel::new();
    model.expect_supports_text_output().return_const(text);
    model.expect_validate_schema().returning(|_| {
        Err(ModelError::UnsupportedOutputSchema {
            reason: "no schemas".to_string(),
        })
    });

    model
}

#[test]
fn output_modes_check_what_the_model_must_support() {
    // Arrange
    let json = OutputMode::from(OutputSchema::new(json!({"type": "object"})).expect("schema"));
    let cases = [
        (&json, true, false, true),
        (&json, false, false, true),
        (&OutputMode::Text, true, true, true),
        (&OutputMode::Text, false, false, false),
    ];

    for (mode, text, checked, text_checked) in cases {
        let model = model(text);

        // Act
        let full = mode.check_model(&model);
        let text_only = mode.require_text_support_if_text(&model);

        // Assert
        assert_eq!(full.is_ok(), checked, "{mode:?} with text {text}");
        assert_eq!(text_only.is_ok(), text_checked, "{mode:?} with text {text}");
        if mode == &OutputMode::Text
            && let Err(error) = full
        {
            assert!(matches!(error, ModelError::UnsupportedOutputMode { .. }));
            assert_eq!(error.error_type(), ModelErrorType::UnsupportedOutput);
        }
    }
}

#[test]
fn generation_params_serialize_only_set_parameters() {
    // Arrange
    let tokens = NonZeroU32::new(64).expect("nonzero tokens");

    // Act
    let unset = GenerationParams::default();
    let bounded = unset.with_max_output_tokens(tokens);

    // Assert
    assert!(unset.is_default());
    assert!(!bounded.is_default());
    assert_eq!(serde_json::to_value(unset).expect("JSON"), json!({}));
    assert_eq!(
        serde_json::to_value(bounded).expect("JSON"),
        json!({"max_output_tokens": 64})
    );
}
