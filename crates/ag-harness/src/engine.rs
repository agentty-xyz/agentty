//! Turn engine that drives model requests and tool calls to a final output,
//! with context fitting, summaries, and overflow recovery.

use std::collections::HashMap;
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::time::{self, timeout_at};

use crate::bash::BashError;
use crate::budget::{BudgetLedger, Estimate, Reservation};
use crate::builtin_tool::{BashHandler, ReadHandler, WriteHandler, read_summary};
use crate::calibration::{Calibration, ContextCalibration};
use crate::cancellation::TurnControl;
use crate::catalog::{ModelId, Pricing};
use crate::context::{ContextEstimator, ContextSummaryPolicy, FitStep, SentHistory};
use crate::effect::Effects;
use crate::event::{
    self, EventSink, Item, ItemId, ItemKind, RunEventKind, SessionId, ToolCallItem, ToolCallStatus,
};
use crate::execution::BashTool;
use crate::file_system::FileSystem;
use crate::lifecycle::{
    ContextSummaryErrorType, ContextSummaryTrigger, LifecycleEmitter, LifecycleId,
    ModelResponseType, ToolErrorType, ToolLifecycle, ToolUnavailableReason,
};
use crate::limit::LimitKind;
use crate::model::{
    CompletionMetadata, Model, ModelError, ModelMessage, ModelRequest, ModelResponse,
    ReasoningEffort, ensure_unique_tool_call_ids,
};
use crate::read::ReadTool;
use crate::repetition::{self, RepeatedCalls, Repetition};
use crate::repository::Repository;
use crate::reservation::WriteJournal;
use crate::round_summary::RoundSummaryJob;
use crate::run::Usage;
use crate::summary_model::SummaryModel;
use crate::tool::{
    Builtin, MAX_TOOL_RESULT_BYTES, MalformedArguments, ReadArguments, ToolCall, ToolCallArguments,
    ToolDefinition, UnknownToolCall,
};
use crate::tool_handler::{SharedResultFits, ToolContext, ToolEntry, ToolErrorKind, ToolHandler};
use crate::turn::{
    ContextActivity, HostToolOutcome, ModelRequestActivity, ModelRequestPurpose, ToolActivity,
    TurnError, TurnOptions, TurnOutcome, TurnReport, sanitize_report_text,
    sanitized_completion_metadata,
};
use crate::usage::{Cost, TurnUsage};
use crate::write::WriteTool;
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

/// Status of the error result answering a host tool call that ran but whose
/// result was too large to send.
const OUTPUT_TOO_LARGE_STATUS: &str = "output_too_large";

/// Request-only feedback after a summary request that continues the turn,
/// tools included, was answered with tool calls; the calls never run.
const SUMMARY_TOOL_CALL_FEEDBACK: &str = "[harness] Those tool calls were not run: this reply \
                                          must be the summary itself. Do not call any tool; reply \
                                          with only the JSON summary object requested above.";

/// Shared execution dependencies and the immutable configuration for one turn.
pub(crate) struct Engine<'a> {
    /// Ledger that reserves every model request before it is sent.
    pub(crate) budget: &'a dyn BudgetLedger,
    /// Permits of the model's concurrency cap, shared by every harness
    /// built from its catalog; one is held for each request in flight.
    pub(crate) concurrency: Option<&'a Semaphore>,
    pub(crate) context_budget: context::ContextBudget,
    pub(crate) context_calibration: &'a ContextCalibration,
    pub(crate) context_estimator: &'a Arc<dyn ContextEstimator>,
    pub(crate) context_summaries: ContextSummaryPolicy,
    /// Control that receives the report of a turn dropped before it
    /// finished, such as a cancelled one.
    pub(crate) control: Option<&'a TurnControl>,
    pub(crate) effects: Effects,
    /// Sink of a run that streams its events.
    pub(crate) events: Option<Arc<EventSink>>,
    pub(crate) file_system: &'a Arc<dyn FileSystem>,
    pub(crate) lifecycle: &'a LifecycleEmitter,
    /// Most tokens one response of the model may contain, from its catalog
    /// entry, which bounds a request's output estimate.
    pub(crate) max_output: Option<u64>,
    pub(crate) model: &'a Arc<dyn Model>,
    /// Catalog identifier of [`Self::model`], when the harness selected it
    /// from a catalog.
    pub(crate) model_id: Option<&'a ModelId>,
    pub(crate) model_reasoning_effort: Option<ReasoningEffort>,
    pub(crate) options: &'a TurnOptions,
    /// Prices of the model, which give every request its cost.
    pub(crate) pricing: Option<Pricing>,
    /// Requests the turn's session sent for it before its first request,
    /// such as automatic compaction, which lead the turn's report.
    pub(crate) prior_requests: Option<&'a PriorRequests>,
    pub(crate) repository: Option<&'a Repository>,
    /// Session whose turn this is, given to tool handlers.
    pub(crate) session: Option<SessionId>,
    /// Model that answers the turn's round summary requests in place of
    /// [`Self::model`], when the harness resolved one.
    pub(crate) summary_model: Option<&'a SummaryModel>,
}

