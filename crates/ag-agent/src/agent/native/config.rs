//! Host configuration, provider credentials, and per-turn harness assembly.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, io};

use ag_contracts::{PermissionMode, ProviderCallBudget, ReasoningLevel};
use ag_harness::bash::{BashConfig, UnsandboxedExecutor};
use ag_harness::lifecycle::{LifecycleObserver, LifecycleObserverSet, LifecycleTraceObserver};
use ag_harness::model::{
    ModelCapabilities, ModelClient, ModelCompletion, ModelMetadata, ModelProfile, ModelRegistry,
    ModelRequest, ReasoningEffort,
};
use ag_harness::provider::{LocalServerKind, ModelConfiguration, ModelProvider};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::{
    Harness, Model, ModelError, OutputSchema, Repository, RepositoryError, Tool, ToolPolicy,
    TurnInput, TurnOptions,
};
use ag_protocol::{ProtocolRequestProfile, SchemaRequiredPolicy, protocol_output_schema};
use ag_session::{
    AgentKind, AgentModel, HarnessModelOption, HarnessModelStatus, MINIMUM_RUNNING_CONTEXT_WINDOW,
    ModelBilling, ServedModel,
};
use ag_telemetry::{Context, TraceContextExt as _};
use async_trait::async_trait;

use crate::agent::model_profile;

/// Revision of Agentty's harness model registrations. Durable sessions record
/// it and require the same value on resume.
const REGISTRATION_REVISION: &str = "agentty-native-2";
/// Conservative profile for a harness model without a declared profile. Its
/// default estimator runs up to 46% under dense content and further under
/// small requests whose fixed provider cost it never weighs.
const FALLBACK_MODEL_PROFILE: ModelProfile =
    ModelProfile::new(128_000).with_estimate_margin(16_384, 50);
/// Placeholder endpoint for resume-only registrations, which never send
/// requests.
const RESUME_ONLY_BASE_URL: &str = "http://127.0.0.1";
/// SQLite file holding one session's durable harness history.
const SESSION_DATABASE_FILE: &str = "harness.db";
/// Trusted Bash executable used by edit-mode `bash` calls.
const BASH_EXECUTABLE: &str = "/bin/bash";
/// Nonsecret identity of the unsandboxed Bash policy.
const BASH_POLICY_REVISION: &str = "agentty-unsandboxed-1";
/// Deadline for one `bash` call.
const BASH_TIMEOUT: Duration = Duration::from_secs(600);
/// Output bytes kept from one `bash` call, the harness maximum.
const BASH_CAPTURE_BYTES: usize = 8192;
/// Environment variable naming the model the OpenAI-compatible server serves
/// for [`AgentModel::OpenAiCompatible`], such as Ollama's `qwen3:latest`.
const OPENAI_COMPATIBLE_MODEL_ENV: &str = "OPENAI_COMPATIBLE_MODEL";
/// Environment prefixes that carry provider keys and endpoints, withheld from
/// `bash`. `OPENAI_` covers both `OpenAI` and OpenAI-compatible variables.
const PROVIDER_ENVIRONMENT_PREFIXES: [&str; 4] = ["DASHSCOPE_", "KIMI_", "MODEL_", "OPENAI_"];
/// Variables granted to `bash` first, so the harness environment limit never
/// drops them.
const PRIORITY_ENVIRONMENT: [&str; 17] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "SSH_AUTH_SOCK",
    "GIT_SSH_COMMAND",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "VIRTUAL_ENV",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
];
/// Session-scoped instructions stored with every harness session.
const SYSTEM_PROMPT: &str = concat!(
    "You are a coding agent working inside one Git worktree through tools. Use only the tools ",
    "offered on the current turn: `read` inspects files, `write` applies unified-diff patches, ",
    "and `bash` runs shell commands in the worktree. Call a tool immediately when you need it; ",
    "never narrate, promise, or defer a tool call. Claim a change or command result only after ",
    "the tool reports it."
);

/// Environment snapshot provider used for credentials and `bash` grants.
type EnvironmentSource = Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>;

/// Host configuration that enables in-process [`AgentKind::Harness`] turns.
///
/// Each session keeps its durable harness history in
/// `<data_root>/<session id>/`, outside the session worktree; hosts remove that
/// directory when they delete the session.
#[derive(Clone)]
pub struct NativeHarnessConfig {
    data_root: PathBuf,
    environment: EnvironmentSource,
}

