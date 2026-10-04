//! Interactive command-line chat powered by the `ag-harness` model runtime.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, io};

use ag_harness::lifecycle::LifecycleTraceObserver;
use ag_harness::model::{ModelCapabilities, ModelRegistry, ModelRegistryError, ReasoningEffort};
use ag_harness::provider::{ModelConfiguration, ModelConfigurationError, ModelProvider};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::SessionInfo;
use ag_harness::{
    ComparisonBase, Harness, Model, OutputSchema, Repository, Session, Tool, ToolPolicy,
    TurnLimits, TurnOptions, TurnOutcome,
};
use ag_telemetry::otlp::{OtlpError, OtlpExport, Service};
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};

const CHAT_COMMAND_HELP: &str = concat!(
    "commands:\n",
    "  /model            list models\n",
    "  /model <MODEL>    switch later turns to a list number, provider/model, or model ID\n",
    "  /help             show commands\n"
);
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

/// Supported harness commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Starts a new durable session with a model.
    Run(RunArgs),
    /// Resumes a durable session.
    Resume(ResumeArgs),
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
                .ok_or_else(|| format!("unsupported reasoning effort `{effort}`"))
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
        help.push_str(&provider.known_models().join(", "));
        help.push('\n');
    }
    help.push_str("\nCredentials:\n");
    for provider in ModelProvider::all() {
        help.push_str("  ");
        help.push_str(provider.as_str());
        help.push_str(": ");
        help.push_str(provider.api_key_environment());
        if provider.default_base_url().is_some() {
            help.push_str(" (");
            help.push_str(provider.base_url_environment());
            help.push_str(" optional)");
        } else {
            help.push_str(", ");
            help.push_str(provider.base_url_environment());
        }
        help.push('\n');
    }
    help.pop();

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
        };
        if stdin_is_terminal && initial_prompt.is_some() {
            return Self::OneShot;
        }

        Self::NonInteractive
    }
}

/// Built-in provider model used for chat turns.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelSelection {
    model: String,
    provider: ModelProvider,
}

impl ModelSelection {
    /// Resolves a `/model` argument: a catalog number, `provider/model`, a
    /// known model ID, or another model ID served by the current provider.
    fn parse(target: &str, current: &Self) -> Result<Self, CliError> {
        let unknown = || CliError::UnknownModel {
            model: target.to_string(),
        };
        let catalog = Self::catalog();
        if let Ok(number) = target.parse::<usize>() {
            return number
                .checked_sub(1)
                .and_then(|index| catalog.get(index))
                .cloned()
                .ok_or_else(unknown);
        }
        if let Some((provider, model)) = target.split_once('/') {
            let provider = provider.parse().map_err(|_| unknown())?;

            return Ok(Self {
                model: model.to_string(),
                provider,
            });
        }

        Ok(catalog
            .into_iter()
            .find(|selection| selection.model == target)
            .unwrap_or_else(|| Self {
                model: target.to_string(),
                provider: current.provider,
            }))
    }

    fn catalog() -> Vec<Self> {
        ModelProvider::all()
            .iter()
            .flat_map(|provider| {
                provider.known_models().iter().map(|model| Self {
                    model: (*model).to_string(),
                    provider: *provider,
                })
            })
            .collect()
    }

