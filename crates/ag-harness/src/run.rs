//! Run-level API: a [`RunRequest`] starts a cancellable [`Run`] that resolves
//! to a [`RunOutcome`] with its output, aggregated [`Usage`], and the files
//! and commands the run left.

use std::fmt;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::bash::{BashConfig, CommandRecord};
use crate::cancellation::{
    ControlledTurn, SettlementError, SettlementPhase, TurnControl, TurnRecord,
};
use crate::comparison::ComparisonBase;
use crate::error::{ErrorKind, RunError};
use crate::event::{EventSink, EventStream};
use crate::input::TurnInput;
use crate::limit::RunLimits;
use crate::model::CompletionUsage;
use crate::policy::ToolPolicy;
use crate::request_option::{GenerationParams, OutputMode};
use crate::session::SessionError;
use crate::store::SessionStore;
use crate::turn::{TurnOptions, TurnReport};
use crate::write_journal::{WriteRecord, WriteStatus};

/// Input and per-run options of one [`Run`], started with
/// [`crate::Session::run`] or [`crate::Harness::run`].
///
/// Every option applies to this run only and never changes a session's
/// stored defaults. An unset option keeps the default: the session's stored
/// output mode and the harness's default tool policy.
#[derive(Clone)]
#[must_use = "a run request does nothing until a run is started with it"]
#[non_exhaustive]
pub struct RunRequest {
    bash: Option<BashConfig>,
    comparison_base: Option<ComparisonBase>,
    generation: GenerationParams,
    input: TurnInput,
    instructions: Option<String>,
    limits: RunLimits,
    output: Option<OutputMode>,
    tool_policy: Option<ToolPolicy>,
}

impl RunRequest {
    /// Starts a request for `input`; plain strings are text input.
    pub fn new(input: impl Into<TurnInput>) -> Self {
        Self {
            bash: None,
            comparison_base: None,
            generation: GenerationParams::default(),
            input: input.into(),
            instructions: None,
            limits: RunLimits::default(),
            output: None,
            tool_policy: None,
        }
    }

    /// Bounds this run's model requests, tool calls, tokens, and wall-clock
    /// time; a run without limits is bounded only by cancellation and the
    /// model's context budget. A run that reaches a limit fails with
    /// [`ErrorKind::BudgetExhausted`]. Wall-clock time starts once the
    /// session admits the run, so it excludes waiting for the session to
    /// become free; a zero wall-clock limit fails with [`ErrorKind::Config`]
    /// before the run starts.
    pub fn limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;

        self
    }

    /// Selects the form of the run's final answer instead of the session's
    /// stored default: an [`crate::OutputSchema`] or [`OutputMode::Json`] for
    /// validated JSON, or [`OutputMode::Text`] for plain text. Required by
    /// [`crate::Harness::run`].
    pub fn output(mut self, output: impl Into<OutputMode>) -> Self {
        self.output = Some(output.into());

        self
    }

    /// Adds host instructions for this run, like
    /// [`TurnOptions::with_instructions`]: they follow the cached prompt
    /// prefix, are stored with the run's turn, and are left out when later
    /// runs replay that turn.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());

        self
    }

    /// Sets provider generation parameters for this run's model requests.
    pub fn generation(mut self, generation: GenerationParams) -> Self {
        self.generation = generation;

        self
    }

    /// Replaces the harness's default tool policy for this run; an empty
    /// policy denies every tool.
    pub fn tool_policy(mut self, policy: ToolPolicy) -> Self {
        self.tool_policy = Some(policy);

        self
    }

    /// Configures the `bash` tool for this run, like
    /// [`TurnOptions::with_bash`]. The tool policy must still allow it.
    pub fn bash(mut self, configuration: BashConfig) -> Self {
        self.bash = Some(configuration);

        self
    }

    /// Pins the host-selected comparison base for this run, like
    /// [`TurnOptions::with_comparison_base`].
    pub fn comparison_base(mut self, base: ComparisonBase) -> Self {
        self.comparison_base = Some(base);

        self
    }

    /// Resolves the input and turn options, using `default_output` and
    /// `default_policy` for unset options.
    ///
    /// # Errors
    /// Returns a [`ErrorKind::Config`] failure when neither the request nor
    /// the caller supplies an output mode, or when a limit cannot be met.
    pub(crate) fn into_turn(
        self,
        default_output: Option<&OutputMode>,
        default_policy: ToolPolicy,
    ) -> Result<(TurnInput, TurnOptions), RunError> {
        if self.limits.is_unsatisfiable() {
            return Err(RunError::with_kind(ErrorKind::Config, ZeroWallTime));
        }
        let output = self
            .output
            .or_else(|| default_output.cloned())
            .ok_or_else(|| RunError::with_kind(ErrorKind::Config, OutputRequired))?;
        let mut options = TurnOptions::new(output, self.tool_policy.unwrap_or(default_policy))
            .with_generation(self.generation)
            .with_limits(self.limits);
        if let Some(configuration) = self.bash {
            options = options.with_bash(configuration);
        }
        if let Some(base) = self.comparison_base {
            options = options.with_comparison_base(base);
        }
        if let Some(instructions) = self.instructions {
            options = options.with_instructions(instructions);
        }

        Ok((self.input, options))
    }
}

