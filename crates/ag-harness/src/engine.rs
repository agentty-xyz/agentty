use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::bash::{BashArguments, BashError};
use crate::calibration::{Calibration, ContextCalibration};
use crate::context::{ContextSummaryPolicy, FitStep};
use crate::effect::Effects;
use crate::execution::BashTool;
use crate::file_system::FileSystem;
use crate::lifecycle::{
    ContextSummaryErrorType, ContextSummaryTrigger, LifecycleEmitter, LifecycleId, ToolErrorType,
    ToolLifecycle,
};
use crate::model::{
    Model, ModelError, ModelMessage, ModelRequest, ModelResponse, ReasoningEffort,
    ensure_unique_tool_call_ids,
};
use crate::read::{self, ReadError, ReadTool};
use crate::repetition::{self, RepeatedCalls, Repetition};
use crate::repository::Repository;
use crate::reservation::WriteJournal;
use crate::round_summary::RoundSummaryJob;
use crate::tool::{
    MalformedArguments, ReadAction, ReadArguments, Tool, ToolCall, ToolCallArguments,
    ToolDefinition, WriteArguments,
};
use crate::turn::{
    ModelRequestActivity, ModelRequestPurpose, ToolActivity, TurnError, TurnOptions, TurnOutcome,
    TurnReport, sanitize_report_text, sanitized_completion_metadata,
};
use crate::write::{WriteError, WriteTool};
use crate::{compaction, context, schema_contract};

/// Final outputs a turn may have rejected for invalid JSON or a schema
/// violation and re-requested with corrective feedback before failing.
pub(crate) const MAX_OUTPUT_CORRECTIONS: usize = 2;

/// Consecutive tool-call responses in which every call has malformed
/// arguments that fail a turn; each earlier one is journaled and its calls
/// are answered with error results. A response with any decodable call
/// resets the count, so a model that keeps correcting itself is never cut
/// off, while one that cannot form a valid call stops after a bounded number
/// of wasted requests.
pub(crate) const MAX_CONSECUTIVE_MALFORMED_TOOL_RESPONSES: usize = 3;

/// Requests of one turn the provider may reject as exceeding its context
/// window and the harness re-send reduced before the rejection fails the
/// turn. Each rejection lowers the calibration's overflow ceiling, so two
/// recoveries already shrink a request to about 56% of the first rejected
/// one, while a provider whose window cannot hold even the mandatory content
/// stops after a bounded number of wasted requests.
pub(crate) const MAX_CONTEXT_OVERFLOW_RECOVERIES: usize = 2;

/// Shared execution dependencies and the immutable configuration for one turn.
pub(crate) struct Engine<'a> {
    pub(crate) context_budget: context::ContextBudget,
    pub(crate) context_calibration: &'a ContextCalibration,
    pub(crate) context_estimator: &'a dyn context::ContextEstimator,
    pub(crate) context_summaries: ContextSummaryPolicy,
    pub(crate) effects: Effects,
    pub(crate) file_system: &'a Arc<dyn FileSystem>,
    pub(crate) lifecycle: &'a LifecycleEmitter,
    pub(crate) model: &'a Arc<dyn Model>,
    pub(crate) model_reasoning_effort: Option<ReasoningEffort>,
    pub(crate) options: &'a TurnOptions,
    pub(crate) repository: Option<&'a Repository>,
}