    fn key(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Returns the registration identity, or `None` when `provider/model`
    /// exceeds the 256-byte identity limit.
    fn identity(&self) -> Option<ExecutionIdentity> {
        ExecutionIdentity::new(self.key(), MODEL_REGISTRATION_REVISION).ok()
    }

    fn registry(
        identity: ExecutionIdentity,
        client: impl Model + 'static,
    ) -> Result<ModelRegistry, CliError> {
        let mut registry = ModelRegistry::new();
        registry.register(
            identity,
            client,
            ModelCapabilities {
                tool_calls: true,
                ..ModelCapabilities::default()
            },
        )?;

        Ok(registry)
    }
}

/// Current chat model and the connector that builds clients for switches.
struct ModelSwitcher<Connect> {
    connect: Connect,
    selection: ModelSelection,
}

impl<Connect, Client> ModelSwitcher<Connect>
where
    Connect: FnMut(&ModelSelection) -> Result<Client, CliError>,
    Client: Model + 'static,
{
    /// Builds a harness for the current model. A registered harness records
    /// its identity for resume; a direct harness resumes sessions stored
    /// without one and serves models too long to register.
    fn harness(&mut self, registered: bool) -> Result<Harness, CliError> {
        let client = (self.connect)(&self.selection)?;
        let Some(identity) = self.selection.identity().filter(|_| registered) else {
            return Ok(Harness::new(client));
        };
        let key = identity.key().to_string();
        let registry = ModelSelection::registry(identity, client)?;

        Ok(Harness::from_registry(&registry, &key)?)
    }

    async fn switch(&mut self, session: &mut Session, target: &str) -> Result<(), CliError> {
        let selection = ModelSelection::parse(target, &self.selection)?;
        let identity = selection
            .identity()
            .ok_or_else(|| CliError::ModelKeyTooLong {
                key: selection.key(),
            })?;
        let registry = ModelSelection::registry(identity, (self.connect)(&selection)?)?;
        session.switch_model(&registry, &selection.key()).await?;
        self.selection = selection;

        Ok(())
    }
}

/// Slash command entered in place of a chat prompt.
#[derive(Debug, Eq, PartialEq)]
enum ChatCommand {
    Help,
    ListModels,
    SwitchModel(String),
    Unknown(String),
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
            ("model", "") => Self::ListModels,
            ("model", model) => Self::SwitchModel(model.to_string()),
            _ => Self::Unknown(name.to_string()),
        })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
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
    let telemetry = OtlpExport::start(
        cli.otlp_endpoint.as_deref(),
        Service {
            name: "ag-harness",
            version: env!("CARGO_PKG_VERSION"),
        },
    )
    .await?;
    if let Some(telemetry) = &telemetry {
        telemetry.install();
    }
    let result = execute_chat(cli, environment, input, output, mode).await;
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
    output: Output,
    mode: ChatMode,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    let database = database_path(cli.database, &mut environment)?;
    let git_executable = cli.git_executable;
    let reasoning_effort = cli.reasoning_effort;
    let trace = cli.otlp_endpoint.is_some();
    match cli.command {
        Command::Run(args) => {
            let session_id = args
                .session
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
            let mut models = ModelSwitcher {
                connect: model_connector(args.provider, args.base_url, &mut environment),
                selection: ModelSelection {
                    model: args.model,
                    provider: args.provider,
                },
            };
            let harness = models.harness(true)?;
            let repository = repository_or_default(args.read_dir, git_executable)?;
            let options = comparison_options(
                &repository,
                cli.comparison_base.as_deref(),
                args.allow_write,
            )
            .await?;
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
            let mut output = output;
            announce_session(&mut output, &session_id).await?;

            run_chat(
                &mut session,
                models,
                args.prompt,
                input,
                output,
                mode,
                options,
            )
            .await
        }
        Command::Resume(args) => {
            let info = SessionInfo::load(&database, &args.session).await?;
            let (provider, model) = stored_model_identity(&info)?;
            let mut models = ModelSwitcher {
                connect: model_connector(provider, args.base_url, &mut environment),
                selection: ModelSelection { model, provider },
            };
            let harness = models.harness(info.registration_identity().is_some())?;
            let repository = repository_or_default(args.read_dir, git_executable)?;
            let options = comparison_options(
                &repository,
                cli.comparison_base.as_deref(),
                args.allow_write,
            )
            .await?;
            let (harness, _) = configured_harness(
                harness,
                database,
                repository,
                args.allow_write,
                reasoning_effort,
                trace,
            );
            let mut session = harness.resume(&args.session).await?;
            let mut output = output;
            announce_session(&mut output, &args.session).await?;

            run_chat(
                &mut session,
                models,
                args.prompt,
                input,
                output,
                mode,
                options,
            )
            .await
        }
    }
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

/// Builds clients for the starting model and later `/model` switches. An
/// explicit base URL applies only to the provider it was given for.
fn model_connector(
    provider: ModelProvider,
    base_url: Option<String>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
) -> impl FnMut(&ModelSelection) -> Result<ag_harness::model::ModelClient, CliError> {
    move |selection| {
        let base_url = base_url
            .as_deref()
            .filter(|_| selection.provider == provider);

        model_client(selection.provider, &selection.model, base_url, environment)
    }
}

