//! Natural-language chat replies to a graph output awaiting acceptance.
//!
//! While a pending node of a live graph waits for a predecessor's output to
//! be accepted, a composer message is a reply to that output. The reply is
//! shown in chat, a read-only utility run on the Run Worker decides whether it
//! accepts the output or asks for a revision, and the decided action is
//! admitted through the command gateway for the exact inspected output version
//! and execution. Only one reply per output is decided at a time.
//!
//! Acceptance cannot carry images, so a reply with attachments always
//! revises without a decision run. A producer without a task conversation, or
//! whose output already reached a started dependent, cannot take revision
//! guidance: chat can only accept its output, and a reply with attachments is
//! rejected before echoing and keeps the draft.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, PoisonError};

use ag_session::{
    ContinueGraphNodeRequest, GraphExecutionRef, GraphNode, GraphNodeContinuation, GraphResultRef,
    SessionError,
};
use uuid::Uuid;

use crate::app::App;
use crate::app::graph_output_decision::{GraphOutputReply, GraphOutputReplyDecision};
use crate::app::prompt_intent::PromptWorkflowOutcome;
use crate::app::session::SessionTaskService;
use crate::domain::session::SessionId;
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;
use crate::presentation::execution_graph::{
    GraphViewOutput, outputs_awaiting_acceptance, takes_revision_guidance,
};

/// Run purpose recorded for the reply decision.
const DECISION_RUN_PURPOSE: &str = "graph output reply decision";

/// Validated output a pending dependent waits to see accepted.
struct AwaitingOutput {
    /// Display text of the exact output version.
    body: String,
    /// Producer's latest execution, the target of revision guidance.
    execution: GraphExecutionRef,
    /// Graph revision the inspected snapshot belongs to.
    graph_revision: NonZeroU64,
    /// Producing node definition.
    node: GraphNode,
    /// Exact output version acceptance targets.
    reference: GraphResultRef,
    /// Whether the producer can still take revision guidance, with the same
    /// restrictions as the inspector's reply action.
    takes_revision_guidance: bool,
}

impl App {
    /// Routes one composer message to the output a pending graph node waits
    /// to see accepted.
    ///
    /// Returns `None` when no output awaits acceptance, so the caller
    /// continues ordinary routing. Rejections keep the composer and its
    /// draft open; an accepted reply is decided in the background.
    pub(crate) async fn submit_graph_output_reply(
        &mut self,
        session_id: &SessionId,
        prompt: &TurnPrompt,
    ) -> Option<PromptWorkflowOutcome> {
        let output = match self.awaiting_graph_output(session_id).await {
            Ok(output) => output?,
            Err(error) => {
                return Some(self.reject_graph_output_reply(session_id, &error).await);
            }
        };

        Some(self.reply_to_graph_output(session_id, output, prompt).await)
    }

