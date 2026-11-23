//! Interactive command-line chat powered by the `ag-harness` model runtime.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use std::{env, io};

use ag_harness::catalog::{
    CatalogError, ModelCatalog, ModelHandle, ModelId, ModelSelector, ModelSpec, Role,
};
use ag_harness::event::Item;
use ag_harness::lifecycle::LifecycleTraceObserver;
use ag_harness::model::{
    ContextBudgetError, ModelCapabilities, ModelProfile, ModelRegistry, ModelRegistryError,
    ReasoningEffort,
};
use ag_harness::provider::{
    DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS, ModelConfiguration,
    ModelConfigurationError, ModelProvider, OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV,
    OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV,
};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::SessionInfo;
use ag_harness::transcript::TranscriptInput;
use ag_harness::{
    ComparisonBase, Cost, Harness, HarnessBuilder, HarnessError, Model, OutputSchema, Repository,
    RunError, RunOutcome, RunRequest, Session, ToolSet, TurnRange, TurnRecord, TurnReport, Usage,
    UsageTotals,
};
use ag_telemetry::otlp::{OtlpError, OtlpExport, Service};
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};

mod login;
mod serve;

const CHAT_COMMAND_HELP: &str = concat!(
    "commands:\n",
    "  /model            list models\n",
    "  /model <MODEL>    switch later turns to a list number, role, provider/model, or model ID\n",
    "  /usage            show the session's tokens and cost\n",
    "  /history          show the session's turns\n",
    "  /help             show commands\n"
);
/// Roles `/model` lists and selects by name, in display order; they are the
/// roles the built-in catalog assigns.
const LISTED_ROLES: [Role; 4] = [Role::SMART, Role::CHEAP, Role::FAST, Role::VISION];
/// One million micro-USD, one US dollar.
const MICRO_USD_PER_USD: u64 = 1_000_000;
/// Context profile assumed for hosted chat models whose window the built-in
/// catalog does not declare: a conservative 128k-token window keeping 16,384
/// tokens free for the model's response. Cataloged `OpenAI` models declare
/// their documented windows, and OpenAI-compatible models declare theirs in
/// the environment.
const MODEL_PROFILE: ModelProfile = ModelProfile::new(128_000).with_reserved_tokens(16_384);
/// Revision of the CLI's built-in model registrations. Sessions record it and
/// require the same value on resume.
const MODEL_REGISTRATION_REVISION: &str = "ag-harness-cli-1";
const READ_ONLY_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a read-only repository harness. The read tool supports file, list, ",
    "search, diff, and show actions. For change review, call diff first when a comparison base is \
     configured, then use search, file, ",
    "list, or show for evidence. Use repository tools only when the user explicitly asks about ",
    "repository contents. Treat an unambiguous reference to the repository, project, codebase, ",
    "code, a file, or a change as an explicit repository request. Treat replies, response speed, ",
    "response visibility, and model behavior as casual chat topics only when they are not ",
    "explicitly tied to the repository, project, codebase, code, a file, or a change. ",
    "Do not call tools for casual conversation or ambiguous requests. When needed, call the tool ",
    "immediately and use its result before answering. ",
    "Never narrate, promise, or defer a future tool call. Never claim that you created, ",
    "modified, deleted, or executed files or commands because filesystem mutation and command ",
    "execution are unavailable. If asked to perform an unsupported action, state that it is ",
    "unsupported."
);
const READ_WRITE_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a repository harness with read and write tools. The read tool supports ",
    "file, list, search, diff, and show actions. For change review, call diff first when a \
     comparison base is configured. When a user ",
    "explicitly asks about repository contents, call read immediately and use its result before ",
    "answering. Treat an unambiguous reference to the repository, project, codebase, code, a \
     file, ",
    "or a change as an explicit repository request. Treat replies, response speed, response ",
    "visibility, and model behavior as casual chat topics only when they are not explicitly tied ",
    "to the repository, project, codebase, code, a file, or a change. Do not ",
    "call tools for casual conversation or ambiguous requests. ",
    "When a user asks to create or modify a file, call the write tool ",
    "immediately in the same response. Never narrate, promise, or defer a future tool call. Only ",
    "claim that a file was created or modified after the write tool succeeds. File deletion and ",
    "command execution are unavailable."
);

/// Chats with models through a bounded repository harness.
#[derive(Debug, Parser)]
#[command(
    name = "ag-harness",
    version,
    about = "Chats with models through a repository harness",
    after_help = provider_help()
)]
struct Cli {
    /// Explicit comparison revision, resolved once to a commit for this
    /// invocation. Omit to keep comparisons unavailable while allowing
    /// other reads.
    #[arg(long, global = true, value_name = "REV")]
    comparison_base: Option<String>,
    /// SQLite database used for durable session history.
    #[arg(long, global = true, value_name = "FILE")]
    database: Option<PathBuf>,
    /// Absolute Git executable override; defaults to the first valid Git found
    /// in PATH.
    #[arg(long, global = true, value_name = "FILE")]
    git_executable: Option<PathBuf>,
    /// Exports traces using OTLP HTTP/protobuf to this complete traces URL.
    #[arg(long, global = true, value_name = "URL")]
    otlp_endpoint: Option<String>,
    /// Model reasoning depth used for chat requests.
    #[arg(
        long,
        global = true,
        default_value = "low",
        value_parser = reasoning_effort_parser()
    )]
    reasoning_effort: ReasoningEffort,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// Returns a set global option that `serve` does not use, since its
    /// requests choose each session's model and store; `--git-executable`
    /// applies only with `--read-dir`.
    fn option_serve_ignores(&self) -> Option<&'static str> {
        let Command::Serve(args) = &self.command else {
            return None;
        };

        [
            ("--comparison-base", self.comparison_base.is_some()),
            ("--database", self.database.is_some()),
            (
                "--git-executable",
                self.git_executable.is_some() && args.read_dir.is_none(),
            ),
            ("--otlp-endpoint", self.otlp_endpoint.is_some()),
        ]
        .into_iter()
        .find_map(|(option, set)| set.then_some(option))
    }
}