impl Engine<'_> {
    /// Runs one turn to its final output. A turn that fails after
    /// preparing its request still reports what it did before failing.
    pub(crate) async fn run(
        &self,
        request: ModelRequest,
        turn_id: Option<LifecycleId>,
        journal: Option<WriteJournal>,
    ) -> Result<FinishedTurn, EngineFailure> {
        let started_at = Instant::now();
        let (request, tools) = self
            .prepare_request(request, journal.as_ref())
            .map_err(EngineFailure::unreported)?;
        // Host instructions are journaled after the input, so the turn
        // replays with them.
        if let Some(instructions) = request.instructions_message() {
            Self::persist(journal.as_ref(), instructions)
                .await
                .map_err(EngineFailure::unreported)?;
        }
        // Only a turn that may summarize asks the model how to; a text turn
        // states no schema in its prefix, so its summaries never share it,
        // and only the turn's own model can continue its cached prefix.
        let shared_prefix_summaries = self.context_summaries == ContextSummaryPolicy::Enabled
            && self.summary_model.is_none()
            && self.model.keeps_prefix_across_schemas()
            && request.schema().is_some();
        let mut progress = Progress {
            control: self.control,
            started_at,
            state: TurnState::new(
                ToolRounds::new(tools, journal),
                context::ContextFit::new(shared_prefix_summaries),
                self.pricing,
            ),
        };
        if let Some(prior) = self.prior_requests {
            progress.state.prepend(prior);
        }
        // A turn's wall-clock limit also covers the compaction before it.
        let deadline = self.options.limits().deadline(
            self.prior_requests
                .map_or_else(time::Instant::now, |prior| prior.started_at),
        );
        let driven = self.drive(request, turn_id, &mut progress.state);
        // Reaching the wall-clock limit drops the turn's pending request or
        // tool call, as cancellation does, and fails the turn; one that
        // dropped a running tool call leaves effects of unknown outcome.
        let (driven, interrupted) = match deadline {
            Some(deadline) => match timeout_at(deadline, driven).await {
                Ok(driven) => (driven, false),
                Err(_) => (
                    Err(TurnError::limit_reached(LimitKind::WallTime)),
                    progress.state.running_tools,
                ),
            },
            None => (driven.await, false),
        };
        match driven {
            Ok((output, request)) => {
                let history = progress.state.context_fit.sent_history(&request);
                let next_request_index = progress.state.model_request_index;
                let report = progress.finish();

                Ok(FinishedTurn {
                    history,
                    messages: request.into_messages(),
                    next_request_index,
                    outcome: TurnOutcome::new(output, report),
                })
            }
            Err(error) => {
                // Summary requests recorded before the failure still reach
                // the stream and the stored usage, so both match the
                // outcome's. A store that cannot record the usage keeps what
                // it recorded before; the turn reports its own failure.
                let _ = self.publish_requests(&mut progress.state).await;

                Err(EngineFailure {
                    error,
                    interrupted,
                    next_request_index: progress.state.model_request_index,
                    report: Some(Box::new(progress.finish())),
                })
            }
        }
    }

    /// Emits `kind` when the turn belongs to a run that streams its events.
    pub(crate) async fn publish(&self, kind: RunEventKind) {
        if let Some(events) = &self.events {
            events.emit(kind).await;
        }
    }

    /// Sends requests and executes tool calls until the model answers with
    /// output, recording what the turn did in `state`.
    async fn drive(
        &self,
        mut request: ModelRequest,
        turn_id: Option<LifecycleId>,
        state: &mut TurnState,
    ) -> Result<(Value, ModelRequest), TurnError> {
        let summary_trigger = (self.context_summaries == ContextSummaryPolicy::Enabled)
            .then_some(ContextSummaryTrigger::Proactive);
        loop {
            // Summary requests and rejected outputs recorded since the last
            // round are published before the next request.
            self.publish_requests(state).await?;
            self.options
                .limits()
                .check_request(state.model_request_index, state.used_tokens())
                .map_err(TurnError::limit_reached)?;
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
                .fit(&**self.context_estimator, budget, &request, summary_trigger)
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
                    .await?;

                    continue;
                }
            };
            // A reduced copy is taken out of the fit, whose weights estimate
            // the request for the budget ledger before it is sent.
            let fitted = fitted.cloned();
            self.publish_context(context_activity).await;
            let sent = SentRequest {
                budget_weight: budget.max_request_weight().get() - budget.reserved_output_weight(),
                calibration,
                context: context_activity,
                estimated_weight: state.context_fit.sent_weight(),
                unreduced_weight: state.context_fit.unreduced_weight(),
            };
            let completed = self
                .metered_request(
                    self.turn_answerer(),
                    fitted.as_ref().unwrap_or(&request),
                    (sent.estimated_weight, calibration),
                    state.model_request_index,
                    turn_id,
                )
                .await
                .map_err(TurnError::budget_refused)?;
            state
                .overflow
                .retried(self.lifecycle, turn_id, sent.estimated_weight);
            state.model_request_index = state.model_request_index.saturating_add(1);
            let (response, activity, reasoning_content) = match completed {
                Ok(completed) => completed,
                Err(failure) => {
                    self.recover(failure, &mut request, state, &sent)?;

                    continue;
                }
            };
            self.record_turn_request(state, activity, &sent);
            self.publish_requests(state).await?;

            let (calls, batched) = match response {
                ModelResponse::Output(output) => {
                    let content = output.to_string();

                    return Ok(self
                        .finish(request, output, content, reasoning_content)
                        .await);
                }
                ModelResponse::Text(text) => {
                    let output = Value::String(text.clone());

                    return Ok(self.finish(request, output, text, reasoning_content).await);
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
            self.run_tool_round(&calls, batched, turn_id, &mut request, state)
                .await?;
        }
    }

    /// Executes one response's tool `calls` after checking them against
    /// the tool-call limit and the malformed-call bound, then acts on any
    /// repetition among them. Calls refused by the tool-call limit are
    /// journaled without results, so the stopped turn replays them as never
    /// run.
    async fn run_tool_round(
        &self,
        calls: &[ToolCall],
        batched: bool,
        turn_id: Option<LifecycleId>,
        request: &mut ModelRequest,
        state: &mut TurnState,
    ) -> Result<(), TurnError> {
        self.publish_calls_message(calls).await;
        if let Err(limit) = self
            .options
            .limits()
            .check_tool_calls(state.rounds.activity.len(), calls.len())
        {
            Self::record_calls(calls, batched, request, &state.rounds).await?;

            return Err(TurnError::limit_reached(limit));
        }
        state.malformed_tool_calls += count_malformed(
            calls,
            &state.rounds.tools,
            &mut state.consecutive_malformed_responses,
        )?;
        Self::record_calls(calls, batched, request, &state.rounds).await?;
        state.running_tools = true;
        let repetition = self
            .execute_tool_calls(
                calls,
                turn_id,
                request,
                &state.context_fit,
                &mut state.rounds,
            )
            .await?;
        state.running_tools = false;
        state.repetition_nudges += self.act_on_repetition(repetition, request, turn_id)?;

        Ok(())
    }

    /// Records one response's `calls` in `request` and journals them.
    async fn record_calls(
        calls: &[ToolCall],
        batched: bool,
        request: &mut ModelRequest,
        rounds: &ToolRounds,
    ) -> Result<(), TurnError> {
        let call_message = match (batched, calls) {
            (false, [call]) => request.record_tool_call(call.clone()),
            _ => request.record_tool_calls(calls.to_vec()),
        };

        Self::persist(rounds.recorder.as_ref(), call_message).await
    }

    /// Ends the turn with its validated final `output`, recorded in
    /// `request` as assistant `content`.
    async fn finish(
        &self,
        mut request: ModelRequest,
        output: Value,
        content: String,
        reasoning_content: Option<String>,
    ) -> (Value, ModelRequest) {
        self.publish_message(reasoning_content.as_deref(), None)
            .await;
        request.record_output_with_reasoning(content, reasoning_content);

        (output, request)
    }

    /// Lets the turn send its next request after `failure`, or fails it. An
    /// invalid final output is re-requested with request-only feedback;
    /// neither it nor the feedback is journaled, so replayed history keeps
    /// only completed exchanges. A request the provider rejected as
    /// exceeding its context window is refitted under a lower calibrated
    /// ceiling and re-sent; the rejection fails the turn only once the
    /// recoveries are spent or the refit cannot shrink it.
    fn recover(
        &self,
        failure: RequestFailure,
        request: &mut ModelRequest,
        state: &mut TurnState,
        sent: &SentRequest,
    ) -> Result<(), TurnError> {
        match failure {
            RequestFailure {
                error: error @ (ModelError::InvalidJson { .. } | ModelError::SchemaViolation { .. }),
                activity,
            } if state.output_corrections < MAX_OUTPUT_CORRECTIONS => {
                state.output_corrections += 1;
                request.record_feedback(output_feedback(&error));
                self.record_turn_request(state, activity.rejected(), sent);

                Ok(())
            }
            RequestFailure { error, .. } if error.is_context_overflow() => state
                .overflow
                .reject(self.context_calibration, error, sent.estimated_weight)
                .map_err(TurnError::from),
            RequestFailure { error, .. } => Err(error.into()),
        }
    }

    /// Reports an answered request of the turn's own conversation with how
    /// it was fitted, and calibrates later estimates against its usage.
    fn record_turn_request(
        &self,
        state: &mut TurnState,
        activity: ModelRequestActivity,
        sent: &SentRequest,
    ) {
        state.model_requests.push(Self::accepted_request(
            self.turn_answerer(),
            activity
                .with_context(sent.context)
                .with_fit(sent.budget_weight, sent.unreduced_weight),
            sent.estimated_weight,
            sent.calibration,
        ));
    }

    /// Returns the turn's own model with everything that sizes, meters,
    /// and prices its requests.
    fn turn_answerer(&self) -> Answerer<'_> {
        Answerer {
            concurrency: self.concurrency,
            context_budget: self.context_budget,
            context_calibration: self.context_calibration,
            context_estimator: self.context_estimator.as_ref(),
            id: self.model_id,
            max_output: self.max_output,
            model: self.model,
            pricing: self.pricing,
            summary: None,
        }
    }

    /// Returns the model that answers the summary `request`: the summary
    /// model when the harness resolved one and the request fits its
    /// calibrated budget, and otherwise the turn's own model, whose budget
    /// sized the request.
    fn summary_answerer(&self, request: &ModelRequest) -> Answerer<'_> {
        self.summary_model
            .map(|summary| Answerer {
                concurrency: summary.concurrency.as_deref(),
                context_budget: summary.context_budget,
                context_calibration: &summary.context_calibration,
                context_estimator: summary.context_estimator.as_ref(),
                id: Some(&summary.id),
                max_output: summary.max_output,
                model: &summary.model,
                pricing: summary.pricing,
                summary: Some(summary),
            })
            .filter(|answerer| answerer.fits(request))
            .unwrap_or_else(|| self.turn_answerer())
    }

    /// Calibrates `answerer`'s later estimates against a request it
    /// answered of `estimated_weight` and returns its activity with that
    /// estimate, the token count `calibration` predicted for it, and the
    /// answering model with the cost of its usage at that model's prices.
    fn accepted_request(
        answerer: Answerer<'_>,
        activity: ModelRequestActivity,
        estimated_weight: u64,
        calibration: Calibration,
    ) -> ModelRequestActivity {
        answerer.context_calibration.observe(
            answerer.context_budget,
            estimated_weight,
            activity.completion(),
        );
        let usage = Usage::from_request(activity.completion().and_then(CompletionMetadata::usage));
        let cost = answerer.pricing.map(|pricing| Cost::of(&usage, pricing));

        activity
            .with_estimate(
                estimated_weight,
                calibration
                    .envelope()
                    .map(|envelope| envelope.tokens(estimated_weight)),
            )
            .with_answerer(answerer.id.cloned(), cost)
    }

    /// Executes one response's journaled `calls` in order and journals each
    /// result, returning the strongest repetition among them.
    /// Each `read` result is bounded as it runs to the share of the
    /// calibrated budget one result may take, and the round's results
    /// together to the share one round may take: the first results that fit
    /// are kept whole, and every later `read` is answered with the exact call
    /// that fetches it in a later response (see [`RoundShare`]).
    async fn execute_tool_calls<'a>(
        &self,
        calls: &'a [ToolCall],
        turn_id: Option<LifecycleId>,
        request: &mut ModelRequest,
        context_fit: &context::ContextFit,
        rounds: &mut ToolRounds,
    ) -> Result<Repetition<'a>, TurnError> {
        let (budget, _) = self
            .context_calibration
            .effective_budget(self.context_budget);
        let mut share = RoundShare::new(Arc::clone(self.context_estimator), budget);
        let mut repetition = Repetition::None;
        for call in calls {
            let repeated = rounds.reads.repeat_of(call, context_fit);
            let shortcut = match &repeated {
                Some((call_id, _)) => Some(ReadShortcut::Repeat(call_id)),
                None if share.deferring => Some(ReadShortcut::Defer),
                None => None,
            };
            let is_read = call.read_arguments().is_some();
            let ran_read = is_read && shortcut.is_none();
            let mut deferred = is_read && matches!(shortcut, Some(ReadShortcut::Defer));
            let item = self.start_item(ItemKind::ToolCall).await;
            let (mut result, mut activity) = self
                .execute_tool_call(
                    call,
                    &rounds.tools,
                    turn_id,
                    shortcut,
                    share.result_fits(call),
                )
                .await?;
            let unavailable = matches!(activity, ToolActivity::Unavailable { .. });
            if !share.keeps(call, &result, ran_read && !unavailable)
                && let Some(arguments) = call.read_arguments()
            {
                result = context::deferred_read_result(arguments);
                activity = ToolActivity::ReadDeferred {
                    action: arguments.action(),
                    duration: activity.duration(),
                    summary: sanitize_report_text(read_summary(arguments)),
                };
                share.keeps(call, &result, false);
                deferred = true;
            }
            let result_bytes = result.len();
            // A read answered as a repeat returned the earlier result.
            let digest = repeated
                .as_ref()
                .map_or_else(|| repetition::result_digest(&result), |(_, digest)| *digest);
            let item = item.map(|id| (id, ToolCallItem::new(call, &result, &activity)));
            Self::persist(
                rounds.recorder.as_ref(),
                request.record_tool_result(call, result),
            )
            .await?;
            if let Some((id, item)) = item {
                self.publish(RunEventKind::ItemCompleted {
                    id,
                    item: Item::ToolCall(item),
                })
                .await;
            }
            let result_index = request.messages().len() - 1;
            rounds.reads.record(
                call,
                result_index,
                (result_bytes, digest),
                repeated.is_some() || deferred || unavailable,
            );
            let counted = if unavailable {
                rounds.repeated_calls.record_unavailable(call)
            } else {
                rounds.repeated_calls.record(call, digest)
            };
            repetition = repetition.max(counted);
            rounds.activity.push(activity);
        }

        Ok(repetition)
    }

    /// Fails the turn when the latest response's calls reached
    /// [`repetition::REPETITION_STOP_AT`] identical calls, or appends a
    /// request-only nudge, never journaled, when they first reached
    /// [`repetition::REPETITION_NUDGE_AT`]; returns the nudges added. The
    /// nudge for calls to unavailable tools says they did not run.
    fn act_on_repetition(
        &self,
        repetition: Repetition<'_>,
        request: &mut ModelRequest,
        turn_id: Option<LifecycleId>,
    ) -> Result<usize, TurnError> {
        let (repeats, tool, unavailable, stopped) = match repetition {
            Repetition::None => return Ok(0),
            Repetition::Nudge {
                repeats,
                tool,
                unavailable,
            } => (repeats, tool, unavailable, false),
            Repetition::Stop {
                repeats,
                tool,
                unavailable,
            } => (repeats, tool, unavailable, true),
        };
        self.lifecycle
            .repeated_tool_calls(tool, repeats, stopped, turn_id);
        if stopped {
            return Err(TurnError::RepeatedToolCalls {
                repeats,
                tool: tool.to_string(),
            });
        }
        request.record_feedback(if unavailable {
            repetition::unavailable_repetition_feedback(tool, repeats)
        } else {
            repetition::repetition_feedback(tool, repeats)
        });

        Ok(1)
    }

    /// Requests the summary `job` describes and installs it in
    /// `context_fit`, or records its failure so the turn falls back to
    /// removing the rounds behind a deterministic note. A cancelled turn
    /// drops this future, which reports the summary as cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError::BudgetRefused`] when the budget ledger refused a
    /// summary request, which ends the turn.
    async fn summarize_rounds(
        &self,
        job: &RoundSummaryJob,
        (request, context_fit): (&ModelRequest, &mut context::ContextFit),
        turn_id: Option<LifecycleId>,
        requests: (&mut u64, &mut Vec<ModelRequestActivity>),
    ) -> Result<(), TurnError> {
        let lifecycle = self.lifecycle.start_context_summary(job.trigger(), turn_id);
        let installed = match self.request_summary(job, turn_id, requests).await {
            Ok(summary) => context_fit
                .install_summary(self.context_estimator.as_ref(), request, job, summary)
                .ok_or(ContextSummaryErrorType::Rejected),
            Err(SummaryFailure::Failed(error_type)) => Err(error_type),
            Err(SummaryFailure::Refused(limit)) => {
                if let Some(lifecycle) = lifecycle {
                    lifecycle.failed(ContextSummaryErrorType::BudgetExhausted);
                }

                return Err(TurnError::budget_refused(limit));
            }
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

        Ok(())
    }

    /// Sends the job's summary request, re-requesting an invalid output, or
    /// tool calls in answer to a request that continues the turn with its
    /// tools, with corrective feedback at most [`MAX_OUTPUT_CORRECTIONS`]
    /// times, and returns a summary that satisfies the summary schema, fits
    /// the job's size limit, and is smaller than its source. Every answered
    /// request is reported with [`ModelRequestPurpose::ContextSummary`] and
    /// calibrates later estimates of the model that answered it: the
    /// summary model when one is set and the request, grown by any
    /// correction, still fits its budget, or else the turn's own model.
    /// That model's estimate reserves the request from the budget ledger,
    /// its concurrency cap meters it, and its prices give it its cost.
    async fn request_summary(
        &self,
        job: &RoundSummaryJob,
        turn_id: Option<LifecycleId>,
        (model_request_index, model_requests): (&mut u64, &mut Vec<ModelRequestActivity>),
    ) -> Result<Value, SummaryFailure> {
        let mut request = job.request().clone();
        if self.lifecycle.is_enabled() {
            request.mark_lifecycle_observed();
        }
        let mut corrections = 0;
        loop {
            // Each correction grows the request, so it may outgrow the
            // summary model's budget.
            let answerer = self.summary_answerer(&request);
            let (_, calibration) = answerer
                .context_calibration
                .effective_budget(answerer.context_budget);
            let estimated_weight = answerer.request_weight(&request);
            let summary_request = answerer.summary.map(|summary| summary.request(&request));
            let completed = self
                .metered_request(
                    answerer,
                    summary_request.as_ref().unwrap_or(&request),
                    (estimated_weight, calibration),
                    *model_request_index,
                    turn_id,
                )
                .await
                .map_err(SummaryFailure::Refused)?;
            *model_request_index = model_request_index.saturating_add(1);
            let (response, activity, _) = match completed {
                Ok(completed) => completed,
                Err(RequestFailure {
                    error:
                        error @ (ModelError::InvalidJson { .. } | ModelError::SchemaViolation { .. }),
                    activity,
                }) => {
                    model_requests.push(Self::accepted_request(
                        answerer,
                        activity
                            .rejected()
                            .with_purpose(ModelRequestPurpose::ContextSummary),
                        estimated_weight,
                        calibration,
                    ));
                    if corrections == MAX_OUTPUT_CORRECTIONS {
                        return Err(ContextSummaryErrorType::InvalidOutput.into());
                    }
                    corrections += 1;
                    request.record_feedback(output_feedback(&error));

                    continue;
                }
                Err(_) => return Err(ContextSummaryErrorType::Model.into()),
            };
            model_requests.push(Self::accepted_request(
                answerer,
                activity.with_purpose(ModelRequestPurpose::ContextSummary),
                estimated_weight,
                calibration,
            ));
            let ModelResponse::Output(summary) = response else {
                if corrections == MAX_OUTPUT_CORRECTIONS || request.tools().is_empty() {
                    return Err(ContextSummaryErrorType::InvalidOutput.into());
                }
                corrections += 1;
                request.record_feedback(SUMMARY_TOOL_CALL_FEEDBACK.to_string());

                continue;
            };

            return compaction::accept_summary(
                &summary,
                job.max_summary_bytes(),
                job.source_bytes(),
            )
            .map(|_| summary)
            .map_err(|_| ContextSummaryErrorType::Rejected.into());
        }
    }

    /// Emits a [`RunEventKind::ModelRequestCompleted`] for every model
    /// request `state` recorded since the last call, attributed to the model
    /// that answered it and priced at its prices, so the events' usage sums
    /// to the run outcome's. Their costs sum to its cost while every request
    /// is priced; once one is not, the outcome's cost is `None` although
    /// the priced requests' events keep their own costs. Then records the
    /// turn's usage so
    /// far on its durable turn. The session recorded zero usage when it
    /// admitted the turn, so a turn stopped before its first response never
    /// reads as unknown.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError::UsageJournal`] when the store cannot record the
    /// usage.
    async fn publish_requests(&self, state: &mut TurnState) -> Result<(), TurnError> {
        let completed = state.model_requests.len();
        if state.published_requests == completed {
            return Ok(());
        }
        for activity in &state.model_requests[state.published_requests..] {
            let usage =
                Usage::from_request(activity.completion().and_then(CompletionMetadata::usage));
            let cost = activity.cost();
            state.cost = Cost::sum(state.cost, cost);
            self.publish(RunEventKind::ModelRequestCompleted {
                cost,
                model: activity.model().cloned(),
                usage,
            })
            .await;
        }
        state.published_requests = completed;
        let Some(recorder) = &state.rounds.recorder else {
            return Ok(());
        };
        let usage = TurnUsage {
            cost: state.cost,
            usage: Usage::from_requests(&state.model_requests),
        };

        recorder
            .record_usage(&usage)
            .await
            .map_err(|source| TurnError::UsageJournal {
                source: Box::new(source),
            })
    }

    /// Emits a [`RunEventKind::ContextReduced`] for a request sent with
    /// `activity`, unless it was sent unchanged.
    async fn publish_context(&self, activity: ContextActivity) {
        if activity != ContextActivity::default() {
            self.publish(RunEventKind::ContextReduced(activity)).await;
        }
    }

    /// Emits the reasoning and assistant text of a tool-call response, which
    /// travel with its first call.
    async fn publish_calls_message(&self, calls: &[ToolCall]) {
        let first = calls.first();
        self.publish_message(
            first.and_then(ToolCall::reasoning_content),
            first.and_then(ToolCall::assistant_text),
        )
        .await;
    }

    /// Emits a response's `reasoning`, then its assistant `text`, as
    /// completed items. The run's final output is emitted only after its
    /// turn commits, by [`EventSink::complete`].
    async fn publish_message(&self, reasoning: Option<&str>, text: Option<&str>) {
        let Some(events) = &self.events else {
            return;
        };
        if let Some(text) = reasoning {
            let item = Item::Reasoning {
                text: text.to_string(),
            };
            events.publish_item(ItemKind::Reasoning, item).await;
        }
        if let Some(text) = text {
            let item = Item::Message {
                text: text.to_string(),
            };
            events.publish_item(ItemKind::Message, item).await;
        }
    }

    /// Starts an item of `item_kind` when the turn streams events.
    async fn start_item(&self, item_kind: ItemKind) -> Option<ItemId> {
        match &self.events {
            Some(events) => Some(events.start_item(item_kind).await),
            None => None,
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
        journal: Option<&WriteJournal>,
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
        let mut tools = Tools::default();
        for entry in self.options.tools().entries() {
            let (definition, handler) = match entry {
                ToolEntry::Handler {
                    definition,
                    handler,
                } => (definition.clone(), Arc::clone(handler)),
                ToolEntry::Builtin(builtin) => {
                    let repository = self.repository.ok_or(TurnError::RepositoryRequired)?;
                    let handler = self.builtin_handler(*builtin, repository, journal)?;

                    (handler.definition(), handler)
                }
            };
            tools
                .handlers
                .push((definition.name().to_string(), handler));
            request = request.with_tool(definition);
        }

        Ok((request, tools))
    }

    /// Binds `builtin` to this turn's `repository`, `journal`, and options
    /// as the handler its calls run through.
    fn builtin_handler(
        &self,
        builtin: Builtin,
        repository: &Repository,
        journal: Option<&WriteJournal>,
    ) -> Result<Arc<dyn ToolHandler>, TurnError> {
        let definition = context::builtin_definition(builtin, self.options);
        let root = repository.root().to_path_buf();

        Ok(match builtin {
            Builtin::Read => Arc::new(ReadHandler {
                definition,
                tool: ReadTool::with_git(
                    Arc::clone(self.file_system),
                    root,
                    repository.git_executable().to_path_buf(),
                    self.options.comparison_base().cloned(),
                ),
            }),
            Builtin::Write => {
                let mut tool = WriteTool::new(Arc::clone(self.file_system), root);
                tool.journal = journal.cloned();
                tool.effects = self.effects.clone();

                Arc::new(WriteHandler { tool })
            }
            Builtin::Bash => Arc::new(BashHandler {
                definition,
                tool: BashTool::new(
                    self.options
                        .bash()
                        .cloned()
                        .ok_or(BashError::InvalidPolicy)?,
                    root,
                    self.effects.commands().clone(),
                    journal.cloned(),
                )?,
            }),
        })
    }

    /// Sends `request` to `answerer`, estimated at `estimated_weight` under
    /// `calibration`, once a permit of that model's concurrency cap is free
    /// and the budget ledger then reserved its [`Estimate`], so a request
    /// queued for a permit holds no budget. The permit is held only while
    /// the request is in flight. Once sent, the request is always charged:
    /// with its actual usage and cost at that model's prices when the
    /// provider answered with usage, and with its whole estimate otherwise,
    /// including when this future is dropped in flight by cancellation or a
    /// wall-clock limit. Dropping this future releases its permit.
    ///
    /// # Errors
    ///
    /// Returns the cap the ledger refused the request at; nothing is sent.
    async fn metered_request(
        &self,
        answerer: Answerer<'_>,
        request: &ModelRequest,
        (estimated_weight, calibration): (u64, Calibration),
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<Result<CompletedRequest, RequestFailure>, LimitKind> {
        // The semaphore is never closed, so acquiring it fails only in
        // theory; a request then runs without a permit.
        let permit = match answerer.concurrency {
            Some(concurrency) => concurrency.acquire().await.ok(),
            None => None,
        };
        let reservation = self
            .budget
            .reserve(self.estimate(answerer, estimated_weight, calibration))
            .await?;
        let charge = SentCharge {
            budget: self.budget,
            reservation: Some(reservation),
        };
        let completed = self
            .complete_model_request(answerer.model, request, model_request_index, turn_id)
            .await;
        drop(permit);
        let activity = match &completed {
            Ok((_, activity, _)) => activity,
            Err(failure) => &failure.activity,
        };
        if let Some(completion) = activity.completion() {
            let usage = Usage::from_request(completion.usage());
            let cost = answerer.pricing.map(|pricing| Cost::of(&usage, pricing));
            charge.commit(&usage, cost);
        }

        Ok(completed)
    }

    /// Estimates a request to `answerer` of `estimated_weight`: input
    /// tokens calibrated by `calibration`, or the weight itself before any
    /// usage report, and output tokens up to the turn's maximum output for
    /// the turn's own model, or else the answering model's cataloged
    /// maximum output, or else the output weight its context budget
    /// reserves, calibrated like input; priced when that model has prices.
    fn estimate(
        &self,
        answerer: Answerer<'_>,
        estimated_weight: u64,
        calibration: Calibration,
    ) -> Estimate {
        let tokens = |weight: u64| {
            calibration
                .envelope()
                .map_or(weight, |envelope| envelope.tokens(weight))
        };
        let input = tokens(estimated_weight);
        // A summary model's request carries none of the turn's generation
        // settings.
        let turn_max_output = answerer
            .summary
            .is_none()
            .then(|| self.options.generation().max_output_tokens)
            .flatten();
        let output = match (turn_max_output, answerer.max_output) {
            (Some(tokens), _) => u64::from(tokens.get()),
            (None, Some(max_output)) => max_output,
            (None, None) => tokens(
                estimated_weight.saturating_add(answerer.context_budget.reserved_output_weight()),
            )
            .saturating_sub(input),
        };
        let estimate = Estimate::new(input, output);
        let Some(pricing) = answerer.pricing else {
            return estimate;
        };
        let usage = Usage {
            input,
            output,
            ..Usage::default()
        };

        estimate.with_cost(Cost::of(&usage, pricing))
    }

    /// Sends `request` to `model`, reporting it to the lifecycle under that
    /// model's metadata, and validates an output against the request.
    async fn complete_model_request(
        &self,
        model: &Arc<dyn Model>,
        request: &ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<CompletedRequest, RequestFailure> {
        let started_at = Instant::now();
        let model_lifecycle =
            self.lifecycle
                .start_model_request(model.metadata(), model_request_index, turn_id);
        let operation =
            event::with_retry_events(self.events.clone(), model.complete(request.clone()));
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

                return Err(RequestFailure::new(error, None, started_at));
            }
        };
        if let Err(error) = Self::validate_output(request, &response) {
            if let Some(model_lifecycle) = model_lifecycle {
                model_lifecycle.failed(error.error_type(), error.http_status());
            }

            return Err(RequestFailure::new(error, completion.as_ref(), started_at));
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

    /// Validates a final answer against the request's output mode:
    /// structured output against its schema, text as nonblank and bounded.
    /// An answer in the other mode is invalid; tool calls pass.
    fn validate_output(request: &ModelRequest, response: &ModelResponse) -> Result<(), ModelError> {
        match (request.schema(), response) {
            (Some(schema), ModelResponse::Output(output)) => {
                schema.validate_value(output).map_err(ModelError::from)
            }
            (None, ModelResponse::Text(text)) if !text.trim().is_empty() => {
                schema_contract::ensure_content_size(text).map_err(ModelError::from)
            }
            (_, ModelResponse::ToolCall(_) | ModelResponse::ToolCalls(_)) => Ok(()),
            _ => Err(ModelError::InvalidResponse),
        }
    }

    /// Runs `call` with the turn's handler of its name, bounding a `read`
    /// result to what `fits` accepts, or answers an allowed `read` with
    /// `shortcut` without running it.
    async fn execute_tool_call(
        &self,
        call: &ToolCall,
        tools: &Tools,
        turn_id: Option<LifecycleId>,
        shortcut: Option<ReadShortcut<'_>>,
        fits: SharedResultFits,
    ) -> Result<(String, ToolActivity), TurnError> {
        let started_at = Instant::now();
        let mut tool_lifecycle =
            self.lifecycle
                .request_tool(call.id().to_string(), call.name().to_string(), turn_id);
        let handler = tools.handler_for(call);
        let execution = match call.arguments() {
            ToolCallArguments::Read(arguments) => handler.map(|handler| match shortcut {
                Some(ReadShortcut::Repeat(earlier)) => {
                    ToolExecution::RepeatedRead(arguments, earlier)
                }
                Some(ReadShortcut::Defer) => ToolExecution::DeferredRead(arguments),
                None => ToolExecution::Handler(handler, self.tool_context(call, Some(fits))),
            }),
            ToolCallArguments::Malformed(arguments) => handler
                .is_some()
                .then_some(ToolExecution::Malformed(arguments, self.options)),
            _ => handler
                .map(|handler| ToolExecution::Handler(handler, self.tool_context(call, None))),
        };
        let Some(execution) = execution else {
            let reason = if call.unknown_tool().is_some() {
                ToolUnavailableReason::Unknown
            } else {
                ToolUnavailableReason::Denied
            };
            if let Some(tool_lifecycle) = tool_lifecycle {
                tool_lifecycle.denied(reason);
            }

            return Ok((
                unavailable_tool_result(call.name(), self.options),
                ToolActivity::Unavailable {
                    duration: started_at.elapsed(),
                    reason,
                    tool: call.name().to_string(),
                },
            ));
        };
        if let Some(tool_lifecycle) = tool_lifecycle.as_mut() {
            tool_lifecycle.started();
        }
        let operation = execute_tool(execution, call);
        let result = match tool_lifecycle.as_ref() {
            Some(tool_lifecycle) => tool_lifecycle.scope(operation).await,
            None => operation.await,
        };

        Self::finish_tool_call(result, tool_lifecycle)
    }

    /// Describes `call` to its handler; `fits` bounds a built-in `read`.
    fn tool_context(&self, call: &ToolCall, fits: Option<SharedResultFits>) -> ToolContext {
        ToolContext {
            call_id: call.id().to_string(),
            control: self.control.cloned(),
            fits,
            run: self.control.and_then(TurnControl::run_id).cloned(),
            session: self.session.clone(),
            workspace: self
                .repository
                .map(|repository| repository.root().to_path_buf()),
        }
    }

    fn finish_tool_call(
        result: Result<(String, ToolActivity), TurnError>,
        tool_lifecycle: Option<ToolLifecycle>,
    ) -> Result<(String, ToolActivity), TurnError> {
        match result {
            Ok(result) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    if ToolCallStatus::from(&result.1) == ToolCallStatus::Rejected {
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

/// Model that answers a request, with everything that sizes, meters, and
/// prices its requests and learns from its usage.
#[derive(Clone, Copy)]
struct Answerer<'a> {
    concurrency: Option<&'a Semaphore>,
    context_budget: context::ContextBudget,
    context_calibration: &'a ContextCalibration,
    context_estimator: &'a dyn ContextEstimator,
    id: Option<&'a ModelId>,
    max_output: Option<u64>,
    model: &'a Arc<dyn Model>,
    pricing: Option<Pricing>,
    /// The summary model this answerer is, which sets its own reasoning
    /// depth and is not bound by the turn's output cap; `None` for the
    /// turn's own model.
    summary: Option<&'a SummaryModel>,
}

impl Answerer<'_> {
    /// Returns whether `request` fits this model's calibrated budget.
    fn fits(&self, request: &ModelRequest) -> bool {
        let (budget, _) = self
            .context_calibration
            .effective_budget(self.context_budget);

        self.request_weight(request)
            <= budget.max_request_weight().get() - budget.reserved_output_weight()
    }

    /// Approximates `request` with this model's estimator: its prompt
    /// schema, advertised tools, and messages.
    fn request_weight(&self, request: &ModelRequest) -> u64 {
        // A continued request states the turn's schema in its prompt and
        // advertises the turn's tools.
        let prompt_schema = request.prefix_schema().or(request.schema());
        let fixed = request.tools().iter().fold(
            self.context_estimator.request_weight(prompt_schema),
            |weight, tool| {
                weight.saturating_add(self.context_estimator.tool_definition_weight(tool))
            },
        );

        request.messages().iter().fold(fixed, |weight, message| {
            weight.saturating_add(self.context_estimator.message_weight(message))
        })
    }
}

/// How one turn request was fitted before it was sent: the calibrated
/// capacity and its weight before and after reduction, all without reserved
/// output, with the reductions it carries.
struct SentRequest {
    budget_weight: u64,
    calibration: Calibration,
    context: ContextActivity,
    estimated_weight: u64,
    unreduced_weight: u64,
}

/// The reservation of a sent model request. Committed with the request's
/// reported usage, or, when dropped first, as a request whose provider
/// reported nothing, which charges its whole estimate: a request that was
/// sent may have been billed even if it failed or was dropped in flight.
struct SentCharge<'a> {
    budget: &'a dyn BudgetLedger,
    reservation: Option<Reservation>,
}

impl SentCharge<'_> {
    /// Commits the request's reported `usage` and `cost`.
    fn commit(mut self, usage: &Usage, cost: Option<Cost>) {
        self.settle(usage, cost);
    }

    /// Commits the reservation unless it was already committed.
    fn settle(&mut self, usage: &Usage, cost: Option<Cost>) {
        if let Some(reservation) = self.reservation.take() {
            self.budget.commit(reservation, usage, cost);
        }
    }
}

impl Drop for SentCharge<'_> {
    fn drop(&mut self) {
        let unreported = Usage {
            unreported_requests: 1,
            ..Usage::default()
        };
        self.settle(&unreported, None);
    }
}

/// An answered model request: its response, its activity, and the
/// reasoning that came with it.
type CompletedRequest = (ModelResponse, ModelRequestActivity, Option<String>);

/// Why a context summary produced no summary: the summary failed, so the
/// turn falls back to removing rounds, or the budget ledger refused a
/// summary request at a cap, which ends the turn.
enum SummaryFailure {
    Failed(ContextSummaryErrorType),
    Refused(LimitKind),
}

impl From<ContextSummaryErrorType> for SummaryFailure {
    fn from(error_type: ContextSummaryErrorType) -> Self {
        Self::Failed(error_type)
    }
}

/// A failed model request's error with its activity: elapsed time and,
/// when the provider answered before the output was rejected, its usage.
struct RequestFailure {
    error: ModelError,
    activity: Box<ModelRequestActivity>,
}

impl RequestFailure {
    fn new(
        error: ModelError,
        completion: Option<&CompletionMetadata>,
        started_at: Instant,
    ) -> Self {
        let activity = ModelRequestActivity::new(
            completion.map(sanitized_completion_metadata),
            started_at.elapsed(),
            ModelResponseType::Output,
        );

        Self {
            error,
            activity: Box::new(activity),
        }
    }
}

/// A finished turn's outcome, its canonical messages, and how its last
/// request carried the history its first request replayed.
pub(crate) struct FinishedTurn {
    pub(crate) history: SentHistory,
    pub(crate) messages: Vec<ModelMessage>,
    /// Lifecycle index the turn's next model request would have taken.
    pub(crate) next_request_index: u64,
    pub(crate) outcome: TurnOutcome,
}

/// A failed turn's error with the report of what it did before failing,
/// or `None` when it failed before preparing its request. An `interrupted`
/// turn stopped while a tool call ran, so that call's effects are unknown.
pub(crate) struct EngineFailure {
    pub(crate) error: TurnError,
    pub(crate) interrupted: bool,
    /// Lifecycle index the turn's next model request would have taken.
    pub(crate) next_request_index: u64,
    pub(crate) report: Option<Box<TurnReport>>,
}

impl EngineFailure {
    fn unreported(error: TurnError) -> Self {
        Self {
            error,
            interrupted: false,
            next_request_index: 0,
            report: None,
        }
    }
}

/// Model requests a session sent for a turn before the turn's first
/// request, such as automatic compaction, each attributed to and priced at
/// the model that answered it and already published as run events.
pub(crate) struct PriorRequests {
    pub(crate) activities: Vec<ModelRequestActivity>,
    /// Lifecycle model request indexes they took, from zero; the turn's own
    /// requests continue from here.
    pub(crate) lifecycle_requests: u64,
    /// When the compaction started, from which the turn's wall-clock limit
    /// runs.
    pub(crate) started_at: time::Instant,
}

/// Everything one turn did so far; its report is built from this whether
/// the turn finishes or fails.
struct TurnState {
    rounds: ToolRounds,
    context_fit: context::ContextFit,
    model_request_index: u64,
    model_requests: Vec<ModelRequestActivity>,
    /// Leading [`Self::model_requests`] already published as run events.
    published_requests: usize,
    /// Summed cost of the published requests, or `None` for an unpriced
    /// model.
    cost: Option<Cost>,
    output_corrections: usize,
    consecutive_malformed_responses: usize,
    malformed_tool_calls: usize,
    repetition_nudges: usize,
    overflow: OverflowRecovery,
    /// Whether a response's tool calls are running.
    running_tools: bool,
}

impl TurnState {
    /// Starts the state of a turn whose model `pricing` prices its requests.
    fn new(rounds: ToolRounds, context_fit: context::ContextFit, pricing: Option<Pricing>) -> Self {
        Self {
            rounds,
            context_fit,
            model_request_index: 0,
            model_requests: Vec::new(),
            published_requests: 0,
            cost: pricing.map(Cost::zero),
            output_corrections: 0,
            consecutive_malformed_responses: 0,
            malformed_tool_calls: 0,
            repetition_nudges: 0,
            overflow: OverflowRecovery::default(),
            running_tools: false,
        }
    }

    /// Returns the input plus output tokens the provider reported for the
    /// turn's answered model requests so far.
    fn used_tokens(&self) -> u64 {
        self.model_requests
            .iter()
            .map(|request| {
                let usage = Usage::from_request(
                    request
                        .completion()
                        .and_then(|completion| completion.usage()),
                );

                usage.input.saturating_add(usage.output)
            })
            .fold(0, u64::saturating_add)
    }

    /// Moves the turn's recorded activity into a report over its elapsed
    /// `duration`.
    fn take_report(&mut self, duration: Duration) -> TurnReport {
        TurnReport::new(
            duration,
            std::mem::take(&mut self.model_requests),
            std::mem::take(&mut self.rounds.activity),
        )
        .with_corrections(self.output_corrections, self.malformed_tool_calls)
        .with_repetition_nudges(self.repetition_nudges)
        .with_context_overflow_recoveries(self.overflow.recoveries)
        .with_context_summary_failures(self.context_fit.summary_failures())
    }

    /// Records the already published `prior` requests ahead of the turn's
    /// own, which counts them toward its limits, cost, and stored usage;
    /// its lifecycle request indexes continue after theirs.
    fn prepend(&mut self, prior: &PriorRequests) {
        self.model_requests.extend_from_slice(&prior.activities);
        self.published_requests = self.model_requests.len();
        self.cost = prior
            .activities
            .iter()
            .map(ModelRequestActivity::cost)
            .fold(self.cost, Cost::sum);
        self.model_request_index = prior.lifecycle_requests;
    }
}

/// A running turn's state; dropping it unfinished, as cancellation,
/// interruption, or a panic does, records its report on the turn's control.
struct Progress<'a> {
    control: Option<&'a TurnControl>,
    started_at: Instant,
    state: TurnState,
}

impl Progress<'_> {
    /// Reports the finished turn's activity; nothing is recorded on drop.
    fn finish(&mut self) -> TurnReport {
        self.control = None;

        self.state.take_report(self.started_at.elapsed())
    }
}

