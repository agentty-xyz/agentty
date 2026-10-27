use std::str::FromStr;
use std::{env, fmt};

use thiserror::Error;

use super::{kimi, muse, openai_compatible, qwen};
use crate::model::{ModelClient, ModelMetadataError, ModelProfile};

/// Provider supported by the built-in model client catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelProvider {
    /// Moonshot AI's Kimi API.
    Kimi,
    /// Meta's Model API for Muse models.
    Muse,
    /// Any server implementing the `OpenAI` Chat Completions API, such as
    /// Ollama, llama.cpp, LM Studio, or vLLM, serving a host-named model.
    OpenAiCompatible,
    /// Alibaba Cloud Model Studio's Qwen API.
    Qwen,
}

impl ModelProvider {
    const ALL: [Self; 4] = [Self::Muse, Self::Kimi, Self::Qwen, Self::OpenAiCompatible];

    /// Returns every built-in provider in display order.
    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Returns the stable identifier for this provider.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kimi => "kimi",
            Self::Muse => "muse",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Qwen => "qwen",
        }
    }

    /// Returns representative model identifiers known to this provider, or
    /// none for [`Self::OpenAiCompatible`], whose server names its models.
    pub const fn known_models(self) -> &'static [&'static str] {
        match self {
            Self::Kimi => &[kimi::KIMI_K3, kimi::KIMI_K2_6],
            Self::Muse => &[muse::MUSE_SPARK_1_3, muse::MUSE_SPARK_1_3_CONTRIBUTOR],
            Self::OpenAiCompatible => &[],
            Self::Qwen => &[qwen::QWEN3_8_MAX, qwen::QWEN_PLUS],
        }
    }

    /// Returns the environment variable containing the provider API key.
    pub const fn api_key_environment(self) -> &'static str {
        match self {
            Self::Kimi => kimi::KIMI_API_KEY_ENV,
            Self::Muse => muse::MODEL_API_KEY_ENV,
            Self::OpenAiCompatible => openai_compatible::OPENAI_COMPATIBLE_API_KEY_ENV,
            Self::Qwen => qwen::DASHSCOPE_API_KEY_ENV,
        }
    }

    /// Returns whether the provider needs an API key. A local
    /// OpenAI-compatible server usually accepts unauthenticated requests, so
    /// its key is optional.
    pub const fn requires_api_key(self) -> bool {
        !matches!(self, Self::OpenAiCompatible)
    }

    /// Returns the environment variable containing the provider base URL.
    pub const fn base_url_environment(self) -> &'static str {
        match self {
            Self::Kimi => kimi::KIMI_BASE_URL_ENV,
            Self::Muse => muse::MODEL_API_BASE_URL_ENV,
            Self::OpenAiCompatible => openai_compatible::OPENAI_COMPATIBLE_BASE_URL_ENV,
            Self::Qwen => qwen::DASHSCOPE_BASE_URL_ENV,
        }
    }

    /// Returns the built-in base URL used when the environment has no
    /// override: Ollama's local endpoint for [`Self::OpenAiCompatible`].
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Muse => Some(muse::DEFAULT_BASE_URL),
            Self::OpenAiCompatible => Some(openai_compatible::DEFAULT_BASE_URL),
            Self::Kimi | Self::Qwen => None,
        }
    }

    /// Returns the model profile the environment declares for this
    /// provider's models, or `None` for providers whose catalog models the
    /// host profiles itself.
    ///
    /// [`Self::OpenAiCompatible`] serves models the harness cannot know, so
    /// its profile comes from `OPENAI_COMPATIBLE_CONTEXT_WINDOW` (default
    /// 32,768 tokens) and `OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS` (default
    /// 8,192), weighed by the default estimator under a 50% margin.
    ///
    /// # Errors
    ///
    /// Returns [`ModelConfigurationError`] for a token count that is not a
    /// positive integer, a reservation filling the whole window, or a value
    /// that is not valid Unicode.
    pub fn environment_profile(
        self,
        environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Option<ModelProfile>, ModelConfigurationError> {
        match self {
            Self::OpenAiCompatible => {
                openai_compatible::profile_from_environment(environment).map(Some)
            }
            Self::Kimi | Self::Muse | Self::Qwen => Ok(None),
        }
    }

    fn client(
        self,
        api_key: Option<String>,
        base_url: String,
        model: String,
    ) -> Result<ModelClient, ModelMetadataError> {
        match self {
            Self::Kimi => ModelClient::kimi(kimi::KimiConfig {
                api_key: api_key.unwrap_or_default(),
                base_url,
                model,
            }),
            Self::Muse => ModelClient::muse(muse::MuseConfig {
                api_key: api_key.unwrap_or_default(),
                base_url,
                model,
            }),
            Self::OpenAiCompatible => {
                ModelClient::openai_compatible(openai_compatible::OpenAiCompatibleConfig {
                    api_key,
                    base_url,
                    model,
                })
            }
            Self::Qwen => ModelClient::qwen(qwen::QwenConfig {
                api_key: api_key.unwrap_or_default(),
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
    /// A provider whose key is optional sends no key when its variable is
    /// unset or blank.
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
        let api_key = match environment(api_key_environment) {
            Ok(api_key) if self.provider.requires_api_key() || !api_key.trim().is_empty() => {
                Some(api_key)
            }
            Ok(_) | Err(env::VarError::NotPresent) if !self.provider.requires_api_key() => None,
            _ => {
                return Err(ModelConfigurationError::ApiKey {
                    name: api_key_environment,
                });
            }
        };
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
    /// A declared token count is not a positive integer.
    #[error("{name} must be a positive token count, not `{value}`")]
    InvalidTokenCount {
        /// Environment variable holding the count.
        name: &'static str,
        /// Rejected value.
        value: String,
    },
    /// The selected model identifier is invalid.
    #[error(transparent)]
    Metadata(#[from] ModelMetadataError),
    /// The declared response reservation leaves no room for input.
    #[error(
        "the {max_output_tokens}-token output reservation must be smaller than the \
         {context_window}-token context window"
    )]
    OutputExceedsContextWindow {
        /// Declared context window in tokens.
        context_window: u64,
        /// Declared response reservation in tokens.
        max_output_tokens: u64,
    },
}

#[cfg(test)]
#[path = "catalog_test.rs"]
mod tests;