/// Supported harness commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Starts a new durable session with a model.
    Run(RunArgs),
    /// Resumes a durable session.
    Resume(ResumeArgs),
    /// Signs in to a provider that uses an account instead of an API key.
    Login(LoginArgs),
    /// Serves an UNSTABLE JSON-RPC 2.0 protocol over standard input and
    /// output for non-Rust hosts.
    #[command(long_about = SERVE_HELP)]
    Serve(ServeArgs),
}

const SERVE_HELP: &str = concat!(
    "Serves an UNSTABLE JSON-RPC 2.0 protocol over standard input and output for non-Rust ",
    "hosts.\n\n",
    "Each line is one JSON message. Requests: session/create, session/resume, run/start, ",
    "run/cancel, session/transcript, session/usage, and shutdown. A message without an id is ",
    "executed but never answered. Each run streams its events as run/event notifications, ",
    "ending with its completed event. Without --read-dir, tools are denied. With it, runs may ",
    "read that repository, and with --allow-write also write to it: each write first sends the ",
    "client an approval/decide request, and a run whose answer does not arrive within ",
    "--approval-timeout-ms fails. The end of input cancels runs in flight and exits.\n\n",
    "Requests choose each session's model and store, so of the global options only ",
    "--reasoning-effort, and --git-executable with --read-dir, apply; the others are ",
    "rejected.\n\n",
    "The protocol and its schema, which --print-schema prints, may change in any release."
);

/// Arguments for serving the unstable protocol.
#[derive(Debug, Args)]
struct ServeArgs {
    /// Enables repository writes through the write tool; each write waits
    /// for the client's approval.
    #[arg(long, requires = "read_dir")]
    allow_write: bool,
    /// Milliseconds the client has to answer each approval request before
    /// the run fails.
    #[arg(
        long,
        value_name = "MS",
        default_value_t = 300_000,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    approval_timeout_ms: u64,
    /// Prints the JSON Schema of every protocol message and exits.
    #[arg(long)]
    print_schema: bool,
    /// Repository directory every run may read; runs offer no tools
    /// without it.
    #[arg(long, value_name = "DIR")]
    read_dir: Option<PathBuf>,
}

/// Arguments for signing in to a provider.
#[derive(Debug, Args)]
struct LoginArgs {
    /// Provider to sign in to; `chatgpt` bills requests to your `ChatGPT` plan.
    #[arg(value_parser = PossibleValuesParser::new(["chatgpt"]))]
    provider: String,
}

/// Arguments for a new durable model session.
#[derive(Debug, Args)]
#[command(after_help = provider_help())]
struct RunArgs {
    /// Model identifier sent to the provider.
    model: String,
    /// Optional first prompt. Further prompts are read from standard input.
    #[arg(value_parser = parse_prompt)]
    prompt: Option<String>,
    /// API base URL, overriding the provider-specific environment variable.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// Enables repository writes through the write tool.
    #[arg(long)]
    allow_write: bool,
    /// Model provider.
    #[arg(
        long,
        default_value_t = ModelProvider::Muse,
        value_parser = model_provider_parser()
    )]
    provider: ModelProvider,
    /// Repository directory available to enabled tools.
    #[arg(long, value_name = "DIR", default_value = ".")]
    read_dir: PathBuf,
    /// Stable session identifier. A random identifier is generated by default.
    #[arg(long, value_name = "ID")]
    session: Option<String>,
}

/// Arguments for resuming a durable model session.
#[derive(Debug, Args)]
struct ResumeArgs {
    /// Session identifier printed by `ag-harness run`.
    session: String,
    /// Optional first prompt. Further prompts are read from standard input.
    #[arg(value_parser = parse_prompt)]
    prompt: Option<String>,
    /// API base URL, overriding the provider-specific environment variable.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// Enables repository writes through the write tool.
    #[arg(long)]
    allow_write: bool,
    /// Repository directory available to enabled tools.
    #[arg(long, value_name = "DIR", default_value = ".")]
    read_dir: PathBuf,
}

fn model_provider_parser() -> impl TypedValueParser<Value = ModelProvider> {
    PossibleValuesParser::new(
        ModelProvider::all()
            .iter()
            .map(|provider| provider.as_str()),
    )
    .try_map(|provider| provider.parse::<ModelProvider>())
}

fn reasoning_effort_parser() -> impl TypedValueParser<Value = ReasoningEffort> {
    PossibleValuesParser::new(ReasoningEffort::ALL.iter().map(|effort| effort.as_str())).try_map(
        |effort| {
            ReasoningEffort::ALL
                .iter()
                .copied()
                .find(|candidate| candidate.as_str() == effort)
                // Unreachable: the possible-values parser already rejected
                // every value outside `ReasoningEffort::ALL`.
                .ok_or("unsupported reasoning effort")
        },
    )
}

fn provider_help() -> String {
    let mut help =
        String::from("Supported models (other endpoint-supported model IDs also work):\n");
    for provider in ModelProvider::all() {
        help.push_str("  ");
        help.push_str(provider.as_str());
        help.push_str(": ");
        if provider.known_models().is_empty() {
            help.push_str("any model the server serves, such as qwen3:latest");
        } else {
            help.push_str(&provider.known_models().join(", "));
        }
        help.push('\n');
    }
    help.push_str("\nCredentials:\n");
    for provider in ModelProvider::all() {
        help.push_str("  ");
        help.push_str(provider.as_str());
        help.push_str(": ");
        help.push_str(provider.api_key_environment());
        if !provider.requires_api_key() {
            help.push_str(" optional");
        }
        if let Some(default_base_url) = provider.default_base_url() {
            help.push_str(" (");
            help.push_str(provider.base_url_environment());
            help.push_str(" optional, default ");
            help.push_str(default_base_url);
            help.push(')');
        } else {
            help.push_str(", ");
            help.push_str(provider.base_url_environment());
        }
        help.push('\n');
    }
    let _ = write!(
        help,
        "\nOpenAI-compatible context, matching the server's own window:\n  \
         {OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV} (default {DEFAULT_CONTEXT_WINDOW_TOKENS} tokens), \
         {OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV} (default {DEFAULT_MAX_OUTPUT_TOKENS})"
    );

    help
}

