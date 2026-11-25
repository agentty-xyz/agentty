//! Natural-language chat replies to a graph output awaiting acceptance.
//!
//! While a pending node of a live or paused graph waits for a predecessor's
//! output to be accepted, a chat message, from the composer or the session
//! API, is a reply to that output. A paused graph is resumed first, so the
//! reply never runs as an ordinary turn outside the graph. The reply is
//! recorded once with its visible prompt, bound to the exact inspected output
//! version and execution. A read-only utility run on the Run Worker decides
//! whether it accepts the output or asks for a revision; the first decision is
//! durable, and its action is admitted through the command gateway. Recovery
//! after a restart or a project switch decides or applies every reply whose
//! action was not attempted, resuming its paused graph first. Only one reply
//! per output is decided at a time.
//!
//! Acceptance cannot carry images, so a reply with attachments always
//! revises without a decision run. A producer without a task conversation, or
//! whose output already reached a started dependent, cannot take revision
//! guidance: chat can only accept its output, and a reply with attachments is
//! rejected before it is recorded and keeps the draft.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus,
    GraphControlRequest, GraphExecutionRef, GraphNode, GraphNodeContinuation,
    GraphOutputReplyOutcome, GraphResult, GraphResultRef, ResumeExecutionGraphRequest,
    SessionError, SessionMessageKind,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::app::command_gateway::{
    CommandDelivery, CommandInitiator, SessionCommand, SessionMutation,
};
use crate::app::graph_output_decision::GraphOutputReplyDecision;
use crate::app::graph_reply::GraphReplyOutcome;
use crate::app::graph_reply_decision::ReplyDecision;
use crate::app::session::SessionTaskService;
use crate::app::{App, AppServices, SessionRuntimeHandle, spawn};
use crate::domain::session::SessionId;
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::GraphOutputReplyRecord;
use crate::presentation::execution_graph::{
    GraphViewOutput, is_focused_review_graph, outputs_awaiting_acceptance, takes_revision_guidance,
};

/// Run purpose recorded for the reply decision.
const DECISION_RUN_PURPOSE: &str = "graph output reply decision";

/// Validated output a chat reply acts on, as inspected with that reply.
struct AwaitingOutput {
    /// Display text of the exact output version.
    body: String,
    /// Producer execution that revision guidance targets.
    execution: GraphExecutionRef,
    /// No live driver dispatches the graph, so acting on the output needs a
    /// resume first.
    is_paused: bool,
    /// Producing node definition.
    node: GraphNode,
    /// Exact output version acceptance targets.
    reference: GraphResultRef,
    /// Whether the producer can still take revision guidance, with the same
    /// restrictions as the inspector's reply action.
    takes_revision_guidance: bool,
}

impl AwaitingOutput {
    /// Captures one output version of `node` from a graph snapshot.
    fn new(
        graph: &ExecutionGraph,
        progress: &ExecutionGraphProgress,
        node: &GraphNode,
        execution: GraphExecutionRef,
        result: GraphResult,
        body: String,
    ) -> Self {
        Self {
            execution,
            is_paused: progress.status == ExecutionGraphStatus::Paused,
            node: node.clone(),
            reference: result.reference.clone(),
            takes_revision_guidance: takes_revision_guidance(&graph.nodes, progress, &node.id),
            body: GraphViewOutput::new(result, body).display().to_string(),
        }
    }

    /// Addresses the graph that owns the output at its inspected revision.
    fn graph_control(&self) -> GraphControlRequest {
        GraphControlRequest {
            graph_id: self.reference.graph_id.clone(),
            revision: self.reference.graph_revision,
        }
    }
}

