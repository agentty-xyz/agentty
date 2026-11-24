//! Provider catalog unit tests for known models, capabilities, prices,
//! profiles, cost tiers, and environment-based configuration.

use std::env;

use ag_router::chatgpt::ChatGptAuthError;

use super::{
    CostTier, ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError,
    anthropic, chatgpt, kimi, muse, openai, openai_compatible, qwen,
};
use crate::catalog::{Capabilities, ModelId, PricingSource};
use crate::chatgpt_sign_in_fixture::save_chatgpt_sign_in;
use crate::input::TurnInput;
use crate::model::{HeuristicContextEstimator, Model, ModelMetadataError, ModelProfile};

/// Returns every cataloged provider and model pair.
fn cataloged_models() -> Vec<(ModelProvider, &'static str)> {
    ModelProvider::all()
        .iter()
        .flat_map(|provider| {
            provider
                .known_models()
                .iter()
                .map(move |model| (*provider, *model))
        })
        .collect()
}

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
            ModelProvider::ChatGpt,
            ModelProvider::Anthropic,
            ModelProvider::OpenAiCompatible
        ]
    );
    assert_eq!(
        ModelProvider::ChatGpt.known_models(),
        &[
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna"
        ]
    );
    assert_eq!(
        ModelProvider::ChatGpt.api_key_environment(),
        "CHATGPT_AUTH_FILE"
    );
    assert_eq!(
        ModelProvider::ChatGpt.base_url_environment(),
        "CHATGPT_BASE_URL"
    );
    assert_eq!(
        ModelProvider::ChatGpt.default_base_url(),
        Some("https://api.openai.com/v1")
    );
    assert!(!ModelProvider::ChatGpt.requires_api_key());
    assert!(ModelProvider::ChatGpt.requires_sign_in());
    assert!(!ModelProvider::OpenAi.requires_sign_in());
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
        &["qwen3.8-max", "qwen-plus", "qwen3.8-flash", "qwen3.7-flash"]
    );
    assert_eq!(
        ModelProvider::Muse.default_base_url(),
        Some("https://api.meta.ai/v1")
    );
    assert_eq!(
        ModelProvider::Kimi.default_base_url(),
        Some("https://api.moonshot.ai/v1")
    );
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
        [
            "muse",
            "kimi",
            "qwen",
            "openai",
            "chatgpt",
            "anthropic",
            "openai-compatible"
        ]
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
    for provider in ModelProvider::all()
        .iter()
        .filter(|provider| !provider.requires_sign_in())
    {
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
    let qwen_flash = [qwen::QWEN3_8_FLASH, qwen::QWEN3_7_FLASH]
        .iter()
        .map(|model| {
            ModelProvider::Qwen
                .declared_profile(model, missing_environment)
                .expect("cataloged models read no environment")
        })
        .collect::<Vec<_>>();
    let fitted = [
        (ModelProvider::Muse, muse::MUSE_SPARK_1_3, 1_048_576),
        (
            ModelProvider::Muse,
            muse::MUSE_SPARK_1_3_CONTRIBUTOR,
            1_048_576,
        ),
        (ModelProvider::Kimi, kimi::KIMI_K3, 1_048_576),
        (ModelProvider::Kimi, kimi::KIMI_K2_6, 262_144),
    ]
    .map(|(provider, model, window)| {
        let profile = provider
            .declared_profile(model, missing_environment)
            .expect("cataloged models read no environment")
            .expect("Kimi and Muse declare a profile");

        (profile, window)
    });
    let host_profiled = ModelProvider::Qwen
        .declared_profile(qwen::QWEN3_8_MAX, missing_environment)
        .expect("host-profiled models read no environment");

    // Assert
    assert_eq!(
        declared.map(ModelProfile::context_window_tokens),
        Some(40_960)
    );
    assert!(openai.iter().all(Option::is_some));
    assert_eq!(unlisted, None);
    assert_eq!(host_profiled, None);
    for (profile, window) in fitted {
        assert_eq!(profile.context_window_tokens(), window);
        // Fitted estimators replace the default one.
        assert_ne!(profile.estimator(), HeuristicContextEstimator::DEFAULT);
    }
    for profile in qwen_flash {
        let profile = profile.expect("Qwen flash models declare a profile");
        assert_eq!(profile.context_window_tokens(), 1_000_000);
        assert_eq!(profile.input_token_budget(), 609_774);
        assert!(profile.input_token_budget() < 991_808);
    }
}

