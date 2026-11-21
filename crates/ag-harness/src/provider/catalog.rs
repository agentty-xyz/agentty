//! Built-in model provider catalog: known models, capabilities, list
//! prices, cost tiers, profiles, and client configuration from the
//! environment.

use std::path::PathBuf;
use std::str::FromStr;
use std::{env, fmt};

use ag_router::LocalServer;
use ag_router::chatgpt::ChatGptAuthError;
#[cfg(feature = "providers-openai")]
use ag_router::chatgpt::ChatGptCredentials;
use thiserror::Error;

#[cfg(feature = "providers-anthropic")]
use super::{AnthropicConfig, ProviderSpec};
use super::{ProviderSpecError, chatgpt, claude, kimi, muse, openai, openai_compatible, qwen};
use crate::catalog::{Capabilities, CostTier, ModelId, ModelSpec, Pricing};
use crate::model::{ModelClient, ModelMetadataError, ModelProfile};

/// Provider supported by the built-in model client catalog.
///
/// New providers join without a breaking release, so a `match` outside
/// this crate needs a wildcard arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelProvider {
    /// Anthropic's Messages API, serving Claude models billed to an API key.
    Anthropic,
    /// `OpenAI`'s Responses API billed to the user's `ChatGPT` plan through
    /// "Sign in with `ChatGPT`" instead of API credits.
    ChatGpt,
    /// Moonshot AI's Kimi API.
    Kimi,
    /// Meta's Model API for Muse models.
    Muse,
    /// `OpenAI`'s Responses API, serving the GPT models Codex runs.
    OpenAi,
    /// Any server implementing the `OpenAI` Chat Completions API, such as
    /// Ollama, llama.cpp, LM Studio, or vLLM, serving a host-named model.
    OpenAiCompatible,
    /// Alibaba Cloud Model Studio's Qwen API.
    Qwen,
}

impl ModelProvider {
    const ALL: [Self; 7] = [
        Self::Muse,
        Self::Kimi,
        Self::Qwen,
        Self::OpenAi,
        Self::ChatGpt,
        Self::Anthropic,
        Self::OpenAiCompatible,
    ];

