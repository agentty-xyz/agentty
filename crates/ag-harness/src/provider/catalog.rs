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
            Self::Muse => &[muse::MUSE_SPARK_1_3, muse::MUSE_SPARK_1_3_CONTRIBUTOR],
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
#[path = "catalog_test.rs"]
mod tests;
