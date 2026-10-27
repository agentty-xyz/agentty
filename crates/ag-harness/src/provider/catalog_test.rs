use std::env;

use super::{
    ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError, kimi,
    muse, openai_compatible,
};
use crate::model::{ModelMetadataError, ModelProfile};

fn missing_environment(_: &str) -> Result<String, env::VarError> {
    Err(env::VarError::NotPresent)
}

#[test]
fn catalog_exposes_every_provider_and_known_model() {
    // Arrange and Act
    let providers = ModelProvider::all();

    // Assert
    assert_eq!(
        providers,
        &[
            ModelProvider::Muse,
            ModelProvider::Kimi,
            ModelProvider::Qwen,
            ModelProvider::OpenAi,
            ModelProvider::OpenAiCompatible
        ]
    );
    assert_eq!(
        ModelProvider::Muse.known_models(),
        &["muse-spark-1.3", "muse-spark-1.3-contributor"]
    );
    assert_eq!(
        ModelProvider::OpenAi.known_models(),
        &[
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-luna",
            "gpt-5.6-terra",
            "gpt-5.3-codex"
        ]
    );
    assert_eq!(
        ModelProvider::OpenAi.api_key_environment(),
        "OPENAI_API_KEY"
    );
    assert_eq!(
        ModelProvider::OpenAi.base_url_environment(),
        "OPENAI_BASE_URL"
    );
    assert_eq!(
        ModelProvider::OpenAi.default_base_url(),
        Some("https://api.openai.com/v1")
    );
    assert!(ModelProvider::OpenAi.requires_api_key());
    assert_eq!(
        ModelProvider::Kimi.known_models(),
        &["kimi-k3", "kimi-k2.6"]
    );
    assert_eq!(
        ModelProvider::Qwen.known_models(),
        &["qwen3.8-max", "qwen-plus"]
    );
    assert_eq!(
        ModelProvider::Muse.default_base_url(),
        Some("https://api.meta.ai/v1")
    );
    assert_eq!(ModelProvider::Kimi.default_base_url(), None);
    assert_eq!(ModelProvider::Qwen.default_base_url(), None);
}

#[test]
fn provider_identifiers_round_trip_and_display() {
    // Arrange and Act
    let parsed = ModelProvider::all()
        .iter()
        .map(|provider| provider.as_str().parse())
        .collect::<Result<Vec<ModelProvider>, _>>()
        .expect("catalog provider identifiers should parse");
    let displayed = ModelProvider::all()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(parsed, ModelProvider::all());
    assert_eq!(
        displayed,
        ["muse", "kimi", "qwen", "openai", "openai-compatible"]
    );
    assert_eq!(
        "unknown".parse::<ModelProvider>(),
        Err(ModelProviderParseError {
            value: "unknown".to_string()
        })
    );
}

#[test]
fn configuration_uses_provider_environment() {
    // Arrange and Act
    for provider in ModelProvider::all() {
        let model = provider.known_models().first().unwrap_or(&"served-model");
        let mut requested_environment = Vec::new();
        let client = ModelConfiguration::new(*provider, *model)
            .client_from_environment(|name| {
                requested_environment.push(name.to_string());
                if name == provider.api_key_environment() {
                    Ok("provider-key".to_string())
                } else {
                    assert_eq!(name, provider.base_url_environment());

                    Ok("https://provider.example/v1".to_string())
                }
            })
            .expect("provider environment should produce a valid client");

        // Assert
        assert_eq!(
            requested_environment,
            [
                provider.api_key_environment().to_string(),
                provider.base_url_environment().to_string()
            ]
        );
        assert_eq!(client.metadata().model(), *model);
    }
}

#[test]
fn openai_compatible_defaults_to_unauthenticated_ollama() {
    // Arrange
    let blank_key = |name: &str| {
        if name == openai_compatible::OPENAI_COMPATIBLE_API_KEY_ENV {
            Ok(" ".to_string())
        } else {
            Err(env::VarError::NotPresent)
        }
    };

    // Act
    let unset = ModelConfiguration::new(ModelProvider::OpenAiCompatible, "qwen3:latest")
        .client_from_environment(missing_environment)
        .expect("a missing optional key should configure a client");
    let blank = ModelConfiguration::new(ModelProvider::OpenAiCompatible, "qwen3:latest")
        .client_from_environment(blank_key)
        .expect("a blank optional key should configure a client");
    let invalid = ModelConfiguration::new(ModelProvider::OpenAiCompatible, "qwen3:latest")
        .client_from_environment(|_| Err(env::VarError::NotUnicode("invalid".into())))
        .err()
        .expect("a non-Unicode optional key should be rejected");

    // Assert
    assert_eq!(
        ModelProvider::OpenAiCompatible.known_models(),
        &[] as &[&str]
    );
    assert!(!ModelProvider::OpenAiCompatible.requires_api_key());
    assert!(ModelProvider::Muse.requires_api_key());
    assert_eq!(
        ModelProvider::OpenAiCompatible.default_base_url(),
        Some("http://127.0.0.1:11434/v1")
    );
    assert_eq!(unset.metadata().provider(), "openai_compatible");
    assert_eq!(blank.metadata().model(), "qwen3:latest");
    assert!(matches!(
        invalid,
        ModelConfigurationError::ApiKey {
            name: openai_compatible::OPENAI_COMPATIBLE_API_KEY_ENV
        }
    ));
}