impl NativeHarnessConfig {
    /// Configures harness storage under `data_root`, reading provider
    /// credentials from the process environment.
    pub fn new(data_root: PathBuf) -> Self {
        Self::with_environment(data_root, process_environment)
    }

    /// Configures harness storage under `data_root` with an explicit
    /// environment source for provider credentials and `bash` grants.
    pub fn with_environment(
        data_root: PathBuf,
        environment: impl Fn() -> BTreeMap<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            data_root,
            environment: Arc::new(environment),
        }
    }

    /// Returns a fresh environment snapshot.
    pub(crate) fn environment(&self) -> BTreeMap<String, String> {
        (self.environment)()
    }

    /// Creates and returns the session database path.
    ///
    /// # Errors
    /// Returns an error for an identifier that is not one path component or
    /// when the session directory cannot be created.
    pub(crate) async fn session_database(&self, session_id: &str) -> Result<PathBuf, String> {
        let is_single_component = !session_id.is_empty()
            && session_id != "."
            && session_id != ".."
            && !session_id.contains(['/', '\\', '\0']);
        if !is_single_component {
            return Err(format!("Invalid harness session id `{session_id}`."));
        }
        let directory = self.data_root.join(session_id);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| format!("Failed to create the harness session directory: {error}"))?;

        Ok(directory.join(SESSION_DATABASE_FILE))
    }
}

/// Returns the process environment, skipping variables that are not valid
/// Unicode.
pub(crate) fn process_environment() -> BTreeMap<String, String> {
    unicode_environment(env::vars_os())
}

/// Returns the harness models a user can choose: the models the local
/// OpenAI-compatible server serves, plus `OPENAI_COMPATIBLE_MODEL` when the
/// server does not list it, then the paid hosted catalog models with the
/// variable each still needs.
///
/// Listing the local server is bounded by short request timeouts; an
/// unreachable server contributes no discovered models.
pub(crate) async fn harness_models(
    environment: &BTreeMap<String, String>,
) -> Vec<HarnessModelOption> {
    let mut options = local_models(environment).await;
    options.extend(
        AgentKind::Harness
            .models()
            .iter()
            .filter_map(|model| hosted_model(*model, environment)),
    );

    options
}

/// Returns the served local models, with the configured model first when the
/// server does not list it.
async fn local_models(environment: &BTreeMap<String, String>) -> Vec<HarnessModelOption> {
    let server = ModelProvider::OpenAiCompatible
        .local_server(lookup(environment))
        .ok()
        .flatten();
    let inventory = async { server?.inventory().await.ok() }.await;
    let provider = inventory
        .as_ref()
        .map_or(LocalServerKind::OpenAiCompatible, |inventory| {
            inventory.kind
        })
        .display_name();
    let local_option = |model: AgentModel, status: HarnessModelStatus| HarnessModelOption {
        billing: ModelBilling::Local,
        context_window: None,
        loaded_context_window: None,
        model,
        provider: provider.to_string(),
        status,
    };
    let mut options = inventory
        .map_or_else(Vec::new, |inventory| inventory.models)
        .into_iter()
        .filter_map(|served| {
            let status = if served.supports_tools == Some(false) {
                HarnessModelStatus::NoToolSupport
            } else {
                HarnessModelStatus::Ready
            };

            Some(HarnessModelOption {
                context_window: served.context_window,
                loaded_context_window: served.loaded_context_window,
                ..local_option(
                    AgentModel::OpenAiCompatible(ServedModel::new(&served.id)?),
                    status,
                )
            })
        })
        .collect::<Vec<_>>();
    let configured = nonblank(environment, OPENAI_COMPATIBLE_MODEL_ENV)
        .and_then(ServedModel::new)
        .map(AgentModel::OpenAiCompatible);
    if let Some(configured) = configured
        && !options.iter().any(|option| option.model == configured)
    {
        options.insert(0, local_option(configured, HarnessModelStatus::Ready));
    }

    options
}

/// Returns one paid catalog model with the variable its provider still
/// needs, or `None` for a model no built-in provider serves.
fn hosted_model(
    model: AgentModel,
    environment: &BTreeMap<String, String>,
) -> Option<HarnessModelOption> {
    let selection = ModelSelection::resolve_known(model.as_str()).ok()?;
    let provider = selection.provider;
    let status = missing_configuration(provider, environment)
        .map_or(HarnessModelStatus::Ready, |variable| {
            HarnessModelStatus::MissingConfiguration(variable)
        });

    Some(HarnessModelOption {
        billing: ModelBilling::Paid,
        context_window: selection
            .profile(environment, None)
            .ok()
            .map(|profile| profile.context_window_tokens()),
        loaded_context_window: None,
        model,
        provider: provider_display_name(provider).to_string(),
        status,
    })
}