fn parse_prompt(prompt: &str) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err("prompt must contain a non-whitespace character".to_string());
    }

    Ok(prompt.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatMode {
    Interactive,
    NonInteractive,
    OneShot,
}

impl ChatMode {
    fn detect(cli: &Cli, stdin_is_terminal: bool, stdout_is_terminal: bool) -> Self {
        if stdin_is_terminal && stdout_is_terminal {
            return Self::Interactive;
        }
        let initial_prompt = match &cli.command {
            Command::Run(args) => &args.prompt,
            Command::Resume(args) => &args.prompt,
            Command::Login(_) | Command::Serve(_) => &None,
        };
        if stdin_is_terminal && initial_prompt.is_some() {
            return Self::OneShot;
        }

        Self::NonInteractive
    }
}

/// Built-in provider model used for chat turns, cataloged as
/// `provider/model`.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelSelection {
    model: String,
    provider: ModelProvider,
}

impl ModelSelection {
    /// Resolves a `/model` argument: a number of the `/model` list, one of
    /// the [`LISTED_ROLES`], `provider/model`, a known model ID of the
    /// current provider or else of the first provider in
    /// [`ModelProvider::all`] order, or another model ID served by the
    /// current provider. Role names win over model IDs; `provider/model`
    /// selects a model named like a role.
    fn parse(target: &str, current: &Self, catalog: &ModelCatalog) -> Result<Self, CliError> {
        let unknown = || CliError::UnknownModel {
            model: target.to_string(),
        };
        if let Ok(number) = target.parse::<usize>() {
            return number
                .checked_sub(1)
                .and_then(|index| catalog.specs().nth(index))
                .and_then(Self::from_spec)
                .ok_or_else(unknown);
        }
        if let Some((provider, model)) = target.split_once('/') {
            let provider = provider.parse().map_err(|_| unknown())?;

            return Ok(Self {
                model: model.to_string(),
                provider,
            });
        }
        if let Some(role) = LISTED_ROLES.iter().find(|role| role.as_str() == target) {
            let handle = catalog
                .resolve(&ModelSelector::Role(role.clone()))
                .map_err(|_| CliError::UnassignedRole { role: role.clone() })?;

            return Self::from_spec(handle.spec()).ok_or_else(unknown);
        }
        let mut knowing = ModelProvider::all()
            .iter()
            .copied()
            .filter(|provider| provider.known_models().contains(&target));
        let provider = if current.provider.known_models().contains(&target) {
            current.provider
        } else {
            knowing.next().unwrap_or(current.provider)
        };

        Ok(Self {
            model: target.to_string(),
            provider,
        })
    }

    /// Returns the built-in model `spec` describes, or `None` for a model
    /// no built-in provider serves.
    fn from_spec(spec: &ModelSpec) -> Option<Self> {
        let provider = spec.provider()?;
        let model = spec.id().as_str().strip_prefix(&format!("{provider}/"))?;

        Some(Self {
            model: model.to_string(),
            provider,
        })
    }

    fn id(&self) -> ModelId {
        ModelId::cataloged(self.provider, &self.model)
    }

    /// Returns the registration identity, or `None` when `provider/model`
    /// exceeds the 256-byte identity limit.
    fn identity(&self) -> Option<ExecutionIdentity> {
        ExecutionIdentity::new(self.id().to_string(), MODEL_REGISTRATION_REVISION).ok()
    }
}

/// Built-in client connected for a model the catalog lacks, with its context
/// profile.
type ConnectedModel = Result<(ag_harness::model::ModelClient, ModelProfile), CliError>;

/// Current chat model, the catalog it is chosen from, and the connector
/// that builds clients, with their context profiles, for models the
/// catalog lacks.
struct ModelSwitcher<Connect> {
    catalog: Arc<ModelCatalog>,
    connect: Connect,
    selection: ModelSelection,
}

impl<Connect, Client> ModelSwitcher<Connect>
where
    Connect: FnMut(&ModelSelection) -> Result<(Client, ModelProfile), CliError>,
    Client: Model + 'static,
{
    /// Builds a harness for the current model. A cataloged model is
    /// selected from the catalog, which prices its runs; another model is
    /// connected directly. A registered harness, which resumes sessions
    /// stored with a registration such as after `/model`, serves the model
    /// through a registry instead and reports no cost.
    fn harness(&mut self, registered: bool) -> Result<Harness, CliError> {
        let selection = self.selection.clone();
        if let Some(identity) = selection.identity().filter(|_| registered) {
            let key = identity.key().to_string();
            let registry = self.registry(identity, &selection)?;

            return Ok(Harness::from_registry(&registry, &key)?);
        }
        let builder = Harness::builder();
        let builder = if let Some(handle) = self.cataloged(&selection) {
            let selector = ModelSelector::Id(handle.id().clone());

            select_cataloged(builder, Arc::clone(&self.catalog), selector)?
        } else {
            let (client, profile) = (self.connect)(&selection)?;

            select_direct(builder, Arc::new(client), profile)?
        };

        Ok(builder.build()?)
    }

    async fn switch(&mut self, session: &mut Session, target: &str) -> Result<(), CliError> {
        let selection = ModelSelection::parse(target, &self.selection, &self.catalog)?;
        let identity = selection
            .identity()
            .ok_or_else(|| CliError::ModelKeyTooLong {
                key: selection.id().to_string(),
            })?;
        let key = identity.key().to_string();
        let registry = self.registry(identity, &selection)?;
        session.switch_model(&registry, &key).await?;
        self.selection = selection;

        Ok(())
    }

    /// Registers `selection` as `identity`: the catalog's client and
    /// profile when cataloged, else a connected client.
    fn registry(
        &mut self,
        identity: ExecutionIdentity,
        selection: &ModelSelection,
    ) -> Result<ModelRegistry, CliError> {
        let (model, profile): (Arc<dyn Model>, _) = if let Some(handle) = self.cataloged(selection)
        {
            (handle.model(), handle.profile().unwrap_or(MODEL_PROFILE))
        } else {
            let (client, profile) = (self.connect)(selection)?;

            (Arc::new(client), profile)
        };
        let mut registry = ModelRegistry::new();
        registry.register_shared(
            identity,
            model,
            ModelCapabilities::new(profile).with_tool_calls(true),
        )?;

        Ok(registry)
    }

    fn cataloged(&self, selection: &ModelSelection) -> Option<ModelHandle> {
        self.catalog
            .resolve(&ModelSelector::Id(selection.id()))
            .ok()
    }

    /// Lists the catalog's models, numbered, with the roles they lead and
    /// their prices.
    fn list(&self) -> String {
        let current = self.selection.id().to_string();
        let mut text = format!("current model: {}\n", single_line_terminal_text(&current));
        for (index, spec) in self.catalog.specs().enumerate() {
            let _ = writeln!(text, "  {}. {}", index + 1, self.describe(spec));
        }
        if self.catalog.specs().next().is_none() {
            text.push_str(
                "  no model is cataloged; set a provider's credentials to list its models\n",
            );
        }
        text.push_str("Type /model <MODEL> to switch.\n");

        text
    }

    /// Describes one catalog model, such as
    /// `qwen/qwen3.8-flash (cheap, fast) $0.05 in, $0.40 out per 1M tokens`.
    fn describe(&self, spec: &ModelSpec) -> String {
        let id = spec.id().to_string();
        let mut text = single_line_terminal_text(&id).into_owned();
        let roles = LISTED_ROLES
            .iter()
            .filter(|role| self.catalog.candidates(role).first() == Some(spec.id()))
            .map(Role::as_str)
            .collect::<Vec<_>>();
        if !roles.is_empty() {
            let _ = write!(text, " ({})", roles.join(", "));
        }
        if let Some(pricing) = spec.pricing() {
            let _ = write!(
                text,
                " {} in, {} out per 1M tokens",
                format_usd(pricing.input_per_mtok_micro_usd()),
                format_usd(pricing.output_per_mtok_micro_usd())
            );
        }

        text
    }
}