/// One started run of a [`RunRequest`].
///
/// Await it, or its [`Self::into_handle`], for the [`RunOutcome`]; nothing
/// executes until then. [`Self::into_parts`] also streams the run's typed
/// events. Dropping it, or cancelling its [`CancelToken`], cancels the run.
/// The outcome resolves only after the run's persistence, writes, and
/// commands settle, retrying failed cleanup once; cleanup that still fails
/// is reported in [`RunOutcome::settlement`], which can retry it.
#[must_use = "runs do not start until awaited"]
#[non_exhaustive]
pub struct Run {
    cancel: CancelToken,
    events: Option<Arc<EventSink>>,
    journal: RunJournal,
    turn: ControlledTurn<'static, RunError>,
}

impl Run {
    /// Returns a token that cancels this run from any task.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Returns a handle that cancels the run or awaits its outcome, for
    /// hosts that keep the run's control and outcome together.
    pub fn into_handle(self) -> RunHandle {
        RunHandle { run: self }
    }

    /// Splits the run into its ordered [`EventStream`] and a handle that
    /// cancels it or awaits its outcome.
    ///
    /// The run still starts only when the handle's outcome is awaited. Read
    /// the stream concurrently: the run waits while too many events are
    /// unread, although cancelling it still resolves its outcome. Dropping
    /// the stream discards later events without stalling or cancelling the
    /// run. Only a split run has a [`crate::event::RunId`], carried by its
    /// events.
    pub fn into_parts(mut self) -> (EventStream, RunHandle) {
        let (sink, stream) = EventSink::channel();
        self.cancel.control.attach_events(&sink);
        self.events = Some(sink);

        (stream, self.into_handle())
    }

    /// Wraps a controlled turn whose durable records `journal` locates.
    pub(crate) fn new(turn: ControlledTurn<'static, RunError>, journal: RunJournal) -> Self {
        Self {
            cancel: CancelToken {
                control: turn.control(),
            },
            events: None,
            journal,
            turn,
        }
    }

    /// Runs the turn, settles it, and reports its outcome, last to
    /// `events` when the run streams them.
    async fn outcome(self) -> RunOutcome {
        let Self {
            cancel: CancelToken { control },
            events,
            journal,
            turn,
        } = self;
        let started_at = Instant::now();
        let result = turn.await;
        let settlement = Self::settle(&control)
            .await
            .map_err(|error| Unsettled::new(control.clone(), error));
        let TurnRecord {
            report,
            turn_position,
        } = control.take_record();
        let (result, report) = match result {
            Ok(outcome) => {
                let (output, report) = outcome.into_parts();

                (Ok(journal.output(output)), report)
            }
            Err(error) => {
                let report = report.unwrap_or_else(|| {
                    TurnReport::new(started_at.elapsed(), Vec::new(), Vec::new())
                });

                (Err(error), report)
            }
        };
        let records = match turn_position {
            Some(turn_position) => journal.load(turn_position).await,
            None => Ok((Vec::new(), Vec::new())),
        };
        let (files_changed, commands, records_error) = match records {
            Ok((files_changed, commands)) => (files_changed, commands, None),
            Err(error) => (Vec::new(), Vec::new(), Some(RunError::from(error))),
        };
        let outcome = RunOutcome {
            commands,
            files_changed,
            records_error,
            result,
            settlement,
            usage: Usage::from_report(&report),
            report,
        };
        if let Some(events) = events {
            events.complete(&outcome).await;
        }

        outcome
    }

    /// Waits until the turn's effects settle, retrying failed cleanup once.
    async fn settle(control: &TurnControl) -> Result<(), SettlementError> {
        if control.settled().await.is_ok() {
            return Ok(());
        }

        Self::retry_settlement(control).await
    }

    /// Retries the turn's failed cleanup and waits for its effects again.
    async fn retry_settlement(control: &TurnControl) -> Result<(), SettlementError> {
        control.retry_settlement().await?;

        control.settled().await
    }
}

impl IntoFuture for Run {
    type IntoFuture = Pin<Box<dyn Future<Output = RunOutcome> + Send>>;
    type Output = RunOutcome;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.outcome())
    }
}