impl App {
    /// Routes one chat message to the output a pending graph node waits to
    /// see accepted.
    ///
    /// Returns `None` when no output awaits acceptance, so the caller
    /// continues ordinary routing. A rejected reply is not recorded; an
    /// accepted reply is decided in the background.
    pub(crate) async fn submit_graph_output_reply(
        &mut self,
        session_id: &SessionId,
        prompt: &TurnPrompt,
    ) -> Option<GraphReplyOutcome> {
        let output = match self.awaiting_graph_output(session_id).await {
            Ok(output) => output?,
            Err(error) => {
                return Some(self.reject_graph_output_reply(session_id, &error).await);
            }
        };

        Some(self.reply_to_graph_output(session_id, output, prompt).await)
    }

    /// Decides or applies the unsettled output replies of the active page's
    /// sessions after a restart or a project switch, at most one per output.
    /// A decided reply reapplies its recorded decision instead of deciding
    /// again.
    pub(crate) async fn recover_graph_output_replies(&mut self) {
        let session_ids: Vec<SessionId> = self
            .sessions
            .sessions()
            .iter()
            .map(|session| session.id.clone())
            .collect();
        for session_id in session_ids {
            // A failed read leaves the replies unsettled for the next start.
            let replies = self
                .services
                .db()
                .graphs()
                .unsettled_output_replies(session_id.as_str())
                .await
                .unwrap_or_default();
            for reply in replies {
                if self.is_deciding_graph_output_reply(&reply.reference.id) {
                    continue;
                }
                if let Some(output) = self.replied_graph_output(&reply).await
                    && let Some((folder, transcript)) = self.reply_session_handles(&session_id)
                {
                    self.spawn_graph_output_reply_decision(
                        &session_id,
                        reply,
                        output,
                        folder,
                        transcript,
                    );
                }
            }
        }
    }

    /// Returns whether an earlier reply to the output is still being decided.
    fn is_deciding_graph_output_reply(&self, result_id: &str) -> bool {
        self.services
            .output_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(result_id)
    }