impl Drop for Progress<'_> {
    fn drop(&mut self) {
        if let Some(control) = self.control {
            control.record_report(&self.finish());
        }
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
    /// change. A call `answered` without running, such as a repeated or
    /// deferred `read` or a call to an unavailable tool, and a malformed call
    /// change nothing.
    fn record(
        &mut self,
        call: &ToolCall,
        result_index: usize,
        (result_bytes, result_digest): (usize, u64),
        answered: bool,
    ) {
        match Self::key(call) {
            _ if answered => {}
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

/// The handlers one turn offers, by name, in advertised order.
#[derive(Default)]
struct Tools {
    handlers: Vec<(String, Arc<dyn ToolHandler>)>,
}

impl Tools {
    /// Returns the handler of the tool named `name`, if the turn offers it.
    fn handler(&self, name: &str) -> Option<&Arc<dyn ToolHandler>> {
        self.handlers
            .iter()
            .find_map(|(handler_name, handler)| (handler_name == name).then_some(handler))
    }

    /// Returns the handler of the tool `call` names exactly; a name
    /// rewritten for replay never runs one.
    fn handler_for(&self, call: &ToolCall) -> Option<&Arc<dyn ToolHandler>> {
        if call.unknown_tool().is_some_and(UnknownToolCall::renamed) {
            return None;
        }

        self.handler(call.name())
    }
}

/// How a `read` call is answered without running it.
#[derive(Clone, Copy)]
enum ReadShortcut<'a> {
    /// The call repeats the named earlier call, whose result is unchanged.
    Repeat(&'a str),
    /// The round's earlier results already fill its share.
    Defer,
}

/// How much of the calibrated budget the `read` results of one tool round
/// take as they run. One result may weigh at most
/// [`context::RESULT_CAP_PERCENT`] of the capacity and is cut to fit as it
/// runs, so it enters the request already bounded. The round's results
/// together may weigh [`context::LATEST_ROUND_CAP_PERCENT`]: the first
/// `read` result that would exceed it, and every `read` after it, is
/// replaced by the exact call that fetches it in a later response. Results
/// of other tools count toward the share but are always kept.
struct RoundShare {
    /// Whether a `read` of this round no longer fits the share.
    deferring: bool,
    estimator: Arc<dyn ContextEstimator>,
    result_cap: u64,
    round_cap: u64,
    used: u64,
}

impl RoundShare {
    fn new(estimator: Arc<dyn ContextEstimator>, budget: context::ContextBudget) -> Self {
        let capacity = budget.max_request_weight().get() - budget.reserved_output_weight();

        Self {
            deferring: false,
            estimator,
            result_cap: capacity.saturating_mul(context::RESULT_CAP_PERCENT) / 100,
            round_cap: capacity.saturating_mul(context::LATEST_ROUND_CAP_PERCENT) / 100,
            used: 0,
        }
    }

    /// Returns whether a `read` result for `call` weighs at most the share
    /// one result may take.
    fn result_fits(&self, call: &ToolCall) -> SharedResultFits {
        let estimator = Arc::clone(&self.estimator);
        let call_id = call.id().to_string();
        let name = call.name().to_string();
        let result_cap = self.result_cap;

        Arc::new(move |content: &str| {
            result_weight(estimator.as_ref(), &call_id, &name, content) <= result_cap
        })
    }

    /// Counts the `result` of `call` toward the round and returns whether
    /// the round keeps it: always, except the result of a `read` that `ran`,
    /// once the round's results would exceed the share, after which later
    /// reads are deferred.
    fn keeps(&mut self, call: &ToolCall, result: &str, ran: bool) -> bool {
        let used = self.used.saturating_add(self.weight(call, result));
        if ran && used > self.round_cap {
            self.deferring = true;

            return false;
        }
        self.used = used;

        true
    }

    /// Weighs `content` as the result message the request records for
    /// `call`, under that call's own tool name.
    fn weight(&self, call: &ToolCall, content: &str) -> u64 {
        result_weight(self.estimator.as_ref(), call.id(), call.name(), content)
    }
}

/// Weighs `content` as the result message of call `call_id` to `name`.
fn result_weight(
    estimator: &dyn ContextEstimator,
    call_id: &str,
    name: &str,
    content: &str,
) -> u64 {
    estimator.message_weight(&ModelMessage::ToolResult {
        call_id: call_id.to_string(),
        content: content.to_string(),
        name: name.to_string(),
    })
}

enum ToolExecution<'a> {
    /// A `read` whose round had no room for its result, answered with the
    /// exact call that fetches it, without running.
    DeferredRead(&'a ReadArguments),
    /// A call the turn's handler of its name runs.
    Handler(&'a Arc<dyn ToolHandler>, ToolContext),
    /// A call whose arguments its tool could not decode, answered with an
    /// error result that lists the tool's argument fields.
    Malformed(&'a MalformedArguments, &'a TurnOptions),
    /// A `read` repeating the named earlier call, answered without running.
    RepeatedRead(&'a ReadArguments, &'a str),
}

async fn execute_tool(
    execution: ToolExecution<'_>,
    call: &ToolCall,
) -> Result<(String, ToolActivity), TurnError> {
    let started_at = Instant::now();

    match execution {
        ToolExecution::Handler(handler, context) => {
            run_handler(handler.as_ref(), call, context, started_at).await
        }
        ToolExecution::DeferredRead(arguments) => Ok((
            context::deferred_read_result(arguments),
            ToolActivity::ReadDeferred {
                action: arguments.action(),
                duration: started_at.elapsed(),
                summary: sanitize_report_text(read_summary(arguments)),
            },
        )),
        ToolExecution::Malformed(arguments, options) => Ok((
            malformed_arguments_result(arguments, options),
            ToolActivity::MalformedArguments {
                duration: started_at.elapsed(),
                reason: sanitize_report_text(arguments.reason()),
                tool: arguments.builtin().into(),
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
    }
}

/// Runs `call` with `handler`. A built-in handler reports its own result
/// and activity. A host handler's result is bounded: arguments that are not
/// a JSON object and a rejection are answered with an error result, and an
/// oversized result is dropped. A panic fails the turn as a tool failure.
async fn run_handler(
    handler: &dyn ToolHandler,
    call: &ToolCall,
    tool_context: ToolContext,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let name = call.name();
    let arguments = match call.arguments_value() {
        Ok(arguments) => arguments,
        Err(reason) => {
            return Ok(rejected_call(
                name,
                HostToolOutcome::InvalidArguments,
                &reason,
                started_at,
            ));
        }
    };
    let result = CatchUnwind(handler.call(arguments, tool_context))
        .await
        .map_err(|_| TurnError::ToolFailed {
            source: "the handler panicked".into(),
            tool: name.to_string(),
        })?;
    match result {
        Ok(output) => Ok(match output.into_parts() {
            (content, Some(activity)) => (content, activity),
            (content, None) if content.len() > MAX_TOOL_RESULT_BYTES => (
                oversized_result(content.len()),
                host_activity(name, HostToolOutcome::ResultTooLarge, started_at),
            ),
            (content, None) => (
                content,
                host_activity(name, HostToolOutcome::Completed, started_at),
            ),
        }),
        Err(error) => match error.kind {
            ToolErrorKind::Rejected(feedback) => Ok(rejected_call(
                name,
                HostToolOutcome::Rejected,
                &feedback,
                started_at,
            )),
            ToolErrorKind::Failed(source) => Err(TurnError::ToolFailed {
                source,
                tool: name.to_string(),
            }),
            ToolErrorKind::Turn(error) => Err(*error),
        },
    }
}

/// Polls a tool call, resolving to the panic that interrupted it instead
/// of unwinding through the turn.
struct CatchUnwind<'a, T>(Pin<Box<dyn Future<Output = T> + Send + 'a>>);

impl<T> Future for CatchUnwind<'_, T> {
    type Output = std::thread::Result<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let future = &mut self.0;
        match panic::catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(context))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    }
}

/// Reports a host tool call to `name` answered with `outcome`.
fn host_activity(name: &str, outcome: HostToolOutcome, started_at: Instant) -> ToolActivity {
    ToolActivity::Host {
        duration: started_at.elapsed(),
        outcome,
        tool: name.to_string(),
    }
}

/// Answers a host tool call to `name` that did not run, or that its handler
/// rejected, with an error result naming `reason`.
fn rejected_call(
    name: &str,
    outcome: HostToolOutcome,
    reason: &str,
    started_at: Instant,
) -> (String, ToolActivity) {
    let result = json!({
        "error": schema_contract::bounded_diagnostic(reason),
        "status": "rejected",
    })
    .to_string();

    (result, host_activity(name, outcome, started_at))
}

/// Tells the model a host tool ran but its `bytes`-byte result was
/// dropped, so it does not repeat the call's effects to see it.
fn oversized_result(bytes: usize) -> String {
    json!({
        "error": format!(
            "the tool ran, but its {bytes}-byte result is over the {MAX_TOOL_RESULT_BYTES}-byte \
             limit and was dropped"
        ),
        "hint": "The call's effects already happened; do not repeat it to see the result. Ask \
                 for less output if the tool supports it.",
        "status": OUTPUT_TOO_LARGE_STATUS,
    })
    .to_string()
}

/// Counts the malformed calls of one response and the run of responses
/// made only of malformed calls, and fails the turn when that run reaches
/// [`MAX_CONSECUTIVE_MALFORMED_TOOL_RESPONSES`]. A malformed call is
/// journaled and answered with an error result like any other call; only a
/// model that sends nothing else for several responses in a row fails the
/// turn, unjournaled. A host tool call whose arguments are not a JSON
/// object counts as malformed too. A malformed call to a tool `tools` does
/// not enable is answered as unavailable and bounded by the repetition guard
/// instead.
fn count_malformed(
    calls: &[ToolCall],
    tools: &Tools,
    consecutive: &mut usize,
) -> Result<usize, TurnError> {
    let malformed = calls
        .iter()
        .filter_map(|call| malformed_call(call, tools))
        .collect::<Vec<_>>();
    if malformed.len() < calls.len() {
        *consecutive = 0;
    } else {
        *consecutive += 1;
        if *consecutive == MAX_CONSECUTIVE_MALFORMED_TOOL_RESPONSES {
            let (name, reason) = &malformed[0];

            return Err(ModelError::InvalidToolArguments {
                reason: schema_contract::bounded_diagnostic(format!("`{name}`: {reason}")),
            }
            .into());
        }
    }

    Ok(malformed.len())
}

/// Returns the tool name and decoding failure of `call` when it calls a
/// tool `tools` offers with arguments that tool cannot decode.
fn malformed_call(call: &ToolCall, tools: &Tools) -> Option<(String, String)> {
    if let Some(arguments) = call.malformed_arguments() {
        let name = arguments.name();

        return tools
            .handler(name)
            .map(|_| (name.to_string(), arguments.reason().to_string()));
    }
    call.unknown_tool()?;
    tools.handler_for(call)?;

    call.arguments_value()
        .err()
        .map(|reason| (call.name().to_string(), reason))
}

/// Renders the bounded feedback that re-requests a rejected final output.
fn output_feedback(error: &ModelError) -> String {
    format!(
        "[harness] Your previous final response was rejected ({}). Respond with only a single \
         JSON value that matches the output schema, with no prose or Markdown around it.",
        schema_contract::bounded_diagnostic(error)
    )
}

/// Renders the error result that answers a call to `name`, a tool the turn
/// does not make available, listing the tools it does.
fn unavailable_tool_result(name: &str, options: &TurnOptions) -> String {
    let available = context::advertised_tools(options)
        .iter()
        .map(|tool| format!("`{}`", tool.name()))
        .collect::<Vec<_>>();
    let hint = if available.is_empty() {
        "This call did not run. No tools are available in this turn; give your final response."
            .to_string()
    } else {
        format!(
            "This call did not run. Call only the available tools ({}) with their documented \
             arguments, or give your final response.",
            available.join(", ")
        )
    };

    json!({
        "error": format!("tool `{name}` is not available in this turn"),
        "hint": hint,
        "status": context::UNAVAILABLE_STATUS,
    })
    .to_string()
}

/// Renders the error result that answers a call with malformed arguments,
/// naming the decoding failure and every argument field of the called tool.
fn malformed_arguments_result(arguments: &MalformedArguments, options: &TurnOptions) -> String {
    let name = arguments.name();
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