impl Engine<'_> {
    /// Runs one turn to its final output. A turn that fails after
    /// preparing its request still reports what it did before failing.
    pub(crate) async fn run(
        &self,
        request: ModelRequest,
        turn_id: Option<LifecycleId>,
        journal: Option<WriteJournal>,
    ) -> Result<(TurnOutcome, Vec<ModelMessage>), EngineFailure> {
        let started_at = Instant::now();
        let recorder = journal.clone();
        let (request, tools) = self
            .prepare_request(request, journal)
            .map_err(EngineFailure::unreported)?;
        let mut state = TurnState::new(ToolRounds::new(tools, recorder));
        match self.drive(request, turn_id, &mut state).await {
            Ok((output, messages)) => {
                let report = state.into_report(started_at.elapsed());

                Ok((TurnOutcome::new(output, report), messages))
            }
            Err(error) => Err(EngineFailure {
                error,
                report: Some(Box::new(state.into_report(started_at.elapsed()))),
            }),
        }
    }

    /// Sends requests and executes tool calls until the model answers with
    /// output, recording what the turn did in `state`.
    async fn drive(
        &self,
        mut request: ModelRequest,
        turn_id: Option<LifecycleId>,
        state: &mut TurnState,
    ) -> Result<(Value, Vec<ModelMessage>), TurnError> {
        let summary_trigger = (self.context_summaries == ContextSummaryPolicy::Enabled)
            .then_some(ContextSummaryTrigger::Proactive);

        loop {
            // Tool traffic grows the request between model calls; a grown
            // request that no longer fits the calibrated budget is sent as a
            // reduced copy, possibly after summarizing earlier rounds, and
            // one that cannot be reduced enough fails typed here instead of
            // overflowing the provider context.
            let (budget, calibration) = self
                .context_calibration
                .effective_budget(self.context_budget);
            let step = state
                .context_fit
                .fit(self.context_estimator, budget, &request, summary_trigger)
                .map_err(|error| {
                    state
                        .overflow
                        .rejection_or(ContextCalibration::declared_failure(
                            self.context_budget,
                            calibration,
                            error,
                        ))
                })?;
            let (fitted, context_activity) = match step {
                FitStep::Send(fitted, context_activity) => (fitted, context_activity),
                FitStep::Summarize(job) => {
                    self.summarize_rounds(
                        &job,
                        (&request, &mut state.context_fit),
                        turn_id,
                        (&mut state.model_request_index, &mut state.model_requests),
                    )
                    .await;

                    continue;
                }
            };
            let completed = self
                .complete_model_request(
                    fitted.unwrap_or(&request),
                    state.model_request_index,
                    turn_id,
                )
                .await;
            let estimated_weight = state.context_fit.sent_weight();
            state
                .overflow
                .retried(self.lifecycle, turn_id, estimated_weight);
            state.model_request_index = state.model_request_index.saturating_add(1);
            let (response, activity, reasoning_content) = match completed {
                // An invalid final output is re-requested with request-only
                // feedback; neither it nor the feedback is journaled, so
                // replayed history keeps only completed exchanges.
                Err(
                    error @ (ModelError::InvalidJson { .. } | ModelError::SchemaViolation { .. }),
                ) if state.output_corrections < MAX_OUTPUT_CORRECTIONS => {
                    state.output_corrections += 1;
                    request.record_feedback(output_feedback(&error));

                    continue;
                }
                // A request the provider rejected as exceeding its context
                // window is refitted under a lower calibrated ceiling and
                // re-sent; the rejection fails the turn only once the
                // recoveries are spent or the refit cannot shrink it.
                Err(error) if error.is_context_overflow() => {
                    state
                        .overflow
                        .reject(self.context_calibration, error, estimated_weight)?;

                    continue;
                }
                completed => completed?,
            };
            state.model_requests.push(self.accepted_request(
                activity.with_context(context_activity),
                estimated_weight,
                calibration,
            ));

            let (calls, batched) = match response {
                ModelResponse::Output(output) => {
                    request.record_output_with_reasoning(&output, reasoning_content);

                    return Ok((output, request.into_messages()));
                }
                ModelResponse::ToolCall(call) => (vec![call], false),
                ModelResponse::ToolCalls(calls) => {
                    if calls.is_empty() {
                        return Err(ModelError::MissingToolCall.into());
                    }
                    ensure_unique_tool_call_ids(&calls)?;

                    (calls, true)
                }
            };
            state.malformed_tool_calls +=
                count_malformed(&calls, &mut state.consecutive_malformed_responses)?;
            let repetition = self
                .execute_tool_calls(
                    &calls,
                    batched,
                    turn_id,
                    &mut request,
                    &state.context_fit,
                    &mut state.rounds,
                )
                .await?;
            state.repetition_nudges += self.act_on_repetition(repetition, &mut request, turn_id)?;
        }
    }

    /// Calibrates later estimates against an answered request of
    /// `estimated_weight` and returns its activity with that estimate and
    /// the token count `calibration` predicted for it.
    fn accepted_request(
        &self,
        activity: ModelRequestActivity,
        estimated_weight: u64,
        calibration: Calibration,
    ) -> ModelRequestActivity {
        self.context_calibration.observe(
            self.context_budget,
            estimated_weight,
            activity.completion(),
        );

        activity.with_estimate(
            estimated_weight,
            calibration
                .envelope()
                .map(|envelope| envelope.tokens(estimated_weight)),
        )
    }

    /// Journals one response's `calls`, then executes them in order and
    /// journals each result, returning the strongest repetition among them.
    async fn execute_tool_calls(
        &self,
        calls: &[ToolCall],
        batched: bool,
        turn_id: Option<LifecycleId>,
        request: &mut ModelRequest,
        context_fit: &context::ContextFit,
        rounds: &mut ToolRounds,
    ) -> Result<Repetition, TurnError> {
        let call_message = match (batched, calls) {
            (false, [call]) => request.record_tool_call(call.clone()),
            _ => request.record_tool_calls(calls.to_vec()),
        };
        Self::persist(rounds.recorder.as_ref(), call_message).await?;
        let mut repetition = Repetition::None;
        for call in calls {
            let repeated = rounds.reads.repeat_of(call, context_fit);
            let (result, activity) = self
                .execute_tool_call(
                    call,
                    &rounds.tools,
                    turn_id,
                    repeated.as_ref().map(|(call_id, _)| call_id.as_str()),
                )
                .await?;
            let result_bytes = result.len();
            // A read answered as a repeat returned the earlier result.
            let digest = repeated
                .as_ref()
                .map_or_else(|| repetition::result_digest(&result), |(_, digest)| *digest);
            Self::persist(
                rounds.recorder.as_ref(),
                request.record_tool_result(call, result),
            )
            .await?;
            let result_index = request.messages().len() - 1;
            rounds.reads.record(
                call,
                result_index,
                (result_bytes, digest),
                repeated.is_some(),
            );
            repetition = repetition.max(rounds.repeated_calls.record(call, digest));
            rounds.activity.push(activity);
        }

        Ok(repetition)
    }

    /// Fails the turn when the latest response's calls reached
    /// [`repetition::REPETITION_STOP_AT`] identical calls, or appends a
    /// request-only nudge, never journaled, when they first reached
    /// [`repetition::REPETITION_NUDGE_AT`]; returns the nudges added.
    fn act_on_repetition(
        &self,
        repetition: Repetition,
        request: &mut ModelRequest,
        turn_id: Option<LifecycleId>,
    ) -> Result<usize, TurnError> {
        let (repeats, tool, stopped) = match repetition {
            Repetition::None => return Ok(0),
            Repetition::Nudge { repeats, tool } => (repeats, tool, false),
            Repetition::Stop { repeats, tool } => (repeats, tool, true),
        };
        self.lifecycle
            .repeated_tool_calls(tool, repeats, stopped, turn_id);
        if stopped {
            return Err(TurnError::RepeatedToolCalls {
                repeats,
                tool: tool.to_string(),
            });
        }
        request.record_feedback(repetition::repetition_feedback(tool, repeats));

        Ok(1)
    }

    /// Requests the summary `job` describes and installs it in
    /// `context_fit`, or records its failure so the turn falls back to
    /// removing the rounds behind a deterministic note. A cancelled turn
    /// drops this future, which reports the summary as cancelled.
    async fn summarize_rounds(
        &self,
        job: &RoundSummaryJob,
        (request, context_fit): (&ModelRequest, &mut context::ContextFit),
        turn_id: Option<LifecycleId>,
        requests: (&mut u64, &mut Vec<ModelRequestActivity>),
    ) {
        let lifecycle = self.lifecycle.start_context_summary(job.trigger(), turn_id);
        let installed = match self.request_summary(job, turn_id, requests).await {
            Ok(summary) => context_fit
                .install_summary(self.context_estimator, request, job, summary)
                .ok_or(ContextSummaryErrorType::Rejected),
            Err(error_type) => Err(error_type),
        };
        match installed {
            Ok(summarized_tool_calls) => {
                if let Some(lifecycle) = lifecycle {
                    lifecycle.completed(summarized_tool_calls);
                }
            }
            Err(error_type) => {
                context_fit.summary_failed();
                if let Some(lifecycle) = lifecycle {
                    lifecycle.failed(error_type);
                }
            }
        }
    }

    /// Sends the tool-free summary request at low reasoning effort,
    /// re-requesting an invalid output with corrective feedback at most
    /// [`MAX_OUTPUT_CORRECTIONS`] times, and returns a summary that satisfies
    /// the summary schema, fits the job's size limit, and is smaller than its
    /// source. Every answered request is reported with
    /// [`ModelRequestPurpose::ContextSummary`] and calibrates later estimates.
    async fn request_summary(
        &self,
        job: &RoundSummaryJob,
        turn_id: Option<LifecycleId>,
        (model_request_index, model_requests): (&mut u64, &mut Vec<ModelRequestActivity>),
    ) -> Result<Value, ContextSummaryErrorType> {
        let mut request = job.request().clone();
        if self.lifecycle.is_enabled() {
            request.mark_lifecycle_observed();
        }
        // Summarizing restates what the rounds hold rather than reasoning
        // about them; a reasoning model at the turn's effort can outlast the
        // request timeout on a large source.
        request = request.with_model_reasoning_effort(ReasoningEffort::Low);
        let mut corrections = 0;
        loop {
            let (_, calibration) = self
                .context_calibration
                .effective_budget(self.context_budget);
            let estimated_weight = request.messages().iter().fold(0_u64, |weight, message| {
                weight.saturating_add(self.context_estimator.message_weight(message))
            });
            let completed = self
                .complete_model_request(&request, *model_request_index, turn_id)
                .await;
            *model_request_index = model_request_index.saturating_add(1);
            let (response, activity, _) = match completed {
                Ok(completed) => completed,
                Err(
                    error @ (ModelError::InvalidJson { .. } | ModelError::SchemaViolation { .. }),
                ) => {
                    if corrections == MAX_OUTPUT_CORRECTIONS {
                        return Err(ContextSummaryErrorType::InvalidOutput);
                    }
                    corrections += 1;
                    request.record_feedback(output_feedback(&error));

                    continue;
                }
                Err(_) => return Err(ContextSummaryErrorType::Model),
            };
            model_requests.push(self.accepted_request(
                activity.with_purpose(ModelRequestPurpose::ContextSummary),
                estimated_weight,
                calibration,
            ));
            let ModelResponse::Output(summary) = response else {
                return Err(ContextSummaryErrorType::InvalidOutput);
            };

            return compaction::accept_summary(
                &summary,
                job.max_summary_bytes(),
                job.source_bytes(),
            )
            .map(|_| summary)
            .map_err(|_| ContextSummaryErrorType::Rejected);
        }
    }

    /// Appends one finished model response or tool result to a durable
    /// turn before the loop continues, so a stopped turn keeps it.
    async fn persist(
        recorder: Option<&WriteJournal>,
        message: &ModelMessage,
    ) -> Result<(), TurnError> {
        let Some(recorder) = recorder else {
            return Ok(());
        };

        recorder
            .append_messages(std::slice::from_ref(message))
            .await
            .map_err(|source| TurnError::MessageJournal {
                source: Box::new(source),
            })
    }

    fn prepare_request(
        &self,
        mut request: ModelRequest,
        journal: Option<WriteJournal>,
    ) -> Result<(ModelRequest, Tools), TurnError> {
        if self.lifecycle.is_enabled() {
            request.mark_lifecycle_observed();
        }
        if request.model_reasoning_effort().is_none()
            && let Some(reasoning_effort) = self.model_reasoning_effort
        {
            request = request.with_model_reasoning_effort(reasoning_effort);
        }
        if let Some(base) = self.options.comparison_base()
            && !self
                .repository
                .is_some_and(|repository| base.matches_repository(repository))
        {
            return Err(TurnError::ComparisonRepositoryMismatch);
        }
        let read_allowed = self.options.tool_policy().allows(Tool::Read);
        let write_allowed = self.options.tool_policy().allows(Tool::Write);
        let bash_allowed = self.options.tool_policy().allows(Tool::Bash);
        if !read_allowed && !write_allowed && !bash_allowed {
            return Ok((request, Tools::default()));
        }
        let repository = self
            .repository
            .as_ref()
            .ok_or(TurnError::RepositoryRequired)?;
        for tool in context::advertised_tools(self.options) {
            request = request.with_tool(tool);
        }
        let read_tool = read_allowed.then(|| {
            ReadTool::with_git(
                Arc::clone(self.file_system),
                repository.root().to_path_buf(),
                repository.git_executable().to_path_buf(),
                self.options.comparison_base().cloned(),
            )
        });
        let write_tool = write_allowed.then(|| {
            let mut tool = WriteTool::new(
                Arc::clone(self.file_system),
                repository.root().to_path_buf(),
            );
            tool.journal.clone_from(&journal);
            tool.effects = self.effects.clone();

            tool
        });

        let bash_tool = if bash_allowed {
            Some(BashTool::new(
                self.options
                    .bash()
                    .cloned()
                    .ok_or(BashError::InvalidPolicy)?,
                repository.root().to_path_buf(),
                self.effects.commands().clone(),
                journal,
            )?)
        } else {
            None
        };

        Ok((
            request,
            Tools {
                read: read_tool,
                write: write_tool,
                bash: bash_tool,
            },
        ))
    }

    async fn complete_model_request(
        &self,
        request: &ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<(ModelResponse, ModelRequestActivity, Option<String>), ModelError> {
        let started_at = Instant::now();
        let model_lifecycle =
            self.lifecycle
                .start_model_request(self.model.metadata(), model_request_index, turn_id);
        let operation = self.model.complete(request.clone());
        let completion = match model_lifecycle.as_ref() {
            Some(model_lifecycle) => model_lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (response, completion, reasoning_content) = match completion {
            Ok(completion) => completion.into_parts(),
            Err(error) => {
                if let Some(model_lifecycle) = model_lifecycle {
                    model_lifecycle.failed(error.error_type(), error.http_status());
                }

                return Err(error);
            }
        };
        if let Some(output) = response.output()
            && let Err(error) = request.schema().validate_value(output)
        {
            let error = ModelError::from(error);
            if let Some(model_lifecycle) = model_lifecycle {
                model_lifecycle.failed(error.error_type(), error.http_status());
            }

            return Err(error);
        }
        let response_type = response.response_type();
        let activity = ModelRequestActivity::new(
            completion.as_ref().map(sanitized_completion_metadata),
            started_at.elapsed(),
            response_type,
        );
        if let Some(model_lifecycle) = model_lifecycle {
            model_lifecycle.completed(completion, response_type);
        }

        Ok((response, activity, reasoning_content))
    }

    /// Runs `call` under the turn's tool policy, or, when `repeats` names
    /// the earlier call an allowed `read` repeats, answers it without running.
    async fn execute_tool_call(
        &self,
        call: &ToolCall,
        tools: &Tools,
        turn_id: Option<LifecycleId>,
        repeats: Option<&str>,
    ) -> Result<(String, ToolActivity), TurnError> {
        let mut tool_lifecycle =
            self.lifecycle
                .request_tool(call.id().to_string(), call.name().to_string(), turn_id);
        let execution = match call.arguments() {
            ToolCallArguments::Bash(arguments) => tools
                .bash
                .as_ref()
                .map(|tool| ToolExecution::Bash(tool, arguments)),
            ToolCallArguments::Read(arguments) => tools.read.as_ref().map(|tool| match repeats {
                Some(earlier) => ToolExecution::RepeatedRead(arguments, earlier),
                None => ToolExecution::Read(tool, arguments),
            }),
            ToolCallArguments::Write(arguments) => tools
                .write
                .as_ref()
                .map(|tool| ToolExecution::Write(tool, arguments)),
            ToolCallArguments::Malformed(arguments) => tools
                .allows(arguments.tool())
                .then_some(ToolExecution::Malformed(arguments, self.options)),
        };
        let Some(execution) = execution else {
            if let Some(tool_lifecycle) = tool_lifecycle {
                tool_lifecycle.denied();
            }

            return Err(TurnError::ToolDenied {
                name: call.name().to_string(),
            });
        };
        if let Some(tool_lifecycle) = tool_lifecycle.as_mut() {
            tool_lifecycle.started();
        }
        let operation = execute_tool(execution, call.id());
        let result = match tool_lifecycle.as_ref() {
            Some(tool_lifecycle) => tool_lifecycle.scope(operation).await,
            None => operation.await,
        };

        Self::finish_tool_call(result, tool_lifecycle)
    }

    fn finish_tool_call(
        result: Result<(String, ToolActivity), TurnError>,
        tool_lifecycle: Option<ToolLifecycle>,
    ) -> Result<(String, ToolActivity), TurnError> {
        match result {
            Ok(result) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    if matches!(
                        &result.1,
                        ToolActivity::MalformedArguments { .. }
                            | ToolActivity::ReadInspectionRejected { .. }
                            | ToolActivity::ReadRejected { .. }
                            | ToolActivity::WriteRejected { .. }
                    ) {
                        tool_lifecycle.failed(ToolErrorType::Execution);
                    } else {
                        tool_lifecycle.completed();
                    }
                }

                Ok(result)
            }
            Err(error) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    tool_lifecycle.failed(ToolErrorType::Execution);
                }

                Err(error)
            }
        }
    }
}