/// Selects `selector` from `catalog` on `builder`, assuming
/// [`MODEL_PROFILE`] for a model whose profile the catalog does not
/// declare, such as a Muse or Kimi model.
///
/// # Errors
/// Returns an invalid [`MODEL_PROFILE`] budget.
fn select_cataloged(
    builder: HarnessBuilder,
    catalog: Arc<ModelCatalog>,
    selector: ModelSelector,
) -> Result<HarnessBuilder, ContextBudgetError> {
    let declared = catalog
        .resolve(&selector)
        .is_ok_and(|handle| handle.profile().is_some());
    let builder = builder.catalog(catalog).model_selector(selector);
    if declared {
        return Ok(builder);
    }

    Ok(builder
        .context_budget(MODEL_PROFILE.context_budget()?)
        .context_estimator(MODEL_PROFILE.estimator()))
}

/// Selects `model` directly on `builder`, bounded by `profile`.
///
/// # Errors
/// Returns an invalid `profile` budget.
fn select_direct(
    builder: HarnessBuilder,
    model: Arc<dyn Model>,
    profile: ModelProfile,
) -> Result<HarnessBuilder, ContextBudgetError> {
    Ok(builder
        .model(model)
        .context_budget(profile.context_budget()?)
        .context_estimator(profile.estimator()))
}

/// Slash command entered in place of a chat prompt.
#[derive(Debug, Eq, PartialEq)]
enum ChatCommand {
    Help,
    History,
    ListModels,
    SwitchModel(String),
    Unknown(String),
    Usage,
}

impl ChatCommand {
    /// Parses `prompt` as a command when its first word starts with `/` and
    /// is not a path.
    fn parse(prompt: &str) -> Option<Self> {
        let prompt = prompt.trim();
        let (name, argument) = prompt
            .split_once(char::is_whitespace)
            .map_or((prompt, ""), |(name, argument)| (name, argument.trim()));
        let command = name.strip_prefix('/')?;
        if command.contains('/') {
            return None;
        }

        Some(match (command, argument) {
            ("" | "help", _) => Self::Help,
            ("history", _) => Self::History,
            ("model", "") => Self::ListModels,
            ("model", model) => Self::SwitchModel(model.to_string()),
            ("usage", _) => Self::Usage,
            _ => Self::Unknown(name.to_string()),
        })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    let stdin_is_terminal = io::stdin().is_terminal();
    let stdout_is_terminal = io::stdout().is_terminal();
    let mode = ChatMode::detect(&cli, stdin_is_terminal, stdout_is_terminal);
    let input = BufReader::new(tokio::io::stdin());
    let output = tokio::io::stdout();

    report_exit(
        execute(cli, |name| env::var(name), input, output, mode).await,
        io::stderr().lock(),
    )
}

async fn execute<Input, Output>(
    cli: Cli,
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    input: Input,
    output: Output,
    mode: ChatMode,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    execute_with_telemetry(cli, environment, input, output, mode, io::stderr()).await
}

/// Runs the chat inside an optional OTLP trace export. Export failures are
/// reported as a warning and never change the chat result.
async fn execute_with_telemetry<Input, Output>(
    cli: Cli,
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    input: Input,
    output: Output,
    mode: ChatMode,
    mut warning_output: impl io::Write,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if let Some(option) = cli.option_serve_ignores() {
        return Err(CliError::ServeOption(option));
    }
    let telemetry = OtlpExport::start(
        cli.otlp_endpoint.as_deref(),
        Service {
            name: "ag-harness",
            user_id: None,
            version: env!("CARGO_PKG_VERSION"),
        },
    )
    .await?;
    if let Some(telemetry) = &telemetry {
        telemetry.install();
    }
    let result = execute_chat(cli, environment, input, output, mode, &mut warning_output).await;
    if let Some(telemetry) = telemetry {
        let warnings = telemetry.shutdown().await;
        if warnings > 0 {
            let _ = writeln!(
                warning_output,
                "OTLP export reported {warnings} warnings or failures; some traces may be missing."
            );
        }
    }

    result
}

fn report_exit(result: Result<(), CliError>, mut error_output: impl io::Write) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let error = error.to_string();
            let error = single_line_terminal_text(&error);
            let _ = writeln!(error_output, "{error}");

            ExitCode::FAILURE
        }
    }
}