/// Returns the name users know a hosted provider by.
fn provider_display_name(provider: ModelProvider) -> &'static str {
    match provider {
        ModelProvider::Kimi => "Kimi",
        ModelProvider::Muse => "Muse",
        ModelProvider::OpenAi => "OpenAI",
        ModelProvider::OpenAiCompatible => LocalServerKind::OpenAiCompatible.display_name(),
        ModelProvider::Qwen => "Qwen",
    }
}

/// Returns the context window `selection`'s server runs its model with, or
/// `None` for hosted providers and servers that do not report it.
///
/// Ollama loads the model first when needed, as the turn's first request
/// would. A server that cannot be queried reports nothing, leaving the
/// declared profile and the turn's own request to surface the failure.
///
/// # Errors
/// Returns an actionable error when the server runs the model with a window
/// too small for harness turns, which Ollama would otherwise fill by
/// silently dropping the start of the conversation.
pub(crate) async fn running_context_window(
    selection: &ModelSelection,
    environment: &BTreeMap<String, String>,
) -> Result<Option<u64>, String> {
    let Ok(Some(server)) = selection.provider.local_server(lookup(environment)) else {
        return Ok(None);
    };
    let Ok(Some(window)) = server.running_context_window(&selection.model).await else {
        return Ok(None);
    };
    if window < MINIMUM_RUNNING_CONTEXT_WINDOW {
        return Err(format!(
            "The local server runs `{}` with a {window}-token context window, below the \
             {MINIMUM_RUNNING_CONTEXT_WINDOW} tokens harness turns need, so it would drop the \
             start of the conversation. Restart Ollama with `OLLAMA_CONTEXT_LENGTH=32768` (or \
             raise the context length in the Ollama app's settings), or pick another model with \
             `/model`.",
            selection.model
        ));
    }

    Ok(Some(window))
}

/// Reads `environment` as the lookup the harness provider catalog expects.
fn lookup(
    environment: &BTreeMap<String, String>,
) -> impl FnMut(&str) -> Result<String, env::VarError> + '_ {
    |name| {
        environment
            .get(name)
            .cloned()
            .ok_or(env::VarError::NotPresent)
    }
}

/// Keeps the variables whose name and value are both valid Unicode.
fn unicode_environment(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<String, String> {
    variables
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// Returns the first variable a provider still needs: its API key unless
/// optional, then a base URL unless it has a default.
fn missing_configuration(
    provider: ModelProvider,
    environment: &BTreeMap<String, String>,
) -> Option<&'static str> {
    if provider.requires_api_key() && !has_value(environment, provider.api_key_environment()) {
        return Some(provider.api_key_environment());
    }
    if provider.default_base_url().is_none()
        && !has_value(environment, provider.base_url_environment())
    {
        return Some(provider.base_url_environment());
    }

    None
}

/// Returns the trimmed value of `name`, or `None` when it is unset or blank.
fn nonblank<'a>(environment: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    environment
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

/// Returns whether `name` holds a nonblank value.
fn has_value(environment: &BTreeMap<String, String>, name: &str) -> bool {
    nonblank(environment, name).is_some()
}

/// One harness model resolved to its built-in provider and the model id
/// sent to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelSelection {
    model: String,
    provider: ModelProvider,
}

impl ModelSelection {
    /// Resolves an Agentty harness model id to its provider. A served
    /// `openai-compatible/<model>` id resolves to that model, and the bare
    /// `openai-compatible` id to the model `OPENAI_COMPATIBLE_MODEL` names.
    ///
    /// # Errors
    /// Returns an error when no built-in provider serves `model`, or when
    /// `OPENAI_COMPATIBLE_MODEL` is unset for `openai-compatible`.
    pub(crate) fn resolve(
        model: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let Some(AgentModel::OpenAiCompatible(served)) = AgentKind::Harness.parse_model(model)
        else {
            return Self::resolve_known(model);
        };
        if let Some(name) = served.name() {
            return Ok(Self {
                model: name.to_string(),
                provider: ModelProvider::OpenAiCompatible,
            });
        }
        let served = nonblank(environment, OPENAI_COMPATIBLE_MODEL_ENV).ok_or_else(|| {
            format!(
                "Cannot use harness model `{model}`: {OPENAI_COMPATIBLE_MODEL_ENV} is unset. Pick \
                 a local model with `/model`, or set it to the model your OpenAI-compatible \
                 server serves, such as `qwen3:latest`, before starting Agentty."
            )
        })?;

        Ok(Self {
            model: served.to_string(),
            provider: ModelProvider::OpenAiCompatible,
        })
    }