/// A failed turn's error with the report of what it did before failing,
/// or `None` when it failed before preparing its request.
pub(crate) struct EngineFailure {
    pub(crate) error: TurnError,
    pub(crate) report: Option<Box<TurnReport>>,
}

impl EngineFailure {
    fn unreported(error: TurnError) -> Self {
        Self {
            error,
            report: None,
        }
    }
}

/// Everything one turn did so far; its report is built from this whether
/// the turn finishes or fails.
struct TurnState {
    rounds: ToolRounds,
    context_fit: context::ContextFit,
    model_request_index: u64,
    model_requests: Vec<ModelRequestActivity>,
    output_corrections: usize,
    consecutive_malformed_responses: usize,
    malformed_tool_calls: usize,
    repetition_nudges: usize,
    overflow: OverflowRecovery,
}

impl TurnState {
    fn new(rounds: ToolRounds) -> Self {
        Self {
            rounds,
            context_fit: context::ContextFit::default(),
            model_request_index: 0,
            model_requests: Vec::new(),
            output_corrections: 0,
            consecutive_malformed_responses: 0,
            malformed_tool_calls: 0,
            repetition_nudges: 0,
            overflow: OverflowRecovery::default(),
        }
    }

    /// Reports the turn's activity over its elapsed `duration`.
    fn into_report(self, duration: Duration) -> TurnReport {
        TurnReport::new(duration, self.model_requests, self.rounds.activity)
            .with_corrections(self.output_corrections, self.malformed_tool_calls)
            .with_repetition_nudges(self.repetition_nudges)
            .with_context_overflow_recoveries(self.overflow.recoveries)
            .with_context_summary_failures(self.context_fit.summary_failures())
    }
}

