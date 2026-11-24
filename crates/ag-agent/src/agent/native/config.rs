//! Host configuration, provider credentials, and per-turn harness assembly.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{env, io};

use ag_contracts::{
    CommandScope, PermissionMode, ProviderCallBudget, ReasoningLevel, SessionCommandOutput,
    SessionCommandRequest,
};
use ag_harness::bash::{BashArguments, BashConfig};
use ag_harness::catalog::{
    Capabilities, ModelCatalog, ModelHandle, ModelId, ModelSelector, ModelSpec,
};
use ag_harness::lifecycle::{LifecycleObserver, LifecycleObserverSet, LifecycleTraceObserver};
use ag_harness::model::{
    ContextPolicy, ModelClient, ModelCompletion, ModelMetadata, ModelProfile, ModelRegistry,
    ModelRequest, ReasoningEffort,
};
use ag_harness::provider::{
    ChatGptConfig, CostTier, LocalServerKind, ModelConfiguration, ModelProvider,
    chatgpt_auth_file_in,
};
use ag_harness::sandbox::{Sandbox, run_command};
use ag_harness::{
    Harness, HarnessError, Model, ModelError, OutputSchema, Repository, RepositoryError,
    RunRequest, ToolSet, TurnInput,
};
use ag_protocol::{ProtocolRequestProfile, SchemaRequiredPolicy, protocol_output_schema};
use ag_session::{
    AgentKind, AgentModel, HarnessModelOption, HarnessModelStatus, MINIMUM_RUNNING_CONTEXT_WINDOW,
    ModelCost, ServedModel,
};
use ag_telemetry::{Context, TraceContextExt as _};
use async_trait::async_trait;

use super::cargo_cache::TrustedCargoCache;
use super::sandbox::{
    CommandSandbox, SESSION_SANDBOX, UTILITY_SANDBOX, command_output, sandbox_state,
};
use crate::agent::model_profile;

/// Revision of Agentty's harness model registrations. Durable sessions record
/// it and require the same value on resume.
///
/// Image input joined the declared capabilities without a new revision: the
/// harness weighs that capability only for image-bearing input, which
/// Agentty rejects before a harness turn, so recorded sessions and text
/// retries are unaffected while older sessions keep resuming.
const REGISTRATION_REVISION: &str = "agentty-native-2";
/// Conservative profile for a harness model without a declared profile. Its
/// default estimator runs up to 46% under dense content and further under
/// small requests whose fixed provider cost it never weighs.
const FALLBACK_MODEL_PROFILE: ModelProfile =
    ModelProfile::new(128_000).with_estimate_margin(16_384, 50);