    /// Resolves a recorded registration key such as `kimi/kimi-k3` or
    /// `openai-compatible/qwen3:latest`, or `None` for a key without a known
    /// provider and model.
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        let (provider, model) = key.split_once('/')?;
        if provider == ModelProvider::OpenAiCompatible.as_str() {
            return (!model.trim().is_empty()).then(|| Self {
                model: model.to_string(),
                provider: ModelProvider::OpenAiCompatible,
            });
        }

        Self::resolve_known(model)
            .ok()
            .filter(|selection| selection.key() == key)
    }

    /// Resolves a catalog model id to the built-in provider listing it.
    fn resolve_known(model: &str) -> Result<Self, String> {
        ModelProvider::all()
            .iter()
            .copied()
            .find(|provider| provider.known_models().contains(&model))
            .map(|provider| Self {
                model: model.to_string(),
                provider,
            })
            .ok_or_else(|| format!("The harness does not serve model `{model}`."))
    }

    /// Returns the stable registry key.
    pub(crate) fn key(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Registers this model's provider client under its key.
    ///
    /// # Errors
    /// Returns an actionable error when credentials or the endpoint are
    /// missing, or the registration is invalid.
    pub(crate) fn registry(
        &self,
        environment: &BTreeMap<String, String>,
        provider_call_budget: Option<&ProviderCallBudget>,
        running_context_window: Option<u64>,
    ) -> Result<ModelRegistry, String> {
        let client = ModelConfiguration::new(self.provider, &self.model)
            .client_from_environment(lookup(environment))
            .map_err(|error| {
                let remedy = if self.provider.requires_api_key() {
                    format!("Set {}", self.provider.api_key_environment())
                } else {
                    format!(
                        "Check {OPENAI_COMPATIBLE_MODEL_ENV} and {}",
                        self.provider.base_url_environment()
                    )
                };

                format!(
                    "Cannot use harness model `{}`: {error}. {remedy} before starting Agentty.",
                    self.model
                )
            })?;

        let profile = self.profile(environment, running_context_window)?;
        match provider_call_budget {
            Some(budget) => self.register(
                BudgetedModel {
                    budget: budget.clone(),
                    inner: client,
                },
                profile,
            ),
            None => self.register(client, profile),
        }
    }

    /// Builds a harness for this model with Agentty's defaults.
    ///
    /// # Errors
    /// Returns the registration failure.
    pub(crate) fn harness(
        &self,
        context: &HarnessContext<'_>,
        observer: impl LifecycleObserver + 'static,
    ) -> Result<Harness, String> {
        let registry = self.registry(
            context.environment,
            context.provider_call_budget,
            context.running_context_window,
        )?;

        self.harness_from(&registry, context, observer)
    }

    /// Builds a harness that resumes a session recorded on this model without
    /// its provider credentials. Switch the session to another model before
    /// running a turn; this registration never sends a request.
    ///
    /// # Errors
    /// Returns the registration failure.
    pub(crate) fn resume_only_harness(
        &self,
        context: &HarnessContext<'_>,
        observer: impl LifecycleObserver + 'static,
    ) -> Result<Harness, String> {
        let metadata = ModelConfiguration::new(self.provider, &self.model)
            .base_url(RESUME_ONLY_BASE_URL)
            .client_from_environment(|_| Ok(String::new()))
            .map_err(failure("Invalid resume-only harness model"))?
            .metadata()
            .clone();
        let profile = self.profile(context.environment, None)?;
        let registry = self.register(ResumeOnlyModel { metadata }, profile)?;

        self.harness_from(&registry, context, observer)
    }

    /// Registers `model` under this selection's key with `profile`.
    fn register(
        &self,
        model: impl Model + 'static,
        profile: ModelProfile,
    ) -> Result<ModelRegistry, String> {
        let identity = ExecutionIdentity::new(self.key(), REGISTRATION_REVISION)
            .map_err(failure("Invalid harness model registration"))?;
        let capabilities = ModelCapabilities {
            tool_calls: true,
            ..ModelCapabilities::new(profile)
        };
        let mut registry = ModelRegistry::new();
        registry
            .register(identity, model, capabilities)
            .map_err(failure("Invalid harness model registration"))?;

        Ok(registry)
    }

    /// Builds a harness over this selection's registration in `registry`.
    ///
    /// A turn built inside a recording trace also exports the harness's
    /// `invoke_agent`, `chat`, and `execute_tool` spans beneath the current
    /// span; an untraced turn skips that projection.
    fn harness_from(
        &self,
        registry: &ModelRegistry,
        context: &HarnessContext<'_>,
        observer: impl LifecycleObserver + 'static,
    ) -> Result<Harness, String> {
        Ok(Harness::from_registry(registry, &self.key())
            .map_err(failure("Invalid harness model registration"))?
            .repository(context.repository.clone())
            .model_reasoning_effort(reasoning_effort(context.reasoning_level))
            .with_lifecycle_observer(traced(observer)))
    }

    /// Returns the context profile sized to the window a local server runs
    /// the model with, else the one the harness provider catalog or the
    /// environment declares for this model, else the Agentty catalog model's
    /// declared profile, else the conservative fallback. Codex models served
    /// by the harness take the provider catalog's profile, not the one
    /// Agentty declares for Codex app-server compaction.
    ///
    /// # Errors
    /// Returns an error for an invalid environment-declared profile.
    fn profile(
        &self,
        environment: &BTreeMap<String, String>,
        running_context_window: Option<u64>,
    ) -> Result<ModelProfile, String> {
        let declared = match running_context_window {
            Some(window) => self.provider.running_profile(lookup(environment), window),
            None => self
                .provider
                .declared_profile(&self.model, lookup(environment)),
        }
        .map_err(failure("Invalid harness model profile"))?;

        Ok(declared
            .or_else(|| {
                AgentKind::Harness
                    .parse_model(&self.model)
                    .and_then(model_profile::declared)
            })
            .unwrap_or(FALLBACK_MODEL_PROFILE))
    }
}

