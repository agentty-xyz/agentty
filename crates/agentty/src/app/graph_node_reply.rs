//! Natural-language chat replies to a graph node awaiting answers.
//!
//! A graph node asks its clarification questions in session chat, where the
//! question panel answers that exact execution. While one waits, a composer
//! message is also that node's answer; when several wait, it answers the
//! question asked last. The reply is shown in chat and admitted through the
//! command gateway as a reply to the exact execution that asked, so a new
//! attempt of the same task conversation continues with it.

use std::sync::Arc;

use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraph, ExecutionGraphStatus, GraphContext,
    GraphExecutionRef, GraphNode, GraphNodeContinuation, GraphNodeStatus, QuestionItem,
};
use uuid::Uuid;

use crate::app::App;
use crate::app::command_gateway::{
    CommandDelivery, CommandInitiator, SessionCommand, SessionMutation,
};
use crate::app::graph_question::GraphAnswerOutcome;
use crate::app::prompt_intent::PromptWorkflowOutcome;
use crate::app::session::SessionTaskService;
use crate::domain::session::SessionId;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;

/// Rejection shown when answered questions no longer wait for a reply.
pub(crate) const QUESTIONS_NOT_PENDING: &str =
    "These questions were already answered or are no longer pending.";

/// Graph node whose latest execution waits for answers in chat.
struct AwaitingNode {
    execution: GraphExecutionRef,
    graph: ExecutionGraph,
    /// No live driver dispatches the answered node until an explicit resume.
    is_paused: bool,
    node_id: String,
    /// Questions the waiting execution asked.
    questions: Vec<QuestionItem>,
}

impl App {
    /// Routes one composer message to the graph node whose pending question
    /// was asked last.
    ///
    /// Returns `None` when no node waits, so the caller continues ordinary
    /// routing. Rejections keep the composer and its draft open.
    pub(crate) async fn submit_graph_node_reply(
        &mut self,
        session_id: &SessionId,
        prompt: &TurnPrompt,
    ) -> Option<PromptWorkflowOutcome> {
        let node = match self.awaiting_graph_node(session_id).await {
            Ok(node) => node?,
            Err(error) => {
                return Some(self.reject_graph_node_reply(session_id, &error).await);
            }
        };

        Some(self.reply_to_graph_node(session_id, node, prompt).await)
    }