async fn execute_chat<Input, Output>(
    cli: Cli,
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    input: Input,
    mut output: Output,
    mode: ChatMode,
    warning_output: &mut impl io::Write,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    let git_executable = cli.git_executable;
    let reasoning_effort = cli.reasoning_effort;
    let trace = cli.otlp_endpoint.is_some();
    let serve_host = (reasoning_effort, git_executable.clone());
    match cli.command {
        Command::Run(args) => {
            let database = database_path(cli.database, &mut environment)?;
            let session_id = args
                .session
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
            let selection = ModelSelection {
                model: args.model,
                provider: args.provider,
            };
            let mut models =
                chat_models(selection, args.base_url, &mut environment, warning_output)?;
            let harness = models.harness(false)?;
            let repository = repository_or_default(args.read_dir, git_executable)?;
            let comparison_base =
                comparison_base(&repository, cli.comparison_base.as_deref()).await?;
            let (harness, system_prompt) = configured_harness(
                harness,
                database,
                repository,
                args.allow_write,
                reasoning_effort,
                trace,
            );
            let mut session = harness
                .session(&session_id, chat_schema()?)
                .system_prompt(system_prompt)
                .create()
                .await?;
            announce_session(&mut output, &session_id).await?;

            run_chat(
                &mut session,
                models,
                args.prompt,
                input,
                output,
                mode,
                comparison_base,
            )
            .await
        }
        Command::Resume(args) => {
            let database = database_path(cli.database, &mut environment)?;
            let info = SessionInfo::load(&database, &args.session).await?;
            let (provider, model) = stored_model_identity(&info)?;
            let selection = ModelSelection { model, provider };
            let mut models =
                chat_models(selection, args.base_url, &mut environment, warning_output)?;
            let harness = models.harness(info.registration_identity().is_some())?;
            let repository = repository_or_default(args.read_dir, git_executable)?;
            let comparison_base =
                comparison_base(&repository, cli.comparison_base.as_deref()).await?;
            let (harness, _) = configured_harness(
                harness,
                database,
                repository,
                args.allow_write,
                reasoning_effort,
                trace,
            );
            let mut session = harness.resume(&args.session).await?;
            announce_session(&mut output, &args.session).await?;

            run_chat(
                &mut session,
                models,
                args.prompt,
                input,
                output,
                mode,
                comparison_base,
            )
            .await
        }
        Command::Login(_) => {
            login::login(
                environment,
                &login::SystemBrowser,
                &mut io::stdout().lock(),
                login::DEFAULT_PORT,
            )
            .await
        }
        Command::Serve(args) => run_serve(args, serve_host, environment, input, output).await,
    }
}

/// Serves the protocol with the host's reasoning effort and Git
/// executable, or prints its schema.
async fn run_serve<Input, Output>(
    args: ServeArgs,
    (reasoning_effort, git_executable): (ReasoningEffort, Option<PathBuf>),
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    input: Input,
    mut output: Output,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if args.print_schema {
        output.write_all(serve::PROTOCOL_SCHEMA.as_bytes()).await?;

        return Ok(output.flush().await?);
    }
    let settings = serve_settings(&args, reasoning_effort, git_executable)?;

    Ok(serve::serve(input, output, environment, settings).await?)
}

/// Returns the settings `args` select: `read` in `--read-dir`, plus
/// `write` with `--allow-write`, or no tools without a directory.
fn serve_settings(
    args: &ServeArgs,
    reasoning_effort: ReasoningEffort,
    git_executable: Option<PathBuf>,
) -> Result<serve::Settings, CliError> {
    let tools = match &args.read_dir {
        Some(root) => {
            let repository = repository_or_default(root.clone(), git_executable)?;
            let tools = ToolSet::empty().with_read();
            let tools = if args.allow_write {
                tools.with_write()
            } else {
                tools
            };

            Some((repository, tools))
        }
        None => None,
    };

    Ok(serve::Settings {
        approval_timeout: Duration::from_millis(args.approval_timeout_ms),
        reasoning_effort,
        tools,
    })
}

async fn announce_session(
    output: &mut (impl AsyncWrite + Unpin),
    session_id: &str,
) -> Result<(), io::Error> {
    let session_id = single_line_terminal_text(session_id);
    output
        .write_all(format!("session: {session_id}\n").as_bytes())
        .await?;
    output.flush().await
}

fn database_path(
    explicit: Option<PathBuf>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Ok(root) = environment("AG_HARNESS_ROOT")
        && !root.trim().is_empty()
    {
        return Ok(PathBuf::from(root).join("db").join("harness.db"));
    }

    let home = environment("HOME").map_err(|_| CliError::DatabaseLocation)?;
    if home.trim().is_empty() {
        return Err(CliError::DatabaseLocation);
    }

    Ok(PathBuf::from(home).join(".ag-harness/db/harness.db"))
}

fn repository_or_default(root: PathBuf, explicit: Option<PathBuf>) -> Result<Repository, CliError> {
    if let Some(explicit) = explicit {
        return Repository::new(root, explicit).map_err(CliError::from);
    }
    let path = env::var_os("PATH");

    repository_from_path(&root, path.as_deref())
}

fn repository_from_path(root: &Path, path: Option<&OsStr>) -> Result<Repository, CliError> {
    let executable_name = format!("git{}", env::consts::EXE_SUFFIX);

    let candidates = path
        .iter()
        .flat_map(env::split_paths)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name));
    for candidate in candidates {
        match Repository::new(root, candidate) {
            Ok(repository) => return Ok(repository),
            Err(
                error @ (ag_harness::RepositoryError::Root { .. }
                | ag_harness::RepositoryError::RootIsGitAdministrative { .. }),
            ) => return Err(error.into()),
            Err(_) => {}
        }
    }

    Err(CliError::GitExecutableNotFound)
}

/// Returns the chat's model switcher starting on `selection`, over the
/// environment's catalog, with `base_url` applied only to its provider.
fn chat_models<'a>(
    selection: ModelSelection,
    base_url: Option<String>,
    environment: &'a mut impl FnMut(&str) -> Result<String, env::VarError>,
    warning_output: &mut impl io::Write,
) -> Result<ModelSwitcher<impl FnMut(&ModelSelection) -> ConnectedModel + 'a>, CliError> {
    let provider = selection.provider;
    let catalog = model_catalog(provider, base_url.as_deref(), environment, warning_output)?;

    Ok(ModelSwitcher {
        catalog: Arc::new(catalog),
        connect: model_connector(provider, base_url, environment),
        selection,
    })
}