#[test]
fn declares_profiles_for_openai_models_and_from_the_compatible_environment() {
    // Arrange and Act
    let declared = ModelProvider::OpenAiCompatible
        .declared_profile("qwen3:latest", |name| {
            if name == openai_compatible::OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV {
                Ok("40960".to_string())
            } else {
                Err(env::VarError::NotPresent)
            }
        })
        .expect("declared window should be valid");
    let openai = ModelProvider::OpenAi
        .known_models()
        .iter()
        .map(|model| {
            ModelProvider::OpenAi
                .declared_profile(model, missing_environment)
                .expect("cataloged models read no environment")
        })
        .collect::<Vec<_>>();
    let unlisted = ModelProvider::OpenAi
        .declared_profile("gpt-unlisted", missing_environment)
        .expect("unlisted models read no environment");
    let host_profiled = [
        ModelProvider::Muse,
        ModelProvider::Kimi,
        ModelProvider::Qwen,
    ]
    .iter()
    .map(|provider| provider.declared_profile(provider.known_models()[0], missing_environment))
    .collect::<Result<Vec<_>, _>>()
    .expect("host-profiled providers read no profile");

    // Assert
    assert_eq!(
        declared.map(ModelProfile::context_window_tokens),
        Some(40_960)
    );
    assert!(openai.iter().all(Option::is_some));
    assert_eq!(unlisted, None);
    assert!(host_profiled.iter().all(Option::is_none));
}

#[test]
fn only_openai_compatible_has_a_running_profile_and_local_server() {
    // Arrange and Act
    let running = ModelProvider::OpenAiCompatible
        .running_profile(missing_environment, 16_384)
        .expect("a running window should be valid");
    let server = ModelProvider::OpenAiCompatible
        .local_server(missing_environment)
        .expect("the default endpoint should be valid");
    let hosted = ModelProvider::all()
        .iter()
        .filter(|provider| **provider != ModelProvider::OpenAiCompatible)
        .map(|provider| {
            Ok::<_, ModelConfigurationError>((
                provider.running_profile(missing_environment, 16_384)?,
                provider.local_server(missing_environment)?,
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .expect("hosted providers read nothing");

    // Assert
    assert_eq!(
        running.map(ModelProfile::context_window_tokens),
        Some(16_384)
    );
    assert!(server.is_some());
    assert!(
        hosted
            .iter()
            .all(|(profile, server)| profile.is_none() && server.is_none())
    );
}

#[test]
fn configuration_uses_default_and_explicit_base_urls() {
    // Arrange
    let default = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_3);
    let explicit = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_3)
        .base_url("https://cli.example/v1");

    // Act
    let default = default
        .client_from_environment(|name| {
            if name == muse::MODEL_API_KEY_ENV {
                Ok("test-key".to_string())
            } else {
                Err(env::VarError::NotPresent)
            }
        })
        .expect("Muse default endpoint should be valid");
    let explicit = explicit
        .client_from_environment(|_| Ok("test-key".to_string()))
        .expect("explicit endpoint should be valid");

    // Assert
    assert_eq!(default.metadata().provider(), "meta");
    assert_eq!(explicit.metadata().provider(), "meta");
}

#[test]
fn configuration_requires_non_default_base_url() {
    // Arrange
    let configuration = ModelConfiguration::new(ModelProvider::Kimi, kimi::KIMI_K2_6);

    // Act
    let error = configuration
        .client_from_environment(|name| {
            if name == kimi::KIMI_API_KEY_ENV {
                Ok("test-key".to_string())
            } else {
                Err(env::VarError::NotPresent)
            }
        })
        .err()
        .expect("Kimi without an endpoint should be rejected");

    // Assert
    assert!(matches!(
        error,
        ModelConfigurationError::BaseUrl {
            name: kimi::KIMI_BASE_URL_ENV
        }
    ));
    assert_eq!(
        error.to_string(),
        "no explicit base URL was provided and KIMI_BASE_URL is unavailable"
    );
}

#[test]
fn configuration_redacts_api_key_lookup_failures() {
    // Arrange
    let secret = "visible-secret-material";
    let configuration = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_3);

    // Act
    let error = configuration
        .client_from_environment(|_| {
            Err(env::VarError::NotUnicode(std::ffi::OsString::from(secret)))
        })
        .err()
        .expect("invalid API key environment should be rejected");
    let message = error.to_string();

    // Assert
    assert_eq!(message, "MODEL_API_KEY is unavailable");
    assert!(!message.contains(secret));
}

#[test]
fn configuration_reports_optional_environment_failures() {
    // Arrange
    let configuration = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_3);

    // Act
    let error = configuration
        .client_from_environment(|name| {
            if name == muse::MODEL_API_BASE_URL_ENV {
                Err(env::VarError::NotUnicode("invalid".into()))
            } else {
                Ok("test-key".to_string())
            }
        })
        .err()
        .expect("invalid optional environment should be rejected");

    // Assert
    assert!(matches!(
        error,
        ModelConfigurationError::Environment {
            name: muse::MODEL_API_BASE_URL_ENV,
            source: env::VarError::NotUnicode(_)
        }
    ));
}

#[test]
fn configuration_reports_missing_api_key() {
    // Arrange
    let configuration = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_3);

    // Act
    let error = configuration
        .client_from_environment(missing_environment)
        .err()
        .expect("missing API key should be rejected");

    // Assert
    assert!(matches!(
        error,
        ModelConfigurationError::ApiKey {
            name: muse::MODEL_API_KEY_ENV
        }
    ));
}

#[test]
fn configuration_rejects_an_empty_model() {
    // Arrange
    let configuration =
        ModelConfiguration::new(ModelProvider::Muse, "  ").base_url("https://models.example/v1");

    // Act
    let error = configuration
        .client_from_environment(|_| Ok("test-key".to_string()))
        .err()
        .expect("empty model should be rejected");

    // Assert
    assert!(matches!(
        error,
        ModelConfigurationError::Metadata(ModelMetadataError::EmptyModel)
    ));
}
