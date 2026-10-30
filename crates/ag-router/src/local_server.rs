//! Model inventory and running context window of local OpenAI-compatible
//! servers.
//!
//! Ollama is recognized by its native API under the endpoint root: models
//! come from `/api/tags`, with `/api/show` filling a missing context length
//! or capability list, and the window a loaded model runs with comes from
//! `/api/ps`. Any other server lists its models through `GET {base}/models`,
//! where vLLM's `max_model_len` is the window it runs.

use std::collections::HashMap;
use std::time::Duration;

use reqwest::{RequestBuilder, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;

/// Deadline for one inventory request, short so a stopped or hung server
/// never stalls a picker refresh.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Deadline for Ollama to load a model into memory, which can take minutes
/// for a large model on a cold disk.
const LOAD_TIMEOUT: Duration = Duration::from_mins(5);
/// Ollama capability required to generate text.
const COMPLETION_CAPABILITY: &str = "completion";
/// Ollama capability required to call tools.
const TOOLS_CAPABILITY: &str = "tools";
/// Suffix of the Ollama `model_info` key holding the trained context length,
/// as in `qwen3.context_length`.
const CONTEXT_LENGTH_SUFFIX: &str = ".context_length";

/// Software serving a local OpenAI-compatible endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalServerKind {
    /// Ollama, recognized by its native `/api` endpoints.
    Ollama,
    /// Any other server implementing the `OpenAI` models API.
    OpenAiCompatible,
}

impl LocalServerKind {
    /// Returns the name shown to users.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::OpenAiCompatible => "OpenAI-compatible server",
        }
    }
}

/// One model a local server serves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalModel {
    /// Context window in tokens the model supports, when reported.
    pub context_window: Option<u64>,
    /// Model identifier sent in chat requests, such as `qwen3:latest`.
    pub id: String,
    /// Context window in tokens the server runs the model with now, when
    /// reported. Ollama reports it only while the model is loaded.
    pub loaded_context_window: Option<u64>,
    /// Whether the model can call tools, when the server reports
    /// capabilities.
    pub supports_tools: Option<bool>,
}

/// Models one local server serves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalInventory {
    /// Software serving the endpoint.
    pub kind: LocalServerKind,
    /// Text-generating models in the order the server lists them.
    pub models: Vec<LocalModel>,
}

/// Failure while querying a local server.
#[derive(Debug, Error)]
pub enum LocalServerError {
    /// The server could not be reached or its response body was invalid.
    #[error("request to {url} failed: {source}")]
    Request {
        /// Requested URL.
        url: String,
        /// Transport or decoding failure.
        source: reqwest::Error,
    },
    /// The server answered with a non-success status.
    #[error("{url} returned HTTP {status}")]
    Status {
        /// Requested URL.
        url: String,
        /// HTTP status code.
        status: u16,
    },
}

/// Client for one local OpenAI-compatible endpoint.
pub struct LocalServer {
    api_key: Option<String>,
    base_url: String,
    client: reqwest::Client,
}

impl LocalServer {
    /// Creates a client for `base_url`, the endpoint's OpenAI-compatible
    /// version path such as Ollama's `http://127.0.0.1:11434/v1`. A nonblank
    /// `api_key` is sent as a bearer token.
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();