/// Loads the catalog of every model whose provider the environment
/// configures, applying an explicit base URL only to `provider`, the one it
/// was given for.
///
/// A configured provider whose models cannot be built, which need not be
/// the one the chat uses, leaves the catalog empty with a warning: every
/// model then connects directly, unpriced, and the starting model still
/// reports its own configuration errors.
fn model_catalog(
    provider: ModelProvider,
    base_url: Option<&str>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
    warning_output: &mut impl io::Write,
) -> Result<ModelCatalog, CliError> {
    let base_url_name = provider.base_url_environment();
    // The harness keeps no default sign-in location, so the CLI passes the
    // one it shares with Agentty.
    let auth_file = login::chatgpt_auth_file(environment);
    let catalog = ModelCatalog::from_environment_with_auth_file(
        |name| match base_url {
            Some(base_url) if name == base_url_name => Ok(base_url.to_string()),
            _ => environment(name),
        },
        auth_file.as_deref(),
    );

    match catalog {
        Ok(catalog) => Ok(catalog),
        Err(error) => {
            let error = error_chain(&error);
            let _ = writeln!(
                warning_output,
                "warning: {}; /model lists no models and turns report no cost",
                single_line_terminal_text(&error)
            );

            Ok(ModelCatalog::builder().build()?)
        }
    }
}

/// Joins the messages of `error` and its sources, outermost first.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = write!(text, ": {cause}");
        source = cause.source();
    }

    text
}

/// Builds clients and context profiles for models the catalog lacks, at
/// start and on `/model` switches. An explicit base URL applies only to the
/// provider it was given for.
fn model_connector(
    provider: ModelProvider,
    base_url: Option<String>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
) -> impl FnMut(&ModelSelection) -> ConnectedModel {
    move |selection| {
        let base_url = base_url
            .as_deref()
            .filter(|_| selection.provider == provider);

        connect_model(selection.provider, &selection.model, base_url, environment)
    }
}