/// The turn's tools and journal, and what its tool rounds did so far:
/// the reads a repeat may reuse, the window of recent identical calls, and
/// the activity the report lists.
struct ToolRounds {
    tools: Tools,
    recorder: Option<WriteJournal>,
    reads: RepeatedReads,
    repeated_calls: RepeatedCalls,
    activity: Vec<ToolActivity>,
}

impl ToolRounds {
    fn new(tools: Tools, recorder: Option<WriteJournal>) -> Self {
        Self {
            tools,
            recorder,
            reads: RepeatedReads::default(),
            repeated_calls: RepeatedCalls::default(),
            activity: Vec::new(),
        }
    }
}

/// Context-window rejections of one turn: how many the turn re-sent
/// reduced, and the latest one while its reduced re-send is pending. While
/// a rejection is pending, the next fit refits the rejected request under
/// the lowered ceiling; a stronger reduction for that refit, such as a
/// forced summary of earlier rounds, belongs there.
#[derive(Default)]
struct OverflowRecovery {
    pending: Option<(ModelError, u64)>,
    recoveries: usize,
}

impl OverflowRecovery {
    /// Lowers `calibration`'s overflow ceiling below the provider's rejected
    /// request of `rejected_weight`, without reserved output, and holds the
    /// rejection until the refit re-sends it reduced; returns the rejection
    /// instead once the turn spent [`MAX_CONTEXT_OVERFLOW_RECOVERIES`].
    fn reject(
        &mut self,
        calibration: &ContextCalibration,
        error: ModelError,
        rejected_weight: u64,
    ) -> Result<(), ModelError> {
        if self.recoveries == MAX_CONTEXT_OVERFLOW_RECOVERIES {
            return Err(error);
        }
        calibration.observe_overflow(rejected_weight);
        self.recoveries += 1;
        self.pending = Some((error, rejected_weight));

        Ok(())
    }

