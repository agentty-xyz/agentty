//! Provider catalog unit tests for builds that omit one client family.

#[cfg(feature = "providers-openai")]
use std::path::PathBuf;

use super::{Credential, ModelConfiguration, ModelConfigurationError, ModelProvider};

/// Returns the providers whose client family this build omits.
fn disabled_providers() -> Vec<ModelProvider> {
    ModelProvider::all()
        .iter()
        .copied()
        .filter(|provider| !provider.is_enabled())
        .collect()
}

#[test]
fn only_the_compiled_family_is_enabled() {
    // Arrange
    #[cfg(feature = "providers-openai")]
    let expected = [
        ModelProvider::Muse,
        ModelProvider::Kimi,
        ModelProvider::Qwen,
        ModelProvider::OpenAiCompatible,
    ];
    #[cfg(feature = "providers-compat")]
    let expected = [ModelProvider::OpenAi, ModelProvider::ChatGpt];

    // Act
    let disabled = disabled_providers();

    // Assert
    assert_eq!(disabled, expected);
}

#[test]
fn disabled_provider_fails_before_reading_the_environment() {
    for provider in disabled_providers() {
        // Arrange
        let model = provider
            .known_models()
            .first()
            .copied()
            .unwrap_or("served-model");
        let mut lookups = Vec::new();

        // Act
        let result = ModelConfiguration::new(provider, model).client_from_environment(|name| {
            lookups.push(name.to_string());

            Ok("configured".to_string())
        });

        // Assert
        let error = result.err().expect("the provider should be disabled");
        assert!(
            matches!(error, ModelConfigurationError::ProviderDisabled { provider: disabled } if disabled == provider),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!("{provider} support is not compiled in; enable its `providers-*` feature")
        );
        assert!(lookups.is_empty(), "{provider} read {lookups:?}");
    }
}

#[test]
fn disabled_provider_builds_no_client_from_any_credential() {
    for provider in disabled_providers() {
        let credentials = vec![
            Credential::ApiKey(Some("provider-key".to_string())),
            #[cfg(feature = "providers-openai")]
            Credential::SignIn(PathBuf::from("chatgpt.json")),
        ];
        for credential in credentials {
            // Arrange
            let base_url = "https://provider.example/v1".to_string();

            // Act
            let result = provider.client(credential, base_url, "served-model".to_string());

            // Assert
            assert!(
                matches!(result, Err(ModelConfigurationError::ProviderDisabled { provider: disabled }) if disabled == provider),
                "{provider} should be disabled"
            );
        }
    }
}