fn model_client(
    provider: ModelProvider,
    model: &str,
    base_url: Option<&str>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<ag_harness::model::ModelClient, CliError> {
    let mut configuration = ModelConfiguration::new(provider, model);
    if let Some(base_url) = base_url {
        configuration = configuration.base_url(base_url);
    }

    configuration
        .client_from_environment(environment)
        .map_err(CliError::from)
}

async fn comparison_options(
    repository: &Repository,
    revision: Option<&str>,
    allow_write: bool,
) -> Result<TurnOptions, CliError> {
    let mut policy = ToolPolicy::default().allow(Tool::Read);
    if allow_write {
        policy = policy.allow(Tool::Write);
    }
    let options = TurnOptions::new(chat_schema()?, policy, TurnLimits::default());

    match revision {
        Some(revision) => {
            Ok(options.with_comparison_base(ComparisonBase::resolve(repository, revision).await?))
        }
        None => Ok(options),
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
        .allow(Tool::Read);
    if trace {
        harness = harness.with_lifecycle_observer(LifecycleTraceObserver::new());
    }
    if allow_write {
        harness = harness.allow(Tool::Write);

        (harness, READ_WRITE_SYSTEM_PROMPT)
    } else {
        (harness, READ_ONLY_SYSTEM_PROMPT)
    }
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
    options: TurnOptions,
) -> Result<(), CliError>
where
    Connect: FnMut(&ModelSelection) -> Result<Client, CliError>,
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
        let result = match ChatCommand::parse(&prompt) {
            Some(command) => run_command(command, session, &mut models, &mut output).await,
            None => match session.turn(prompt).options(options.clone()).await {
                Ok(outcome) => {
                    write_outcome(&mut output, &models.selection.model, &outcome).await?;

                    Ok(())
                }
                Err(error) => Err(error.into()),
            },
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
    Connect: FnMut(&ModelSelection) -> Result<Client, CliError>,
    Client: Model + 'static,
{
    let text = match command {
        ChatCommand::Help => CHAT_COMMAND_HELP.to_string(),
        ChatCommand::ListModels => {
            let current = models.selection.key();
            let mut text = format!("current model: {}\n", single_line_terminal_text(&current));
            for (index, selection) in ModelSelection::catalog().iter().enumerate() {
                let _ = writeln!(text, "  {}. {}", index + 1, selection.key());
            }
            text.push_str("Type /model <MODEL> to switch.\n");

            text
        }
        ChatCommand::SwitchModel(target) => {
            models.switch(session, &target).await?;
            let current = models.selection.key();

            format!("model: {}\n", single_line_terminal_text(&current))
        }
        ChatCommand::Unknown(name) => return Err(CliError::UnknownCommand { name }),
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

async fn write_outcome(
    output: &mut (impl AsyncWrite + Unpin),
    requested_model: &str,
    outcome: &TurnOutcome,
) -> Result<(), CliError> {
    let message = outcome
        .output()
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or(CliError::MissingMessage)?;
    output.write_all(assistant_text(message).as_bytes()).await?;
    output.write_all(b"---\n").await?;
    output
        .write_all(format!("turn: {}\n", format_duration(outcome.report().duration())).as_bytes())
        .await?;
    output
        .write_all(format!("model calls: {}\n", outcome.report().model_requests().len()).as_bytes())
        .await?;
    for (index, request) in outcome.report().model_requests().iter().enumerate() {
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
            .map_or_else(|| "tokens unavailable".to_string(), format_usage);
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
    if outcome.report().tool_calls().is_empty() {
        output.write_all(b"tools: none\n").await?;
    } else {
        output.write_all(b"tools:\n").await?;
        for activity in outcome.report().tool_calls() {
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

fn format_usage(usage: &ag_harness::model::CompletionUsage) -> String {
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
    let text = terminal_text(text);
    let mut framed = String::new();
    for (index, line) in text.split('\n').enumerate() {
        framed.push_str(if index == 0 {
            "assistant> "
        } else {
            "           "
        });
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
    #[error("--base-url or {name} is required")]
    BaseUrlRequired { name: &'static str },
    #[error("one or more chat turns failed")]
    ChatTurnsFailed,
    #[error("--database, AG_HARNESS_ROOT, or HOME is required for durable session storage")]
    DatabaseLocation,
    #[error("No valid Git executable was found in PATH; pass --git-executable <FILE>")]
    GitExecutableNotFound,
    #[error("model output did not contain a message")]
    MissingMessage,
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
    #[error(transparent)]
    Session(#[from] ag_harness::SessionError),
    #[error(transparent)]
    Telemetry(#[from] OtlpError),
    #[error(transparent)]
    Turn(#[from] ag_harness::TurnError),
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