    /// Returns the pending rejection in place of the refit's `error`: the
    /// rejected request could not shrink below the lowered ceiling, so the
    /// turn fails with the provider's verdict. Returns any other failure
    /// unchanged.
    fn rejection_or(&mut self, error: TurnError) -> TurnError {
        self.pending
            .take()
            .map_or(error, |(rejection, _)| rejection.into())
    }

    /// Reports that the pending rejection was re-sent as a reduced request
    /// of `retry_weight`, without reserved output.
    fn retried(
        &mut self,
        lifecycle: &LifecycleEmitter,
        turn_id: Option<LifecycleId>,
        retry_weight: u64,
    ) {
        if let Some((_, rejected_weight)) = self.pending.take() {
            lifecycle.context_overflow_retried(
                u64::try_from(self.recoveries).unwrap_or(u64::MAX),
                rejected_weight,
                retry_weight,
                turn_id,
            );
        }
    }
}

/// Executed `read` calls of the current turn, keyed by their canonical
/// arguments, so a repeat may be answered without running it while the
/// model still sees the earlier result whole and nothing changed since.
#[derive(Default)]
struct RepeatedReads {
    /// `write` and `bash` calls handled so far in the turn.
    effects: usize,
    executed: HashMap<String, ExecutedRead>,
}

