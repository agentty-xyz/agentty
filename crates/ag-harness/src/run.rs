//! Run-level API: a [`RunRequest`] starts a cancellable [`Run`] that resolves
//! to a [`RunOutcome`] with its output, aggregated [`Usage`], and the durable
//! write and command records the run left.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

use crate::bash::{BashConfig, CommandRecord};
use crate::cancellation::{ControlledTurn, SettlementError, TurnControl, TurnRecord};
use crate::comparison::ComparisonBase;
use crate::error::{ErrorKind, RunError};
use crate::input::TurnInput;
use crate::policy::ToolPolicy;
use crate::schema_contract::OutputSchema;
use crate::session::SessionError;
use crate::store::SessionStore;
use crate::turn::{TurnOptions, TurnReport};
use crate::write_journal::WriteRecord;

/// Input and per-run options of one [`Run`], started with
/// [`crate::Session::run`] or [`crate::Harness::run`].
///
/// Every option applies to this run only and never changes a session's
/// stored defaults. An unset option keeps the default: the session's stored
/// output schema and the harness's default tool policy.
#[derive(Clone)]
#[must_use = "a run request does nothing until a run is started with it"]
#[non_exhaustive]
pub struct RunRequest {
    bash: Option<BashConfig>,
    comparison_base: Option<ComparisonBase>,
    input: TurnInput,
    output: Option<OutputSchema>,
    tool_policy: Option<ToolPolicy>,
}

impl RunRequest {
    /// Starts a request for `input`; plain strings are text input.
    pub fn new(input: impl Into<TurnInput>) -> Self {
        Self {
            bash: None,
            comparison_base: None,
            input: input.into(),
            output: None,
            tool_policy: None,
        }
    }

    /// Validates the run's final output against `schema` instead of the
    /// session's stored schema. Required by [`crate::Harness::run`].
    pub fn output(mut self, schema: OutputSchema) -> Self {
        self.output = Some(schema);

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

    /// Resolves the input and turn options, using `default_schema` and
    /// `default_policy` for unset options.
    ///
    /// # Errors
    /// Returns a [`ErrorKind::Config`] failure when neither the request nor
    /// the caller supplies an output schema.
    pub(crate) fn into_turn(
        self,
        default_schema: Option<&OutputSchema>,
        default_policy: ToolPolicy,
    ) -> Result<(TurnInput, TurnOptions), RunError> {
        let schema = self
            .output
            .or_else(|| default_schema.cloned())
            .ok_or_else(|| RunError::with_kind(ErrorKind::Config, OutputRequired))?;
        let mut options = TurnOptions::new(schema, self.tool_policy.unwrap_or(default_policy));
        if let Some(configuration) = self.bash {
            options = options.with_bash(configuration);
        }
        if let Some(base) = self.comparison_base {
            options = options.with_comparison_base(base);
        }

        Ok((self.input, options))
    }
}

/// One started run of a [`RunRequest`].
///
/// Await it, or its [`Self::into_handle`], for the [`RunOutcome`]; nothing
/// executes until then. Dropping it, or cancelling its [`CancelToken`],
/// cancels the run. The outcome resolves only after the run's persistence,
/// writes, and commands settle, so the host never needs
/// [`TurnControl::retry_settlement`].
#[must_use = "runs do not start until awaited"]
pub struct Run {
    cancel: CancelToken,
    future: Pin<Box<dyn Future<Output = RunOutcome> + Send>>,
}

impl Run {
    /// Returns a token that cancels this run from any task.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Returns a handle that cancels the run or awaits its outcome.
    pub fn into_handle(self) -> RunHandle {
        RunHandle { run: self }
    }

    /// Wraps a controlled turn whose durable records `journal` locates.
    pub(crate) fn new(turn: ControlledTurn<'static, RunError>, journal: RunJournal) -> Self {
        let control = turn.control();
        let cancel = CancelToken {
            control: control.clone(),
        };
        let future = Box::pin(async move {
            let started_at = Instant::now();
            let result = turn.await;
            let settlement = Self::settle(&control).await;
            let TurnRecord {
                failure_report,
                turn_position,
            } = control.take_record();
            let (mut result, report) = match result {
                Ok(outcome) => {
                    let (output, report) = outcome.into_parts();

                    (Ok(RunOutput::Json(output)), report)
                }
                Err(error) => {
                    let report = failure_report.unwrap_or_else(|| {
                        TurnReport::new(started_at.elapsed(), Vec::new(), Vec::new())
                    });

                    (Err(error), report)
                }
            };
            if let Err(error) = settlement {
                result = Err(RunError::with_kind(ErrorKind::Store, error));
            }
            let (files_changed, commands) = match turn_position {
                Some(turn_position) => match journal.load(turn_position).await {
                    Ok(records) => records,
                    Err(error) => {
                        // A run that already failed keeps its own error.
                        result = result.and(Err(RunError::from(error)));

                        (Vec::new(), Vec::new())
                    }
                },
                None => (Vec::new(), Vec::new()),
            };

            RunOutcome {
                commands,
                files_changed,
                result,
                usage: Usage::from_report(&report),
                report,
            }
        });

        Self { cancel, future }
    }