    /// Returns the questions and asking execution of the node a chat reply
    /// would answer now.
    ///
    /// # Errors
    /// Returns an error when graph lookup fails.
    pub(crate) async fn pending_graph_node_questions(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<(GraphExecutionRef, Vec<QuestionItem>)>, String> {
        Ok(self
            .awaiting_graph_node(session_id)
            .await?
            .map(|node| (node.execution, node.questions)))
    }

    /// Sends answers to the exact node execution whose questions the user
    /// answered, rejecting them as stale once that execution no longer waits.
    pub(crate) async fn answer_graph_node_questions(
        &mut self,
        session_id: &SessionId,
        execution: &GraphExecutionRef,
        prompt: &TurnPrompt,
    ) -> GraphAnswerOutcome {
        let node = match self.awaiting_graph_nodes(session_id).await {
            Ok(nodes) => nodes.into_iter().find(|node| node.execution == *execution),
            Err(error) => {
                self.reject_graph_node_reply(session_id, &error).await;

                return GraphAnswerOutcome::Retry;
            }
        };
        let Some(node) = node else {
            self.reject_graph_node_reply(session_id, QUESTIONS_NOT_PENDING)
                .await;

            return GraphAnswerOutcome::Stale;
        };

        GraphAnswerOutcome::from_reply(&self.reply_to_graph_node(session_id, node, prompt).await)
    }

    /// Admits one reply for the node's asking execution and echoes it in
    /// chat.
    async fn reply_to_graph_node(
        &mut self,
        session_id: &SessionId,
        node: AwaitingNode,
        prompt: &TurnPrompt,
    ) -> PromptWorkflowOutcome {
        let request = ContinueGraphNodeRequest {
            action_id: Uuid::new_v4().to_string(),
            execution: node.execution,
            graph_id: node.graph.id.clone(),
            kind: GraphNodeContinuation::Reply {
                attachments: prompt.attachments.clone(),
                message: prompt.text.clone(),
            },
            node_id: node.node_id.clone(),
            revision: node.graph.revision,
        };
        if let Err(error) = self
            .admit_session_command(SessionCommand::new(
                CommandInitiator::SessionView,
                CommandDelivery::Admission {
                    request: SessionMutation::ContinueExecutionGraphNode {
                        request: Box::new(request),
                    },
                    response_tx: None,
                },
            ))
            .await
        {
            return self
                .reject_graph_node_reply(session_id, &error.to_string())
                .await;
        }
        if let Ok(handles) = self.sessions.session_handles_or_err(session_id) {
            let transcript = Arc::clone(&handles.transcript);
            SessionTaskService::append_user_prompt(
                &transcript,
                self.services.db(),
                &self.services.event_sender(),
                &self.services.session_update_versions(),
                session_id,
                &prompt.transcript_text(),
            )
            .await;
            SessionTaskService::append_workflow_notice(
                &transcript,
                self.services.db(),
                &self.services.event_sender(),
                &self.services.session_update_versions(),
                session_id,
                &if node.is_paused {
                    format!(
                        "Answer sent to `{}`. The graph is paused; resume it from `w` to continue.",
                        node.node_id
                    )
                } else {
                    format!("Answer sent to `{}` · continuing", node.node_id)
                },
            )
            .await;
        }
        self.graph_chat.invalidate();

        PromptWorkflowOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    }

    /// Finds the node whose unanswered chat question was asked last, so a
    /// reply answers the question the user most recently saw.
    ///
    /// Candidates are nodes whose latest task-conversation execution asked
    /// questions that no reply has answered yet. Questions without a recorded
    /// chat message rank before every recorded one, and ties keep graph
    /// creation and declaration order.
    async fn awaiting_graph_node(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<AwaitingNode>, String> {
        let mut candidates = self.awaiting_graph_nodes(session_id).await?;
        if candidates.len() < 2 {
            return Ok(candidates.pop());
        }
        let repository = self.services.db().graphs();
        let mut latest: Option<(Option<i64>, AwaitingNode)> = None;
        for candidate in candidates {
            let question = repository
                .task_question_message_id(&candidate.execution)
                .await
                .map_err(|error| error.to_string())?;
            if latest
                .as_ref()
                .is_none_or(|(latest_question, _)| question > *latest_question)
            {
                latest = Some((question, candidate));
            }
        }

        Ok(latest.map(|(_, candidate)| candidate))
    }

    /// Lists every node, in graph creation and declaration order, whose latest
    /// task-conversation execution waits for a chat answer.
    async fn awaiting_graph_nodes(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AwaitingNode>, String> {
        let graphs = self
            .services
            .db()
            .graphs()
            .list_graphs(session_id)
            .await
            .map_err(|error| error.to_string())?;
        let mut candidates = Vec::new();
        for graph in graphs {
            let asks_in_chat = |node: &GraphNode| {
                matches!(
                    node.execution.context,
                    GraphContext::TaskConversation { .. }
                )
            };
            if !graph.nodes.iter().any(asks_in_chat) {
                continue;
            }
            let Some(progress) = self
                .execution_graph_progress(&graph.id)
                .await
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            if progress.status == ExecutionGraphStatus::Cancelled {
                continue;
            }
            let is_paused = !self.has_live_graph_driver(&graph.id);
            let awaiting: Vec<_> = graph
                .nodes
                .iter()
                .zip(progress.nodes)
                .filter(|(definition, state)| {
                    asks_in_chat(definition) && state.status == GraphNodeStatus::AwaitingAnswers
                })
                .filter_map(|(_, mut state)| {
                    Some((state.executions.pop()?, state.node_id, state.questions))
                })
                .collect();
            candidates.extend(awaiting.into_iter().map(|(execution, node_id, questions)| {
                AwaitingNode {
                    execution,
                    graph: graph.clone(),
                    is_paused,
                    node_id,
                    questions,
                }
            }));
        }

        Ok(candidates)
    }

    /// Reports a rejected answer and keeps the composer open.
    async fn reject_graph_node_reply(
        &self,
        session_id: &SessionId,
        message: &str,
    ) -> PromptWorkflowOutcome {
        self.append_prompt_status_line(
            session_id,
            TranscriptNotice::ReplyError,
            &format!("Graph answer: {message}"),
        )
        .await;

        PromptWorkflowOutcome::KeepPrompt
    }
}

#[cfg(test)]
#[path = "graph_node_reply_test.rs"]
pub(crate) mod tests;