    /// Finds the first awaited output, in declaration order, of the most
    /// recently created graph that has one. Cancelled graphs are skipped.
    ///
    /// A paused graph counts only while it is the session's newest working
    /// graph, the one the sidebar shows: its output still waits on the user.
    /// An older paused graph the user moved on from never captures a message,
    /// because the reply would resume it. Focused reviews are not working
    /// graphs, so a review that ran since does not hide the paused graph.
    async fn awaiting_graph_output(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<AwaitingOutput>, String> {
        let repository = self.services.db().graphs();
        let graphs = repository
            .list_graphs(session_id)
            .await
            .map_err(|error| error.to_string())?;
        let mut is_newest_working_graph = true;
        for graph in graphs.iter().rev() {
            let may_resume = is_newest_working_graph;
            is_newest_working_graph &= is_focused_review_graph(graph);
            let Some(progress) = self
                .execution_graph_progress(&graph.id)
                .await
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            if progress.status == ExecutionGraphStatus::Paused && !may_resume {
                continue;
            }
            let awaiting = outputs_awaiting_acceptance(&progress).into_iter().find_map(
                |(producer, result)| {
                    let node = graph
                        .nodes
                        .iter()
                        .find(|node| node.id == producer.node_id)?;

                    Some((node, producer.executions.last()?, result))
                },
            );
            let Some((node, execution, result)) = awaiting else {
                continue;
            };
            let body = repository
                .load_result(&result.reference.id)
                .await
                .map_err(|error| error.to_string())?
                .map_or_default(|stored| stored.body);

            return Ok(Some(AwaitingOutput::new(
                graph,
                &progress,
                node,
                execution.clone(),
                result.clone(),
                body,
            )));
        }

        Ok(None)
    }

    /// Reads the output a recorded reply answered, with the execution its
    /// revision guidance was bound to. A failed or empty read leaves the reply
    /// unsettled for the next recovery.
    async fn replied_graph_output(&self, reply: &GraphOutputReplyRecord) -> Option<AwaitingOutput> {
        let repository = self.services.db().graphs();
        let graph = repository
            .load_graph(&reply.reference.graph_id, None)
            .await
            .ok()??;
        let progress = self.execution_graph_progress(&graph.id).await.ok()??;
        let node = graph
            .nodes
            .iter()
            .find(|node| node.id == reply.reference.node_id)?;
        let stored = repository.load_result(&reply.reference.id).await.ok()??;

        Some(AwaitingOutput::new(
            &graph,
            &progress,
            node,
            reply.execution.clone(),
            stored.result,
            stored.body,
        ))
    }

    /// Records one reply with its visible prompt and decides it in the
    /// background. A reply with attachments to a producer that cannot take
    /// revision guidance is rejected before it is recorded, as is a reply to
    /// a paused graph that cannot be resumed.
    async fn reply_to_graph_output(
        &mut self,
        session_id: &SessionId,
        mut output: AwaitingOutput,
        prompt: &TurnPrompt,
    ) -> GraphReplyOutcome {
        let Some((folder, transcript)) = self.reply_session_handles(session_id) else {
            return GraphReplyOutcome::Rejected(SessionError::NotFound);
        };
        let node_id = output.node.id.clone();
        if !prompt.attachments.is_empty() && !output.takes_revision_guidance {
            return self
                .reject_graph_output_reply(
                    session_id,
                    &format!(
                        "`{node_id}` cannot take images as revision guidance. Remove them to \
                         accept its output in chat, or use `w`."
                    ),
                )
                .await;
        }
        if self.is_deciding_graph_output_reply(&output.reference.id) {
            return self
                .reject_graph_output_reply(
                    session_id,
                    &format!(
                        "Still deciding on your previous reply to `{node_id}`. Wait for it to \
                         finish."
                    ),
                )
                .await;
        }
        if output.is_paused {
            if let Err(error) = self.resume_paused_graph(&output).await {
                return self
                    .reject_graph_output_reply(session_id, &resume_failure(&node_id, &error))
                    .await;
            }
            output.is_paused = false;
        }
        let reply = GraphOutputReplyRecord {
            attachments: prompt.attachments.clone(),
            execution: output.execution.clone(),
            id: Uuid::new_v4().to_string(),
            message: prompt.text.clone(),
            outcome: None,
            reference: output.reference.clone(),
        };
        if let Err(error) = self
            .services
            .db()
            .graphs()
            .record_output_reply(&reply)
            .await
        {
            return self
                .reject_graph_output_reply(session_id, &error.to_string())
                .await;
        }
        SessionTaskService::mirror_session_transcript_message(
            &transcript,
            &self.services.event_sender(),
            &self.services.session_update_versions(),
            session_id,
            SessionMessageKind::UserPrompt,
            &prompt.transcript_text(),
        );
        self.spawn_graph_output_reply_decision(session_id, reply, output, folder, transcript);

        GraphReplyOutcome::Accepted
    }

    /// Restarts dispatch of the paused graph that owns `output`, through the
    /// same admission as the inspector's resume control. The reply is the
    /// user's explicit request to continue, so it does not replay anything on
    /// its own: nodes run only from what the graph already admitted.
    async fn resume_paused_graph(&mut self, output: &AwaitingOutput) -> Result<(), SessionError> {
        self.admit_session_command(SessionCommand::new(
            CommandInitiator::SessionView,
            CommandDelivery::Admission {
                request: SessionMutation::ResumeExecutionGraph {
                    request: ResumeExecutionGraphRequest {
                        graph: output.graph_control(),
                    },
                },
                response_tx: None,
            },
        ))
        .await
    }

    /// Decides with the producer's captured selection, session ownership, and
    /// a bounded provider budget, then admits the action for the exact
    /// replied-to output and execution. An output or execution that changed
    /// meanwhile rejects the action instead of retargeting it. Stopping the
    /// session abandons a reply whose action was not attempted, decided or
    /// not; shutdown leaves it for restart recovery.
    fn spawn_graph_output_reply_decision(
        &self,
        session_id: &SessionId,
        reply: GraphOutputReplyRecord,
        output: AwaitingOutput,
        folder: PathBuf,
        transcript: Arc<Mutex<SessionTranscript>>,
    ) {
        let cancellation = CancellationToken::new();
        let is_abandoned = Arc::new(AtomicBool::new(false));
        self.services
            .output_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                reply.reference.id.clone(),
                ReplyDecision {
                    cancellation: cancellation.clone(),
                    is_abandoned: Arc::clone(&is_abandoned),
                    session_id: session_id.clone(),
                },
            );
        let decision = GraphOutputReplyDecision {
            agent: output.node.agent,
            folder,
            output: format!(
                "Node `{}` ({}) produced output `{}`:\n\n{}",
                output.node.id, output.node.objective, output.reference.output_name, output.body
            ),
            reply: reply.message.clone(),
        };
        let run_client = ag_worker::scoped_client(
            self.services.run_client(),
            ag_worker::RunScope {
                cancellation: Some(cancellation.clone()),
                purpose: Some(DECISION_RUN_PURPOSE.to_string()),
                session_id: Some(session_id.to_string()),
                usage_purpose: Some(ag_contracts::UsagePurpose::Other),
                ..ag_worker::RunScope::default()
            },
        );
        let handle = self.sessions.handle();
        let services = self.services.clone();
        let session_id = session_id.clone();
        let status = self
            .sessions
            .session_handles_or_err(&session_id)
            .ok()
            .map(|handles| Arc::clone(&handles.status));
        let activity = services.begin_graph_activity(&session_id, status);
        self.services
            .track_cleanup_task(spawn::spawn_detached(async move {
                activity.record(&services).await;
                let outcome = match reply.outcome {
                    Some(outcome) => outcome,
                    None if reply.attachments.is_empty() => {
                        decision.decide(run_client.as_ref()).await
                    }
                    None => GraphOutputReplyOutcome::Revise,
                };
                let notice = if cancellation.is_cancelled() {
                    None
                } else {
                    services
                        .apply_output_reply(&handle, &cancellation, &reply, &output, outcome)
                        .await
                };
                let notice = match notice {
                    // The action was not attempted and the session was
                    // stopped, whether or not a decision was recorded.
                    None if is_abandoned.load(Ordering::Acquire) => {
                        // A failed write leaves the reply for restart recovery.
                        let _ = services.db().graphs().settle_output_reply(&reply.id).await;

                        Some(reply_failure(&format!(
                            "the reply decision for `{}` was canceled. Reply again to act on its \
                             output.",
                            output.node.id
                        )))
                    }
                    notice => notice,
                };
                services
                    .output_reply_decisions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&reply.reference.id);
                // A reply left for recovery reports once its action is
                // attempted.
                if let Some(notice) = notice {
                    SessionTaskService::append_workflow_notice(
                        &transcript,
                        services.db(),
                        &services.event_sender(),
                        &services.session_update_versions(),
                        &session_id,
                        &notice,
                    )
                    .await;
                }
                activity.finish(&services).await;
            }));
    }

    /// Reports a rejected reply in the session transcript and returns it.
    async fn reject_graph_output_reply(
        &self,
        session_id: &SessionId,
        message: &str,
    ) -> GraphReplyOutcome {
        let message = format!("Graph output: {message}");
        self.append_prompt_status_line(session_id, TranscriptNotice::ReplyError, &message)
            .await;

        GraphReplyOutcome::Rejected(SessionError::Operation(message))
    }
}