/// Cancels a run, or awaits its outcome, from the task that owns it.
///
/// Dropping the handle, or the future returned by [`Self::outcome`], before
/// the outcome resolves cancels the run. Take a [`Self::cancel_token`] first
/// to cancel while the outcome is awaited.
#[must_use = "runs do not start until their outcome is awaited"]
#[non_exhaustive]
pub struct RunHandle {
    run: Run,
}

impl RunHandle {
    /// Requests cancellation, like [`CancelToken::cancel`].
    pub fn cancel(&self) {
        self.run.cancel.cancel();
    }

    /// Returns a token that cancels the run from any task, including while
    /// [`Self::outcome`] is awaited.
    pub fn cancel_token(&self) -> CancelToken {
        self.run.cancel_token()
    }

    /// Runs to completion and returns the outcome.
    pub async fn outcome(self) -> RunOutcome {
        self.run.await
    }
}

/// Cloneable cancellation of one [`Run`].
///
/// Cancelling stops the run promptly with [`ErrorKind::Cancelled`]; its
/// outcome still waits until started writes and commands settle. Repeated
/// calls, and calls after the run finished, have no effect.
#[derive(Clone)]
#[non_exhaustive]
pub struct CancelToken {
    control: TurnControl,
}

impl CancelToken {
    /// Requests cancellation of the run.
    pub fn cancel(&self) {
        self.control.cancel();
    }
}

/// Settled result of one [`Run`], with what the run did either way.
#[derive(Debug)]
#[non_exhaustive]
pub struct RunOutcome {
    /// Command records the run's `bash` tool calls left, in invocation
    /// order; empty unless the request configured `bash`.
    pub commands: Vec<CommandRecord>,
    /// Files the run's `write` tool calls targeted, in order, with each
    /// write's status: applied, failed, or never acknowledged.
    pub files_changed: Vec<FileChange>,
    /// Why [`Self::files_changed`] and [`Self::commands`] could not be read,
    /// leaving them empty. It never changes [`Self::result`]: the run's turn
    /// and its records were committed either way.
    pub records_error: Option<RunError>,
    /// Content-free model and tool activity, including the model requests
    /// a failed, cancelled, or interrupted run completed. A run that failed
    /// before its first model request reports none.
    pub report: TurnReport,
    /// The validated output, or why the run's turn failed.
    pub result: Result<RunOutput, RunError>,
    /// Whether the run's persistence, writes, and commands settled. Cleanup
    /// that still fails after one automatic retry keeps the session from
    /// admitting a new run until [`Unsettled::retry`] succeeds or the
    /// session is reopened; it never changes [`Self::result`].
    pub settlement: Result<(), Unsettled>,
    /// Tokens of every model request in [`Self::report`].
    pub usage: Usage,
}

/// One file a run's `write` tool call targeted.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct FileChange {
    /// Repository-relative target path.
    pub path: String,
    /// Whether the write was applied, failed, or never acknowledged.
    pub status: WriteStatus,
}

impl From<WriteRecord> for FileChange {
    fn from(record: WriteRecord) -> Self {
        Self {
            path: record.path,
            status: record.status,
        }
    }
}

/// Validated final output of a successful run.
///
/// It serializes as `{"type": "json" | "text", "value": ...}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunOutput {
    /// JSON output validated against the run's output schema.
    Json(Value),
    /// The final assistant text of a run in text output mode.
    Text(String),
}

impl RunOutput {
    /// Deserializes the output into `T`; text output deserializes as a
    /// JSON string.
    ///
    /// # Errors
    /// Returns an [`ErrorKind::InvalidOutput`] failure when the output does
    /// not match `T`.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, RunError> {
        let parsed = match self {
            Self::Json(value) => T::deserialize(value),
            Self::Text(text) => T::deserialize(Value::String(text.clone())),
        };

        parsed.map_err(|error| RunError::with_kind(ErrorKind::InvalidOutput, error))
    }
}

/// Cleanup of a finished run that failed to settle, keeping its session
/// from admitting a new run.
#[non_exhaustive]
pub struct Unsettled {
    control: TurnControl,
    error: RunError,
}

impl Unsettled {
    /// Returns why the run did not settle: an [`ErrorKind::Store`] failure
    /// for persistence, else an [`ErrorKind::ToolFailed`] failure for writes
    /// or commands. It downcasts to [`crate::turn::SettlementError`].
    pub fn error(&self) -> &RunError {
        &self.error
    }

    /// Retries the failed cleanup and waits for the run's effects again;
    /// never executes a model, tool, or command.
    ///
    /// # Errors
    /// Returns the cleanup that still fails, which may be retried again.
    pub async fn retry(self) -> Result<(), Self> {
        let result = Run::retry_settlement(&self.control).await;

        result.map_err(|error| Self::new(self.control, error))
    }