/// Placeholder endpoint for resume-only registrations, which never send
/// requests.
const RESUME_ONLY_BASE_URL: &str = "http://127.0.0.1";
/// Placeholder key for resume-only registrations, nonblank because a blank
/// required key counts as missing.
const RESUME_ONLY_API_KEY: &str = "resume-only";
/// SQLite file holding one session's durable harness history.
const SESSION_DATABASE_FILE: &str = "harness.db";
/// Trusted Bash executable used by edit-mode `bash` calls.
const BASH_EXECUTABLE: &str = "/bin/bash";
/// Nonsecret identity of the native sandbox Bash policy. Change it when the
/// grants, the Bash executable, or the `ag-harness` sandbox enforcement
/// change.
const BASH_POLICY_REVISION: &str = "agentty-native-sandbox-1";
/// Directory beside a session's harness database that its commands use as
/// `TMPDIR` and for tool caches.
const SESSION_TEMPORARY_DIRECTORY: &str = "tmp";
/// Directory under the data root holding one temporary directory per
/// running utility prompt. Agentty session ids never start with a dot.
const UTILITY_TEMPORARY_PARENT: &str = ".utility";
/// Deadline for one `bash` call.
const BASH_TIMEOUT: Duration = Duration::from_secs(600);
/// Deadline for one command a host runs in a session's sandbox, such as a
/// repository's commit hooks, which can build and test a whole project:
/// the longest deadline the harness accepts.
const SESSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(3600);
/// Environment variable naming the model the OpenAI-compatible server serves
/// for [`AgentModel::OpenAiCompatible`], such as Ollama's `qwen3:latest`.
const OPENAI_COMPATIBLE_MODEL_ENV: &str = "OPENAI_COMPATIBLE_MODEL";
/// Command that stores the sign-in [`ModelProvider::ChatGpt`] needs.
const CHATGPT_SIGN_IN_COMMAND: &str = "ag-harness login chatgpt";
/// Environment variable naming Agentty's data directory, `~/.agentty` by
/// default.
const AGENTTY_ROOT_ENV: &str = "AGENTTY_ROOT";
/// Environment prefixes that carry provider keys, credential files, and
/// endpoints, withheld from `bash`. `OPENAI_` covers both `OpenAI` and
/// OpenAI-compatible variables, and `CLAUDE_CODE_OAUTH_TOKEN` the Claude
/// subscription token Claude Code reads beside the `ANTHROPIC_` variables.
const PROVIDER_ENVIRONMENT_PREFIXES: [&str; 7] = [
    "ANTHROPIC_",
    "CHATGPT_",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "DASHSCOPE_",
    "KIMI_",
    "MODEL_",
    "OPENAI_",
];
/// Prefixes of variable names withheld from `bash` as credentials, matched
/// case-insensitively.
const CREDENTIAL_PREFIXES: [&str; 5] = ["AWS_", "GH_", "GITHUB_", "GITLAB_", "GLAB_"];
/// Endings of variable names withheld from `bash` as credentials, matched
/// case-insensitively: `*_KEY` (including `*_API_KEY`) and the connection
/// strings that embed passwords.
const CREDENTIAL_SUFFIXES: [&str; 3] = ["_KEY", "DATABASE_URL", "_DSN"];
/// Words whose presence anywhere in a variable name withholds it from
/// `bash` as a credential, matched case-insensitively, such as
/// `NPM_TOKEN` or `CARGO_REGISTRY_TOKEN`.
const CREDENTIAL_WORDS: [&str; 5] = ["TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"];
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
/// `<data_root>/<session id>/`, outside the session worktree; hosts remove
/// that directory when they delete the session. Its `tmp` directory is the
/// `TMPDIR` and tool cache of the session's commands, which cannot read the
/// rest of the data root; utility prompts get a fresh one each.
///
/// `bash` runs inside the native sandbox, which needs the host to call
/// [`super::dispatch_sandbox_helper`] first in `main`; where the sandbox
/// cannot run, commands run without it and every turn reports why.
#[derive(Clone)]
pub struct NativeHarnessConfig {
    /// Directory of the per-project Cargo caches session turns reuse, or
    /// `None` when sessions download their own dependencies.
    cargo_cache_root: Option<PathBuf>,
    data_root: PathBuf,
    environment: EnvironmentSource,
    /// Whether this process already removed the utility temporary
    /// directories earlier processes left behind.
    swept_utility_directories: Arc<AtomicBool>,
}

impl NativeHarnessConfig {
    /// Configures harness storage under `data_root`, reading provider
    /// settings from the process environment.
    pub fn new(data_root: PathBuf) -> Self {
        Self::with_environment(data_root, process_environment)
    }

