//! Foreground admission for the Planner → accepted plan → Implementer graph.
//!
//! A task chain is a static execution graph. These handlers translate chain
//! actions into graph operations (launch, exact plan acceptance, explicit
//! continuation, and stage cancellation) and keep the graph's driver running.
//! Admission, gates, pinned handoff, and execution stay with the graph store,
//! executor, and session workers.

use std::collections::BTreeMap;

use ag_session::{
    AcceptTaskChainPlanRequest, ContinueGraphNodeRequest, ContinueTaskChainRequest,
    GraphExecutionRef, GraphNodeContinuation, SessionError, SessionId, SessionMessage,
    SessionMessageKind, SessionTranscript, StartExecutionGraphRequest,
    StartPlanThenImplementRequest, TASK_CHAIN_PLAN_OUTPUT, TaskAgentRole, TaskChain,
    TaskChainPhase,
};

use crate::app::App;
use crate::app::session::SessionTaskService;
use crate::infra::db::DbError;

impl App {
    /// Saves and starts only newly admitted workflows; matching launch
    /// redelivery is a receipt.
    ///
    /// New admission atomically records the graph with the original request
    /// and graph marker, then mirrors them into live chat before dispatch, so
    /// redelivery never duplicates them and a dispatch failure keeps the graph
    /// visible.
    pub(super) async fn start_api_task_chain(
        &mut self,
        session_id: &SessionId,
        mut request: StartPlanThenImplementRequest,
    ) -> Result<(), SessionError> {
        if let Some(existing) = self
            .services
            .db()
            .graphs()
            .load_graph(&request.chain_id, None)
            .await
            .map_err(|error| api_error(&error))?
        {
            return Self::require_matching_launch(&existing, session_id, &request);
        }
        self.require_task_chain_session_ready(session_id, TaskAgentRole::Planner)?;
        request.agents.planner = self
            .resolve_task_agent_definition(session_id, &request.agents.planner.id)
            .await?;
        request.agents.implementer = self
            .resolve_task_agent_definition(session_id, &request.agents.implementer.id)
            .await?;
        self.sessions
            .prepare_graph_workspace(&self.services, session_id)
            .await
            .map_err(|error| SessionError::Operation(error.to_string()))?;
        let conversation = self.task_chain_launch_conversation(session_id).await?;
        let graph = request.graph(session_id, conversation.as_deref());
        let admitted = self
            .services
            .db()
            .graphs()
            .start_task_chain(&graph, &request.user_request)
            .await
            .map_err(|error| api_error(&error))?;
        if !admitted {
            return Ok(());
        }
        self.auto_address_review_iterations.remove(session_id);
        self.sessions
            .initialize_task_chain_title(&self.services, session_id, &request.user_request)
            .await;
        self.mirror_task_chain_transcript_message(
            session_id,
            SessionMessageKind::UserPrompt,
            &request.user_request,
        );
        self.mirror_task_chain_transcript_message(
            session_id,
            SessionMessageKind::AgentGraph,
            &request.chain_id,
        );

        self.ensure_task_chain_driver(&request.chain_id).await
    }

    /// Resolves a definition through the injected catalog before task
    /// admission.
    pub(super) async fn resolve_task_agent_definition(
        &self,
        session_id: &SessionId,
        id: &str,
    ) -> Result<ag_session::AgentDefinition, SessionError> {
        let folder = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == *session_id)
            .map(|session| session.folder.clone())
            .ok_or(SessionError::NotFound)?;