#[test]
fn declares_a_cost_tier_for_every_cataloged_model() {
    // Arrange
    let cataloged = cataloged_models();

    // Act
    let cheap = cataloged
        .iter()
        .filter(|(provider, model)| provider.cost_tier(model) == Some(CostTier::Cheap))
        .map(|(_, model)| *model)
        .collect::<Vec<_>>();
    let premium = cataloged
        .iter()
        .filter(|(provider, model)| provider.cost_tier(model) == Some(CostTier::Premium))
        .map(|(_, model)| *model)
        .collect::<Vec<_>>();

    // Assert
    assert!(
        cataloged
            .iter()
            .all(|(provider, model)| provider.cost_tier(model).is_some())
    );
    assert_eq!(
        cheap,
        [
            "muse-spark-1.3-contributor",
            "qwen-plus",
            "qwen3.8-flash",
            "qwen3.7-flash",
            "gpt-6-luna",
            "claude-haiku-5-5"
        ]
    );
    assert_eq!(
        premium,
        [
            "kimi-k3",
            "gpt-6-astra",
            "claude-fable-5-2",
            "claude-opus-5-5"
        ]
    );
    assert_eq!(
        ModelProvider::Muse.cost_tier(muse::MUSE_SPARK_1_3),
        Some(CostTier::Standard)
    );
    assert_eq!(
        ModelProvider::OpenAiCompatible.cost_tier("qwen3:latest"),
        None
    );
    assert_eq!(ModelProvider::Qwen.cost_tier("qwen-unlisted"), None);
    assert_eq!(ModelProvider::Kimi.cost_tier(muse::MUSE_SPARK_1_3), None);
}

#[test]
fn openai_compatible_servers_are_local_only_at_local_or_private_endpoints() {
    // Arrange
    let local = [
        "http://127.0.0.1:11434/v1",
        "http://LOCALHOST:8080/v1",
        "http://llm.localhost/v1",
        "http://0.0.0.0:8000/v1",
        "http://10.0.0.5/v1",
        "http://172.16.1.2/v1",
        "http://192.168.1.20:1234/v1",
        "http://169.254.1.1/v1",
        "http://[::1]:11434/v1",
        "http://[::]:8000/v1",
        "http://[fd12::1]/v1",
        "http://[fe80::1]/v1",
        "http://localhost.:11434/v1",
        "http://ollama:11434/v1",
        "http://gpu-box.local:8000/v1",
        "http://host.docker.internal:11434/v1",
        "http://100.64.0.1/v1",
        "http://100.127.255.254/v1",
        "http://[::ffff:127.0.0.1]:11434/v1",
        "http://[::ffff:192.168.1.20]/v1",
    ];
    let hosted = [
        "https://openrouter.ai/api/v1",
        "https://api.together.xyz/v1",
        "https://openrouter.ai./api/v1",
        "http://8.8.8.8/v1",
        "http://100.63.255.255/v1",
        "http://100.128.0.1/v1",
        "http://[::ffff:8.8.8.8]/v1",
        "http://[2001:db8::1]/v1",
        "not a url",
        "file:///tmp/socket",
    ];

    // Act / Assert
    for base_url in local {
        assert_eq!(
            ModelProvider::OpenAiCompatible.cost_tier_at("qwen3:latest", base_url),
            Some(CostTier::Local),
            "{base_url}"
        );
    }
    for base_url in hosted {
        assert_eq!(
            ModelProvider::OpenAiCompatible.cost_tier_at("qwen3:latest", base_url),
            None,
            "{base_url}"
        );
    }
    // Other providers bill the same at any endpoint.
    assert_eq!(
        ModelProvider::Kimi.cost_tier_at(kimi::KIMI_K3, "http://127.0.0.1/v1"),
        Some(CostTier::Premium)
    );
}

#[test]
fn cost_tiers_order_and_display_from_local_to_premium() {
    // Arrange
    let tiers = [
        CostTier::Premium,
        CostTier::Local,
        CostTier::Standard,
        CostTier::Plan,
        CostTier::Cheap,
    ];

    // Act
    let mut sorted = tiers;
    sorted.sort();
    let labels = sorted.map(|tier| tier.to_string());

    // Assert
    assert_eq!(labels, ["local", "plan", "cheap", "standard", "premium"]);
}