/// Connects `model` of `provider` with credentials from `environment`, and
/// returns it with the profile its provider or the environment declares,
/// else [`MODEL_PROFILE`].
///
/// # Errors
/// Returns the provider configuration the environment lacks or gets wrong,
/// or [`CliError::ChatGptAuthFileLocation`] when no `ChatGPT` sign-in
/// location is known.
fn connect_model(
    provider: ModelProvider,
    model: &str,
    base_url: Option<&str>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<(ag_harness::model::ModelClient, ModelProfile), CliError> {
    let mut configuration = ModelConfiguration::new(provider, model);
    if let Some(base_url) = base_url {
        configuration = configuration.base_url(base_url);
    }
    // Only the `ChatGPT` provider reads a sign-in; the harness keeps no
    // default location, so the CLI passes the one it shares with Agentty.
    if provider.requires_sign_in() {
        let auth_file =
            login::chatgpt_auth_file(environment).ok_or(CliError::ChatGptAuthFileLocation)?;
        configuration = configuration.auth_file(auth_file);
    }
    let client = configuration.client_from_environment(&mut *environment)?;
    let profile = provider
        .declared_profile(model, environment)?
        .unwrap_or(MODEL_PROFILE);

    Ok((client, profile))
}

/// Resolves the explicit comparison `revision`, if any, once for the whole
/// chat.
async fn comparison_base(
    repository: &Repository,
    revision: Option<&str>,
) -> Result<Option<ComparisonBase>, CliError> {
    match revision {
        Some(revision) => Ok(Some(ComparisonBase::resolve(repository, revision).await?)),
        None => Ok(None),
    }
}

fn configured_harness(
    harness: Harness,
    database: PathBuf,
    repository: Repository,
    allow_write: bool,
    reasoning_effort: ReasoningEffort,
    trace: bool,
) -> (Harness, &'static str) {
    let mut harness = harness
        .database(database)
        .model_reasoning_effort(reasoning_effort)
        .repository(repository)
        .tools(repository_tools(allow_write));
    if trace {
        harness = harness.with_lifecycle_observer(LifecycleTraceObserver::new());
    }
    if allow_write {
        (harness, READ_WRITE_SYSTEM_PROMPT)
    } else {
        (harness, READ_ONLY_SYSTEM_PROMPT)
    }
}

/// Returns the built-in repository tools a turn offers: `read`, plus
/// `write` when writes are allowed.
fn repository_tools(allow_write: bool) -> ToolSet {
    let tools = ToolSet::empty().with_read();
    if allow_write {
        return tools.with_write();
    }

    tools
}

fn stored_model_identity(info: &SessionInfo) -> Result<(ModelProvider, String), CliError> {
    stored_model_identity_parts(info.provider(), info.model())
}

fn stored_model_identity_parts(
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(ModelProvider, String), CliError> {
    let model = model.ok_or(CliError::MissingModelIdentity)?.to_string();
    let provider = match provider {
        Some("meta") => ModelProvider::Muse,
        Some("moonshot_ai") => ModelProvider::Kimi,
        Some("alibaba_cloud") => ModelProvider::Qwen,
        Some("openai") => ModelProvider::OpenAi,
        Some("openai_chatgpt") => ModelProvider::ChatGpt,
        Some("anthropic") => ModelProvider::Anthropic,
        Some("openai_compatible") => ModelProvider::OpenAiCompatible,
        _ => return Err(CliError::MissingModelIdentity),
    };

    Ok((provider, model))
}

async fn run_chat<Connect, Client, Input, Output>(
    session: &mut Session,
    mut models: ModelSwitcher<Connect>,
    initial_prompt: Option<String>,
    mut input: Input,
    mut output: Output,
    mode: ChatMode,
    comparison_base: Option<ComparisonBase>,
) -> Result<(), CliError>
where
    Connect: FnMut(&ModelSelection) -> Result<(Client, ModelProfile), CliError>,
    Client: Model + 'static,
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if mode == ChatMode::Interactive {
        let requested_model = single_line_terminal_text(&models.selection.model);
        output
            .write_all(
                format!("Chat with {requested_model}. Type / for commands, Ctrl-D to exit.\n")
                    .as_bytes(),
            )
            .await?;
    }

    let mut pending_prompt = initial_prompt;
    let mut turn_failed = false;
    loop {
        let Some(prompt) = read_prompt(&mut pending_prompt, &mut input, &mut output, mode).await?
        else {
            break;
        };
        if prompt.trim().is_empty() {
            continue;
        }
        let result = if let Some(command) = ChatCommand::parse(&prompt) {
            run_command(command, session, &mut models, &mut output).await
        } else {
            let mut request = RunRequest::new(prompt);
            if let Some(base) = &comparison_base {
                request = request.comparison_base(base.clone());
            }
            let outcome = session.run(request).await;

            write_outcome(&mut output, &models.selection.model, outcome).await
        };
        match result {
            Ok(()) => {}
            Err(error) if mode == ChatMode::Interactive => {
                write_turn_error(&mut output, &error).await?;
            }
            Err(error) if mode == ChatMode::OneShot => return Err(error),
            Err(error) => {
                write_turn_error(&mut output, &error).await?;
                turn_failed = true;
            }
        }
        if mode == ChatMode::OneShot {
            break;
        }
    }

    if turn_failed {
        Err(CliError::ChatTurnsFailed)
    } else {
        Ok(())
    }
}

async fn run_command<Connect, Client>(
    command: ChatCommand,
    session: &mut Session,
    models: &mut ModelSwitcher<Connect>,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<(), CliError>
where
    Connect: FnMut(&ModelSelection) -> Result<(Client, ModelProfile), CliError>,
    Client: Model + 'static,
{
    let text = match command {
        ChatCommand::Help => CHAT_COMMAND_HELP.to_string(),
        ChatCommand::History => history_text(&session.transcript(TurnRange::All).await?),
        ChatCommand::ListModels => models.list(),
        ChatCommand::SwitchModel(target) => {
            models.switch(session, &target).await?;
            let current = models.selection.id().to_string();

            format!("model: {}\n", single_line_terminal_text(&current))
        }
        ChatCommand::Unknown(name) => return Err(CliError::UnknownCommand { name }),
        ChatCommand::Usage => usage_totals_text(&session.usage().await?),
    };
    output.write_all(text.as_bytes()).await?;
    output.flush().await?;

    Ok(())
}

async fn read_prompt<Input, Output>(
    pending_prompt: &mut Option<String>,
    input: &mut Input,
    output: &mut Output,
    mode: ChatMode,
) -> Result<Option<String>, io::Error>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if let Some(prompt) = pending_prompt.take() {
        return Ok(Some(prompt));
    }
    if mode == ChatMode::Interactive {
        output.write_all(b">>> ").await?;
        output.flush().await?;
    }
    let mut prompt = String::new();
    if input.read_line(&mut prompt).await? == 0 {
        return Ok(None);
    }
    trim_line_ending(&mut prompt);

    Ok(Some(prompt))
}

/// Writes the answer of a successful run with its timing, usage, cost, and
/// activity, or returns why the run failed.
async fn write_outcome(
    output: &mut (impl AsyncWrite + Unpin),
    requested_model: &str,
    outcome: RunOutcome,
) -> Result<(), CliError> {
    let RunOutcome {
        cost,
        report,
        result,
        usage,
        ..
    } = outcome;
    let reply = result?.parse::<ChatReply>()?;
    output
        .write_all(assistant_text(&reply.message).as_bytes())
        .await?;
    output.write_all(b"---\n").await?;
    output
        .write_all(format!("turn: {}\n", format_duration(report.duration())).as_bytes())
        .await?;
    output
        .write_all(
            format!(
                "usage: {}; {}\n",
                format_summed_tokens(&usage),
                format_cost(cost)
            )
            .as_bytes(),
        )
        .await?;
    write_activity(output, requested_model, &report).await
}

/// Writes the model requests and tool calls of `report`.
async fn write_activity(
    output: &mut (impl AsyncWrite + Unpin),
    requested_model: &str,
    report: &TurnReport,
) -> Result<(), CliError> {
    output
        .write_all(format!("model calls: {}\n", report.model_requests().len()).as_bytes())
        .await?;
    for (index, request) in report.model_requests().iter().enumerate() {
        let response_type = request.response_type();
        let completion = request.completion();
        let model = completion
            .and_then(|metadata| metadata.response_model())
            .unwrap_or(requested_model);
        let finish_reason = completion.map_or(
            "unavailable",
            ag_harness::model::CompletionMetadata::finish_reason,
        );
        let model = single_line_terminal_text(model);
        let finish_reason = single_line_terminal_text(finish_reason);
        let usage = completion
            .and_then(|metadata| metadata.usage())
            .map_or_else(|| "tokens unavailable".to_string(), format_request_usage);
        output
            .write_all(
                format!(
                    "  {}. {response_type}; {model}; {finish_reason}; {}; {usage}\n",
                    index + 1,
                    format_duration(request.duration()),
                )
                .as_bytes(),
            )
            .await?;
    }
    if report.tool_calls().is_empty() {
        output.write_all(b"tools: none\n").await?;
    } else {
        output.write_all(b"tools:\n").await?;
        for activity in report.tool_calls() {
            output
                .write_all(format!("  {activity}\n").as_bytes())
                .await?;
        }
    }
    output.flush().await?;

    Ok(())
}

async fn write_turn_error(
    output: &mut (impl AsyncWrite + Unpin),
    error: &(impl std::fmt::Display + ?Sized),
) -> Result<(), io::Error> {
    let error = error.to_string();
    let error = single_line_terminal_text(&error);
    output
        .write_all(format!("error: {error}\n").as_bytes())
        .await?;
    output.flush().await
}

fn format_request_usage(usage: &ag_harness::model::CompletionUsage) -> String {
    let input = usage
        .input_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());
    let output = usage
        .output_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());
    let total = usage
        .total_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());

    format!("tokens {input} in, {output} out, {total} total")
}

/// Formats summed tokens, such as `tokens 9 in, 3 out, 4 cached`, naming
/// the model calls whose provider reported no counts.
fn format_summed_tokens(usage: &Usage) -> String {
    let mut text = format!("tokens {} in, {} out", usage.input, usage.output);
    if usage.cache_read > 0 {
        let _ = write!(text, ", {} cached", usage.cache_read);
    }
    if usage.reasoning > 0 {
        let _ = write!(text, ", {} reasoning", usage.reasoning);
    }
    if usage.unreported_requests > 0 {
        let _ = write!(
            text,
            ", {} model calls unreported",
            usage.unreported_requests
        );
    }

    text
}

/// Formats a cost, or `cost unavailable` for an unpriced model.
fn format_cost(cost: Option<Cost>) -> String {
    cost.map_or_else(
        || "cost unavailable".to_string(),
        |cost| format!("cost {}", format_usd(cost.micro_usd)),
    )
}

