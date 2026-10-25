//! Graph conversations on the existing serial session worker.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentDefinitionPrompt, AgentRequestKind, OneShotError, TurnContinuation, TurnRequest,
};
use ag_session::{GraphExecutionRef, GraphPostProcessing, GraphWorkspaceAccess};
use ag_worker::{GraphRunRequest, RunClient};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::post_turn::{PostTurnContext, TurnFinalizerContext};
use super::worker::{SessionWorkerContext, TurnMetadata};
use super::{StatusTransition, isolation, post_turn, turn};
use crate::app::service::SessionRunFactory;
use crate::app::session::SessionError;
use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::TaskAgentTurnMetadata;

/// Captured graph input and completion owned by one serial mailbox command.
pub(super) struct GraphTaskCommand {
    pub(super) cancellation: Arc<CancellationToken>,
    pub(super) completion: GraphTaskCompletion,
    pub(super) factory: Arc<dyn SessionRunFactory>,
    pub(super) operation_id: String,
    pub(super) request: GraphRunRequest,
    pub(super) resources: Arc<ag_worker::GraphResources>,
}

/// A worker completion acknowledges durable settlement, never provider return.
#[derive(Clone)]
pub(super) struct GraphTaskCompletion {
    body: Arc<Mutex<Option<String>>>,
    environment: Arc<Mutex<Option<GraphTaskEnvironment>>>,
    sender: Arc<Mutex<Option<GraphTaskResponse>>>,
}

type GraphTaskResponse = oneshot::Sender<Result<String, OneShotError>>;

impl GraphTaskCompletion {
    pub(super) fn new(sender: oneshot::Sender<Result<String, OneShotError>>) -> Self {
        Self {
            body: Arc::default(),
            environment: Arc::default(),
            sender: Arc::new(Mutex::new(Some(sender))),
        }
    }

    pub(super) fn finish(&self, result: Result<(), String>) {
        let result = result
            .and_then(|()| {
                self.body
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .ok_or_else(|| "Graph task finished without a response".into())
            })
            .map_err(OneShotError::new);
        drop(
            self.environment
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        if let Some(sender) = self
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(result);
        }
    }
}

pub(super) async fn run(
    context: &SessionWorkerContext,
    run_client: Arc<dyn RunClient>,
    task: &GraphTaskCommand,
) -> Result<(), SessionError> {
    *context
        .cancel_token
        .lock()
        .map_err(|_| SessionError::Workflow("cancel token lock poisoned".into()))? =
        task.cancellation.as_ref().clone();
    let transition = StatusTransition::from_parts(
        context.app_event_tx.clone(),
        context.clock.clone(),
        context.db.clone(),
        context.session_id.clone(),
        context.session_update_versions.clone(),
        context.status.clone(),
    );
    transition
        .apply_or_invalid_transition(Status::InProgress)
        .await?;
    let result = async {
        let environment = tokio::select! {
            biased;
            () = task.cancellation.cancelled() => return Err(stopped()),
            environment = GraphTaskEnvironment::acquire(task) => environment,
        };
        *task
            .completion
            .environment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(environment);

        execute(context, run_client, task).await
    }
    .await;
    let status_result = match &result {
        Ok(()) | Err(SessionError::StoppedByUser(_)) => Ok(Status::Review),
        Err(error) => Err(SessionError::Workflow(error.to_string())),
    };
    post_turn::finalize_channel_turn(&TurnFinalizerContext::from_worker(context), &status_result)
        .await;

    result
}

