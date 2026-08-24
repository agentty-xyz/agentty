use std::str::FromStr;
use std::{env, fmt};

use thiserror::Error;

use super::{kimi, muse, qwen};
use crate::model::{ModelClient, ModelMetadataError};

/// Provider supported by the built-in model client catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelProvider {
    /// Moonshot AI's Kimi API.
    Kimi,
    /// Meta's Model API for Muse models.
    Muse,
    /// Alibaba Cloud Model Studio's Qwen API.
    Qwen,
}

impl ModelProvider {
    const ALL: [Self; 3] = [Self::Muse, Self::Kimi, Self::Qwen];

    /// Returns every built-in provider in display order.
    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Returns the stable identifier for this provider.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kimi => "kimi",
            Self::Muse => "muse",
            Self::Qwen => "qwen",
        }
    }

    /// Returns representative model identifiers known to this provider.
    pub const fn known_models(self) -> &'static [&'static str] {
        match self {
            Self::Kimi => &[kimi::KIMI_K2_6],
            Self::Muse => &[muse::MUSE_SPARK_1_2, muse::MUSE_SPARK_1_2_CONTRIBUTOR],
            Self::Qwen => &[qwen::QWEN_PLUS],
        }
    }

    /// Returns the environment variable containing the provider API key.
    pub const fn api_key_environment(self) -> &'static str {
        match self {
            Self::Kimi => kimi::KIMI_API_KEY_ENV,
            Self::Muse => muse::MODEL_API_KEY_ENV,
            Self::Qwen => qwen::DASHSCOPE_API_KEY_ENV,
        }
    }

    /// Returns the environment variable containing the provider base URL.
    pub const fn base_url_environment(self) -> &'static str {
        match self {
            Self::Kimi => kimi::KIMI_BASE_URL_ENV,
            Self::Muse => muse::MODEL_API_BASE_URL_ENV,
            Self::Qwen => qwen::DASHSCOPE_BASE_URL_ENV,
        }
    }

    /// Returns the built-in base URL used when the environment has no override.
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Muse => Some(muse::DEFAULT_BASE_URL),
            Self::Kimi | Self::Qwen => None,
        }
    }

    fn client(
        self,
        api_key: String,
        base_url: String,
        model: String,
    ) -> Result<ModelClient, ModelMetadataError> {
        match self {
            Self::Kimi => ModelClient::kimi(kimi::KimiConfig {
                api_key,
                base_url,
                model,
            }),
            Self::Muse => ModelClient::muse(muse::MuseConfig {
                api_key,
                base_url,
                model,
            }),
            Self::Qwen => ModelClient::qwen(qwen::QwenConfig {
                api_key,
                base_url,
                model,
            }),
        }
    }
}

impl fmt::Display for ModelProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModelProvider {
    type Err = ModelProviderParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|provider| provider.as_str() == value)
            .ok_or_else(|| ModelProviderParseError {
                value: value.to_string(),
            })
    }
}

/// Unsupported built-in model provider identifier.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("unsupported model provider `{value}`")]
pub struct ModelProviderParseError {
    value: String,
}

/// Provider-neutral configuration used to construct a built-in model client.
pub struct ModelConfiguration {
    base_url: Option<String>,
    model: String,
    provider: ModelProvider,
}

impl ModelConfiguration {
    /// Creates configuration for `model` served by `provider`.
    pub fn new(provider: ModelProvider, model: impl Into<String>) -> Self {
        Self {
            base_url: None,
            model: model.into(),
            provider,
        }
    }

    /// Overrides the provider base URL and its environment variable.
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());

        self
    }

    /// Constructs the configured client through an injected environment lookup.
    ///
    /// # Errors
    ///
    /// Returns [`ModelConfigurationError`] when credentials, an endpoint, or
    /// the model identifier are unavailable or invalid.
    pub fn client_from_environment(
        self,
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<ModelClient, ModelConfigurationError> {
        let api_key_environment = self.provider.api_key_environment();
        let api_key =
            environment(api_key_environment).map_err(|_| ModelConfigurationError::ApiKey {
                name: api_key_environment,
            })?;
        let base_url_environment = self.provider.base_url_environment();
        let base_url = if let Some(base_url) = self.base_url {
            base_url
        } else {
            match environment(base_url_environment) {
                Ok(base_url) => base_url,
                Err(env::VarError::NotPresent) => {
                    self.provider.default_base_url().map(str::to_string).ok_or(
                        ModelConfigurationError::BaseUrl {
                            name: base_url_environment,
                        },
                    )?
                }
                Err(source) => {
                    return Err(ModelConfigurationError::Environment {
                        name: base_url_environment,
                        source,
                    });
                }
            }
        };

        self.provider
            .client(api_key, base_url, self.model)
            .map_err(ModelConfigurationError::from)
    }
}

/// Failure returned while configuring a built-in model client.
#[derive(Debug, Error)]
pub enum ModelConfigurationError {
    /// The provider API key is missing or is not valid Unicode.
    #[error("{name} is unavailable")]
    ApiKey {
        /// Environment variable that could not be read.
        name: &'static str,
    },
    /// Neither a base URL override nor a provider default is available.
    #[error("no explicit base URL was provided and {name} is unavailable")]
    BaseUrl {
        /// Provider base-URL environment variable.
        name: &'static str,
    },
    /// An optional provider environment variable is not valid Unicode.
    #[error("{name} is unavailable: {source}")]
    Environment {
        /// Environment variable that could not be read.
        name: &'static str,
        /// Environment lookup failure.
        source: env::VarError,
    },
    /// The selected model identifier is invalid.
    #[error(transparent)]
    Metadata(#[from] ModelMetadataError),
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ModelProvider::Qwen
            ]
        );
        assert_eq!(
            ModelProvider::Muse.known_models(),
            &["muse-spark-1.2", "muse-spark-1.2-contributor"]
        );
        assert_eq!(ModelProvider::Kimi.known_models(), &["kimi-k2.6"]);
        assert_eq!(ModelProvider::Qwen.known_models(), &["qwen-plus"]);
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
        assert_eq!(displayed, ["muse", "kimi", "qwen"]);
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
            let mut requested_environment = Vec::new();
            let client = ModelConfiguration::new(*provider, provider.known_models()[0])
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
            assert_eq!(client.metadata().model(), provider.known_models()[0]);
        }
    }

    #[test]
    fn configuration_uses_default_and_explicit_base_urls() {
        // Arrange
        let default = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_2);
        let explicit = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_2)
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
        let configuration = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_2);

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
        let configuration = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_2);

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
        let configuration = ModelConfiguration::new(ModelProvider::Muse, muse::MUSE_SPARK_1_2);

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
        let configuration = ModelConfiguration::new(ModelProvider::Muse, "  ")
            .base_url("https://models.example/v1");

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
}