/// Formats micro-USD as dollars with at least two decimals and no trailing
/// zeros beyond them, such as `$0.25` or `$0.000012`.
fn format_usd(micro_usd: u64) -> String {
    let fraction = format!("{:06}", micro_usd % MICRO_USD_PER_USD);
    let significant = fraction.trim_end_matches('0').len().max(2);

    format!(
        "${}.{}",
        micro_usd / MICRO_USD_PER_USD,
        &fraction[..significant]
    )
}

/// Formats a session's summed usage for `/usage`.
fn usage_totals_text(totals: &UsageTotals) -> String {
    let mut text = format!(
        "session usage: {} turns; {}; {}",
        totals.turns,
        format_summed_tokens(&totals.usage),
        format_cost(totals.cost)
    );
    if totals.unpriced_turns > 0 {
        let _ = write!(text, "; {} turns unpriced", totals.unpriced_turns);
    }
    if totals.unknown_turns > 0 {
        let _ = write!(text, "; {} turns without usage", totals.unknown_turns);
    }
    text.push('\n');

    text
}

/// Formats settled turns for `/history`: each turn's status, text input,
/// and answer.
fn history_text(turns: &[TurnRecord]) -> String {
    if turns.is_empty() {
        return "no turns yet\n".to_string();
    }
    let mut text = String::new();
    for turn in turns {
        // The wire name, such as `completed`, is the status's stable label.
        let status = serde_json::to_value(turn.status).unwrap_or_default();
        let status = status.as_str().unwrap_or("unknown");
        let _ = writeln!(text, "turn {} ({status})", turn.turn.get() + 1);
        for input in &turn.input {
            if let TranscriptInput::Text { text: prompt } = input {
                text.push_str(&framed_text("you> ", prompt));
            }
        }
        for item in &turn.items {
            if let Item::Output { output } = item
                && let Ok(reply) = output.parse::<ChatReply>()
            {
                text.push_str(&assistant_text(&reply.message));
            }
        }
    }

    text
}

fn format_duration(duration: std::time::Duration) -> String {
    if duration.as_millis() == 0 {
        "<1 ms".to_string()
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

fn assistant_text(text: &str) -> String {
    framed_text("assistant> ", text)
}

/// Frames terminal-safe `text` behind `label`, indenting continuation lines
/// so no line can pass for CLI output.
fn framed_text(label: &str, text: &str) -> String {
    let text = terminal_text(text);
    let indent = " ".repeat(label.len());
    let mut framed = String::new();
    for (index, line) in text.split('\n').enumerate() {
        framed.push_str(if index == 0 { label } else { &indent });
        framed.push_str(line);
        framed.push('\n');
    }

    framed
}

fn terminal_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(is_terminal_safe) {
        return Cow::Borrowed(text);
    }

    Cow::Owned(
        text.chars()
            .map(|character| {
                if is_terminal_safe(character) {
                    character
                } else {
                    '\u{fffd}'
                }
            })
            .collect(),
    )
}

fn single_line_terminal_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(|character| !character.is_control()) {
        return Cow::Borrowed(text);
    }

    Cow::Owned(
        text.chars()
            .map(|character| {
                if character.is_control() {
                    '\u{fffd}'
                } else {
                    character
                }
            })
            .collect(),
    )
}

fn is_terminal_safe(character: char) -> bool {
    !character.is_control() || matches!(character, '\n' | '\t')
}

/// Answer every chat turn returns, as [`chat_schema`] requires.
#[derive(Deserialize)]
struct ChatReply {
    message: String,
}

fn chat_schema() -> Result<OutputSchema, CliError> {
    let message = Value::Object(Map::from_iter([(
        "type".to_string(),
        Value::String("string".to_string()),
    )]));
    let properties = Value::Object(Map::from_iter([("message".to_string(), message)]));
    let schema = Value::Object(Map::from_iter([
        ("type".to_string(), Value::String("object".to_string())),
        ("properties".to_string(), properties),
        (
            "required".to_string(),
            Value::Array(vec![Value::String("message".to_string())]),
        ),
        ("additionalProperties".to_string(), Value::Bool(false)),
    ]));

    OutputSchema::new(schema).map_err(CliError::from)
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    ComparisonBase(#[from] ag_harness::ComparisonBaseError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    ContextBudget(#[from] ContextBudgetError),
    #[error("--base-url or {name} is required")]
    BaseUrlRequired { name: &'static str },
    #[error("CHATGPT_AUTH_FILE, AGENTTY_ROOT, or HOME is required to locate the ChatGPT sign-in")]
    ChatGptAuthFileLocation,
    #[error("one or more chat turns failed")]
    ChatTurnsFailed,
    #[error(transparent)]
    Login(#[from] ag_harness::provider::ChatGptSignInError),
    #[error("--database, AG_HARNESS_ROOT, or HOME is required for durable session storage")]
    DatabaseLocation,
    #[error("No valid Git executable was found in PATH; pass --git-executable <FILE>")]
    GitExecutableNotFound,
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error("stored session does not identify a supported built-in model")]
    MissingModelIdentity,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ModelConfiguration(ModelConfigurationError),
    #[error("cannot switch to `{key}`; `provider/model` must fit in 256 bytes")]
    ModelKeyTooLong { key: String },
    #[error(transparent)]
    ModelRegistry(#[from] ModelRegistryError),
    #[error(transparent)]
    OutputSchema(#[from] ag_harness::OutputSchemaError),
    #[error(transparent)]
    Repository(#[from] ag_harness::RepositoryError),
    #[error("{0} does not apply to `serve`; requests choose each session's model and store")]
    ServeOption(&'static str),
    #[error(transparent)]
    Session(#[from] ag_harness::SessionError),
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    Telemetry(#[from] OtlpError),
    #[error("no configured model serves the `{role}` role; type /model to list models")]
    UnassignedRole { role: Role },
    #[error("unknown command `{name}`; type /help for commands")]
    UnknownCommand { name: String },
    #[error("unknown model `{model}`; type /model to list models")]
    UnknownModel { model: String },
}

impl From<ModelConfigurationError> for CliError {
    fn from(error: ModelConfigurationError) -> Self {
        match error {
            ModelConfigurationError::BaseUrl { name } => Self::BaseUrlRequired { name },
            error => Self::ModelConfiguration(error),
        }
    }
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