impl RepeatedReads {
    /// Returns the identifier of the earlier call of this turn that the
    /// `read` `call` repeats when no `write` or `bash` call was handled since,
    /// the last request sent that call's result whole, and the result is
    /// larger than the note that would point to it; `None` otherwise,
    /// including for any other tool.
    fn repeat_of(
        &self,
        call: &ToolCall,
        context_fit: &context::ContextFit,
    ) -> Option<(String, u64)> {
        let executed = self.executed.get(&Self::key(call)?)?;

        (executed.effects == self.effects
            && context_fit.sends_whole(executed.result_index)
            && context::repeated_read_result(&executed.call_id).len() < executed.result_bytes)
            .then(|| (executed.call_id.clone(), executed.result_digest))
    }

    /// Remembers a handled call whose result of `result_bytes` with digest
    /// `result_digest` is the request message at `result_index`: an
    /// executed `read` as the latest
    /// for its arguments, and any `write` or `bash` call as a possible
    /// change. A `read` answered as a repeat and a malformed call change
    /// nothing.
    fn record(
        &mut self,
        call: &ToolCall,
        result_index: usize,
        (result_bytes, result_digest): (usize, u64),
        repeated: bool,
    ) {
        match Self::key(call) {
            Some(_) if repeated => {}
            Some(key) => {
                self.executed.insert(
                    key,
                    ExecutedRead {
                        call_id: call.id().to_string(),
                        effects: self.effects,
                        result_bytes,
                        result_digest,
                        result_index,
                    },
                );
            }
            None if call.malformed_arguments().is_none() => self.effects += 1,
            None => {}
        }
    }