impl AppServices {
    /// Records the first decision and admits its action for the replied-to
    /// output, then settles the reply whether or not the attempted action
    /// succeeded, and returns the notice to show.
    ///
    /// The session runtime can stop before it answers. The action then was
    /// not attempted, so the reply stays unsettled and `None` is returned:
    /// recovery applies the recorded decision unless the caller abandons the
    /// reply with its stopped session. A reply recovered at startup waits for
    /// the runtime to accept commands; cancellation ends that wait.
    async fn apply_output_reply(
        &self,
        handle: &SessionRuntimeHandle,
        cancellation: &CancellationToken,
        reply: &GraphOutputReplyRecord,
        output: &AwaitingOutput,
        outcome: GraphOutputReplyOutcome,
    ) -> Option<String> {
        let graphs = self.db().graphs();
        // An earlier recorded decision wins over this one.
        let outcome = match graphs.decide_output_reply(&reply.id, outcome).await {
            Ok(recorded) => recorded,
            Err(error) => return Some(reply_failure(&error.to_string())),
        };
        tokio::select! {
            biased;
            () = handle.wait_until_available() => {}
            () = cancellation.cancelled() => return None,
        }
        let result = Self::act_on_graph_output(handle, reply, output, outcome).await;
        if result
            .as_ref()
            .is_err_and(SessionRuntimeHandle::is_unavailable)
        {
            return None;
        }
        let settled = graphs
            .settle_output_reply(&reply.id)
            .await
            .map_err(|error| error.to_string());

        Some(
            result
                .map_err(|error| error.to_string())
                .and_then(|success| settled.map(|()| success))
                .unwrap_or_else(|error| reply_failure(&error)),
        )
    }