    /// Finds the first awaited output, in declaration order, of the most
    /// recently created graph that has one. Paused and cancelled graphs are
    /// skipped.
    async fn awaiting_graph_output(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<AwaitingOutput>, String> {
        let repository = self.services.db().graphs();
        let graphs = repository
            .list_graphs(session_id)
            .await
            .map_err(|error| error.to_string())?;
        for graph in graphs.iter().rev() {
            let Some(progress) = self
                .execution_graph_progress(&graph.id)
                .await
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
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

            return Ok(Some(AwaitingOutput {
                body: GraphViewOutput::new(result.clone(), body)
                    .display()
                    .to_string(),
                execution: execution.clone(),
                graph_revision: graph.revision,
                node: node.clone(),
                reference: result.reference.clone(),
                takes_revision_guidance: takes_revision_guidance(&graph.nodes, &progress, &node.id),
            }));
        }

        Ok(None)
    }

    /// Echoes one reply in chat and decides it in the background. A reply
    /// with attachments to a producer that cannot take revision guidance is
    /// rejected up front and keeps the composer open.
    async fn reply_to_graph_output(
        &mut self,
        session_id: &SessionId,
        output: AwaitingOutput,
        prompt: &TurnPrompt,
    ) -> PromptWorkflowOutcome {
        let Some((folder, transcript)) = self
            .sessions
            .session_and_handles_or_err(session_id)
            .ok()
            .map(|(session, handles)| (session.folder.clone(), Arc::clone(&handles.transcript)))
        else {
            return PromptWorkflowOutcome::KeepPrompt;
        };
        if !prompt.attachments.is_empty() && !output.takes_revision_guidance {
            return self
                .reject_graph_output_reply(
                    session_id,
                    &format!(
                        "`{}` cannot take images as revision guidance. Remove them to accept its \
                         output in chat, or use `w`.",
                        output.node.id
                    ),
                )
                .await;
        }
        if !self
            .services
            .output_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(output.reference.id.clone())
        {
            return self
                .reject_graph_output_reply(
                    session_id,
                    &format!(
                        "Still deciding on your previous reply to `{}`. Wait for it to finish.",
                        output.node.id
                    ),
                )
                .await;
        }
        SessionTaskService::append_user_prompt(
            &transcript,
            self.services.db(),
            &self.services.event_sender(),
            &self.services.session_update_versions(),
            session_id,
            &prompt.transcript_text(),
        )
        .await;
        let decision = GraphOutputReplyDecision {
            agent: output.node.agent,
            folder,
            output: format!(
                "Node `{}` ({}) produced output `{}`:\n\n{}",
                output.node.id, output.node.objective, output.reference.output_name, output.body
            ),
            reply: prompt.text.clone(),
        };
        self.spawn_graph_output_reply_decision(session_id, decision, output, prompt, transcript);

        PromptWorkflowOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    }

    /// Decides with the producer's captured selection, then admits the
    /// action for the exact inspected output and execution. An output or
    /// execution that changed meanwhile rejects the action instead of
    /// retargeting it.
    fn spawn_graph_output_reply_decision(
        &self,
        session_id: &SessionId,
        decision: GraphOutputReplyDecision,
        output: AwaitingOutput,
        prompt: &TurnPrompt,
        transcript: Arc<Mutex<SessionTranscript>>,
    ) {
        let run_client = ag_worker::scoped_client(
            self.services.run_client(),
            ag_worker::RunScope {
                purpose: Some(DECISION_RUN_PURPOSE.to_string()),
                session_id: Some(session_id.to_string()),
                ..ag_worker::RunScope::default()
            },
        );
        let handle = self.sessions.handle();
        let services = self.services.clone();
        let session_id = session_id.clone();
        let attachments = prompt.attachments.clone();
        tokio::spawn(async move {
            let node_id = output.node.id.clone();
            let reply = if attachments.is_empty() {
                decision.decide(run_client.as_ref()).await
            } else {
                GraphOutputReply::Revise
            };
            let result = match reply {
                GraphOutputReply::Accept => handle
                    .accept_execution_graph_result(output.reference.clone())
                    .await
                    .map(|()| {
                        format!(
                            "Accepted `{}` from `{node_id}` · continuing",
                            output.reference.output_name
                        )
                    }),
                GraphOutputReply::Revise if !output.takes_revision_guidance => {
                    Err(SessionError::Operation(format!(
                        "`{node_id}` cannot take revision guidance. Reply with clear acceptance, \
                         or accept or cancel it from `w`."
                    )))
                }
                GraphOutputReply::Revise => handle
                    .continue_execution_graph_node(ContinueGraphNodeRequest {
                        action_id: Uuid::new_v4().to_string(),
                        execution: output.execution.clone(),
                        graph_id: output.reference.graph_id.clone(),
                        kind: GraphNodeContinuation::Reply {
                            attachments,
                            message: decision.reply.clone(),
                        },
                        node_id: node_id.clone(),
                        revision: output.graph_revision,
                    })
                    .await
                    .map(|()| format!("Revision guidance sent to `{node_id}` · continuing")),
            };
            services
                .output_reply_decisions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&output.reference.id);
            let notice = result.unwrap_or_else(|error| {
                TranscriptNotice::ReplyError.format(format!("Graph output: {error}"))
            });
            SessionTaskService::append_workflow_notice(
                &transcript,
                services.db(),
                &services.event_sender(),
                &services.session_update_versions(),
                &session_id,
                &notice,
            )
            .await;
        });
    }

    /// Reports a rejected reply and keeps the composer open.
    async fn reject_graph_output_reply(
        &self,
        session_id: &SessionId,
        message: &str,
    ) -> PromptWorkflowOutcome {
        self.append_prompt_status_line(
            session_id,
            TranscriptNotice::ReplyError,
            &format!("Graph output: {message}"),
        )
        .await;

        PromptWorkflowOutcome::KeepPrompt
    }
}

#[cfg(test)]
#[path = "graph_output_reply_test.rs"]
mod tests;