/// Inputs shared by every harness built for one turn.
pub(crate) struct HarnessContext<'a> {
    /// Environment snapshot used for credentials.
    pub(crate) environment: &'a BTreeMap<String, String>,
    /// Optional limit charged for every model request.
    pub(crate) provider_call_budget: Option<&'a ProviderCallBudget>,
    /// Requested reasoning depth.
    pub(crate) reasoning_level: ReasoningLevel,
    /// Validated worktree available to tools.
    pub(crate) repository: &'a Repository,
    /// Context window the local server runs the model with, when known.
    pub(crate) running_context_window: Option<u64>,
}

/// Returns the instructions stored with new harness sessions.
pub(crate) fn system_prompt() -> &'static str {
    SYSTEM_PROMPT
}

/// Returns the protocol output schema for one request profile.
///
/// # Errors
/// Returns an error when the harness rejects the protocol schema.
pub(crate) fn output_schema(profile: ProtocolRequestProfile) -> Result<OutputSchema, String> {
    OutputSchema::new(protocol_output_schema(
        profile,
        SchemaRequiredPolicy::MinimumProtocolKeys,
    ))
    .map_err(failure("The harness rejected the protocol schema"))
}

/// Builds explicit options for one turn: `read` always, plus `write` and
/// unsandboxed `bash` outside read-only mode.
///
/// # Errors
/// Returns an error when the schema or `bash` policy is rejected.
pub(crate) fn turn_options(
    profile: ProtocolRequestProfile,
    permission_mode: PermissionMode,
    environment: &BTreeMap<String, String>,
) -> Result<TurnOptions, String> {
    let schema = output_schema(profile)?;
    let policy = ToolPolicy::default().allow(Tool::Read);
    if permission_mode == PermissionMode::ReadOnly {
        return Ok(TurnOptions::new(schema, policy));
    }

    Ok(
        TurnOptions::new(schema, policy.allow(Tool::Write).allow(Tool::Bash))
            .with_bash(bash_config(environment)?),
    )
}