        self.services
            .agent_profile_catalog_client()
            .resolve(folder, id.to_string())
            .await
            .ok_or_else(|| {
                SessionError::Operation(format!(
                    "Agent definition `{id}` is unavailable or invalid"
                ))
            })
    }

    /// Accepts the exact latest validated plan and releases the Implementer
    /// gate. Returns whether this call recorded the acceptance; repeating the
    /// same acceptance is a receipt and never resumes dispatch.
    pub(super) async fn accept_api_task_chain_plan(
        &mut self,
        chain_id: &str,
        request: AcceptTaskChainPlanRequest,
    ) -> Result<bool, SessionError> {
        let chain = self.require_task_chain(chain_id).await?;
        if let Some(plan) = &chain.accepted_plan {
            if plan.attempt_id != request.plan_attempt_id {
                return Err(SessionError::Operation(
                    "Workflow already accepted a different plan version".into(),
                ));
            }

            return Ok(false);
        }
        if chain.phase != TaskChainPhase::AwaitingPlanAcceptance
            || chain.current_attempt_id() != Some(request.plan_attempt_id.as_str())
        {
            return Err(SessionError::Operation(
                "Only the latest validated plan awaiting acceptance can be accepted".into(),
            ));
        }
        self.require_task_chain_session_ready(&chain.session_id, TaskAgentRole::Implementer)?;
        let reference = self
            .services
            .db()
            .graphs()
            .list_results(
                chain_id,
                TaskAgentRole::Planner.id(),
                TASK_CHAIN_PLAN_OUTPUT,
            )
            .await
            .map_err(|error| api_error(&error))?
            .into_iter()
            .map(|result| result.result.reference)
            .find(|reference| Some(&reference.execution) == chain.current_execution.as_ref())
            .ok_or_else(|| SessionError::Operation("Plan version is missing".into()))?;
        self.accept_api_execution_graph_result(reference).await?;
        self.auto_address_review_iterations
            .remove(&chain.session_id);
        self.ensure_task_chain_driver(chain_id).await?;

        Ok(true)
    }

    /// Records an explicit continuation of the current stage and dispatches
    /// it. Guidance becomes a graph reply; no guidance retries failed work.
    pub(super) async fn continue_api_task_chain(
        &mut self,
        chain_id: &str,
        request: ContinueTaskChainRequest,
    ) -> Result<(), SessionError> {
        let chain = self.require_task_chain(chain_id).await?;
        let progress = self
            .services
            .db()
            .graphs()
            .progress(chain_id)
            .await
            .map_err(|error| api_error(&error))?
            .ok_or(SessionError::NotFound)?;
        let admitted = progress.nodes.iter().any(|node| {
            node.executions
                .iter()
                .any(|execution| execution_attempt_id(execution) == Some(&request.attempt_id))
        });
        let (node_id, source) = progress
            .nodes
            .iter()
            .find_map(|node| {
                node.executions
                    .iter()
                    .find(|execution| {
                        execution_attempt_id(execution) == Some(&request.expected_attempt_id)
                    })
                    .map(|execution| (node.node_id.clone(), execution.clone()))
            })
            .ok_or_else(|| {
                SessionError::Operation("Continuation source attempt is missing".into())
            })?;
        let kind = match request.clarification {
            Some(message) => GraphNodeContinuation::Reply {
                attachments: request.attachments,
                message,
            },
            None if request.attachments.is_empty() => GraphNodeContinuation::Retry,
            None => {
                return Err(SessionError::Operation(
                    "Images require accompanying guidance".into(),
                ));
            }
        };
        if !admitted {
            self.require_task_chain_session_ready(&chain.session_id, chain.current_role)?;
        }
        self.continue_api_execution_graph_node(ContinueGraphNodeRequest {
            action_id: request.attempt_id,
            execution: source,
            graph_id: chain.id.clone(),
            kind,
            node_id,
            revision: chain.revision,
        })
        .await?;
        if admitted {
            return Ok(());
        }
        self.auto_address_review_iterations
            .remove(&chain.session_id);

        self.ensure_task_chain_driver(chain_id).await
    }

    /// Requests cancellation of the current stage's queued or running
    /// execution, plus any pending reply decision. The graph stays open, so
    /// the next explicit continuation can retry the canceled stage.
    pub(super) async fn cancel_api_task_chain(
        &mut self,
        chain_id: &str,
    ) -> Result<(), SessionError> {
        let chain = self.require_task_chain(chain_id).await?;
        if let Some(cancellation) = self.sessions.deciding_task_chain_ids.remove(chain_id) {
            cancellation.cancel();
        }
        let execution = self
            .services
            .db()
            .graphs()
            .cancel_task_chain_stage(&chain.id)
            .await
            .map_err(|error| api_error(&error))?;
        if let Some(GraphExecutionRef::TaskAttempt { operation_id, .. }) = &execution {
            self.sessions.cancel_agent_task_attempt(operation_id);
            self.services.wake_graphs();
            SessionTaskService::emit_session_updated(
                &self.services.event_sender(),
                &self.services.session_update_versions(),
                &chain.session_id,
            );
        }

        Ok(())
    }

    pub(super) fn require_task_chain_session_ready(
        &mut self,
        session_id: &SessionId,
        definition: TaskAgentRole,
    ) -> Result<(), SessionError> {
        self.sessions
            .validate_agent_task_admission(session_id, definition)
            .map_err(|error| match error {
                crate::app::SessionError::NotFound => SessionError::NotFound,
                other => SessionError::Operation(other.to_string()),
            })
    }

    pub(super) async fn require_task_chain(
        &self,
        chain_id: &str,
    ) -> Result<TaskChain, SessionError> {
        self.services
            .db()
            .graphs()
            .load_task_chain(chain_id)
            .await
            .map_err(|error| api_error(&error))?
            .ok_or(SessionError::NotFound)
    }

    /// Starts or restarts the chain's graph driver without limits; repeated
    /// calls reuse an active driver, and admitted nodes are never replayed.
    pub(super) async fn ensure_task_chain_driver(
        &mut self,
        chain_id: &str,
    ) -> Result<(), SessionError> {
        let graph = self
            .services
            .db()
            .graphs()
            .load_graph(chain_id, None)
            .await
            .map_err(|error| api_error(&error))?
            .ok_or(SessionError::NotFound)?;

        self.start_api_execution_graph(StartExecutionGraphRequest {
            graph,
            provider_call_limits: BTreeMap::new(),
        })
        .await
    }

    /// Mirrors a newly committed chain message into the owning live session.
    ///
    /// Callers own idempotency: append only after a newly admitted transition
    /// so redelivered requests never duplicate transcript rows.
    fn mirror_task_chain_transcript_message(
        &self,
        session_id: &SessionId,
        kind: SessionMessageKind,
        content: &str,
    ) {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        SessionTaskService::mirror_session_transcript_message(
            &handles.transcript,
            &self.services.event_sender(),
            &self.services.session_update_versions(),
            session_id,
            kind,
            content,
        );
    }

    /// Captures the durable conversation before launch as historical Planner
    /// input. Workflow notices are excluded.
    async fn task_chain_launch_conversation(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<String>, SessionError> {
        let messages = self
            .services
            .db()
            .sessions()
            .load_session_messages(session_id)
            .await
            .map_err(|error| api_error(&error))?;
        let conversation = messages
            .into_iter()
            .filter_map(|message| {
                let kind = message.kind.parse::<SessionMessageKind>().ok()?;

                kind.is_conversation_message()
                    .then(|| SessionMessage::new(message.position, kind, message.content))
            })
            .collect();

        Ok(SessionTranscript::ephemeral(conversation).conversation_replay_text())
    }

    /// Accepts identical launch redelivery and rejects reuse of the workflow
    /// identity with different inputs.
    fn require_matching_launch(
        existing: &ag_session::ExecutionGraph,
        session_id: &SessionId,
        request: &StartPlanThenImplementRequest,
    ) -> Result<(), SessionError> {
        let matches = existing.session_id == *session_id
            && matches!(
                existing.nodes.as_slice(),
                [planner, implementer]
                    if planner.agent == request.agent
                        && planner.objective == request.user_request
                        && planner.request.attachments == request.attachments
                        && planner.definition.id == request.agents.planner.id
                        && implementer.definition.id == request.agents.implementer.id
                        && TaskChain::role(&existing.id, planner) == Some(TaskAgentRole::Planner)
            );
        if !matches {
            return Err(SessionError::Operation(
                "Workflow identity was already used with different inputs".into(),
            ));
        }

        Ok(())
    }
}

/// Returns the task attempt identity of a graph execution.
pub(super) fn execution_attempt_id(execution: &GraphExecutionRef) -> Option<&String> {
    match execution {
        GraphExecutionRef::TaskAttempt { attempt_id, .. } => Some(attempt_id),
        GraphExecutionRef::UtilityRun { .. } => None,
    }
}

/// Converts a repository failure into a public operation error.
pub(super) fn api_error(error: &DbError) -> SessionError {
    SessionError::Operation(error.to_string())
}

#[cfg(test)]
#[path = "task_chain_test.rs"]
mod tests;

#[cfg(test)]
#[path = "task_chain_test_support_test.rs"]
pub(super) mod test_support;

#[cfg(test)]
#[path = "task_chain_title_test.rs"]
mod title_tests;