    /// Admits the decided action for the exact replied-to output and
    /// execution, resuming a recovered reply's paused graph first, and
    /// returns the success notice.
    ///
    /// Revision guidance reuses the reply identity as its action identity,
    /// so redelivery after an interrupted settlement does not continue the
    /// node twice.
    async fn act_on_graph_output(
        handle: &SessionRuntimeHandle,
        reply: &GraphOutputReplyRecord,
        output: &AwaitingOutput,
        outcome: GraphOutputReplyOutcome,
    ) -> Result<String, SessionError> {
        let node_id = &output.node.id;
        if outcome == GraphOutputReplyOutcome::Revise && !output.takes_revision_guidance {
            return Err(SessionError::Operation(format!(
                "`{node_id}` cannot take revision guidance. Reply with clear acceptance, or \
                 accept or cancel it from `w`."
            )));
        }
        if output.is_paused {
            handle
                .resume_execution_graph(ResumeExecutionGraphRequest {
                    graph: output.graph_control(),
                })
                .await
                .map_err(|error| {
                    if SessionRuntimeHandle::is_unavailable(&error) {
                        error
                    } else {
                        SessionError::Operation(resume_failure(node_id, &error))
                    }
                })?;
        }
        match outcome {
            GraphOutputReplyOutcome::Accept => handle
                .accept_execution_graph_result(output.reference.clone())
                .await
                .map(|()| {
                    format!(
                        "Accepted `{}` from `{node_id}` · continuing",
                        output.reference.output_name
                    )
                }),
            GraphOutputReplyOutcome::Revise => handle
                .continue_execution_graph_node(ContinueGraphNodeRequest {
                    action_id: reply.id.clone(),
                    execution: reply.execution.clone(),
                    graph_id: reply.reference.graph_id.clone(),
                    kind: GraphNodeContinuation::Reply {
                        attachments: reply.attachments.clone(),
                        message: reply.message.clone(),
                    },
                    node_id: node_id.clone(),
                    revision: reply.reference.graph_revision,
                })
                .await
                .map(|()| format!("Revision guidance sent to `{node_id}` · continuing")),
        }
    }
}

/// Describes a paused graph that could not resume for a reply.
fn resume_failure(node_id: &str, error: &SessionError) -> String {
    format!(
        "the paused graph could not resume to act on `{node_id}`: {error}. Resume it from `w`, \
         then reply again."
    )
}

/// Describes a reply whose action was not applied.
fn reply_failure(error: &str) -> String {
    TranscriptNotice::ReplyError.format(format!("Graph output: {error}"))
}

#[cfg(test)]
#[path = "graph_output_reply_test.rs"]
mod tests;