    /// Configures harness storage under `data_root` with an explicit
    /// environment source for provider settings and `bash` grants. A
    /// relative `data_root` is resolved against the current directory.
    ///
    /// Provider API keys come from the API key file the environment
    /// locates, `auth/api_keys.json` in Agentty's data directory:
    /// `AGENTTY_ROOT`, else `~/.agentty`. A key variable is only imported into
    /// that file.
    pub fn with_environment(
        data_root: PathBuf,
        environment: impl Fn() -> BTreeMap<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            cargo_cache_root: None,
            data_root: std::path::absolute(&data_root).unwrap_or(data_root),
            environment: Arc::new(environment),
            swept_utility_directories: Arc::default(),
        }
    }

    /// Makes session turns reuse the per-project Cargo caches under
    /// `cache_root`, which the host fills through
    /// [`super::TrustedCargoCache::refresh`] with the same root. A relative
    /// `cache_root` is resolved against the current directory.
    #[must_use]
    pub fn with_trusted_cargo_cache(mut self, cache_root: PathBuf) -> Self {
        self.cargo_cache_root = Some(std::path::absolute(&cache_root).unwrap_or(cache_root));

        self
    }

    /// Runs `request`'s command with Bash in its folder inside the sandbox
    /// its scope selects. [`CommandScope::Session`] uses the grants,
    /// environment, and temporary directory of the session's own commands,
    /// and, where the native sandbox cannot run, runs without it with the
    /// warning a session turn reports. [`CommandScope::Workspace`] uses a
    /// utility prompt's grants in the folder with a fresh temporary
    /// directory, and refuses where the native sandbox cannot run. Hosts use
    /// it for commands they run on their own behalf over content commands
    /// could have written, such as the repository's Git hooks.
    ///
    /// # Errors
    /// Returns an error for an invalid folder, session id, or command, when
    /// a workspace command cannot run inside the native sandbox, or when the
    /// command cannot be run at all; a command that runs and fails is
    /// reported through its exit code instead.
    pub async fn run_session_command(
        &self,
        request: SessionCommandRequest,
    ) -> Result<SessionCommandOutput, String> {
        let environment = self.environment();
        let repository = repository(&request.folder, &environment)?;
        // Kept until the command ends; dropping it removes a workspace
        // command's temporary directory.
        let (sandbox, policy, _temporary_directory) = self
            .command_sandbox(&request.scope, &request.folder, &environment)
            .await?;
        let configuration = bash_config(
            &environment,
            &sandbox,
            SESSION_COMMAND_TIMEOUT,
            BashConfig::MAX_CAPTURE_BYTES,
        )?;
        let command =
            BashArguments::new(request.command).map_err(failure("Invalid session command"))?;
        let run = run_command(&configuration, policy, &repository, &command)
            .await
            .map_err(failure("Failed to run the session command"))?;

        Ok(SessionCommandOutput {
            exit_code: run.outcome.exit_code,
            output: command_output(&run.outcome),
            sandbox: sandbox_state(run.warning.as_ref()),
        })
    }

    /// Returns a fresh environment snapshot, before stored API keys replace
    /// its key variables.
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

    /// Returns the sandbox of the session's commands run in `folder`, with
    /// the session's own temporary directory, which keeps one path for the
    /// session's lifetime because the harness fingerprints it into the
    /// policy, and goes with the session's harness directory. When the host
    /// keeps Cargo caches and passes the project's `main_checkout`, its
    /// `CARGO_HOME` receives copies from that project's cache.
    ///
    /// # Errors
    /// Returns an error for an invalid session id or when the directory
    /// cannot be prepared.
    pub(crate) async fn session_sandbox(
        &self,
        session_id: &str,
        folder: &Path,
        main_checkout: Option<&Path>,
        environment: &BTreeMap<String, String>,
    ) -> Result<CommandSandbox, String> {
        let database = self.session_database(session_id).await?;
        let temporary_directory = database.with_file_name(SESSION_TEMPORARY_DIRECTORY);
        tokio::fs::create_dir_all(&temporary_directory)
            .await
            .map_err(|error| {
                format!("Failed to create the harness temporary directory: {error}")
            })?;
        let cargo_cache = self
            .cargo_cache_root
            .as_deref()
            .zip(main_checkout)
            .map(|(root, main_checkout)| TrustedCargoCache::for_project(root, main_checkout));

        CommandSandbox::prepare(
            &self.data_root,
            temporary_directory,
            folder,
            environment,
            cargo_cache,
        )
        .await
    }

    /// Returns the sandbox of one utility prompt's commands run in
    /// `workspace`, with a fresh temporary directory removed when the
    /// returned guard drops. The first call of this process first removes
    /// the directories earlier processes left behind, which only one process
    /// per data root can own.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be created or prepared.
    pub(crate) async fn utility_sandbox(
        &self,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<(CommandSandbox, tempfile::TempDir), String> {
        let parent = self.data_root.join(UTILITY_TEMPORARY_PARENT);
        if !self.swept_utility_directories.swap(true, Ordering::SeqCst) {
            let _ = tokio::fs::remove_dir_all(&parent).await;
        }
        let directory = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&parent)
                .and_then(|()| tempfile::Builder::new().prefix("run-").tempdir_in(&parent))
        })
        .await
        .map_err(io::Error::other)
        .flatten()
        .map_err(|error| format!("Failed to create the harness temporary directory: {error}"))?;
        let sandbox = CommandSandbox::prepare(
            &self.data_root,
            directory.path().to_path_buf(),
            workspace,
            environment,
            None,
        )
        .await?;

        Ok((sandbox, directory))
    }

    /// Returns the sandbox of one host command in `folder` that `scope`
    /// selects, the policy for a host where the native sandbox cannot run,
    /// and the command's own temporary directory, if any, removed when it
    /// drops. Scopes other than a session's get the refusing workspace
    /// sandbox. A session command copies nothing new from the Cargo cache;
    /// it reuses what the session's turns already copied.
    ///
    /// # Errors
    /// Returns an error for an invalid session id or when the sandbox
    /// cannot be prepared.
    async fn command_sandbox(
        &self,
        scope: &CommandScope,
        folder: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<(CommandSandbox, Sandbox, Option<tempfile::TempDir>), String> {
        let CommandScope::Session { session_id } = scope else {
            let (sandbox, directory) = self.utility_sandbox(folder, environment).await?;

            return Ok((sandbox, UTILITY_SANDBOX, Some(directory)));
        };
        let sandbox = self
            .session_sandbox(session_id, folder, None, environment)
            .await?;

        Ok((sandbox, SESSION_SANDBOX, None))
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
/// API key, variable, or sign-in each still needs. `is_signed_in` reports
/// whether a sign-in provider's credential file holds a usable sign-in.
///
/// Listing the local server is bounded by short request timeouts; an
/// unreachable server contributes no discovered models.
pub(crate) async fn harness_models(
    environment: &BTreeMap<String, String>,
    is_signed_in: impl Fn(&Path) -> bool,
) -> Vec<HarnessModelOption> {
    let mut options = local_models(environment).await;
    options.extend(
        AgentKind::Harness
            .models()
            .iter()
            .filter_map(|model| hosted_model(*model, environment, &is_signed_in)),
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
        context_window: None,
        cost: ModelCost::Local,
        loaded_context_window: None,
        model,
        provider: provider.to_string(),
        status,
        trains_on_prompts: false,
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

/// Returns one paid catalog model with the API key, variable, or sign-in its
/// provider still needs, or `None` for a model no built-in provider serves.
fn hosted_model(
    model: AgentModel,
    environment: &BTreeMap<String, String>,
    is_signed_in: impl Fn(&Path) -> bool,
) -> Option<HarnessModelOption> {
    let selection = ModelSelection::resolve_known(model.as_str()).ok()?;
    let provider = selection.provider;
    let cost = model_cost(provider.cost_tier(&selection.model)?);
    let status = configuration_status(provider, environment, is_signed_in);

    Some(HarnessModelOption {
        context_window: selection
            .profile(environment, None)
            .ok()
            .map(ModelProfile::context_window_tokens),
        cost,
        loaded_context_window: None,
        model,
        provider: provider_display_name(provider).to_string(),
        status,
        trains_on_prompts: provider.trains_on_prompts(&selection.model),
    })
}

/// Returns the cost Agentty shows for a model of the harness catalog's
/// price class `tier`; a tier added to the catalog later shows as standard.
fn model_cost(tier: CostTier) -> ModelCost {
    match tier {
        CostTier::Local => ModelCost::Local,
        CostTier::Plan => ModelCost::Plan,
        CostTier::Cheap => ModelCost::Cheap,
        CostTier::Premium => ModelCost::Premium,
        _ => ModelCost::Standard,
    }
}

/// Returns the name users know a hosted provider by, or the provider
/// identifier for a provider the harness adds later.
fn provider_display_name(provider: ModelProvider) -> &'static str {
    const DISPLAY_NAMES: [(ModelProvider, &str); 7] = [
        (ModelProvider::Anthropic, "Anthropic"),
        (ModelProvider::ChatGpt, "ChatGPT plan"),
        (ModelProvider::Kimi, "Kimi"),
        (ModelProvider::Muse, "Muse"),
        (ModelProvider::OpenAi, "OpenAI"),
        (
            ModelProvider::OpenAiCompatible,
            LocalServerKind::OpenAiCompatible.display_name(),
        ),
        (ModelProvider::Qwen, "Qwen"),
    ];

    DISPLAY_NAMES
        .iter()
        .find(|(known, _)| *known == provider)
        .map_or(provider.as_str(), |(_, name)| name)
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
pub(super) fn lookup(
    environment: &BTreeMap<String, String>,
) -> impl FnMut(&str) -> Result<String, env::VarError> + '_ {
    |name| {
        environment
            .get(name)
            .cloned()
            .ok_or(env::VarError::NotPresent)
    }
}

/// Returns the `ChatGPT` credential file Agentty signs in to and reads,
/// shared with `ag-harness login chatgpt` through [`chatgpt_auth_file_in`]:
/// `CHATGPT_AUTH_FILE`, else `auth/chatgpt.json` under `AGENTTY_ROOT` or
/// `~/.agentty`, or `None` when none of them is set. A data-directory
/// variable is used as given unless it is empty, as `agentty`'s data
/// directory reads it.
pub(super) fn chatgpt_auth_file(environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    chatgpt_auth_file_in(lookup(environment), agentty_home(environment).as_deref())
}

/// Returns Agentty's data directory as `agentty` reads it: `AGENTTY_ROOT`,
/// else `~/.agentty`, each used as given unless empty, or `None` when
/// neither variable is set.
pub(super) fn agentty_home(environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    let set = |name: &str| {
        environment
            .get(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };

    set(AGENTTY_ROOT_ENV).or_else(|| set("HOME").map(|home| home.join(".agentty")))
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

/// Returns whether a provider can serve requests now: a sign-in provider
/// needs a usable sign-in, as `is_signed_in` reports; another needs a base
/// URL unless it has a default, then its stored API key unless optional.
/// The base URL is checked first so that a provider asks for a key only
/// when saving one makes it ready.
fn configuration_status(
    provider: ModelProvider,
    environment: &BTreeMap<String, String>,
    is_signed_in: impl Fn(&Path) -> bool,
) -> HarnessModelStatus {
    if provider.requires_sign_in() {
        let signed_in =
            chatgpt_auth_file(environment).is_some_and(|auth_file| is_signed_in(&auth_file));

        return if signed_in {
            HarnessModelStatus::Ready
        } else {
            HarnessModelStatus::SignInRequired(CHATGPT_SIGN_IN_COMMAND)
        };
    }
    if provider.default_base_url().is_none()
        && !has_value(environment, provider.base_url_environment())
    {
        return HarnessModelStatus::MissingConfiguration(provider.base_url_environment());
    }
    if provider.requires_api_key() && !has_value(environment, provider.api_key_environment()) {
        return HarnessModelStatus::ApiKeyRequired(provider.as_str());
    }

    HarnessModelStatus::Ready
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

    /// Resolves a recorded registration key such as `kimi/kimi-k3`,
    /// `chatgpt/gpt-6.1-sol`, or `openai-compatible/qwen3:latest`, or `None`
    /// for a key without a known provider and model.
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        let (provider, model) = key.split_once('/')?;
        if provider == ModelProvider::OpenAiCompatible.as_str() {
            return (!model.trim().is_empty()).then(|| Self {
                model: model.to_string(),
                provider: ModelProvider::OpenAiCompatible,
            });
        }
        // A ChatGPT-plan model id is its key, `chatgpt/<model>`.
        let id = if provider == ModelProvider::ChatGpt.as_str() {
            key
        } else {
            model
        };

        Self::resolve_known(id)
            .ok()
            .filter(|selection| selection.key() == key)
    }

    /// Resolves a catalog model id to the built-in provider listing it. A
    /// `chatgpt/<model>` id resolves only to [`ModelProvider::ChatGpt`], and
    /// a bare id never does, so the `ChatGPT` plan and API-billed `OpenAI`
    /// never stand in for each other.
    fn resolve_known(model: &str) -> Result<Self, String> {
        let unknown = || format!("The harness does not serve model `{model}`.");
        if let Some(plan_model) = model
            .strip_prefix(ModelProvider::ChatGpt.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
        {
            return ModelProvider::ChatGpt
                .known_models()
                .contains(&plan_model)
                .then(|| Self {
                    model: plan_model.to_string(),
                    provider: ModelProvider::ChatGpt,
                })
                .ok_or_else(unknown);
        }

        ModelProvider::all()
            .iter()
            .copied()
            .filter(|provider| !provider.requires_sign_in())
            .find(|provider| provider.known_models().contains(&model))
            .map(|provider| Self {
                model: model.to_string(),
                provider,
            })
            .ok_or_else(unknown)
    }

    /// Returns the stable registry key.
    pub(crate) fn key(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Returns this model's entry in the harness provider catalog, with its
    /// list prices and declared profile, or `None` for a model the catalog
    /// does not describe, such as one a local server serves.
    pub(crate) fn catalog_spec(&self) -> Option<ModelSpec> {
        self.provider.model_spec(&self.model)
    }

    /// Returns the catalog entry of the API model a plan-billed model
    /// mirrors: for a `ChatGPT` plan model, the same-named `OpenAI` API
    /// model, whose list prices give the plan usage's API equivalent. Other
    /// models, and plan models without an API twin, return `None`.
    pub(crate) fn api_twin_spec(&self) -> Option<ModelSpec> {
        (self.provider == ModelProvider::ChatGpt)
            .then(|| ModelProvider::OpenAi.model_spec(&self.model))
            .flatten()
    }

    /// Returns the context window turns on this model are fitted to, as
    /// [`Self::profile`] resolves it, or `None` for an invalid
    /// environment-declared profile.
    pub(crate) fn context_window(
        &self,
        environment: &BTreeMap<String, String>,
        running_context_window: Option<u64>,
    ) -> Option<u64> {
        self.profile(environment, running_context_window)
            .ok()
            .map(ModelProfile::context_window_tokens)
    }

    /// Returns the Agentty model id of the registration key `key`, as users
    /// pick the model: `ChatGPT` plan and local server models keep their
    /// provider, as in `chatgpt/gpt-6-luna`, and other cataloged models are
    /// bare, as in `gpt-6-luna`. An unknown key is returned unchanged.
    pub(crate) fn model_id_of(key: &str) -> String {
        match Self::from_key(key) {
            Some(selection)
                if !matches!(
                    selection.provider,
                    ModelProvider::ChatGpt | ModelProvider::OpenAiCompatible
                ) =>
            {
                selection.model
            }
            _ => key.to_string(),
        }
    }

    /// Returns the model that writes compaction summaries for sessions
    /// running on this selection in its place: `requested`, such as the
    /// user's Default Fast Model, when it resolves to another harness model
    /// whose provider can serve requests now, as `is_signed_in` reports for a
    /// sign-in provider, and that declares structured output for the summary
    /// schema. Otherwise returns `None`, so the session's own model
    /// summarizes, and logs why a requested model was passed over.
    pub(crate) fn summary_model(
        &self,
        requested: Option<&str>,
        environment: &BTreeMap<String, String>,
        is_signed_in: impl Fn(&Path) -> bool,
    ) -> Option<Self> {
        let requested = requested?;
        let rejection = match Self::resolve(requested, environment) {
            Err(error) => error,
            Ok(candidate) if candidate == *self => return None,
            Ok(candidate) => {
                match configuration_status(candidate.provider, environment, is_signed_in) {
                    HarnessModelStatus::Ready
                        if candidate
                            .provider
                            .capabilities(&candidate.model)
                            .is_some_and(Capabilities::structured_output) =>
                    {
                        return Some(candidate);
                    }
                    HarnessModelStatus::Ready => "it declares no structured output".to_string(),
                    status => format!("it is not ready ({status:?})"),
                }
            }
        };
        tracing::warn!(summary_model = requested, %rejection, "summarizing on the session model");

        None
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
        let model = self.client(environment, provider_call_budget)?;

        self.register(model, self.profile(environment, running_context_window)?)
    }

    /// Returns this model's catalog entry as the harness provider catalog
    /// declares it, with its prices, profile, and output limit, so the
    /// requests it answers as a summary model are priced at its own prices.
    ///
    /// # Errors
    /// Returns an actionable error when credentials or the endpoint are
    /// missing, or the entry is invalid.
    fn summary_entry(
        &self,
        environment: &BTreeMap<String, String>,
        provider_call_budget: Option<&ProviderCallBudget>,
    ) -> Result<ModelHandle, String> {
        let id = ModelId::cataloged(self.provider, &self.model);
        let spec = self
            .provider
            .model_spec(&self.model)
            .ok_or("The harness provider catalog does not list the summary model.")?;

        ModelCatalog::builder()
            .model(spec, self.client(environment, provider_call_budget)?)
            .build()
            .and_then(|catalog| catalog.resolve(&ModelSelector::Id(id)))
            .map_err(failure("Invalid harness summary model"))
    }

    /// Returns this model's provider client, charging `provider_call_budget`
    /// for every request when one is set.
    ///
    /// # Errors
    /// Returns an actionable error when credentials or the endpoint are
    /// missing.
    fn client(
        &self,
        environment: &BTreeMap<String, String>,
        provider_call_budget: Option<&ProviderCallBudget>,
    ) -> Result<Arc<dyn Model>, String> {
        let mut configuration = ModelConfiguration::new(self.provider, &self.model);
        // Only the `ChatGPT` provider reads a sign-in, and the harness keeps
        // no default location for it.
        if self.provider.requires_sign_in()
            && let Some(auth_file) = chatgpt_auth_file(environment)
        {
            configuration = configuration.auth_file(auth_file);
        }
        let client = configuration
            .client_from_environment(lookup(environment))
            .map_err(|error| {
                let remedy = if self.provider.requires_sign_in() {
                    format!("Run `{CHATGPT_SIGN_IN_COMMAND}`, then retry.")
                } else if self.provider.requires_api_key() {
                    "Pick the model with `/model` to paste its API key, then retry.".to_string()
                } else {
                    format!(
                        "Check {OPENAI_COMPATIBLE_MODEL_ENV} and {} before starting Agentty.",
                        self.provider.base_url_environment()
                    )
                };

                format!(
                    "Cannot use harness model `{}`: {error}. {remedy}",
                    self.model
                )
            })?;

        let model: Arc<dyn Model> = match provider_call_budget {
            Some(budget) => Arc::new(BudgetedModel {
                budget: budget.clone(),
                inner: client,
            }),
            None => Arc::new(client),
        };

        Ok(model)
    }

    /// Builds a harness for this model with Agentty's defaults, summarizing
    /// on `context.summary_model` when it is set and can serve summaries, as
    /// [`HarnessContext::apply_summary_model`] decides.
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
        let mut harness = self.harness_from(&registry, context, observer)?;
        context.apply_summary_model(|summary| harness.set_summary_model(summary));

        Ok(harness)
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
        // A sign-in provider's client reads no credential until a request,
        // so it needs no stored sign-in here.
        let client = if self.provider.requires_sign_in() {
            ModelClient::chatgpt(ChatGptConfig {
                auth_file: PathBuf::new(),
                base_url: RESUME_ONLY_BASE_URL.to_string(),
                model: self.model.clone(),
            })
            .map_err(failure("Invalid resume-only harness model"))?
        } else {
            ModelConfiguration::new(self.provider, &self.model)
                .base_url(RESUME_ONLY_BASE_URL)
                .client_from_environment(|_| Ok(RESUME_ONLY_API_KEY.to_string()))
                .map_err(failure("Invalid resume-only harness model"))?
        };
        let metadata = client.metadata().clone();
        let profile = self.profile(context.environment, None)?;
        let registry = self.register(Arc::new(ResumeOnlyModel { metadata }), profile)?;

        self.harness_from(&registry, context, observer)
    }

    /// Registers `model` as the catalog entry of this selection's key, with
    /// `profile` and the capabilities the harness provider catalog declares
    /// for the model, under [`REGISTRATION_REVISION`]. A selection only holds
    /// a cataloged model or one an OpenAI-compatible server serves, both of
    /// which the catalog describes.
    ///
    /// The entry carries no prices or output limit, so runs stay unpriced
    /// and unmetered as before, and the harness derives the same key,
    /// revision, and capabilities recorded sessions hold.
    fn register(
        &self,
        model: Arc<dyn Model>,
        profile: ModelProfile,
    ) -> Result<ModelRegistry, String> {
        let id = ModelId::cataloged(self.provider, &self.model);
        let declared = self
            .provider
            .capabilities(&self.model)
            .unwrap_or(Capabilities::NONE);
        let spec = ModelSpec::new(id.clone())
            .with_provider(self.provider)
            .with_capabilities(declared)
            .with_profile(profile);
        let handle = ModelCatalog::builder()
            .model(spec, model)
            .build()
            .and_then(|catalog| catalog.resolve(&ModelSelector::Id(id)))
            .map_err(failure("Invalid harness model registration"))?;
        let mut registry = ModelRegistry::new();
        registry
            .register_catalog_entry(&handle, REGISTRATION_REVISION)
            .map_err(failure("Invalid harness model registration"))?;

        Ok(registry)
    }

    /// Builds a harness over this selection's registration in `registry`
    /// whose durable sessions, when `context.compacts`, compact their earlier
    /// turns automatically once replayed history nears the context budget,
    /// and otherwise evict them.
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
        let context_policy = if context.compacts {
            ContextPolicy::auto_compact()
        } else {
            ContextPolicy::Evict
        };

        Ok(Harness::from_registry(registry, &self.key())
            .map_err(failure("Invalid harness model registration"))?
            .context_policy(context_policy)
            .repository(context.repository.clone())
            .sandbox(context.sandbox)
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
    /// Whether durable sessions compact their earlier turns automatically.
    pub(crate) compacts: bool,
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
    /// How `bash` runs where the native sandbox cannot.
    pub(crate) sandbox: Sandbox,
    /// Model that writes compaction summaries in place of the turn's model,
    /// resolved by [`ModelSelection::summary_model`].
    pub(crate) summary_model: Option<&'a ModelSelection>,
}

impl HarnessContext<'_> {
    /// Hands the catalog entry of [`Self::summary_model`] to `apply`, which
    /// sets it on a harness or session. When the entry cannot be built or
    /// `apply` refuses it, logs why and leaves the session's own model
    /// summarizing rather than failing the turn.
    pub(crate) fn apply_summary_model(
        &self,
        apply: impl FnOnce(&ModelHandle) -> Result<(), HarnessError>,
    ) {
        let Some(summary_model) = self.summary_model else {
            return;
        };
        let applied = summary_model
            .summary_entry(self.environment, self.provider_call_budget)
            .and_then(|entry| apply(&entry).map_err(|error| error.to_string()));
        if let Err(error) = applied {
            let summary_model = summary_model.key();
            tracing::warn!(%summary_model, %error, "summarizing on the session model");
        }
    }
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

/// Explicit output schema and tools of one harness turn or run.
pub(crate) struct RunOptions {
    bash: Option<BashConfig>,
    output: OutputSchema,
    tools: ToolSet,
}

impl RunOptions {
    /// Builds the options for one turn: `read` always, plus `write` and
    /// `bash` in `sandbox` outside read-only mode.
    ///
    /// # Errors
    /// Returns an error when the schema or `bash` policy is rejected.
    pub(crate) fn new(
        profile: ProtocolRequestProfile,
        permission_mode: PermissionMode,
        environment: &BTreeMap<String, String>,
        sandbox: &CommandSandbox,
    ) -> Result<Self, String> {
        let output = output_schema(profile)?;
        if permission_mode == PermissionMode::ReadOnly {
            return Ok(Self {
                bash: None,
                output,
                tools: ToolSet::empty().with_read(),
            });
        }

        Ok(Self {
            bash: Some(bash_config(
                environment,
                sandbox,
                BASH_TIMEOUT,
                BashConfig::DEFAULT_CAPTURE_BYTES,
            )?),
            output,
            tools: ToolSet::builtins(),
        })
    }

    /// Returns whether the run offers `bash`, which the native sandbox runs.
    pub(crate) fn offers_bash(&self) -> bool {
        self.bash.is_some()
    }

    /// Returns a run request for `input` with these options.
    pub(crate) fn request(self, input: String) -> RunRequest {
        let request = RunRequest::new(input).output(self.output).tools(self.tools);

        match self.bash {
            Some(bash) => request.bash(bash),
            None => request,
        }
    }
}

/// Configures `bash` in the native sandbox with `sandbox`'s grants and the
/// host environment minus provider variables, keeping essentials first
/// within the harness grant limits, with each command's `timeout` and
/// `capture_bytes`. Host values of the variables the grants set, such as
/// `TMPDIR` and the tool caches, are not granted.
fn bash_config(
    environment: &BTreeMap<String, String>,
    sandbox: &CommandSandbox,
    timeout: Duration,
    capture_bytes: usize,
) -> Result<BashConfig, String> {
    let mut config = BashConfig::native(
        PathBuf::from(BASH_EXECUTABLE),
        BASH_POLICY_REVISION.to_string(),
        timeout,
        capture_bytes,
    )
    .and_then(|config| sandbox.grant(config.with_host_information()))
    .map_err(failure("Invalid harness bash policy"))?;
    for (name, value) in bash_environment(environment) {
        if let Ok(granted) = config.clone().with_environment(name.clone(), value.clone()) {
            config = granted;
        }
    }

    Ok(config)
}

/// Orders granted variables: essentials first, then the rest by name,
/// without provider variables or credentials.
fn bash_environment(environment: &BTreeMap<String, String>) -> Vec<(&String, &String)> {
    let is_granted = |name: &str| {
        !PROVIDER_ENVIRONMENT_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
            && !is_credential(name)
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

/// Returns whether the variable `name` likely carries a credential, which
/// commands with network access could send anywhere: tokens, secrets, keys,
/// passwords, credentials, and connection strings, plus every AWS, GitHub,
/// and `GitLab` variable, whatever their case.
fn is_credential(name: &str) -> bool {
    let name = name.to_ascii_uppercase();

    CREDENTIAL_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || CREDENTIAL_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || CREDENTIAL_WORDS.iter().any(|word| name.contains(word))
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
/// the current execution span is recording. Its `execute_tool` spans repeat
/// the run identity the turn inherits, like other harnesses' tool spans.
fn traced(observer: impl LifecycleObserver + 'static) -> LifecycleObserverSet {
    let observers = LifecycleObserverSet::new(observer);
    if !Context::current().span().is_recording() {
        return observers;
    }

    observers.with_observer(
        LifecycleTraceObserver::new().with_tool_attributes(ag_telemetry::run_identity),
    )
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

    fn replays_reasoning_text(&self) -> bool {
        Model::replays_reasoning_text(&self.inner)
    }

    fn replays_turn_instructions(&self) -> bool {
        Model::replays_turn_instructions(&self.inner)
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