        Self {
            api_key: api_key.filter(|key| !key.trim().is_empty()),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Lists the text-generating models the server serves, with their
    /// context windows and tool support when the server reports them.
    ///
    /// # Errors
    /// Returns an error when neither Ollama's native API nor the `OpenAI`
    /// models API answers.
    pub async fn inventory(&self) -> Result<LocalInventory, LocalServerError> {
        match self.ollama_tags().await {
            Ok(tags) => Ok(LocalInventory {
                kind: LocalServerKind::Ollama,
                models: self.ollama_models(tags).await,
            }),
            Err(_) => Ok(LocalInventory {
                kind: LocalServerKind::OpenAiCompatible,
                models: self.openai_models().await?,
            }),
        }
    }

    /// Returns the context window in tokens the server runs `model` with,
    /// or `None` when the server does not report it.
    ///
    /// Ollama reports the window only for a loaded model, so an unloaded
    /// model is loaded first, as the next chat request would load it, with
    /// the server's default options. Other servers report vLLM's
    /// `max_model_len` from the models API.
    ///
    /// # Errors
    /// Returns an error when the server cannot be reached or Ollama cannot
    /// load the model.
    pub async fn running_context_window(
        &self,
        model: &str,
    ) -> Result<Option<u64>, LocalServerError> {
        let Ok(loaded) = self.ollama_loaded_context_windows().await else {
            return Ok(self
                .openai_models()
                .await?
                .into_iter()
                .find(|served| served.id == model)
                .and_then(|served| served.loaded_context_window));
        };
        if let Some(window) = loaded.get(model) {
            return Ok(Some(*window));
        }
        let url = self.ollama_url("/api/generate");
        self.send::<Value>(
            self.client
                .post(&url)
                .json(&json!({"model": model}))
                .timeout(LOAD_TIMEOUT),
            url,
        )
        .await?;

        Ok(self
            .ollama_loaded_context_windows()
            .await?
            .get(model)
            .copied())
    }

    /// Returns Ollama's model tags, failing for any other server.
    async fn ollama_tags(&self) -> Result<Vec<OllamaTag>, LocalServerError> {
        let url = self.ollama_url("/api/tags");
        let tags: OllamaTags = self
            .send(self.client.get(&url).timeout(PROBE_TIMEOUT), url)
            .await?;

        Ok(tags.models)
    }

    /// Completes Ollama tags with `/api/show` details and loaded windows,
    /// dropping models that cannot generate text.
    async fn ollama_models(&self, tags: Vec<OllamaTag>) -> Vec<LocalModel> {
        let loaded = self
            .ollama_loaded_context_windows()
            .await
            .unwrap_or_default();
        let mut models = Vec::with_capacity(tags.len());
        for tag in tags {
            let mut capabilities = tag.capabilities;
            let mut context_window = tag.details.context_length;
            if (capabilities.is_none() || context_window.is_none())
                && let Ok(shown) = self.ollama_show(&tag.name).await
            {
                context_window = context_window.or_else(|| shown.context_length());
                capabilities = capabilities.or(shown.capabilities);
            }
            let has = |capability: &str| {
                capabilities
                    .as_ref()
                    .map(|capabilities| capabilities.iter().any(|item| item == capability))
            };
            if has(COMPLETION_CAPABILITY) == Some(false) {
                continue;
            }
            models.push(LocalModel {
                context_window,
                loaded_context_window: loaded.get(&tag.name).copied(),
                supports_tools: has(TOOLS_CAPABILITY),
                id: tag.name,
            });
        }

        models
    }

    /// Returns Ollama's details for one model.
    async fn ollama_show(&self, model: &str) -> Result<OllamaShow, LocalServerError> {
        let url = self.ollama_url("/api/show");

        self.send(
            self.client
                .post(&url)
                .json(&json!({"model": model}))
                .timeout(PROBE_TIMEOUT),
            url,
        )
        .await
    }

    /// Returns the context window of every model Ollama has loaded, failing
    /// for any other server.
    async fn ollama_loaded_context_windows(
        &self,
    ) -> Result<HashMap<String, u64>, LocalServerError> {
        let url = self.ollama_url("/api/ps");
        let running: OllamaRunning = self
            .send(self.client.get(&url).timeout(PROBE_TIMEOUT), url)
            .await?;

        Ok(running
            .models
            .into_iter()
            .filter_map(|model| Some((model.name, model.context_length?)))
            .collect())
    }

    /// Lists models through the `OpenAI` models API.
    async fn openai_models(&self) -> Result<Vec<LocalModel>, LocalServerError> {
        let url = format!("{}/models", self.base_url);
        let models: OpenAiModels = self
            .send(self.client.get(&url).timeout(PROBE_TIMEOUT), url)
            .await?;

        Ok(models
            .data
            .into_iter()
            .map(|model| LocalModel {
                context_window: model.max_model_len,
                id: model.id,
                loaded_context_window: model.max_model_len,
                supports_tools: None,
            })
            .collect())
    }

    /// Returns Ollama's native API URL for `path`, beside the `/v1` path.
    fn ollama_url(&self, path: &str) -> String {
        let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);

        format!("{root}{path}")
    }

    /// Sends one authenticated request and decodes its JSON body.
    async fn send<T: DeserializeOwned>(
        &self,
        builder: RequestBuilder,
        url: String,
    ) -> Result<T, LocalServerError> {
        let builder = match &self.api_key {
            Some(api_key) => builder.bearer_auth(api_key),
            None => builder,
        };
        let response = builder.send().await.map_err(request_error(&url))?;

        decode(response, url).await
    }
}

/// Decodes a successful JSON response.
async fn decode<T: DeserializeOwned>(
    response: Response,
    url: String,
) -> Result<T, LocalServerError> {
    let status = response.status();
    if !status.is_success() {
        return Err(LocalServerError::Status {
            status: status.as_u16(),
            url,
        });
    }

    response.json().await.map_err(request_error(&url))
}

/// Returns a mapper from a transport failure for `url`.
fn request_error(url: &str) -> impl FnOnce(reqwest::Error) -> LocalServerError {
    let url = url.to_string();

    move |source| LocalServerError::Request { source, url }
}

/// Ollama `/api/tags` response.
#[derive(Deserialize)]
struct OllamaTags {
    models: Vec<OllamaTag>,
}

/// One Ollama model tag.
#[derive(Deserialize)]
struct OllamaTag {
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    details: OllamaDetails,
    name: String,
}

/// Model details Ollama includes in a tag.
#[derive(Default, Deserialize)]
struct OllamaDetails {
    #[serde(default)]
    context_length: Option<u64>,
}

/// Ollama `/api/show` response.
#[derive(Deserialize)]
struct OllamaShow {
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    model_info: HashMap<String, Value>,
}

impl OllamaShow {
    /// Returns the trained context length from `<architecture>.context_length`.
    fn context_length(&self) -> Option<u64> {
        self.model_info
            .iter()
            .find(|(key, _)| key.ends_with(CONTEXT_LENGTH_SUFFIX))
            .and_then(|(_, value)| value.as_u64())
    }
}

/// Ollama `/api/ps` response.
#[derive(Deserialize)]
struct OllamaRunning {
    models: Vec<OllamaRunningModel>,
}

/// One model Ollama has loaded.
#[derive(Deserialize)]
struct OllamaRunningModel {
    #[serde(default)]
    context_length: Option<u64>,
    name: String,
}

/// `OpenAI` models API response.
#[derive(Deserialize)]
struct OpenAiModels {
    data: Vec<OpenAiModel>,
}

/// One model in the `OpenAI` models API, with vLLM's window extension.
#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
    #[serde(default)]
    max_model_len: Option<u64>,
}