    /// Classifies `error` by its settlement phase.
    fn new(control: TurnControl, error: SettlementError) -> Self {
        let kind = match error.phase() {
            SettlementPhase::Persistence => ErrorKind::Store,
            SettlementPhase::Writes | SettlementPhase::Commands => ErrorKind::ToolFailed,
        };

        Self {
            control,
            error: RunError::with_kind(kind, error),
        }
    }
}

impl fmt::Debug for Unsettled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Unsettled")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// Provider-reported tokens, summed over a run's model requests.
///
/// The counts overlap: `cache_read` is part of `input`, and `reasoning` is
/// part of `output`. A count a provider omitted adds zero; a request whose
/// provider reported no input or output count is counted in
/// `unreported_requests`, so totals with unreported requests are lower
/// bounds. Requests the provider rejected as exceeding its context window,
/// and a request in flight when the run was cancelled, are not counted.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Usage {
    /// Input tokens, including `cache_read`.
    pub input: u64,
    /// Output tokens, including `reasoning`.
    pub output: u64,
    /// Input tokens served from the provider's prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the provider's prompt cache, or `None` when
    /// unknown; no supported provider reports them yet.
    pub cache_write: Option<u64>,
    /// Reasoning tokens.
    pub reasoning: u64,
    /// Model requests whose provider reported no input or output count.
    pub unreported_requests: u32,
}

impl Usage {
    /// Sums the usage of every model request in `report`.
    pub(crate) fn from_report(report: &TurnReport) -> Self {
        report
            .model_requests()
            .iter()
            .map(|request| {
                Self::from_request(
                    request
                        .completion()
                        .and_then(|completion| completion.usage()),
                )
            })
            .fold(Self::default(), |total, usage| Self {
                input: total.input.saturating_add(usage.input),
                output: total.output.saturating_add(usage.output),
                cache_read: total.cache_read.saturating_add(usage.cache_read),
                cache_write: None,
                reasoning: total.reasoning.saturating_add(usage.reasoning),
                unreported_requests: total
                    .unreported_requests
                    .saturating_add(usage.unreported_requests),
            })
    }

    /// Converts one request's provider usage, if it reported any.
    pub(crate) fn from_request(usage: Option<&CompletionUsage>) -> Self {
        let count = |tokens: fn(CompletionUsage) -> Option<u64>| usage.copied().and_then(tokens);
        let input = count(CompletionUsage::input_tokens);
        let output = count(CompletionUsage::output_tokens);

        Self {
            input: input.unwrap_or(0),
            output: output.unwrap_or(0),
            cache_read: count(CompletionUsage::cache_hit_tokens).unwrap_or(0),
            cache_write: None,
            reasoning: count(CompletionUsage::reasoning_tokens).unwrap_or(0),
            unreported_requests: u32::from(input.is_none() || output.is_none()),
        }
    }
}

/// Locates the durable records of one run's turn, and records whether the
/// turn answers in text.
pub(crate) struct RunJournal {
    bash: bool,
    session_id: String,
    store: Arc<dyn SessionStore>,
    text: bool,
}

impl RunJournal {
    /// Locates records of `session_id` in `store`; command records are
    /// loaded only when `turn` configured `bash`, since stores without
    /// command storage support no other run.
    pub(crate) fn new(
        store: Arc<dyn SessionStore>,
        session_id: String,
        turn: &Result<(TurnInput, TurnOptions), RunError>,
    ) -> Self {
        let options = turn.as_ref().ok().map(|(_, options)| options);

        Self {
            bash: options.is_some_and(|options| options.bash().is_some()),
            session_id,
            store,
            text: options.is_some_and(|options| options.output() == &OutputMode::Text),
        }
    }

    /// Wraps the validated output of the run's turn: a text turn's output is
    /// a JSON string holding its text.
    fn output(&self, output: Value) -> RunOutput {
        match output {
            Value::String(text) if self.text => RunOutput::Text(text),
            output => RunOutput::Json(output),
        }
    }

    /// Loads the file changes and command records of the turn at
    /// `turn_position`.
    async fn load(
        &self,
        turn_position: i64,
    ) -> Result<(Vec<FileChange>, Vec<CommandRecord>), SessionError> {
        let writes = self.store.load_writes(&self.session_id).await?;
        let files_changed = writes
            .into_iter()
            .filter(|write| write.turn_position == turn_position)
            .map(FileChange::from)
            .collect();
        let mut commands = if self.bash {
            self.store.load_commands(&self.session_id).await?
        } else {
            Vec::new()
        };
        commands.retain(|command| command.owner().turn_position == turn_position);

        Ok((files_changed, commands))
    }
}

/// A one-shot run without an output schema.
#[derive(Debug, Error)]
#[error("run requires an output mode; call `RunRequest::output`")]
struct OutputRequired;

/// A run whose wall-clock limit is zero.
#[derive(Debug, Error)]
#[error("run wall-clock limit must be greater than zero")]
struct ZeroWallTime;