#[test]
fn chatgpt_models_are_plan_billed_and_profiled() {
    // Arrange
    let models = ModelProvider::ChatGpt.known_models();

    // Act
    let tiers = models
        .iter()
        .map(|model| ModelProvider::ChatGpt.cost_tier(model))
        .collect::<Vec<_>>();
    let windows = models
        .iter()
        .map(|model| {
            ModelProvider::ChatGpt
                .declared_profile(model, missing_environment)
                .expect("plan profiles read nothing")
                .map(ModelProfile::context_window_tokens)
        })
        .collect::<Vec<_>>();

    // Assert
    assert!(tiers.iter().all(|tier| *tier == Some(CostTier::Plan)));
    assert_eq!(ModelProvider::ChatGpt.cost_tier("gpt-unlisted"), None);
    assert_eq!(
        windows,
        [
            Some(500_000),
            Some(500_000),
            Some(272_000),
            Some(272_000),
            Some(272_000),
            Some(272_000),
            Some(272_000),
            Some(272_000)
        ]
    );
    assert_eq!(
        ModelProvider::ChatGpt
            .declared_profile("gpt-unlisted", missing_environment)
            .expect("unlisted models read nothing"),
        None
    );
}

#[test]
fn chatgpt_configuration_requires_a_stored_sign_in() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("auth/chatgpt.json");
    let auth_file_value = auth_file.to_string_lossy().into_owned();
    let environment = |name: &str| match name {
        "CHATGPT_AUTH_FILE" => Ok(auth_file_value.clone()),
        _ => Err(env::VarError::NotPresent),
    };

    // Act
    let missing = ModelConfiguration::new(ModelProvider::ChatGpt, "gpt-6-luna")
        .client_from_environment(environment);
    save_chatgpt_sign_in(&auth_file);
    let client = ModelConfiguration::new(ModelProvider::ChatGpt, "gpt-6-luna")
        .client_from_environment(environment)
        .expect("a stored sign-in should configure a client");
    let unlocated = ModelConfiguration::new(ModelProvider::ChatGpt, "gpt-6-luna")
        .client_from_environment(missing_environment);
    let injected = ModelConfiguration::new(ModelProvider::ChatGpt, "gpt-6-luna")
        .auth_file(&auth_file)
        .client_from_environment(missing_environment);

    // Assert
    assert!(injected.is_ok(), "{:?}", injected.err());
    assert!(matches!(
        missing,
        Err(ModelConfigurationError::SignIn(
            ChatGptAuthError::NotSignedIn { .. }
        ))
    ));
    assert_eq!(client.metadata().provider(), "openai_chatgpt");
    assert_eq!(client.metadata().model(), "gpt-6-luna");
    assert!(matches!(
        unlocated,
        Err(ModelConfigurationError::AuthFile(_))
    ));
}

#[test]
fn only_the_muse_contributor_tier_trains_on_prompts() {
    // Arrange
    let cataloged = ModelProvider::all().iter().flat_map(|provider| {
        provider
            .known_models()
            .iter()
            .map(move |model| (*provider, *model))
    });

    // Act
    let training = cataloged
        .filter(|(provider, model)| provider.trains_on_prompts(model))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        training,
        [(ModelProvider::Muse, muse::MUSE_SPARK_1_3_CONTRIBUTOR)]
    );
    assert!(!ModelProvider::Kimi.trains_on_prompts(muse::MUSE_SPARK_1_3_CONTRIBUTOR));
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
fn configuration_treats_a_blank_base_url_variable_as_unset() {
    // Arrange
    let blank_base_url = |name: &str| match name {
        kimi::KIMI_API_KEY_ENV | qwen::DASHSCOPE_API_KEY_ENV => Ok("test-key".to_string()),
        _ => Ok("  ".to_string()),
    };

    // Act
    let kimi = ModelConfiguration::new(ModelProvider::Kimi, kimi::KIMI_K3)
        .client_from_environment(blank_base_url);
    let qwen = ModelConfiguration::new(ModelProvider::Qwen, qwen::QWEN_PLUS)
        .client_from_environment(blank_base_url);

    // Assert
    // Kimi falls back to its default endpoint, as when the variable is
    // unset; Qwen, without a default, still asks for it.
    assert!(kimi.is_ok(), "{:?}", kimi.err());
    assert!(matches!(
        qwen.err(),
        Some(ModelConfigurationError::BaseUrl {
            name: qwen::DASHSCOPE_BASE_URL_ENV
        })
    ));
}