    /// Waits until the turn's effects settle, retrying failed cleanup once.
    async fn settle(control: &TurnControl) -> Result<(), SettlementError> {
        if control.settled().await.is_ok() {
            return Ok(());
        }
        control.retry_settlement().await?;

        control.settled().await
    }
}

impl IntoFuture for Run {
    type IntoFuture = Pin<Box<dyn Future<Output = RunOutcome> + Send>>;
    type Output = RunOutcome;

    fn into_future(self) -> Self::IntoFuture {
        self.future
    }
}

/// Cancels a run, or awaits its outcome, from the task that owns it.
///
/// Dropping the handle, or the future returned by [`Self::outcome`], before
/// the outcome resolves cancels the run.
#[must_use = "runs do not start until their outcome is awaited"]
pub struct RunHandle {
    run: Run,
}

impl RunHandle {
    /// Requests cancellation, like [`CancelToken::cancel`].
    pub fn cancel(&self) {
        self.run.cancel.cancel();
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
    /// Durable write records the run's `write` tool calls left, with each
    /// write's status: applied, failed, or never acknowledged.
    pub files_changed: Vec<WriteRecord>,
    /// Content-free model and tool activity. A run that failed before its
    /// first model request, or was cancelled, reports none.
    pub report: TurnReport,
    /// The validated output, or why the run failed. A failure to settle
    /// the run's effects, even after one retry, replaces the run's own
    /// result with an [`ErrorKind::Store`] failure, since the session admits
    /// no new run until they settle.
    pub result: Result<RunOutput, RunError>,
    /// Tokens of every model request in [`Self::report`].
    pub usage: Usage,
}

/// Validated final output of a successful run.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RunOutput {
    /// JSON output validated against the run's output schema.
    Json(Value),
    /// Plain text output.
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

/// Provider-reported tokens, summed over a run's model requests.
///
/// The counts overlap: `cache_read` is part of `input`, and `reasoning` is
/// part of `output`. A request whose provider reported no usage, or no
/// count of one kind, adds zero for it. Requests the provider rejected as
/// exceeding its context window, and an in-flight request a cancellation
/// stopped, are not counted.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct Usage {
    /// Input tokens, including `cache_read`.
    pub input: u64,
    /// Output tokens, including `reasoning`.
    pub output: u64,
    /// Input tokens served from the provider's prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the provider's prompt cache; zero while no
    /// supported provider reports them.
    pub cache_write: u64,
    /// Reasoning tokens.
    pub reasoning: u64,
}

impl Usage {
    /// Sums the usage of every model request in `report`.
    fn from_report(report: &TurnReport) -> Self {
        report
            .model_requests()
            .iter()
            .filter_map(|request| request.completion()?.usage().copied())
            .fold(Self::default(), |total, usage| Self {
                input: total
                    .input
                    .saturating_add(usage.input_tokens().unwrap_or(0)),
                output: total
                    .output
                    .saturating_add(usage.output_tokens().unwrap_or(0)),
                cache_read: total
                    .cache_read
                    .saturating_add(usage.cache_hit_tokens().unwrap_or(0)),
                cache_write: total.cache_write,
                reasoning: total
                    .reasoning
                    .saturating_add(usage.reasoning_tokens().unwrap_or(0)),
            })
    }
}

/// Locates the durable records of one run's turn.
pub(crate) struct RunJournal {
    bash: bool,
    session_id: String,
    store: Arc<dyn SessionStore>,
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
        Self {
            bash: turn
                .as_ref()
                .is_ok_and(|(_, options)| options.bash().is_some()),
            session_id,
            store,
        }
    }

    /// Loads the write and command records of the turn at `turn_position`.
    async fn load(
        &self,
        turn_position: i64,
    ) -> Result<(Vec<WriteRecord>, Vec<CommandRecord>), SessionError> {
        let mut writes = self.store.load_writes(&self.session_id).await?;
        writes.retain(|write| write.turn_position == turn_position);
        let mut commands = if self.bash {
            self.store.load_commands(&self.session_id).await?
        } else {
            Vec::new()
        };
        commands.retain(|command| command.owner().turn_position == turn_position);

        Ok((writes, commands))
    }
}

/// A one-shot run without an output schema.
#[derive(Debug, Error)]
#[error("run requires an output schema; call `RunRequest::output`")]
struct OutputRequired;