    /// Returns every built-in provider in display order.
    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Returns the stable identifier for this provider.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::ChatGpt => "chatgpt",
            Self::Kimi => "kimi",
            Self::Muse => "muse",
            Self::OpenAi => "openai",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Qwen => "qwen",
        }
    }

    /// Returns representative model identifiers known to this provider, or
    /// none for [`Self::OpenAiCompatible`], whose server names its models.
    pub const fn known_models(self) -> &'static [&'static str] {
        match self {
            Self::Anthropic => &[
                claude::CLAUDE_OPUS_5_5,
                claude::CLAUDE_FABLE_5_2,
                claude::CLAUDE_SONNET_5_5,
                claude::CLAUDE_HAIKU_5_5,
            ],
            Self::ChatGpt => &[
                chatgpt::CHATGPT_GPT_6_1_SOL,
                chatgpt::CHATGPT_GPT_6_1_TERRA,
                chatgpt::CHATGPT_GPT_6_ASTRA,
                chatgpt::CHATGPT_GPT_6_SOL,
                chatgpt::CHATGPT_GPT_6_LUNA,
                chatgpt::CHATGPT_GPT_5_6_SOL,
                chatgpt::CHATGPT_GPT_5_6_TERRA,
                chatgpt::CHATGPT_GPT_5_6_LUNA,
            ],
            Self::Kimi => &[kimi::KIMI_K3, kimi::KIMI_K2_6],
            Self::Muse => &[muse::MUSE_SPARK_1_3, muse::MUSE_SPARK_1_3_CONTRIBUTOR],
            Self::OpenAi => &[
                openai::GPT_6_1_SOL,
                openai::GPT_6_ASTRA,
                openai::GPT_6_LUNA,
                openai::GPT_5_6_TERRA,
                openai::GPT_5_3_CODEX,
            ],
            Self::OpenAiCompatible => &[],
            Self::Qwen => &[
                qwen::QWEN3_8_MAX,
                qwen::QWEN_PLUS,
                qwen::QWEN3_8_FLASH,
                qwen::QWEN3_7_FLASH,
            ],
        }
    }

    /// Returns the endpoint-independent price class of `model`: every
    /// cataloged [`Self::ChatGpt`] model is [`CostTier::Plan`], and other
    /// cataloged models take the class of their [`Self::pricing`] through
    /// [`Pricing::cost_tier`].
    ///
    /// `None` means either a model this catalog does not list or a
    /// [`Self::OpenAiCompatible`] model, whose price depends on where the
    /// server runs. A caller that classifies OpenAI-compatible models uses
    /// [`Self::cost_tier_at`], the only method that can; `None` here does
    /// not mean no provider serves the model.
    pub fn cost_tier(self, model: &str) -> Option<CostTier> {
        match self {
            Self::OpenAiCompatible => None,
            Self::ChatGpt => self
                .known_models()
                .contains(&model)
                .then_some(CostTier::Plan),
            Self::Anthropic | Self::Kimi | Self::Muse | Self::OpenAi | Self::Qwen => {
                self.pricing(model).map(Pricing::cost_tier)
            }
        }
    }

    /// Returns the price class of `model` served from `base_url`.
    ///
    /// An [`Self::OpenAiCompatible`] server is [`CostTier::Local`] only when
    /// `base_url`, the endpoint its requests use, parses as a URL whose host
    /// is one of:
    ///
    /// - `localhost` or a name under `.localhost`, `.local` (mDNS), or
    ///   `.internal` (such as `host.docker.internal`), with or without a
    ///   trailing dot, or a single-label name such as a Compose service
    ///   (`ollama`), which public DNS does not serve;
    /// - an IPv4 loopback, unspecified, RFC 1918 private, link-local, or shared
    ///   (`100.64.0.0/10`, as Tailscale uses) address;
    /// - an IPv6 loopback, unspecified, unique-local, or link-local address, or
    ///   an IPv4-mapped address (`::ffff:a.b.c.d`) of a local IPv4 one.
    ///
    /// Any other host, such as a hosted gateway, and an unparsable URL are
    /// `None`, so the host declares the price instead of the catalog calling
    /// a server free; this is a heuristic, and a host that knows better
    /// classifies the server itself. Other providers bill the same at any
    /// endpoint and return [`Self::cost_tier`].
    pub fn cost_tier_at(self, model: &str, base_url: &str) -> Option<CostTier> {
        match self {
            Self::OpenAiCompatible => is_local_endpoint(base_url).then_some(CostTier::Local),
            Self::Anthropic
            | Self::ChatGpt
            | Self::Kimi
            | Self::Muse
            | Self::OpenAi
            | Self::Qwen => self.cost_tier(model),
        }
    }

    /// Returns the built-in list prices of `model`, or `None` for a model
    /// this catalog does not list or that is not billed per token, such as
    /// a [`Self::ChatGpt`] plan model or an [`Self::OpenAiCompatible`]
    /// server's model.
    pub fn pricing(self, model: &str) -> Option<Pricing> {
        self.cataloged(model)?.pricing
    }

    /// Returns what `model` can do, or `None` for a model this catalog does
    /// not list. An [`Self::OpenAiCompatible`] server's model is assumed to
    /// make tool calls and nothing more; the host declares other
    /// capabilities itself.
    pub fn capabilities(self, model: &str) -> Option<Capabilities> {
        let tools_only = Capabilities::NONE.with_tool_calls(true);
        if self == Self::OpenAiCompatible {
            return Some(tools_only);
        }
        let cataloged = self.cataloged(model)?;

        Some(
            tools_only
                .with_image_input(cataloged.supports.image_input())
                .with_reasoning(cataloged.supports.reasoning())
                .with_structured_output(matches!(
                    self,
                    Self::Anthropic | Self::ChatGpt | Self::Muse | Self::OpenAi
                )),
        )
    }

    /// Returns the catalog specification of a known `model` under its
    /// [`ModelId::cataloged`] key, with its capabilities, prices, and the
    /// profile and output limit this catalog declares, or `None` for a
    /// model this catalog does not list.
    pub fn model_spec(self, model: &str) -> Option<ModelSpec> {
        let capabilities = self.capabilities(model)?;
        let cataloged = self.cataloged(model)?;
        let (profile, max_output) = self.declared_limits(model);
        let mut spec = ModelSpec::new(ModelId::cataloged(self, model))
            .with_provider(self)
            .with_capabilities(capabilities);
        if let Some(pricing) = cataloged.pricing {
            spec = spec.with_pricing(pricing);
        }
        if let Some(profile) = profile {
            spec = spec.with_profile(profile);
        }
        if let Some(max_output) = max_output {
            spec = spec.with_max_output(max_output);
        }

        Some(spec)
    }

    /// Returns whether the provider may use `model`'s prompts and
    /// completions to train its models, which Meta's discounted
    /// contributor tier requires.
    pub fn trains_on_prompts(self, model: &str) -> bool {
        self == Self::Muse && model == muse::MUSE_SPARK_1_3_CONTRIBUTOR
    }

    /// Returns the environment variable containing the provider API key, or
    /// for [`Self::ChatGpt`] the optional override of its credential file.
    pub const fn api_key_environment(self) -> &'static str {
        match self {
            Self::Anthropic => claude::ANTHROPIC_API_KEY_ENV,
            Self::ChatGpt => chatgpt::CHATGPT_AUTH_FILE_ENV,
            Self::Kimi => kimi::KIMI_API_KEY_ENV,
            Self::Muse => muse::MODEL_API_KEY_ENV,
            Self::OpenAi => openai::OPENAI_API_KEY_ENV,
            Self::OpenAiCompatible => openai_compatible::OPENAI_COMPATIBLE_API_KEY_ENV,
            Self::Qwen => qwen::DASHSCOPE_API_KEY_ENV,
        }
    }

    /// Returns whether the provider needs an API key. A local
    /// OpenAI-compatible server usually accepts unauthenticated requests, so
    /// its key is optional, and [`Self::ChatGpt`] uses a stored sign-in.
    pub const fn requires_api_key(self) -> bool {
        !matches!(self, Self::ChatGpt | Self::OpenAiCompatible)
    }

    /// Returns whether the provider authenticates with a stored "Sign in
    /// with `ChatGPT`" sign-in instead of an API key.
    pub const fn requires_sign_in(self) -> bool {
        matches!(self, Self::ChatGpt)
    }

    /// Returns whether this build includes the provider's client family:
    /// `providers-openai` for [`Self::OpenAi`] and [`Self::ChatGpt`],
    /// `providers-anthropic` for [`Self::Anthropic`], and `providers-compat`
    /// for the Chat Completions providers.
    pub const fn is_enabled(self) -> bool {
        match self {
            Self::Anthropic => cfg!(feature = "providers-anthropic"),
            Self::ChatGpt | Self::OpenAi => cfg!(feature = "providers-openai"),
            Self::Kimi | Self::Muse | Self::OpenAiCompatible | Self::Qwen => {
                cfg!(feature = "providers-compat")
            }
        }
    }

    /// Fails with [`ModelConfigurationError::ProviderDisabled`] when this
    /// build omits the provider's client family.
    fn ensure_enabled(self) -> Result<(), ModelConfigurationError> {
        self.is_enabled()
            .then_some(())
            .ok_or(ModelConfigurationError::ProviderDisabled { provider: self })
    }

    /// Returns the environment variable containing the provider base URL.
    ///
    /// [`Self::Anthropic`] reads `ANTHROPIC_BASE_URL` as Anthropic's SDKs
    /// do, as the API origin without the `/v1` version path, which the
    /// client appends; an explicit [`ModelConfiguration::base_url`] takes
    /// the same form.
    pub const fn base_url_environment(self) -> &'static str {
        match self {
            Self::Anthropic => claude::ANTHROPIC_BASE_URL_ENV,
            Self::ChatGpt => chatgpt::CHATGPT_BASE_URL_ENV,
            Self::Kimi => kimi::KIMI_BASE_URL_ENV,
            Self::Muse => muse::MODEL_API_BASE_URL_ENV,
            Self::OpenAi => openai::OPENAI_BASE_URL_ENV,
            Self::OpenAiCompatible => openai_compatible::OPENAI_COMPATIBLE_BASE_URL_ENV,
            Self::Qwen => qwen::DASHSCOPE_BASE_URL_ENV,
        }
    }

    /// Returns the built-in base URL used when the environment has no
    /// override: Moonshot AI's international `https://api.moonshot.ai/v1`
    /// for [`Self::Kimi`], the `https://api.anthropic.com` origin for
    /// [`Self::Anthropic`], and Ollama's local endpoint for
    /// [`Self::OpenAiCompatible`]. [`Self::Qwen`] has none, since Model
    /// Studio serves each region from its own endpoint.
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some(claude::DEFAULT_BASE_URL),
            Self::ChatGpt => Some(chatgpt::DEFAULT_BASE_URL),
            Self::Kimi => Some(kimi::DEFAULT_BASE_URL),
            Self::Muse => Some(muse::DEFAULT_BASE_URL),
            Self::OpenAi => Some(openai::DEFAULT_BASE_URL),
            Self::OpenAiCompatible => Some(openai_compatible::DEFAULT_BASE_URL),
            Self::Qwen => None,
        }
    }

    /// Returns the context profile this catalog declares for `model`, or
    /// `None` when the host profiles the provider's models itself.
    ///
    /// [`Self::Kimi`] and [`Self::Muse`] declare the documented window and
    /// output reservation of each cataloged model, weighed by an estimator
    /// fitted to the provider's reported input tokens under a 17% (Kimi) or
    /// 18% (Muse) margin. [`Self::OpenAi`] and [`Self::Anthropic`] declare
    /// the documented window and output limit of each cataloged model, and
    /// [`Self::Qwen`] of its
    /// flash models, weighed by the default estimator under a 50% margin,
    /// and `None` for other models. [`Self::OpenAiCompatible`] serves
    /// models the harness cannot know, so its profile comes from
    /// `OPENAI_COMPATIBLE_CONTEXT_WINDOW` (default 32,768 tokens) and
    /// `OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS` (default 8,192), weighed the
    /// same way.
    ///
    /// # Errors
    ///
    /// Returns [`ModelConfigurationError`] for a token count that is not a
    /// positive integer, a reservation filling the whole window, or a value
    /// that is not valid Unicode.
    pub fn declared_profile(
        self,
        model: &str,
        environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Option<ModelProfile>, ModelConfigurationError> {
        match self {
            Self::Anthropic => Ok(claude::profile(model)),
            Self::ChatGpt => Ok(chatgpt::profile(model)),
            Self::Kimi => Ok(kimi::profile(model)),
            Self::Muse => Ok(muse::profile(model)),
            Self::OpenAi => Ok(openai::profile(model)),
            Self::OpenAiCompatible => {
                openai_compatible::profile_from_environment(environment).map(Some)
            }
            Self::Qwen => Ok(qwen::profile(model)),
        }
    }

    /// Returns the profile of a model the server runs with a
    /// `running_context_window`-token window, or `None` for providers whose
    /// catalog models the host profiles itself.
    ///
    /// For [`Self::OpenAiCompatible`], a declared
    /// `OPENAI_COMPATIBLE_CONTEXT_WINDOW` larger than the running window is
    /// capped to it, so requests never outgrow what the server keeps, and an
    /// undeclared output reservation is a quarter of the window, at most
    /// 8,192 tokens.
    ///
    /// # Errors
    ///
    /// Returns [`ModelConfigurationError`] like [`Self::declared_profile`].
    pub fn running_profile(
        self,
        environment: impl FnMut(&str) -> Result<String, env::VarError>,
        running_context_window: u64,
    ) -> Result<Option<ModelProfile>, ModelConfigurationError> {
        match self {
            Self::OpenAiCompatible => {
                openai_compatible::running_profile(environment, running_context_window).map(Some)
            }
            Self::Anthropic
            | Self::ChatGpt
            | Self::Kimi
            | Self::Muse
            | Self::OpenAi
            | Self::Qwen => Ok(None),
        }
    }

    /// Returns a client for the local server behind this provider, or `None`
    /// for hosted providers.
    ///
    /// [`Self::OpenAiCompatible`] reads `OPENAI_COMPATIBLE_BASE_URL`,
    /// defaulting to Ollama's local endpoint, and the optional
    /// `OPENAI_COMPATIBLE_API_KEY`.
    ///
    /// # Errors
    ///
    /// Returns [`ModelConfigurationError::Environment`] for a base URL that
    /// is not valid Unicode.
    pub fn local_server(
        self,
        environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Option<LocalServer>, ModelConfigurationError> {
        match self {
            Self::OpenAiCompatible => openai_compatible::local_server(environment).map(Some),
            Self::Anthropic
            | Self::ChatGpt
            | Self::Kimi
            | Self::Muse
            | Self::OpenAi
            | Self::Qwen => Ok(None),
        }
    }

    /// Builds the client, or fails for a provider whose family feature is
    /// not compiled in.
    fn client(
        self,
        credential: Credential,
        base_url: String,
        model: String,
    ) -> Result<ModelClient, ModelConfigurationError> {
        let (api_key, auth_file) = match credential {
            Credential::ApiKey(api_key) => (api_key, None::<PathBuf>),
            #[cfg(feature = "providers-openai")]
            Credential::SignIn(auth_file) => (None, Some(auth_file)),
        };
        let client = match self {
            // The specification rejects a blank key, so it counts as missing;
            // the base URL is the API origin, as `ANTHROPIC_BASE_URL` names it.
            #[cfg(feature = "providers-anthropic")]
            Self::Anthropic => {
                let api_key = api_key.filter(|key| !key.trim().is_empty()).ok_or(
                    ModelConfigurationError::ApiKey {
                        name: claude::ANTHROPIC_API_KEY_ENV,
                    },
                )?;
                let spec = ProviderSpec::anthropic(AnthropicConfig {
                    api_key,
                    base_url: format!("{}/v1", base_url.trim_end_matches('/')),
                    model,
                });

                return ModelClient::from_spec(spec).map_err(ModelConfigurationError::from);
            }
            #[cfg(feature = "providers-openai")]
            Self::ChatGpt => ModelClient::chatgpt(chatgpt::ChatGptConfig {
                auth_file: auth_file.unwrap_or_default(),
                base_url,
                model,
            }),
            #[cfg(feature = "providers-compat")]
            Self::Kimi => ModelClient::kimi(kimi::KimiConfig {
                api_key: api_key.unwrap_or_default(),
                base_url,
                model,
            }),
            #[cfg(feature = "providers-compat")]
            Self::Muse => ModelClient::muse(muse::MuseConfig {
                api_key: api_key.unwrap_or_default(),
                base_url,
                model,
            }),
            #[cfg(feature = "providers-openai")]
            Self::OpenAi => ModelClient::openai(openai::OpenAiConfig {
                api_key: api_key.unwrap_or_default(),
                base_url,
                model,
            }),
            #[cfg(feature = "providers-compat")]
            Self::OpenAiCompatible => {
                ModelClient::openai_compatible(openai_compatible::OpenAiCompatibleConfig {
                    api_key,
                    base_url,
                    model,
                })
            }
            #[cfg(feature = "providers-compat")]
            Self::Qwen => ModelClient::qwen(qwen::QwenConfig {
                api_key: api_key.unwrap_or_default(),
                base_url,
                model,
            }),
            // `client_from_environment` rejects a disabled family first; its
            // credential and endpoint go unused here.
            #[cfg(not(all(
                feature = "providers-anthropic",
                feature = "providers-compat",
                feature = "providers-openai"
            )))]
            provider => {
                drop((api_key, auth_file, base_url, model));

                return Err(ModelConfigurationError::ProviderDisabled { provider });
            }
        };

        client.map_err(ModelConfigurationError::from)
    }

    /// Returns the context profile and output limit this catalog declares
    /// for `model`, independent of the environment; an OpenAI-compatible
    /// server declares neither, since its limits come from the server.
    fn declared_limits(self, model: &str) -> (Option<ModelProfile>, Option<u64>) {
        match self {
            Self::Anthropic => (claude::profile(model), claude::max_output_tokens(model)),
            Self::ChatGpt => (chatgpt::profile(model), chatgpt::max_output_tokens(model)),
            Self::Kimi => (kimi::profile(model), kimi::max_output_tokens(model)),
            Self::Muse => (muse::profile(model), muse::max_output_tokens(model)),
            Self::OpenAi => (openai::profile(model), openai::max_output_tokens(model)),
            Self::OpenAiCompatible => (None, None),
            Self::Qwen => (qwen::profile(model), qwen::max_output_tokens(model)),
        }
    }

    /// Returns the catalog record of `model`, if this provider lists it.
    fn cataloged(self, model: &str) -> Option<&'static CatalogedModel> {
        CATALOGED_MODELS
            .iter()
            .find(|cataloged| cataloged.provider == self && cataloged.model == model)
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
    #[cfg(feature = "providers-openai")]
    auth_file: Option<PathBuf>,
    base_url: Option<String>,
    model: String,
    provider: ModelProvider,
}

