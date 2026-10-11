//! Host configuration, provider credentials, and per-turn harness assembly.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Display;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, io};

use ag_contracts::{PermissionMode, ProviderCallBudget, ReasoningLevel};
use ag_harness::bash::{BashConfig, UnsandboxedExecutor};
use ag_harness::lifecycle::LifecycleObserver;
use ag_harness::model::{
    ContextBudget, ModelCapabilities, ModelClient, ModelCompletion, ModelMetadata, ModelRegistry,
    ModelRequest, ReasoningEffort,
};
use ag_harness::provider::{ModelConfiguration, ModelProvider};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::{
    Harness, Model, ModelError, OutputSchema, Repository, RepositoryError, Tool, ToolPolicy,
    TurnInput, TurnOptions,
};
use ag_protocol::{ProtocolRequestProfile, SchemaRequiredPolicy, protocol_output_schema};
use ag_session::{AgentKind, AgentModel, ModelContextLimits};
use async_trait::async_trait;

/// Revision of Agentty's harness model registrations. Durable sessions record
/// it and require the same value on resume.
const REGISTRATION_REVISION: &str = "agentty-native-1";
/// Conservative limits for a harness model without declared limits.
const FALLBACK_CONTEXT_LIMITS: ModelContextLimits = ModelContextLimits {
    context_window_tokens: 128_000,
    input_headroom_tokens: 16_384,
};
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
/// Environment prefixes that carry provider keys and endpoints, withheld from
/// `bash`.
const PROVIDER_ENVIRONMENT_PREFIXES: [&str; 3] = ["DASHSCOPE_", "KIMI_", "MODEL_"];
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

/// Returns the first harness model whose provider has an API key and a
/// reachable endpoint, or `None` when no provider is configured.
pub(crate) fn default_model(environment: &BTreeMap<String, String>) -> Option<AgentModel> {
    AgentKind::Harness.models().iter().copied().find(|model| {
        ModelSelection::resolve(model.as_str())
            .is_ok_and(|selection| provider_configured(selection.provider, environment))
    })
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

/// Returns whether one provider has a nonempty key and a base URL.
fn provider_configured(provider: ModelProvider, environment: &BTreeMap<String, String>) -> bool {
    let has_value = |name: &str| {
        environment
            .get(name)
            .is_some_and(|value| !value.trim().is_empty())
    };

    has_value(provider.api_key_environment())
        && (provider.default_base_url().is_some() || has_value(provider.base_url_environment()))
}

/// One harness model resolved to its built-in provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelSelection {
    model: String,
    provider: ModelProvider,
}

impl ModelSelection {
    /// Resolves an Agentty harness model id to its provider.
    ///
    /// # Errors
    /// Returns an error when no built-in provider serves `model`.
    pub(crate) fn resolve(model: &str) -> Result<Self, String> {
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

    /// Resolves a recorded registration key such as `kimi/kimi-k3`, or
    /// `None` for a key without a known provider and model.
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        let (_, model) = key.split_once('/')?;

        Self::resolve(model)
            .ok()
            .filter(|selection| selection.key() == key)
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
    ) -> Result<ModelRegistry, String> {
        let client = ModelConfiguration::new(self.provider, &self.model)
            .client_from_environment(|name| {
                environment
                    .get(name)
                    .cloned()
                    .ok_or(env::VarError::NotPresent)
            })
            .map_err(|error| {
                format!(
                    "Cannot use harness model `{}`: {error}. Set {} before starting Agentty.",
                    self.model,
                    self.provider.api_key_environment()
                )
            })?;

        match provider_call_budget {
            Some(budget) => self.register(BudgetedModel {
                budget: budget.clone(),
                inner: client,
            }),
            None => self.register(client),
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
        let registry = self.registry(context.environment, context.provider_call_budget)?;

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
        let registry = self.register(ResumeOnlyModel { metadata })?;

        self.harness_from(&registry, context, observer)
    }

    /// Registers `model` under this selection's key and declared capabilities.
    fn register(&self, model: impl Model + 'static) -> Result<ModelRegistry, String> {
        let identity = ExecutionIdentity::new(self.key(), REGISTRATION_REVISION)
            .map_err(failure("Invalid harness model registration"))?;
        let capabilities = ModelCapabilities {
            tool_calls: true,
            ..ModelCapabilities::new(self.context_budget()?)
        };
        let mut registry = ModelRegistry::new();
        registry
            .register(identity, model, capabilities)
            .map_err(failure("Invalid harness model registration"))?;

        Ok(registry)
    }

    /// Builds a harness over this selection's registration in `registry`.
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
            .with_lifecycle_observer(observer))
    }

    /// Returns the declared context budget for this model.
    fn context_budget(&self) -> Result<ContextBudget, String> {
        let limits = AgentKind::Harness
            .parse_model(&self.model)
            .and_then(AgentModel::context_limits)
            .unwrap_or(FALLBACK_CONTEXT_LIMITS);
        let window = NonZeroU64::new(limits.context_window_tokens).unwrap_or(NonZeroU64::MIN);

        ContextBudget::new(window)
            .with_reserved_output(limits.input_headroom_tokens)
            .map_err(failure("Invalid harness context budget"))
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
