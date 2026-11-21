//! Built-in model clients.
//!
//! [`ModelConfiguration`] selects a provider and model by name at runtime
//! and reads the provider's API key from an injected environment lookup:
//!
//! ```no_run
//! # #[cfg(feature = "providers-compat")]
//! # {
//! use ag_harness::provider::{MUSE_SPARK_1_3, ModelConfiguration, ModelProvider};
//!
//! let model = ModelConfiguration::new(ModelProvider::Muse, MUSE_SPARK_1_3)
//!     .client_from_environment(|name| std::env::var(name))?;
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`ModelProvider::OpenAi`] serves `OpenAI` models through the stateless,
//! streamed Responses API with `OPENAI_API_KEY`, and
//! [`ModelProvider::Anthropic`] serves Claude models through the streamed
//! Anthropic Messages API with `ANTHROPIC_API_KEY`. [`ModelProvider::ChatGpt`]
//! serves the same API billed to the user's `ChatGPT` plan, authenticated by
//! a refreshing sign-in stored in a credential file the host chooses with
//! [`ModelConfiguration::auth_file`] and [`ChatGptSignIn::new`], or else
//! names in `CHATGPT_AUTH_FILE`.
//! [`ModelProvider::OpenAiCompatible`] serves any model of a local or
//! self-hosted OpenAI-compatible server, by default Ollama's
//! `http://127.0.0.1:11434/v1`, with an optional API key and a context
//! profile declared through [`ModelProvider::declared_profile`] or sized to
//! the window the server runs through [`ModelProvider::running_profile`].
//! [`ModelProvider::local_server`] lists that server's models through
//! [`LocalServer`]. [`ModelProvider::pricing`] and
//! [`ModelProvider::capabilities`] declare each cataloged model's list
//! prices and capabilities, and [`ModelProvider::cost_tier`] derives its
//! [`CostTier`], so hosts can route simple tasks to cheap models;
//! [`ModelProvider::cost_tier_at`] classifies an OpenAI-compatible server
//! as [`CostTier::Local`] only when its endpoint is local or private.
//! [`crate::catalog::ModelCatalog::from_environment`] catalogs these models
//! by role.
//!
//! [`ProviderSpec`] opens the list: a host describes any OpenAI-compatible
//! Chat Completions, Responses, or Anthropic Messages provider by its
//! [`WireProtocol`], base URL, [`Auth`], extra headers and query parameters,
//! and request policy, and serves it through
//! [`crate::model::ModelClient::from_spec`]. The built-in providers are
//! presets of the same type, such as [`ProviderSpec::kimi`].
//!
//! This module requires a `providers-*` feature. Every cataloged provider's
//! entries, profiles, and prices are available with `providers-openai` or
//! `providers-compat`; building a client needs its family's feature:
//! `providers-openai` for [`ModelProvider::OpenAi`] and
//! [`ModelProvider::ChatGpt`], including the `ChatGPT` sign-in flow,
//! `providers-compat` for the Chat Completions providers, and
//! `providers-anthropic` for [`ModelProvider::Anthropic`] and the
//! `ProviderSpec::anthropic` preset.
//! [`ModelConfiguration::client_from_environment`] returns
//! `ModelConfigurationError::ProviderDisabled` for a provider whose family
//! feature is off.

#[cfg(feature = "providers-anthropic")]
mod anthropic;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod catalog;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod chatgpt;
#[cfg(feature = "providers-openai")]
mod chatgpt_sign_in;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod claude;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod kimi;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod muse;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod openai;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod openai_compatible;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
mod qwen;
mod spec;
#[cfg(test)]
#[cfg(feature = "providers-compat")]
mod test_support;

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use ag_router::chatgpt::ChatGptAuthError;
#[cfg(feature = "providers-openai")]
pub use ag_router::chatgpt::{ChatGptAuth, ChatGptCredentials, ChatGptLogin};
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use ag_router::{LocalInventory, LocalModel, LocalServer, LocalServerError, LocalServerKind};
#[cfg(feature = "providers-anthropic")]
pub use anthropic::{AnthropicConfig, DEFAULT_ANTHROPIC_BASE_URL};
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use catalog::{
    ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError,
};
#[cfg(feature = "providers-openai")]
pub(crate) use chatgpt::profile as chatgpt_profile;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use chatgpt::{
    AuthFileUnavailable, CHATGPT_AUTH_FILE_ENV, CHATGPT_CREDENTIAL_FILE, CHATGPT_GPT_5_6_LUNA,
    CHATGPT_GPT_5_6_SOL, CHATGPT_GPT_5_6_TERRA, CHATGPT_GPT_6_1_SOL, CHATGPT_GPT_6_1_TERRA,
    CHATGPT_GPT_6_ASTRA, CHATGPT_GPT_6_LUNA, CHATGPT_GPT_6_SOL,
};
#[cfg(feature = "providers-openai")]
pub use chatgpt::{
    ChatGptConfig, auth_file_in as chatgpt_auth_file_in, is_signed_in as chatgpt_is_signed_in,
};
#[cfg(feature = "providers-openai")]
pub use chatgpt_sign_in::{
    CHATGPT_AUTH_URL_ENV, ChatGptSignIn, ChatGptSignInError, PendingChatGptSignIn,
};
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub(crate) use claude::{CLAUDE_HAIKU_5_5, CLAUDE_OPUS_5_5};
#[cfg(feature = "providers-compat")]
pub use kimi::KimiConfig;
#[cfg(feature = "providers-compat")]
pub(crate) use kimi::profile as kimi_profile;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use kimi::{KIMI_K2_6, KIMI_K3};
#[cfg(feature = "providers-compat")]
pub use muse::MuseConfig;
#[cfg(feature = "providers-compat")]
pub(crate) use muse::profile as muse_profile;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use muse::{MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR};
#[cfg(feature = "providers-openai")]
pub use openai::OpenAiConfig;
#[cfg(feature = "providers-openai")]
pub(crate) use openai::profile as openai_profile;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use openai::{GPT_5_3_CODEX, GPT_5_6_TERRA, GPT_6_1_SOL, GPT_6_ASTRA, GPT_6_LUNA};
#[cfg(feature = "providers-compat")]
pub use openai_compatible::OpenAiCompatibleConfig;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use openai_compatible::{
    DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS, OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV,
    OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV,
};
#[cfg(feature = "providers-compat")]
pub use qwen::QwenConfig;
#[cfg(feature = "providers-compat")]
pub(crate) use qwen::profile as qwen_profile;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub use qwen::{QWEN_PLUS, QWEN3_7_FLASH, QWEN3_8_FLASH, QWEN3_8_MAX};
pub use spec::{
    Auth, CredentialSource, EffortNames, InstructionPlacement, Jitter, MaxTokensField,
    ProviderSpec, ProviderSpecError, ReasoningMapping, RetryKind, RetryPolicy,
    StructuredOutputMode, ThinkingOptions, WireProtocol,
};

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
#[doc(no_inline)]
pub use crate::catalog::CostTier;