impl ModelConfiguration {
    /// Creates configuration for `model` served by `provider`.
    pub fn new(provider: ModelProvider, model: impl Into<String>) -> Self {
        Self {
            #[cfg(feature = "providers-openai")]
            auth_file: None,
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

    /// Sets the credential file of a [`ModelProvider::ChatGpt`] sign-in,
    /// in place of `CHATGPT_AUTH_FILE`, so the host decides where its
    /// sign-in lives. Other providers ignore it, so a host sets it only when
    /// [`ModelProvider::requires_sign_in`] holds. Available only with the
    /// `providers-openai` feature, like the `ChatGPT` provider itself.
    #[cfg(feature = "providers-openai")]
    #[must_use]
    pub fn auth_file(mut self, auth_file: impl Into<PathBuf>) -> Self {
        self.auth_file = Some(auth_file.into());

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
    ///
    /// [`ModelProvider::ChatGpt`] reads the credential file set by
    /// [`Self::auth_file`], else the one `CHATGPT_AUTH_FILE` names, and
    /// requires the user to have signed in. A provider this build omits
    /// fails with [`ModelConfigurationError::ProviderDisabled`] before any
    /// variable or credential file is read.
    pub fn client_from_environment(
        mut self,
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<ModelClient, ModelConfigurationError> {
        self.provider.ensure_enabled()?;
        #[cfg(feature = "providers-openai")]
        let credential = if self.provider.requires_sign_in() {
            let auth_file = match self.auth_file.take() {
                Some(auth_file) => auth_file,
                None => chatgpt::auth_file(&mut environment)?,
            };
            ChatGptCredentials::load(&auth_file)?;
            Credential::SignIn(auth_file)
        } else {
            self.api_key_credential(&mut environment)?
        };
        // Only the disabled `ChatGPT` provider signs in.
        #[cfg(not(feature = "providers-openai"))]
        let credential = self.api_key_credential(&mut environment)?;
        let base_url = self.resolve_base_url(environment)?;

        self.provider.client(credential, base_url, self.model)
    }

    /// Returns the provider's API key, or `None` when it is optional and
    /// unset or blank.
    fn api_key_credential(
        &self,
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Credential, ModelConfigurationError> {
        let api_key_environment = self.provider.api_key_environment();
        match environment(api_key_environment) {
            Ok(api_key) if self.provider.requires_api_key() || !api_key.trim().is_empty() => {
                Ok(Credential::ApiKey(Some(api_key)))
            }
            Ok(_) | Err(env::VarError::NotPresent) if !self.provider.requires_api_key() => {
                Ok(Credential::ApiKey(None))
            }
            _ => Err(ModelConfigurationError::ApiKey {
                name: api_key_environment,
            }),
        }
    }

    /// Returns the explicit base URL, else the provider's environment
    /// variable, else its default.
    fn resolve_base_url(
        &mut self,
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<String, ModelConfigurationError> {
        if let Some(base_url) = self.base_url.take() {
            return Ok(base_url);
        }
        let base_url_environment = self.provider.base_url_environment();

        match environment(base_url_environment) {
            Ok(base_url) if !base_url.trim().is_empty() => Ok(base_url),
            // A blank variable counts as unset, so it falls back to the
            // default like a missing one instead of failing validation.
            Ok(_) | Err(env::VarError::NotPresent) => {
                self.provider.default_base_url().map(str::to_string).ok_or(
                    ModelConfigurationError::BaseUrl {
                        name: base_url_environment,
                    },
                )
            }
            Err(source) => Err(ModelConfigurationError::Environment {
                name: base_url_environment,
                source,
            }),
        }
    }
}

/// How a configured client authenticates.
enum Credential {
    /// API key read from the provider's variable, or `None` when optional.
    ApiKey(Option<String>),
    /// Credential file of a stored "Sign in with `ChatGPT`" sign-in.
    #[cfg(feature = "providers-openai")]
    SignIn(PathBuf),
}

/// Failure returned while configuring a built-in model client.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModelConfigurationError {
    /// The `ChatGPT` credential file location is unknown.
    #[error(transparent)]
    AuthFile(#[from] chatgpt::AuthFileUnavailable),
    /// The user has not signed in with `ChatGPT`, or the stored sign-in is
    /// unreadable.
    #[error(transparent)]
    SignIn(#[from] ChatGptAuthError),
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
    /// No request can be sent with the provider specification built from
    /// the configuration, such as for a base URL that is not a URL.
    #[error(transparent)]
    Spec(#[from] ProviderSpecError),
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
    /// This build omits the provider's client; see
    /// [`ModelProvider::is_enabled`].
    #[error("{provider} support is not compiled in; enable its `providers-*` feature")]
    ProviderDisabled {
        /// Provider whose client family is disabled.
        provider: ModelProvider,
    },
}

/// Returns whether `base_url` reaches a server on this machine or a private
/// network, by the host rules [`ModelProvider::cost_tier_at`] documents. An
/// unparsable URL is not local.
fn is_local_endpoint(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };

    match url.host() {
        Some(url::Host::Domain(name)) => is_local_name(name),
        Some(url::Host::Ipv4(address)) => is_local_ipv4(address),
        Some(url::Host::Ipv6(address)) => match address.to_ipv4_mapped() {
            Some(mapped) => is_local_ipv4(mapped),
            None => {
                address.is_loopback()
                    || address.is_unspecified()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
            }
        },
        None => false,
    }
}

/// Returns whether host `name` resolves only on this machine or a private
/// network: `localhost`, a name under `.localhost`, `.local`, or
/// `.internal`, or a single-label name, ignoring a trailing dot.
fn is_local_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.strip_suffix('.').unwrap_or(&name);

    !name.contains('.')
        || [".localhost", ".local", ".internal"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

/// Returns whether `address` is loopback, unspecified, RFC 1918 private,
/// link-local, or in the shared `100.64.0.0/10` range.
fn is_local_ipv4(address: std::net::Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();

    address.is_loopback()
        || address.is_unspecified()
        || address.is_private()
        || address.is_link_local()
        || (first == 100 && (64..128).contains(&second))
}

/// Catalog record of one known model: its image and reasoning support, as
/// the provider's wire policy accepts them, and its list prices.
struct CatalogedModel {
    model: &'static str,
    pricing: Option<Pricing>,
    provider: ModelProvider,
    /// Image and reasoning support; [`ModelProvider::capabilities`] adds
    /// the provider-wide ones.
    supports: Capabilities,
}

/// Image input and reasoning effort, which most cataloged models support.
const MULTIMODAL_REASONING: Capabilities = Capabilities::NONE
    .with_image_input(true)
    .with_reasoning(true);

/// Every cataloged model. Hosted list prices, in micro-USD per million
/// input and output tokens at the lowest input tier, are as published on
/// 2026-10-29 by Meta's Model API pricing, Alibaba Cloud Model Studio's
/// international (Singapore) pricing, Moonshot AI's Kimi pricing, and
/// `OpenAI`'s API pricing, and on 2026-11-21 by Anthropic's Claude API
/// pricing. Cache discounts that some of these providers publish are not
/// modeled yet, so cached input falls back to the input price. `ChatGPT`
/// plan models carry no per-token price.
const CATALOGED_MODELS: &[CatalogedModel] = &[
    CatalogedModel {
        model: muse::MUSE_SPARK_1_3,
        pricing: Some(Pricing::built_in(1_250_000, 4_250_000)),
        provider: ModelProvider::Muse,
        supports: MULTIMODAL_REASONING,
    },
    // Meta trains on this tier's prompts and completions.
    CatalogedModel {
        model: muse::MUSE_SPARK_1_3_CONTRIBUTOR,
        pricing: Some(Pricing::built_in(100_000, 200_000)),
        provider: ModelProvider::Muse,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: kimi::KIMI_K3,
        pricing: Some(Pricing::built_in(3_000_000, 15_000_000)),
        provider: ModelProvider::Kimi,
        supports: MULTIMODAL_REASONING,
    },
    // Moonshot AI retires the K2 models by the end of 2026.
    CatalogedModel {
        model: kimi::KIMI_K2_6,
        pricing: Some(Pricing::built_in(950_000, 4_000_000)),
        provider: ModelProvider::Kimi,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: qwen::QWEN3_8_MAX,
        pricing: Some(Pricing::built_in(2_000_000, 6_000_000)),
        provider: ModelProvider::Qwen,
        supports: MULTIMODAL_REASONING,
    },
    // Under 256,000 input tokens.
    CatalogedModel {
        model: qwen::QWEN_PLUS,
        pricing: Some(Pricing::built_in(400_000, 1_200_000)),
        provider: ModelProvider::Qwen,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: qwen::QWEN3_8_FLASH,
        pricing: Some(Pricing::built_in(150_000, 470_000)),
        provider: ModelProvider::Qwen,
        supports: MULTIMODAL_REASONING,
    },
    // Under 32,000 input tokens; text only, without reasoning effort.
    CatalogedModel {
        model: qwen::QWEN3_7_FLASH,
        pricing: Some(Pricing::built_in(30_000, 130_000)),
        provider: ModelProvider::Qwen,
        supports: Capabilities::NONE,
    },
    CatalogedModel {
        model: openai::GPT_6_1_SOL,
        pricing: Some(Pricing::built_in(2_000_000, 10_000_000)),
        provider: ModelProvider::OpenAi,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: openai::GPT_6_ASTRA,
        pricing: Some(Pricing::built_in(10_000_000, 50_000_000)),
        provider: ModelProvider::OpenAi,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: openai::GPT_6_LUNA,
        pricing: Some(Pricing::built_in(100_000, 500_000)),
        provider: ModelProvider::OpenAi,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: openai::GPT_5_6_TERRA,
        pricing: Some(Pricing::built_in(2_000_000, 12_000_000)),
        provider: ModelProvider::OpenAi,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: openai::GPT_5_3_CODEX,
        pricing: Some(Pricing::built_in(1_750_000, 14_000_000)),
        provider: ModelProvider::OpenAi,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: claude::CLAUDE_OPUS_5_5,
        pricing: Some(Pricing::built_in(4_000_000, 20_000_000)),
        provider: ModelProvider::Anthropic,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: claude::CLAUDE_FABLE_5_2,
        pricing: Some(Pricing::built_in(8_000_000, 40_000_000)),
        provider: ModelProvider::Anthropic,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: claude::CLAUDE_SONNET_5_5,
        pricing: Some(Pricing::built_in(2_000_000, 10_000_000)),
        provider: ModelProvider::Anthropic,
        supports: MULTIMODAL_REASONING,
    },
    // Prompts of up to 100,000 tokens.
    CatalogedModel {
        model: claude::CLAUDE_HAIKU_5_5,
        pricing: Some(Pricing::built_in(100_000, 500_000)),
        provider: ModelProvider::Anthropic,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_6_1_SOL,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_6_1_TERRA,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_6_ASTRA,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_6_SOL,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_6_LUNA,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_5_6_SOL,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_5_6_TERRA,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
    CatalogedModel {
        model: chatgpt::CHATGPT_GPT_5_6_LUNA,
        pricing: None,
        provider: ModelProvider::ChatGpt,
        supports: MULTIMODAL_REASONING,
    },
];

#[cfg(test)]
#[cfg(all(
    feature = "providers-anthropic",
    feature = "providers-compat",
    feature = "providers-openai"
))]
#[path = "catalog_test.rs"]
mod tests;

#[cfg(test)]
#[cfg(not(all(
    feature = "providers-anthropic",
    feature = "providers-compat",
    feature = "providers-openai"
)))]
#[path = "catalog_disabled_test.rs"]
mod disabled_tests;