/// Configures unsandboxed `bash` with the host environment minus provider
/// variables, keeping essentials first within the harness grant limits.
fn bash_config(environment: &BTreeMap<String, String>) -> Result<BashConfig, String> {
    let mut config = BashConfig::for_executor(
        Arc::new(UnsandboxedExecutor::without_isolation()),
        PathBuf::from(BASH_EXECUTABLE),
        BASH_POLICY_REVISION.to_string(),
        BASH_TIMEOUT,
        BASH_CAPTURE_BYTES,
    )
    .map_err(failure("Invalid harness bash policy"))?
    .with_host_information();
    for (name, value) in bash_environment(environment) {
        if let Ok(granted) = config.clone().with_environment(name.clone(), value.clone()) {
            config = granted;
        }
    }

    Ok(config)
}

/// Orders granted variables: essentials first, then the rest by name.
fn bash_environment(environment: &BTreeMap<String, String>) -> Vec<(&String, &String)> {
    let is_granted = |name: &str| {
        !PROVIDER_ENVIRONMENT_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
    };
    let priority = PRIORITY_ENVIRONMENT
        .iter()
        .filter_map(|name| environment.get_key_value(*name));
    let remaining = environment
        .iter()
        .filter(|(name, _)| !PRIORITY_ENVIRONMENT.contains(&name.as_str()));

    priority
        .chain(remaining)
        .filter(|(name, _)| is_granted(name))
        .collect()
}

/// Validates the worktree with the first absolute `git` on `PATH`.
///
/// # Errors
/// Returns an error for an invalid worktree or when no usable Git exists.
pub(crate) fn repository(
    folder: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<Repository, String> {
    let executable_name = format!("git{}", env::consts::EXE_SUFFIX);
    let candidates = environment
        .get("PATH")
        .into_iter()
        .flat_map(env::split_paths)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name));
    for candidate in candidates {
        match Repository::new(folder, candidate) {
            Ok(repository) => return Ok(repository),
            Err(
                error @ (RepositoryError::Root { .. }
                | RepositoryError::RootIsGitAdministrative { .. }),
            ) => return Err(format!("Invalid harness worktree: {error}")),
            Err(_) => {}
        }
    }

    Err("The harness needs a Git executable on PATH.".to_string())
}

/// Returns an error mapper that prefixes a harness failure with `context`.
pub(crate) fn failure<E: Display>(context: &'static str) -> impl FnOnce(E) -> String {
    move |error| format!("{context}: {error}")
}

/// Maps Agentty's reasoning level to the harness reasoning effort.
fn reasoning_effort(reasoning_level: ReasoningLevel) -> ReasoningEffort {
    match reasoning_level {
        ReasoningLevel::Low => ReasoningEffort::Low,
        ReasoningLevel::Medium => ReasoningEffort::Medium,
        ReasoningLevel::High => ReasoningEffort::High,
        ReasoningLevel::XHigh => ReasoningEffort::XHigh,
        ReasoningLevel::Max => ReasoningEffort::Max,
    }
}

/// Adds the harness's OpenTelemetry span projection after `observer` when
/// the current execution span is recording.
fn traced(observer: impl LifecycleObserver + 'static) -> LifecycleObserverSet {
    let observers = LifecycleObserverSet::new(observer);
    if !Context::current().span().is_recording() {
        return observers;
    }

    observers.with_observer(LifecycleTraceObserver::new())
}

/// Model wrapper that charges one provider call before every request.
struct BudgetedModel {
    budget: ProviderCallBudget,
    inner: ModelClient,
}

#[async_trait]
impl Model for BudgetedModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Model::metadata(&self.inner)
    }

    fn validate_schema(&self, schema: &OutputSchema) -> Result<(), ModelError> {
        Model::validate_schema(&self.inner, schema)
    }

    fn validate_input(&self, input: &TurnInput) -> Result<(), ModelError> {
        Model::validate_input(&self.inner, input)
    }

    fn keeps_prefix_across_schemas(&self) -> bool {
        Model::keeps_prefix_across_schemas(&self.inner)
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.budget.consume().map_err(ModelError::request)?;

        Model::complete(&self.inner, request).await
    }
}

/// Stand-in for a recorded model that only lets its session resume and switch
/// away.
struct ResumeOnlyModel {
    metadata: ModelMetadata,
}

#[async_trait]
impl Model for ResumeOnlyModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.metadata.clone())
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        Err(ModelError::request(io::Error::other(
            "the recorded harness model can only resume its session",
        )))
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