#[test]
fn openai_compatible_servers_declare_no_catalog_limits() {
    // Arrange / Act
    let limits = ModelProvider::OpenAiCompatible.declared_limits("qwen3:latest");

    // Assert
    assert_eq!(limits, (None, None));
}

#[test]
fn configuration_requires_non_default_base_url() {
    // Arrange
    let configuration = ModelConfiguration::new(ModelProvider::Qwen, qwen::QWEN_PLUS);

    // Act
    let error = configuration
        .client_from_environment(|name| {
            if name == qwen::DASHSCOPE_API_KEY_ENV {
                Ok("test-key".to_string())
            } else {
                Err(env::VarError::NotPresent)
            }
        })
        .err()
        .expect("Qwen without an endpoint should be rejected");

    // Assert
    assert!(matches!(
        error,
        ModelConfigurationError::BaseUrl {
            name: qwen::DASHSCOPE_BASE_URL_ENV
        }
    ));
    assert_eq!(
        error.to_string(),
        "no explicit base URL was provided and DASHSCOPE_BASE_URL is unavailable"
    );
}

#[test]
fn kimi_defaults_to_moonshot_ai_and_declares_its_profile() {
    // Arrange
    let configuration = ModelConfiguration::new(ModelProvider::Kimi, kimi::KIMI_K2_6);

    // Act
    let client = configuration
        .client_from_environment(|name| {
            if name == kimi::KIMI_API_KEY_ENV {
                Ok("test-key".to_string())
            } else {
                Err(env::VarError::NotPresent)
            }
        })
        .expect("Kimi defaults to Moonshot AI's endpoint");

    // Assert
    assert_eq!(client.metadata().provider(), "moonshot_ai");
    assert_eq!(
        client.profile().map(ModelProfile::context_window_tokens),
        Some(262_144)
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

#[test]
fn every_per_token_model_has_sane_built_in_pricing() {
    // Arrange
    let cataloged = cataloged_models();

    // Act
    let priced = cataloged
        .iter()
        // Claude models carry separate cache prices, checked on their own.
        .filter(|(provider, _)| *provider != ModelProvider::Anthropic)
        .filter_map(|(provider, model)| Some((*provider, *model, provider.pricing(model)?)))
        .collect::<Vec<_>>();

    // Assert
    for (_, model, pricing) in &priced {
        assert_eq!(pricing.source(), PricingSource::BuiltIn, "{model}");
    }
    // Input, output, cache-read, and cache-write prices. Without a
    // published price, cache reads and writes bill at the input price.
    assert_eq!(
        priced
            .iter()
            .map(|(provider, model, pricing)| (
                *provider,
                *model,
                [
                    pricing.input_per_mtok_micro_usd(),
                    pricing.output_per_mtok_micro_usd(),
                    pricing.cache_read_per_mtok_micro_usd(),
                    pricing.cache_write_per_mtok_micro_usd(),
                ]
            ))
            .collect::<Vec<_>>(),
        [
            (
                ModelProvider::Muse,
                muse::MUSE_SPARK_1_3,
                [1_250_000, 4_250_000, 150_000, 1_250_000]
            ),
            (
                ModelProvider::Muse,
                muse::MUSE_SPARK_1_3_CONTRIBUTOR,
                [100_000, 200_000, 2_000, 100_000]
            ),
            (
                ModelProvider::Kimi,
                kimi::KIMI_K3,
                [3_000_000, 15_000_000, 300_000, 3_000_000]
            ),
            (
                ModelProvider::Kimi,
                kimi::KIMI_K2_6,
                [950_000, 4_000_000, 160_000, 950_000]
            ),
            (
                ModelProvider::Qwen,
                qwen::QWEN3_8_MAX,
                [2_000_000, 6_000_000, 2_000_000, 2_000_000]
            ),
            (
                ModelProvider::Qwen,
                qwen::QWEN_PLUS,
                [400_000, 1_200_000, 80_000, 400_000]
            ),
            (
                ModelProvider::Qwen,
                qwen::QWEN3_8_FLASH,
                [150_000, 470_000, 150_000, 150_000]
            ),
            (
                ModelProvider::Qwen,
                qwen::QWEN3_7_FLASH,
                [30_000, 130_000, 6_000, 30_000]
            ),
            (
                ModelProvider::OpenAi,
                openai::GPT_6_1_SOL,
                [2_000_000, 10_000_000, 100_000, 2_500_000]
            ),
            (
                ModelProvider::OpenAi,
                openai::GPT_6_ASTRA,
                [10_000_000, 50_000_000, 1_000_000, 12_500_000]
            ),
            (
                ModelProvider::OpenAi,
                openai::GPT_6_LUNA,
                [100_000, 500_000, 10_000, 125_000]
            ),
            (
                ModelProvider::OpenAi,
                openai::GPT_5_6_TERRA,
                [2_000_000, 12_000_000, 200_000, 2_500_000]
            ),
            (
                ModelProvider::OpenAi,
                openai::GPT_5_3_CODEX,
                [1_750_000, 14_000_000, 175_000, 1_750_000]
            ),
        ]
    );
    assert_eq!(
        ModelProvider::ChatGpt.pricing(chatgpt::CHATGPT_GPT_6_LUNA),
        None
    );
    assert_eq!(
        ModelProvider::OpenAiCompatible.pricing("qwen3:latest"),
        None
    );
    assert_eq!(ModelProvider::OpenAi.pricing("gpt-unlisted"), None);
}

#[test]
fn declared_capabilities_match_each_client_wire_policy() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("chatgpt.json");
    save_chatgpt_sign_in(&auth_file);
    let auth_file = auth_file.to_string_lossy().into_owned();
    let image = TurnInput::image_probe();

    // Act
    let mismatched = cataloged_models()
        .into_iter()
        .filter(|(provider, model)| {
            let client = ModelConfiguration::new(*provider, *model)
                .base_url("https://models.example/v1")
                .client_from_environment(|name| {
                    Ok(if name == "CHATGPT_AUTH_FILE" {
                        auth_file.clone()
                    } else {
                        "test-key".to_string()
                    })
                })
                .expect("cataloged model should configure");
            let capabilities = provider.capabilities(model).expect("cataloged model");

            capabilities
                != Capabilities::NONE
                    .with_image_input(client.validate_input(&image).is_ok())
                    .with_reasoning(capabilities.reasoning())
                    .with_structured_output(!client.keeps_prefix_across_schemas())
                    .with_tool_calls(true)
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(mismatched, []);
    assert_eq!(
        ModelProvider::OpenAiCompatible.capabilities("qwen3:latest"),
        Some(Capabilities::NONE.with_tool_calls(true))
    );
    assert_eq!(ModelProvider::Qwen.capabilities("qwen-unlisted"), None);
    assert!(
        !ModelProvider::Qwen
            .capabilities(qwen::QWEN3_7_FLASH)
            .expect("cataloged")
            .reasoning()
    );
}

#[test]
fn model_specs_carry_cataloged_profiles_limits_and_prices() {
    // Arrange
    let luna = (ModelProvider::OpenAi, openai::GPT_6_LUNA);
    let plan = (ModelProvider::ChatGpt, chatgpt::CHATGPT_GPT_6_1_SOL);
    let flash = (ModelProvider::Qwen, qwen::QWEN3_8_FLASH);
    let max = (ModelProvider::Qwen, qwen::QWEN3_8_MAX);
    let kimi = (ModelProvider::Kimi, kimi::KIMI_K3);

    // Act
    let specs = [luna, plan, flash, max, kimi].map(|(provider, model)| {
        provider
            .model_spec(model)
            .expect("cataloged model has a spec")
    });

    // Assert
    let [luna, plan, flash, max, kimi] = specs;
    assert_eq!(luna.id(), &ModelId::new("openai/gpt-6-luna"));
    assert_eq!(luna.provider(), Some(ModelProvider::OpenAi));
    assert_eq!(luna.context_window(), Some(1_050_000));
    assert_eq!(luna.max_output(), Some(128_000));
    assert_eq!(
        luna.pricing(),
        ModelProvider::OpenAi.pricing(openai::GPT_6_LUNA)
    );
    assert!(luna.capabilities().structured_output());
    assert!(luna.capabilities().image_input());
    assert!(luna.capabilities().tool_calls());
    assert_eq!(plan.context_window(), Some(500_000));
    assert_eq!(plan.max_output(), Some(128_000));
    assert_eq!(plan.pricing(), None);
    assert_eq!(flash.context_window(), Some(1_000_000));
    assert_eq!(flash.max_output(), Some(131_072));
    assert!(!flash.capabilities().structured_output());
    assert_eq!(max.profile(), None);
    assert_eq!(max.max_output(), None);
    assert_eq!(kimi.context_window(), Some(1_048_576));
    assert_eq!(kimi.max_output(), Some(131_072));
    assert!(kimi.pricing().is_some());
    assert_eq!(luna.max_concurrency(), None);
    assert_eq!(
        ModelProvider::OpenAiCompatible.model_spec("qwen3:latest"),
        None
    );
    assert_eq!(ModelProvider::OpenAi.model_spec("gpt-unlisted"), None);
    assert_eq!(
        ModelProvider::Muse
            .model_spec(muse::MUSE_SPARK_1_3_CONTRIBUTOR)
            .map(|spec| spec.max_output()),
        Some(Some(131_072))
    );
}

#[test]
fn claude_models_carry_built_in_prices_with_cache_rates() {
    // Arrange
    let provider = ModelProvider::Anthropic;

    // Act
    let prices = provider
        .known_models()
        .iter()
        .map(|model| {
            let pricing = provider.pricing(model).expect("Claude models are priced");

            (
                *model,
                pricing.source(),
                pricing.input_per_mtok_micro_usd(),
                pricing.output_per_mtok_micro_usd(),
                pricing.cache_read_per_mtok_micro_usd(),
                pricing.cache_write_per_mtok_micro_usd(),
            )
        })
        .collect::<Vec<_>>();
    let tiers = provider
        .known_models()
        .iter()
        .map(|model| provider.cost_tier_at(model, "http://127.0.0.1/v1"))
        .collect::<Vec<_>>();

    // Assert
    let built_in = PricingSource::BuiltIn;
    assert_eq!(
        prices,
        [
            (
                "claude-fable-5-2",
                built_in,
                8_000_000,
                40_000_000,
                200_000,
                10_000_000
            ),
            (
                "claude-opus-5-5",
                built_in,
                4_000_000,
                20_000_000,
                200_000,
                5_000_000
            ),
            (
                "claude-sonnet-5-5",
                built_in,
                2_000_000,
                10_000_000,
                100_000,
                2_500_000
            ),
            (
                "claude-haiku-5-5",
                built_in,
                100_000,
                500_000,
                10_000,
                125_000
            ),
        ]
    );
    assert_eq!(
        tiers,
        [
            Some(CostTier::Premium),
            Some(CostTier::Premium),
            Some(CostTier::Standard),
            Some(CostTier::Cheap)
        ]
    );
}

#[test]
fn anthropic_configures_claude_clients_from_an_api_key() {
    // Arrange
    let provider = ModelProvider::Anthropic;
    let api_key_only = |name: &str| {
        if name == anthropic::ANTHROPIC_API_KEY_ENV {
            Ok("claude-key".to_string())
        } else {
            Err(env::VarError::NotPresent)
        }
    };

    // Act
    let spec = provider
        .model_spec(anthropic::CLAUDE_OPUS_5_5)
        .expect("cataloged model has a spec");
    let client = ModelConfiguration::new(provider, anthropic::CLAUDE_OPUS_5_5)
        .client_from_environment(api_key_only)
        .expect("Anthropic defaults to its public endpoint");
    let missing_key = ModelConfiguration::new(provider, anthropic::CLAUDE_OPUS_5_5)
        .client_from_environment(missing_environment);
    let unlisted_profile = provider
        .declared_profile("claude-unlisted", missing_environment)
        .expect("unlisted models read no environment");

    // Assert
    assert_eq!(
        provider.known_models(),
        &[
            "claude-fable-5-2",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5"
        ]
    );
    assert_eq!(provider.api_key_environment(), "ANTHROPIC_API_KEY");
    assert_eq!(provider.base_url_environment(), "ANTHROPIC_BASE_URL");
    assert_eq!(
        provider.default_base_url(),
        Some("https://api.anthropic.com/v1")
    );
    assert!(provider.requires_api_key());
    assert!(!provider.requires_sign_in());
    assert_eq!(spec.id(), &ModelId::new("anthropic/claude-opus-5-5"));
    assert_eq!(spec.context_window(), Some(1_000_000));
    assert_eq!(spec.max_output(), Some(128_000));
    assert!(spec.capabilities().structured_output());
    assert!(spec.capabilities().image_input());
    assert!(spec.capabilities().reasoning());
    assert_eq!(client.metadata().provider(), "anthropic");
    assert_eq!(
        client.profile().map(ModelProfile::context_window_tokens),
        Some(1_000_000)
    );
    assert_eq!(unlisted_profile, None);
    assert_eq!(provider.model_spec("claude-unlisted"), None);
    assert!(matches!(
        missing_key,
        Err(ModelConfigurationError::ApiKey {
            name: anthropic::ANTHROPIC_API_KEY_ENV
        })
    ));
}

#[test]
fn every_provider_is_enabled_with_every_family() {
    // Arrange and Act
    let disabled: Vec<_> = ModelProvider::all()
        .iter()
        .filter(|provider| !provider.is_enabled())
        .collect();

    // Assert
    assert!(disabled.is_empty(), "{disabled:?}");
}

#[test]
fn haiku_bills_five_times_its_prices_above_a_100k_token_prompt() {
    // Arrange
    let haiku = ModelProvider::Anthropic
        .pricing(anthropic::CLAUDE_HAIKU_5_5)
        .expect("Haiku is priced");

    // Act
    let short = haiku.for_prompt(100_000);
    let long = haiku.for_prompt(100_001);

    // Assert
    assert_eq!(haiku.source(), PricingSource::BuiltIn);
    assert_eq!(haiku.cost_tier(), CostTier::Cheap);
    assert_eq!(
        (
            short.input_per_mtok_micro_usd(),
            short.output_per_mtok_micro_usd(),
            short.cache_read_per_mtok_micro_usd(),
            short.cache_write_per_mtok_micro_usd()
        ),
        (100_000, 500_000, 10_000, 125_000)
    );
    assert_eq!(
        (
            long.input_per_mtok_micro_usd(),
            long.output_per_mtok_micro_usd(),
            long.cache_read_per_mtok_micro_usd(),
            long.cache_write_per_mtok_micro_usd(),
            long.source()
        ),
        (500_000, 2_500_000, 50_000, 625_000, PricingSource::BuiltIn)
    );
    // The other Claude models bill one price at any prompt length.
    let sonnet = ModelProvider::Anthropic
        .pricing(anthropic::CLAUDE_SONNET_5_5)
        .expect("Sonnet is priced");
    assert_eq!(sonnet.for_prompt(900_000), sonnet);
}

#[test]
fn anthropic_base_url_variable_takes_the_origin_or_the_versioned_url() {
    // Arrange
    let anthropic_urls = [
        ("https://proxy.example", "https://proxy.example/v1"),
        ("https://proxy.example/", "https://proxy.example/v1"),
        ("https://proxy.example/v1", "https://proxy.example/v1"),
        ("https://proxy.example/v1/", "https://proxy.example/v1/"),
    ];

    // Act
    let versioned =
        anthropic_urls.map(|(value, _)| ModelProvider::Anthropic.versioned(value.to_string()));
    let other = ModelProvider::Kimi.versioned("https://kimi.example".to_string());

    // Assert
    assert_eq!(
        versioned,
        anthropic_urls.map(|(_, expected)| expected.to_string())
    );
    assert_eq!(other, "https://kimi.example");
    assert_eq!(
        ModelProvider::Anthropic.default_base_url(),
        Some(anthropic::DEFAULT_ANTHROPIC_BASE_URL)
    );
}

#[test]
fn configuration_treats_a_blank_required_key_as_missing() {
    for provider in ModelProvider::all()
        .iter()
        .filter(|provider| provider.requires_api_key())
    {
        // Arrange
        let model = provider.known_models()[0];

        // Act
        let error = ModelConfiguration::new(*provider, model)
            .base_url("https://provider.example/v1")
            .client_from_environment(|_| Ok("  ".to_string()))
            .err()
            .expect("a blank required key should be rejected");

        // Assert
        assert!(
            matches!(error, ModelConfigurationError::ApiKey { name } if name == provider.api_key_environment()),
            "{provider}: {error:?}"
        );
    }
}