    /// Returns a `read` call's canonical arguments; `None` for other tools.
    fn key(call: &ToolCall) -> Option<String> {
        call.read_arguments()?;

        call.arguments_json().ok()
    }
}

/// One executed `read` call of the current turn.
struct ExecutedRead {
    call_id: String,
    /// `write` and `bash` calls handled before it.
    effects: usize,
    result_bytes: usize,
    /// Digest of the result, which a repeat answered without running
    /// returns again.
    result_digest: u64,
    result_index: usize,
}

#[derive(Default)]
struct Tools {
    bash: Option<BashTool>,
    read: Option<ReadTool>,
    write: Option<WriteTool>,
}

impl Tools {
    /// Whether the turn's tool policy enabled `tool`.
    fn allows(&self, tool: Tool) -> bool {
        match tool {
            Tool::Bash => self.bash.is_some(),
            Tool::Read => self.read.is_some(),
            Tool::Write => self.write.is_some(),
        }
    }
}

enum ToolExecution<'a> {
    Bash(&'a BashTool, &'a BashArguments),
    Read(&'a ReadTool, &'a ReadArguments),
    /// A call whose arguments its tool could not decode, answered with an
    /// error result that lists the tool's argument fields.
    Malformed(&'a MalformedArguments, &'a TurnOptions),
    /// A `read` repeating the named earlier call, answered without running.
    RepeatedRead(&'a ReadArguments, &'a str),
    Write(&'a WriteTool, &'a WriteArguments),
}

async fn execute_tool(
    execution: ToolExecution<'_>,
    call_id: &str,
) -> Result<(String, ToolActivity), TurnError> {
    let started_at = Instant::now();

    match execution {
        ToolExecution::Bash(tool, arguments) => {
            let outcome = tool.execute(arguments, call_id).await?;
            let result = serde_json::to_string(&outcome).map_err(|_| BashError::Execution)?;

            Ok((
                result,
                ToolActivity::Bash {
                    duration: started_at.elapsed(),
                },
            ))
        }
        ToolExecution::Read(read_tool, arguments) => {
            execute_read_tool(read_tool, arguments, started_at).await
        }
        ToolExecution::Malformed(arguments, options) => Ok((
            malformed_arguments_result(arguments, options),
            ToolActivity::MalformedArguments {
                duration: started_at.elapsed(),
                reason: sanitize_report_text(arguments.reason()),
                tool: arguments.tool(),
            },
        )),
        ToolExecution::RepeatedRead(arguments, earlier) => Ok((
            context::repeated_read_result(earlier),
            ToolActivity::ReadRepeated {
                action: arguments.action(),
                duration: started_at.elapsed(),
                summary: sanitize_report_text(read_summary(arguments)),
            },
        )),
        ToolExecution::Write(write_tool, arguments) => {
            execute_write_tool(write_tool, arguments, started_at, call_id).await
        }
    }
}

async fn execute_read_tool(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    if let Some(error) = arguments.validation_error() {
        return reject_invalid_read_arguments(arguments, error, started_at);
    }
    if arguments.action() == ReadAction::File {
        return execute_file_read(read_tool, arguments, started_at).await;
    }

    execute_repository_inspection(read_tool, arguments, started_at).await
}

fn reject_invalid_read_arguments(
    arguments: &ReadArguments,
    error: &str,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let summary = read_summary(arguments);
    let result = read::invalid_arguments_tool_result(error, summary).map_err(ReadError::from)?;
    let activity = if arguments.action() == ReadAction::File {
        ToolActivity::ReadRejected {
            duration: started_at.elapsed(),
            path: sanitize_report_text(summary),
        }
    } else {
        ToolActivity::ReadInspectionRejected {
            action: arguments.action(),
            duration: started_at.elapsed(),
            summary: sanitize_report_text(summary),
        }
    };

    Ok((result, activity))
}

async fn execute_file_read(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    match read_tool.execute(arguments).await {
        Ok(output) => match output.to_tool_result() {
            Ok(result) => Ok((
                result,
                ToolActivity::Read {
                    duration: started_at.elapsed(),
                    end_line: output.end_line(),
                    path: sanitize_report_text(output.path()),
                    start_line: output.start_line(),
                    truncated: output.truncated(),
                },
            )),
            Err(error) => error
                .to_tool_result(output.path())
                .map_err(ReadError::from)
                .map_err(TurnError::from)
                .map(|result| {
                    (
                        result,
                        ToolActivity::ReadRejected {
                            duration: started_at.elapsed(),
                            path: sanitize_report_text(output.path()),
                        },
                    )
                }),
        },
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(arguments.path())
            .map_err(ReadError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::ReadRejected {
                        duration: started_at.elapsed(),
                        path: sanitize_report_text(arguments.path()),
                    },
                )
            }),
        Err(error) => Err(error.into()),
    }
}