/// Completion retains the selected task's leases through durable operation
/// settlement. Cancellation while acquiring either gate releases partial
/// leases.
struct GraphTaskEnvironment {
    _background_writer: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
    _reader: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
    _writer: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

impl GraphTaskEnvironment {
    async fn acquire(task: &GraphTaskCommand) -> Self {
        let node = &task.request.dispatch.node;
        let environment = task.resources.environment(&node.env_id);

        match node.execution.workspace_access {
            GraphWorkspaceAccess::SharedRead => Self {
                _background_writer: None,
                _reader: Some(environment.read_owned().await),
                _writer: None,
            },
            GraphWorkspaceAccess::ExclusiveWrite => Self {
                _background_writer: Some(
                    task.resources
                        .background_readers(&node.env_id)
                        .write_owned()
                        .await,
                ),
                _reader: None,
                _writer: Some(environment.write_owned().await),
            },
        }
    }
}

async fn execute(
    context: &SessionWorkerContext,
    run_client: Arc<dyn RunClient>,
    task: &GraphTaskCommand,
) -> Result<(), SessionError> {
    let dispatch = &task.request.dispatch;
    let GraphExecutionRef::TaskAttempt { attempt_id, .. } = &dispatch.record.execution else {
        return Err(SessionError::Workflow(
            "Graph conversation requires a task attempt".into(),
        ));
    };
    let prepared = tokio::select! {
        biased;
        () = task.cancellation.cancelled() => return Err(stopped()),
        prepared = prepare(context, &task.request) => prepared?,
    };
    let prompt = prepared.prompt.clone();
    let permission_mode = prepared.permission_mode;
    let client = task
        .factory
        .create_graph_task(attempt_id, &context.session_id, dispatch.node.agent.kind())
        .map_err(turn::session_error_from_agent_error)?;
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let consumer = tokio::spawn(turn::consume_turn_events(
        event_rx,
        context.app_event_tx.clone(),
        context.session_id.clone(),
        context.child_pid.clone(),
    ));
    let result = client
        .submit(prepared, event_tx, task.cancellation.as_ref().clone())
        .await;
    let cleanup = client.shutdown().await;
    let activity = consumer.await.unwrap_or_default();
    activity.clear_live(&context.app_event_tx, &context.session_id);
    let result = result.map_err(turn::session_error_from_agent_error)?;
    cleanup.map_err(turn::session_error_from_agent_error)?;
    check_cancellation(&task.cancellation)?;
    let body = serde_json::json!(result.assistant_message).to_string();
    let instruction_conversation_id =
        if ag_worker::uses_persistent_session(dispatch.node.agent.kind()) {
            ag_contracts::normalize_instruction_conversation_id(
                result.provider_conversation_id.as_deref(),
            )
        } else {
            None
        };
    context
        .db
        .graphs()
        .record_task_turn(
            dispatch,
            &TaskAgentTurnMetadata {
                assistant_message: body.clone(),
                harness_turn_report: result.harness_report,
                instruction_conversation_id,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                provider_conversation_id: result.provider_conversation_id,
            },
        )
        .await?;
    if dispatch.node.execution.post_processing == GraphPostProcessing::SessionWorkflow
        && result.assistant_message.questions.is_empty()
        && permission_mode != PermissionMode::ReadOnly
    {
        apply_effects(
            context,
            run_client,
            task,
            &prompt,
            &result.assistant_message,
        )
        .await?;
    }
    check_cancellation(&task.cancellation)?;
    *task
        .completion
        .body
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(body);

    Ok(())
}

/// Runs host effects with only the graph node's explicit input and response.
async fn apply_effects(
    context: &SessionWorkerContext,
    run_client: Arc<dyn RunClient>,
    task: &GraphTaskCommand,
    prompt: &TurnPrompt,
    response: &ag_protocol::AgentResponse,
) -> Result<(), SessionError> {
    let dispatch = &task.request.dispatch;
    let run_client = ag_worker::scoped_client(
        run_client,
        ag_worker::RunScope {
            cancellation: Some(task.cancellation.as_ref().clone()),
            ..ag_worker::RunScope::default()
        },
    );
    let run_client: Arc<dyn RunClient> = match &task.request.provider_call_budget {
        Some(budget) => Arc::new(GraphEffectClient {
            client: run_client,
            budget: budget.clone(),
        }),
        None => run_client,
    };
    let mut post =
        PostTurnContext::from_worker(context, run_client, task.cancellation.as_ref().clone());
    let mut transcript = SessionTranscript::ephemeral(Vec::<SessionMessage>::new());
    transcript.append_message(SessionMessageKind::UserPrompt, &prompt.agent_text());
    transcript.append_message(SessionMessageKind::AssistantAnswer, &response.answer);
    post.transcript = Arc::new(Mutex::new(transcript));
    post_turn::apply_successful_turn_effects(
        &post,
        TurnMetadata {
            published_upstream_ref: context.load_published_upstream_ref().await,
            review_comment_thread_ids: Vec::new(),
            session_agent: dispatch.node.agent,
        },
        &[],
        Status::Review,
        post_turn::TurnEffectPolicy::Task,
    )
    .await?;

    Ok(())
}

async fn prepare(
    context: &SessionWorkerContext,
    request: &GraphRunRequest,
) -> Result<TurnRequest, SessionError> {
    drop(context.branch_operation_lock.lock().await);
    let env = isolation::validate_session_worktree(
        context.fs_client.as_ref(),
        context.git_client.as_ref(),
        &context.folder,
        &context.session_id,
    )
    .await?;
    let node = &request.dispatch.node;
    let permission = turn::load_session_permission_mode(&context.db, &context.session_id).await?;
    let permission_mode = intersect_permissions(
        request.permission_mode,
        node.execution.permission_mode(&node.definition, permission),
    );
    Ok(TurnRequest {
        agent_profile: AgentDefinitionPrompt::active(node.definition.prompt.clone(), true),
        continuation: TurnContinuation::fresh(),
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        folder: env.folder,
        main_checkout_root: env.main_checkout,
        model: node.agent.model().provider_model_str().to_string(),
        permission_mode,
        prompt: graph_prompt(request),
        provider_call_budget: request.provider_call_budget.clone(),
        reasoning_level: turn::load_session_reasoning_level(&context.db, &context.session_id).await,
        request_kind: AgentRequestKind::SessionStart,
        response_style: turn::load_session_response_style(&context.db, &context.session_id).await,
        speed_mode: turn::load_session_speed_mode(&context.db, &context.session_id).await,
    })
}

fn intersect_permissions(captured: PermissionMode, current: PermissionMode) -> PermissionMode {
    if captured == PermissionMode::ReadOnly || current == PermissionMode::ReadOnly {
        PermissionMode::ReadOnly
    } else if captured == PermissionMode::AutoEdit || current == PermissionMode::AutoEdit {
        PermissionMode::AutoEdit
    } else {
        current
    }
}

fn graph_prompt(request: &GraphRunRequest) -> TurnPrompt {
    let node = &request.dispatch.node;
    let mut prompt = TurnPrompt::from_agent_data(node.request.agent_text());
    prompt.attachments.clone_from(&node.request.attachments);
    // Keep the original attachment placeholders and user/evidence boundary.
    let input = serde_json::json!({
        "objective": node.objective,
        "bound_results": request.dispatch.inputs.iter().map(|input| serde_json::json!({
            "reference": input.reference,
            "body": input.body,
        })).collect::<Vec<_>>(),
    });
    let _ = write!(
        prompt.text,
        "\n\nGraph objective and exact bound results (JSON data):\n{input}"
    );

    prompt
}

fn stopped() -> SessionError {
    SessionError::StoppedByUser("[Stopped] Graph task interrupted by user.".into())
}

fn check_cancellation(cancellation: &CancellationToken) -> Result<(), SessionError> {
    if cancellation.is_cancelled() {
        return Err(stopped());
    }

    Ok(())
}

/// Ordered utility children retain their own limits under the graph ceiling.
struct GraphEffectClient {
    budget: ag_contracts::ProviderCallBudget,
    client: Arc<dyn RunClient>,
}

#[async_trait]
impl RunClient for GraphEffectClient {
    async fn submit(
        &self,
        mut request: ag_contracts::OneShotRequest,
    ) -> Result<ag_contracts::OneShotSubmission, OneShotError> {
        request.provider_call_budget = Some(match request.provider_call_budget {
            Some(budget) => budget.with_ceiling(self.budget.clone()),
            None => self.budget.clone(),
        });

        self.client.submit(request).await
    }
}

#[cfg(test)]
#[path = "graph_turn_test.rs"]
mod tests;