async fn execute_repository_inspection(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let fallback_summary = read_summary(arguments);
    match read_tool.execute_inspection(arguments).await {
        Ok((result, summary)) => Ok((
            result,
            ToolActivity::ReadInspection {
                action: arguments.action(),
                duration: started_at.elapsed(),
                summary: sanitize_report_text(&summary),
            },
        )),
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(fallback_summary)
            .map_err(ReadError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::ReadInspectionRejected {
                        action: arguments.action(),
                        duration: started_at.elapsed(),
                        summary: sanitize_report_text(fallback_summary),
                    },
                )
            }),
        Err(error) => Err(error.into_read_error(fallback_summary.to_string()).into()),
    }
}

async fn execute_write_tool(
    write_tool: &WriteTool,
    arguments: &WriteArguments,
    started_at: Instant,
    call_id: &str,
) -> Result<(String, ToolActivity), TurnError> {
    match write_tool.execute(arguments, call_id).await {
        Ok(output) => {
            let activity = ToolActivity::Write {
                bytes_written: output.bytes_written(),
                duration: started_at.elapsed(),
                path: sanitize_report_text(output.path()),
            };
            let result = output
                .to_tool_result()
                .map_err(WriteError::from)
                .map_err(TurnError::from)?;

            Ok((result, activity))
        }
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(arguments.path())
            .map_err(WriteError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::WriteRejected {
                        duration: started_at.elapsed(),
                        path: sanitize_report_text(arguments.path()),
                    },
                )
            }),
        Err(error) => Err(error.into()),
    }
}

/// Counts the malformed calls of one response and the run of responses
/// made only of malformed calls, and fails the turn when that run reaches
/// [`MAX_CONSECUTIVE_MALFORMED_TOOL_RESPONSES`]. A malformed call is
/// journaled and answered with an error result like any other call; only a
/// model that sends nothing else for several responses in a row fails the
/// turn, unjournaled.
fn count_malformed(calls: &[ToolCall], consecutive: &mut usize) -> Result<usize, TurnError> {
    let malformed = calls
        .iter()
        .filter_map(ToolCall::malformed_arguments)
        .collect::<Vec<_>>();
    if malformed.len() < calls.len() {
        *consecutive = 0;
    } else {
        *consecutive += 1;
        if *consecutive == MAX_CONSECUTIVE_MALFORMED_TOOL_RESPONSES {
            return Err(ModelError::InvalidToolArguments {
                reason: schema_contract::bounded_diagnostic(format!(
                    "`{}`: {}",
                    malformed[0].tool().name(),
                    malformed[0].reason()
                )),
            }
            .into());
        }
    }

    Ok(malformed.len())
}

/// Returns the bounded report summary of a `read`: its path, else its
/// query, else the tool name.
fn read_summary(arguments: &ReadArguments) -> &str {
    arguments
        .path_filter()
        .or_else(|| arguments.query())
        .unwrap_or("read")
}

/// Renders the bounded feedback that re-requests a rejected final output.
fn output_feedback(error: &ModelError) -> String {
    format!(
        "[harness] Your previous final response was rejected ({}). Respond with only a single \
         JSON value that matches the output schema, with no prose or Markdown around it.",
        schema_contract::bounded_diagnostic(error)
    )
}

/// Renders the error result that answers a call with malformed arguments,
/// naming the decoding failure and every argument field of the called tool.
fn malformed_arguments_result(arguments: &MalformedArguments, options: &TurnOptions) -> String {
    let name = arguments.tool().name();
    let fields = context::advertised_tools(options)
        .into_iter()
        .find(|tool| tool.name() == name)
        .map_or_else(String::new, |tool| argument_fields(&tool));

    json!({
        "error": schema_contract::bounded_diagnostic(format!(
            "failed to parse function arguments: {}",
            arguments.reason()
        )),
        "hint": format!(
            "This call did not run. Call `{name}` again with valid JSON arguments using only its \
             documented fields: {fields}."
        ),
        "status": "rejected",
    })
    .to_string()
}

/// Lists a tool's argument fields, marking the required ones.
fn argument_fields(tool: &ToolDefinition) -> String {
    let parameters = tool.parameters();
    let required = parameters["required"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut names: Vec<&String> = parameters["properties"]
        .as_object()
        .into_iter()
        .flat_map(|properties| properties.keys())
        .collect();
    // Sorted so the result does not depend on map ordering.
    names.sort();

    names
        .into_iter()
        .map(|field| {
            if required.iter().any(|name| name == field.as_str()) {
                format!("`{field}` (required)")
            } else {
                format!("`{field}`")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "engine_test.rs"]
mod tests;
