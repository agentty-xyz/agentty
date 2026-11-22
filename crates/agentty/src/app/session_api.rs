//! Agentty adapter for the frontend-neutral `ag-session` programmatic API.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use ag_contracts::{ReasoningLevel, ResponseStyle, SpeedMode};
use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    DurableMessageVisibility, HostContext, HostSessionMode, QuestionAnswer, ReviewRequest,
    ReviewRequestState, SessionBackend, SessionError as ApiSessionError, SessionFileEntry,
    SessionId, SessionMessage, SessionMessageKind, SessionOperation, SessionService,
    SessionSettings, SessionSettingsChange, SessionStatus, parse_persisted_session_agent_model,
};
use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::app::message_admission::MessagePlan;
use crate::app::prompt_intent::PromptModelChange;
use crate::app::session::{SessionCreationSettings, migrate_session_off_retired_model};
use crate::app::session_creation::PreparedSessionCreation;
use crate::app::task::TaskService;
use crate::app::{
    App, AppError, AppEvent, SessionError, SessionRuntimeCommand, SessionRuntimeHandle, spawn,
};
use crate::domain::agent::{AgentKind, AgentSelection, HarnessModelStatus};
use crate::domain::file_entry::FileEntry;
use crate::domain::question::question_answer_message;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{SessionMessageRow, SessionReviewRequestRow, SessionRow};

#[async_trait]
impl SessionBackend for SessionRuntimeHandle {
    fn supports(&self, operation: SessionOperation) -> bool {
        matches!(
            operation,
            SessionOperation::CreateSession
                | SessionOperation::HostContext
                | SessionOperation::MergeSession
                | SessionOperation::SessionDiff
                | SessionOperation::SessionFiles
                | SessionOperation::StopTurn
                | SessionOperation::RebaseSession
                | SessionOperation::SessionSettings
                | SessionOperation::PublishSession
        )
    }

    async fn generate_graph_proposal(
        &self,
        request: ag_session::GenerateGraphProposalRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::generate_graph_proposal(self, request).await
    }

    async fn get_graph_proposal(
        &self,
        id: &str,
    ) -> Result<Option<ag_session::GraphProposalState>, ApiSessionError> {
        SessionRuntimeHandle::get_graph_proposal(self, id).await
    }

    async fn act_on_graph_proposal(
        &self,
        id: &str,
        version: u32,
        action: ag_session::GraphProposalAction,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::act_on_graph_proposal(self, id, version, action).await
    }

    async fn get_execution_graph(
        &self,
        graph_id: &str,
    ) -> Result<Option<ag_session::ExecutionGraphProgress>, ApiSessionError> {
        SessionRuntimeHandle::get_execution_graph(self, graph_id).await
    }

    async fn cancel_execution_graph(
        &self,
        request: ag_session::GraphControlRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::cancel_execution_graph(self, request).await
    }

    async fn resume_execution_graph(
        &self,
        request: ag_session::ResumeExecutionGraphRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::resume_execution_graph(self, request).await
    }

    async fn extend_execution_graph_budget(
        &self,
        request: ag_session::ExtendGraphBudgetRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::extend_execution_graph_budget(self, request).await
    }

    async fn continue_execution_graph_node(
        &self,
        request: ag_session::ContinueGraphNodeRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::continue_execution_graph_node(self, request).await
    }

    async fn start_execution_graph(
        &self,
        request: ag_session::StartExecutionGraphRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::start_execution_graph(self, request).await
    }

    async fn accept_execution_graph_result(
        &self,
        reference: ag_session::GraphResultRef,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::accept_execution_graph_result(self, reference).await
    }

    async fn submit_agent_task(
        &self,
        session_id: &SessionId,
        request: ag_session::SubmitAgentTaskRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::submit_agent_task(self, session_id, request).await
    }

    async fn get_agent_task_history(
        &self,
        task_id: &str,
    ) -> Result<Option<ag_session::TaskAgentHistory>, ApiSessionError> {
        SessionRuntimeHandle::get_agent_task_history(self, task_id).await
    }

    async fn accept_agent_plan(
        &self,
        session_id: &SessionId,
        request: ag_session::AcceptTaskPlanRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::accept_agent_plan(self, session_id, request).await
    }

    async fn create_task(
        &self,
        session_id: &SessionId,
        request: ag_session::CreateTaskRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::create_task(self, session_id, request).await
    }

    async fn list_tasks(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ag_session::Task>, ApiSessionError> {
        SessionRuntimeHandle::list_tasks(self, session_id).await
    }

    async fn submit_task(
        &self,
        session_id: &SessionId,
        request: ag_session::SubmitTaskRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::submit_task(self, session_id, request).await
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<ag_session::Task>, ApiSessionError> {
        SessionRuntimeHandle::get_task(self, task_id).await
    }

    async fn complete_task(&self, task_id: &str) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::complete_task(self, task_id).await
    }

    async fn host_context(&self) -> Result<HostContext, ApiSessionError> {
        SessionRuntimeHandle::host_context(self).await
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, ApiSessionError> {
        SessionRuntimeHandle::create_session(self, request).await
    }

    async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        SessionRuntimeHandle::get_session(self, session_id).await
    }

    async fn get_session_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<ag_session::SessionDiffFile>>, ApiSessionError> {
        SessionRuntimeHandle::get_session_diff(self, session_id).await
    }

    async fn list_session_files(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<ag_session::SessionFileEntry>>, ApiSessionError> {
        SessionRuntimeHandle::list_session_files(self, session_id).await
    }

    async fn list_sessions(&self) -> Result<Vec<ag_session::SessionSummary>, ApiSessionError> {
        SessionRuntimeHandle::list_sessions(self).await
    }

    async fn list_session_entries(
        &self,
    ) -> Result<Vec<ag_session::SessionListEntry>, ApiSessionError> {
        SessionRuntimeHandle::list_session_entries(self).await
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::send_message(self, session_id, message).await
    }

    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::submit_durable_message(self, session_id, request).await
    }

    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::answer_questions(self, session_id, request).await
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::cancel_session(self, session_id).await
    }

    async fn stop_turn(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::stop_turn(self, session_id).await
    }

    async fn rebase_session(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::rebase_session(self, session_id).await
    }

    async fn update_session_settings(
        &self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::update_session_settings(self, session_id, change).await
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::merge_session(self, session_id).await
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, ApiSessionError> {
        SessionRuntimeHandle::create_review_request(self, session_id).await
    }
}

impl App {
    /// Returns a cloneable frontend-neutral session capability.
    pub fn session_service(&self) -> SessionService {
        SessionService::new(Arc::new(self.sessions.handle()))
    }

    /// Uses the same mailbox for terminal actions while retaining provenance.
    pub(crate) fn session_view_service(&self) -> SessionService {
        SessionService::new(Arc::new(
            self.sessions
                .handle()
                .with_initiator(crate::app::command_gateway::CommandInitiator::SessionView),
        ))
    }

    /// Drives one local API request while processing the actor commands ahead
    /// of it.
    ///
    /// Background callers rely on the terminal event loop to drive the same
    /// mailbox. Foreground callers use this helper so awaiting their own
    /// response never deadlocks the foreground executor. Pending creation also
    /// pumps reducer events so its background completion can acknowledge the
    /// request; other commands retain their normal snapshot ordering.
    pub(crate) async fn drive_session_request<RequestFuture>(
        &mut self,
        request: RequestFuture,
    ) -> RequestFuture::Output
    where
        RequestFuture: Future,
    {
        let _session_runtime_consumer = self.sessions.foreground_consumer();
        tokio::pin!(request);

        loop {
            tokio::select! {
                biased;
                result = &mut request => return result,
                event = async {
                    if self.pending_session_creations.is_empty() {
                        crate::app::AppRuntimeEvent::Session(Box::new(
                            self.sessions.next_command().await,
                        ))
                    } else {
                        self.next_runtime_event().await
                    }
                } => {
                    match event {
                        crate::app::AppRuntimeEvent::App(event) => {
                            Box::pin(self.apply_app_events(*event)).await;
                        }
                        crate::app::AppRuntimeEvent::Session(command) => {
                            self.apply_session_runtime_command(*command).await;
                        }
                    }
                }
            }
        }
    }

    /// Routes mutations to shared admission and keeps reads on the query path.
    pub(crate) async fn apply_session_runtime_command(&mut self, command: SessionRuntimeCommand) {
        match command {
            SessionRuntimeCommand::GetGraphProposal { id, response_tx } => {
                let result = self
                    .services
                    .db()
                    .graph_proposals()
                    .get(&id, None)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()));
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::GetExecutionGraph {
                graph_id,
                response_tx,
            } => {
                let _ = response_tx.send(self.execution_graph_progress(&graph_id).await);
            }
            SessionRuntimeCommand::RunGraphNode {
                admitted_tx,
                cancellation,
                request,
                response_tx,
            } => {
                self.enqueue_graph_node(*request, cancellation, admitted_tx, response_tx)
                    .await;
            }
            SessionRuntimeCommand::GetAgentTaskHistory {
                response_tx,
                task_id,
            } => {
                let _ = response_tx.send(self.load_api_task_history(&task_id).await);
            }
            SessionRuntimeCommand::ListTasks {
                response_tx,
                session_id,
            } => {
                let result = self
                    .services
                    .db()
                    .tasks()
                    .list_tasks(&session_id)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()));
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::GetTask {
                response_tx,
                task_id,
            } => {
                let result = self
                    .services
                    .db()
                    .tasks()
                    .load_task(&task_id)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()));
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::Get {
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.get_api_session(&session_id).await);
            }
            SessionRuntimeCommand::GetDiff {
                response_tx,
                session_id,
            } => self.start_api_session_diff(&session_id, response_tx),
            SessionRuntimeCommand::List { response_tx } => {
                let _ = response_tx.send(Ok(self.list_api_session_entries()));
            }
            SessionRuntimeCommand::ListFiles {
                response_tx,
                session_id,
            } => self.start_api_session_files(&session_id, response_tx),
            SessionRuntimeCommand::HostContext { response_tx } => {
                let _ = response_tx.send(self.api_host_context().await);
            }
            SessionRuntimeCommand::Mutation(command) => {
                let _ = self.admit_session_command(command).await;
                self.publish_session_change();
            }
        }
    }

    /// Summarizes the loaded sessions in their display order, with the diff
    /// size, model, timer, merge-conflict, and review figures the session
    /// list shows.
    fn list_api_session_entries(&self) -> Vec<ag_session::SessionListEntry> {
        let git_statuses = self.sessions.render_parts().session_git_statuses;

        self.sessions
            .sessions()
            .iter()
            .map(|session| ag_session::SessionListEntry {
                stats: Some(ag_session::SessionListStats {
                    added_lines: session.stats.added_lines,
                    deleted_lines: session.stats.deleted_lines,
                    has_merge_conflict: git_statuses
                        .get(&session.id)
                        .and_then(|status| status.has_merge_conflict)
                        .unwrap_or(false),
                    in_progress_started_at: session.in_progress_started_at,
                    in_progress_total_seconds: session.in_progress_total_seconds,
                    model: session.agent.model(),
                    reasoning_level: session.effective_reasoning_level(),
                    review_request: session
                        .review_request
                        .as_ref()
                        .map(|review_request| review_request.summary.clone()),
                    size_label: session.size.label().to_string(),
                }),
                summary: ag_session::SessionSummary {
                    id: session.id.clone(),
                    status: session.status,
                    title: session.title.clone(),
                    updated_at: session.updated_at,
                },
            })
            .collect()
    }

    /// Answers one programmatic file-listing query: captures the session's
    /// worktree folder on the foreground loop, then walks it on a blocking
    /// worker unless the `@`-mention index already caches it. A session the
    /// host has not loaded answers `None`; a session without a worktree on
    /// disk lists nothing, never the project checkout.
    fn start_api_session_files(
        &mut self,
        session_id: &SessionId,
        response_tx: oneshot::Sender<Result<Option<Vec<SessionFileEntry>>, ApiSessionError>>,
    ) {
        let Some(session) = self.sessions.session_for_id(session_id) else {
            let _ = response_tx.send(Ok(None));

            return;
        };
        let folder = session.folder.clone();
        if !self.services.fs_client().is_dir(folder.clone()) {
            let _ = response_tx.send(Ok(Some(Vec::new())));

            return;
        }
        if let Some(entries) = self.sessions.at_mention_index_for_root(&folder) {
            let _ = response_tx.send(Ok(Some(api_file_entries(entries))));

            return;
        }
        let session_id = session_id.clone();
        spawn::spawn_detached(async move {
            let entries = TaskService::load_at_mention_entries(folder, &session_id).await;
            let _ = response_tx.send(Ok(Some(api_file_entries(entries))));
        });
    }

    /// Projects isolated task history without exposing storage metadata.
    async fn load_api_task_history(
        &self,
        task_id: &str,
    ) -> Result<Option<ag_session::TaskAgentHistory>, ApiSessionError> {
        self.services
            .db()
            .tasks()
            .load_agent_history(task_id)
            .await
            .map(|history| {
                history.map(|history| ag_session::TaskAgentHistory {
                    context: history.context.context,
                    input: history.context.input,
                    turns: history
                        .turns
                        .into_iter()
                        .map(|turn| ag_session::TaskAgentTurn {
                            assistant_message: turn.assistant_message,
                            attempt_id: turn.attempt_id,
                            input: turn.input,
                            input_tokens: turn.input_tokens,
                            output_tokens: turn.output_tokens,
                        })
                        .collect(),
                })
            })
            .map_err(|error| ApiSessionError::Operation(error.to_string()))
    }

    /// Validates one creation request and captures its launch settings. Drafts
    /// persist immediately; materialized worktrees return an owned effect plan.
    pub(super) async fn prepare_api_session_creation(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<PreparedSessionCreation, ApiSessionError> {
        self.validate_api_session_request(&request)?;
        self.ensure_project_checkout_available(request.project_id)
            .map_err(api_error_from_app)?;

        let inherited_settings = self
            .inherited_creation_settings(
                request.inherit_from_session_id.as_ref(),
                request.project_id,
            )
            .await?;
        let (base_branch_override, creation_settings) = inherited_settings
            .map_or((None, None), |inherited| {
                (Some(inherited.base_branch), Some(inherited.settings))
            });
        let runs_on_harness = request.mode == CreateSessionMode::Harness;
        match request.mode {
            CreateSessionMode::Draft => {
                let project = self.api_project_creation_context(base_branch_override)?;
                let session_id = self
                    .sessions
                    .create_draft_session_for_project_with_settings(
                        &self.services,
                        request.project_id,
                        &project.base_branch,
                        creation_settings,
                    )
                    .await
                    .map_err(api_error_from_session)?;

                return Ok(PreparedSessionCreation::Persisted(session_id));
            }
            CreateSessionMode::Regular | CreateSessionMode::Harness => {}
        }
        let project = self.api_project_creation_context(base_branch_override)?;
        let settings = self
            .sessions
            .resolve_session_creation_settings(
                &self.services,
                request.project_id,
                creation_settings,
            )
            .await
            .map_err(api_error_from_session)?;
        let harness_model = runs_on_harness
            .then(|| self.services.harness_availability().default_model())
            .flatten();
        let settings = if let Some(harness_model) = harness_model {
            SessionCreationSettings {
                agent: AgentSelection::new(AgentKind::Harness, harness_model),
                speed_mode: SpeedMode::Normal,
                ..settings
            }
        } else {
            settings
        };

        Ok(PreparedSessionCreation::Materialized {
            base_branch: project.base_branch,
            project_id: request.project_id,
            settings,
        })
    }

    /// Describes the active project, its base branch and default model, and
    /// the creation modes [`Self::validate_api_session_request`] accepts now.
    async fn api_host_context(&mut self) -> Result<HostContext, ApiSessionError> {
        let project_id = self.active_project_id();
        let project = self.api_project_creation_context(None)?;
        let settings = self
            .sessions
            .resolve_session_creation_settings(&self.services, project_id, None)
            .await
            .map_err(api_error_from_session)?;
        let harness_model = self.services.harness_availability().default_model();
        let mut session_modes = Vec::new();
        if self.services.has_agent_cli() {
            session_modes.push(HostSessionMode {
                agent: settings.agent,
                mode: CreateSessionMode::Regular,
            });
        }
        if let Some(harness_model) = harness_model {
            session_modes.push(HostSessionMode {
                agent: AgentSelection::new(AgentKind::Harness, harness_model),
                mode: CreateSessionMode::Harness,
            });
        }
        // A host that offers only Harness sessions starts them on the harness
        // model, not on the regular agent's default.
        let default_model = session_modes
            .first()
            .map_or(settings.agent, |session_mode| session_mode.agent)
            .model();

        Ok(HostContext {
            base_branch: project.base_branch,
            default_model,
            models: self.api_session_models(),
            project_id,
            project_name: self.projects.project_name().to_string(),
            session_modes,
        })
    }

    /// Lists the agents and models a session can switch to now: every model
    /// of each available agent CLI, then, when the launch offers the harness,
    /// its ready listed models, or its fixed catalog before any are listed.
    /// A session
    /// switches only among its own agent's entries, as the terminal's
    /// `/model` offers them.
    fn api_session_models(&self) -> Vec<AgentSelection> {
        let mut models = self
            .services
            .available_agent_kinds()
            .into_iter()
            .flat_map(|kind| {
                kind.models()
                    .iter()
                    .map(move |model| AgentSelection::new(kind, *model))
            })
            .collect::<Vec<_>>();
        if self.services.harness_availability().is_visible() {
            let options = self.services.harness_model_options();
            let harness_models = if options.is_empty() {
                AgentKind::Harness.models().to_vec()
            } else {
                options
                    .into_iter()
                    .filter(|option| option.status == HarnessModelStatus::Ready)
                    .map(|option| option.model)
                    .collect()
            };
            models.extend(
                harness_models
                    .into_iter()
                    .map(|model| AgentSelection::new(AgentKind::Harness, model)),
            );
        }

        models
    }

    /// Changes one session's settings through the terminal's own setting
    /// workflows, after checking the whole change.
    ///
    /// Nothing is written until the whole change passes the checks the
    /// terminal makes by construction: the session's status shows the
    /// composer, where `/model` and the other setting commands are reachable;
    /// a model is one the session's own agent offers in `/model`; and fast
    /// mode suits the model the session will have. The writes then run in
    /// the order model, reasoning, style, speed, so the switch most likely
    /// to fail runs first. They are not transactional: when a later one fails
    /// to persist, the earlier ones stay applied. A model or speed change
    /// waits while the session still has accepted work, exactly as it does
    /// in the terminal; reasoning and style apply at once.
    pub(super) async fn update_api_session_settings(
        &mut self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), ApiSessionError> {
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        let status = session.status;
        let current_agent = session.agent;
        if !status.allows_chat_composer() {
            return Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot change settings in status `{status}`"
            )));
        }
        if let Some(model) = change.model
            && !self
                .api_session_models()
                .into_iter()
                .any(|offered| offered.kind() == current_agent.kind() && offered.model() == model)
        {
            return Err(ApiSessionError::Operation(format!(
                "Model `{}` is not available to session `{session_id}`",
                model.as_str()
            )));
        }
        let selected_agent = change
            .model
            .map(|model| AgentSelection::new(current_agent.kind(), model));
        let resulting_agent = selected_agent
            .or_else(|| {
                self.deferred_session_settings
                    .get(session_id)
                    .and_then(|settings| settings.agent)
            })
            .unwrap_or(current_agent);
        if change.speed_mode == Some(SpeedMode::Fast) && !resulting_agent.supports_fast_mode() {
            return Err(ApiSessionError::Operation(format!(
                "Fast mode is not supported by `{}`",
                resulting_agent.model().as_str()
            )));
        }

        if let Some(agent) = selected_agent
            && self.update_prompt_session_model(session_id, agent).await
                == PromptModelChange::Failed
        {
            return Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` could not switch to `{}`",
                agent.model().as_str()
            )));
        }
        if let Some(reasoning_level) = change.reasoning_level {
            self.set_session_reasoning_level(session_id, reasoning_level)
                .await
                .map_err(api_error_from_app)?;
        }
        if let Some(response_style) = change.response_style {
            self.set_session_response_style(session_id, response_style)
                .await
                .map_err(api_error_from_app)?;
        }
        if let Some(speed_mode) = change.speed_mode {
            self.try_update_prompt_session_speed_mode(session_id, speed_mode)
                .await
                .map_err(api_error_from_session)?;
        }

        Ok(())
    }

    fn validate_api_session_request(
        &self,
        request: &CreateSessionRequest,
    ) -> Result<(), ApiSessionError> {
        if request.project_id != self.active_project_id() {
            return Err(ApiSessionError::Operation(format!(
                "Project `{}` is not active",
                request.project_id
            )));
        }
        if request.mode == CreateSessionMode::Harness
            && !self.services.harness_availability().is_available()
        {
            return Err(ApiSessionError::Operation(
                "Harness sessions need `agentty --experimental-harness` and a runnable model: a \
                 local Ollama or OpenAI-compatible server serving a model, `MODEL_API_KEY`, \
                 `KIMI_API_KEY`, or `DASHSCOPE_API_KEY` with `DASHSCOPE_BASE_URL`."
                    .to_string(),
            ));
        }
        if request.mode != CreateSessionMode::Harness && !self.services.has_agent_cli() {
            return Err(ApiSessionError::Operation(
                "No supported backend CLI found on `PATH`; only Harness sessions are available."
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Loads one complete session aggregate from persistence plus live queue
    /// state.
    async fn get_api_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        let Some(row) = self
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(None);
        };
        let session_status = row
            .status
            .parse::<SessionStatus>()
            .unwrap_or(SessionStatus::Done);
        migrate_session_off_retired_model(
            self.services.db(),
            &row.id,
            &row.agent,
            &row.model,
            session_status,
        )
        .await;
        let message_rows = self
            .services
            .db()
            .sessions()
            .load_session_messages(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let queued_messages = self
            .sessions
            .session_for_id(session_id)
            .map_or_default(|session| {
                session
                    .queued_messages
                    .iter()
                    .map(|message| message.transcript_text().to_string())
                    .collect()
            });

        build_api_session(row, message_rows, queued_messages).map(Some)
    }

    /// Sends one validated API message through the existing session workflow.
    ///
    /// Graph work waiting for the session's chat receives the message first,
    /// as it does for terminal messages. Accepting a graph reply acknowledges
    /// that it was recorded, not that its decision finished.
    pub(super) async fn send_api_message(
        &mut self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), ApiSessionError> {
        if message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot send an empty session message".to_string(),
            ));
        }

        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        let status = session.status;
        let plan = MessagePlan::for_api(status, session.is_draft_session());
        let prompt = TurnPrompt::from_text(message);
        // Like the terminal, a staged draft prompt is never a graph reply.
        // Boxed because accepting a reply can admit nested gateway commands.
        if plan != MessagePlan::StageAndStart
            && let Some(outcome) = Box::pin(self.submit_graph_reply(session_id, &prompt)).await
        {
            return outcome.into_result();
        }

        if self
            .execute_message_plan(session_id, prompt, plan)
            .await
            .map_err(api_error_from_app)?
        {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot accept a message in status `{status}`"
        )))
    }

    /// Submits a durable turn only when it can bypass the lossy
    /// in-memory chat queue and enter the serialized session worker directly.
    pub(super) async fn submit_api_durable_message(
        &mut self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), ApiSessionError> {
        if request.message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit an empty durable message".to_string(),
            ));
        }
        if request.operation_id.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit a durable message without an operation id".to_string(),
            ));
        }

        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        let status = session.status;
        if !(status.allows_review_actions() || status == SessionStatus::Question) {
            return Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept a durable message in status `{status}`"
            )));
        }

        let outcome = self
            .sessions
            .reply_to_durable_message(
                &self.services,
                session_id,
                request.operation_id,
                request.visibility == DurableMessageVisibility::Visible,
                TurnPrompt::from_agent_data(request.message),
            )
            .await;
        if outcome.is_accepted() {
            if outcome.is_new() {
                self.clear_review_output_for_new_turn(session_id);
            }
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` could not enqueue the durable message"
        )))
    }

    /// Admits explicit work after checking durable delivery identity.
    pub(super) async fn submit_api_agent_task(
        &mut self,
        session_id: &SessionId,
        mut request: ag_session::SubmitAgentTaskRequest,
    ) -> Result<(), ApiSessionError> {
        if let Some(history) = self
            .services
            .db()
            .tasks()
            .load_agent_history(&request.task_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        {
            if history.context.context.assignment.definition.id == request.assignment.definition.id
            {
                request.assignment.definition = history.context.context.assignment.definition;
            }
        } else {
            request.assignment.definition = self
                .resolve_task_agent_definition(session_id, &request.assignment.definition.id)
                .await?;
        }
        if self
            .services
            .db()
            .tasks()
            .contains_agent_attempt(session_id, &request)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        {
            return Ok(());
        }
        self.sessions
            .validate_agent_task_admission(session_id, request.assignment.role)
            .map_err(api_error_from_session)?;
        self.services
            .db()
            .tasks()
            .verify_agent_plan(session_id, &request)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        self.sessions
            .validate_agent_task_workspace(&self.services, session_id)
            .map_err(api_error_from_session)?;
        let operation_id = format!("task-attempt:{}", request.attempt_id);
        let claimed = self
            .services
            .db()
            .tasks()
            .claim_agent_attempt(session_id, &request, "agent_task")
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        if claimed {
            if let Err(error) = self
                .suppress_agent_task_automation(session_id, &request.attempt_id)
                .await
            {
                self.services
                    .db()
                    .operations()
                    .mark_session_operation_failed(&operation_id, &error.to_string())
                    .await
                    .map_err(|failure| ApiSessionError::Operation(failure.to_string()))?;

                return Err(error);
            }
            self.sessions
                .enqueue_claimed_agent_task(&self.services, session_id, request)
                .await
                .map_err(api_error_from_session)?;
        }

        Ok(())
    }

    /// Resolves a definition through the injected catalog before task
    /// admission.
    async fn resolve_task_agent_definition(
        &self,
        session_id: &SessionId,
        id: &str,
    ) -> Result<ag_session::AgentDefinition, ApiSessionError> {
        let folder = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == *session_id)
            .map(|session| session.folder.clone())
            .ok_or(ApiSessionError::NotFound)?;

        self.services
            .agent_profile_catalog_client()
            .resolve(folder, id.to_string())
            .await
            .ok_or_else(|| {
                ApiSessionError::Operation(format!(
                    "Agent definition `{id}` is unavailable or invalid"
                ))
            })
    }

    /// Invalidates prior ordinary review work and its durable startup trigger
    /// before an admitted role attempt can run. Only a new ordinary admission,
    /// explicit review, or the successful completion of this same Implementer
    /// attempt can release this ownership barrier. Like a submitted prompt,
    /// the admission starts a new automatic remediation cycle.
    pub(in crate::app) async fn suppress_agent_task_automation(
        &mut self,
        session_id: &SessionId,
        attempt_id: &str,
    ) -> Result<(), ApiSessionError> {
        self.supersede_review_diff_loads(&HashSet::from([session_id.clone()]));
        self.deferred_auto_review_session_ids.remove(session_id);
        self.auto_address_review_iterations.remove(session_id);
        self.suppress_review_output(session_id);
        self.role_review_suppression
            .insert(session_id.clone(), attempt_id.to_string());
        self.services
            .db()
            .sessions()
            .update_session_focused_review(session_id, None, None, None)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))
    }

    /// Admits explicit work after checking durable delivery identity.
    pub(super) async fn submit_api_task(
        &mut self,
        session_id: &SessionId,
        request: ag_session::SubmitTaskRequest,
    ) -> Result<(), ApiSessionError> {
        if self
            .services
            .db()
            .tasks()
            .contains_attempt(session_id, &request)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        {
            return Ok(());
        }
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        if !(session.status.allows_review_actions() || session.status == SessionStatus::Question) {
            return Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept a task attempt in status `{}`",
                session.status
            )));
        }
        let outcome = self
            .sessions
            .reply_to_task(&self.services, session_id, request)
            .await;
        if outcome.is_accepted() {
            if outcome.is_new() {
                self.clear_review_output_for_new_turn(session_id);
            }
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` could not enqueue the task attempt"
        )))
    }

    /// Cancels a session through the ordinary lifecycle workflow.
    pub(super) async fn cancel_api_session(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        self.cancel_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    /// Stops the session's running turn through the same workflow as the
    /// terminal's `Ctrl+C`.
    pub(super) async fn stop_api_turn(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        self.stop_session_turn(session_id)
            .await
            .map_err(api_error_from_app)
    }

    /// Starts or queues a session branch rebase through the same workflow as
    /// the terminal's `r`.
    pub(super) async fn rebase_api_session(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        self.rebase_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    /// Claims structured question answers against the current persisted
    /// question set before resuming the session with a reply turn that
    /// carries the answers' image attachments.
    pub(super) async fn answer_api_questions(
        &mut self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        let row = self
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            .ok_or(ApiSessionError::NotFound)?;
        let persisted_questions = row.questions.unwrap_or_default();
        let questions = api_questions_from_json(Some(&persisted_questions), session_id)?;
        validate_question_answers(&questions, &request.answers)?;
        let prompt = TurnPrompt {
            attachments: request.attachments,
            ..TurnPrompt::from_text(question_answer_message(&request.answers))
        };
        let status = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?
            .status;

        self.services
            .db()
            .sessions()
            .update_session_questions(session_id, "")
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let send_result = if self
            .sessions
            .reply_to_question_answers(&self.services, session_id, prompt)
            .await
        {
            Ok(())
        } else {
            Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept question answers in status `{status}`"
            )))
        };
        if let Err(send_error) = send_result {
            self.services
                .db()
                .sessions()
                .update_session_questions(session_id, &persisted_questions)
                .await
                .map_err(|restore_error| question_restore_error(&send_error, &restore_error))?;

            return Err(send_error);
        }
        self.clear_review_output_for_new_turn(session_id);
        self.clear_diff_comment_progress(session_id);
        self.services.emit_app_event(AppEvent::RefreshSessions);

        Ok(())
    }

    pub(super) async fn merge_api_session(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        self.sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;

        self.merge_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    /// Loads inherited launch settings and verifies that the source belongs
    /// to the requested project.
    async fn inherited_creation_settings(
        &self,
        source_session_id: Option<&SessionId>,
        project_id: i64,
    ) -> Result<Option<InheritedCreationSettings>, ApiSessionError> {
        let Some(source_session_id) = source_session_id else {
            return Ok(None);
        };
        let source_row = self
            .services
            .db()
            .sessions()
            .load_session(source_session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            .ok_or(ApiSessionError::NotFound)?;
        let source = build_api_session(source_row, Vec::new(), Vec::new())?;
        if source.settings.project_id != project_id {
            return Err(ApiSessionError::Operation(format!(
                "Session `{source_session_id}` belongs to project `{}`, not `{project_id}`",
                source.settings.project_id
            )));
        }

        Ok(Some(InheritedCreationSettings {
            base_branch: source.settings.base_branch,
            settings: SessionCreationSettings {
                agent: source.settings.agent,
                reasoning_level: source.settings.reasoning_level,
                response_style: source.settings.response_style,
                speed_mode: source.settings.speed_mode,
            },
        }))
    }

    /// Resolves the active project into worktree creation inputs.
    fn api_project_creation_context(
        &self,
        base_branch_override: Option<String>,
    ) -> Result<ApiProjectCreationContext, ApiSessionError> {
        let base_branch = base_branch_override
            .or_else(|| self.projects.git_branch().map(str::to_string))
            .ok_or_else(|| {
                ApiSessionError::Operation("Git branch is required to create a session".to_string())
            })?;

        Ok(ApiProjectCreationContext { base_branch })
    }

    /// Attempts to register a newly persisted active-project session before
    /// acknowledging creation, scheduling a refresh retry when loading is
    /// temporarily unavailable.
    pub(super) async fn finish_api_session_creation(&mut self, session_id: &str) {
        if self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        let _ = self
            .sessions
            .register_created_session(&self.services, session_id, self.projects.working_dir())
            .await;
        if self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.services.emit_app_event(AppEvent::RefreshSessions);
    }
}

/// Worktree inputs resolved for one API-requested project.
struct ApiProjectCreationContext {
    base_branch: String,
}

/// Launch settings loaded from one existing session.
struct InheritedCreationSettings {
    base_branch: String,
    settings: SessionCreationSettings,
}

/// Converts indexed worktree entries into the session API's file entries,
/// keeping their order.
fn api_file_entries(entries: Vec<FileEntry>) -> Vec<SessionFileEntry> {
    entries
        .into_iter()
        .map(|entry| SessionFileEntry {
            is_dir: entry.is_dir,
            path: entry.path,
        })
        .collect()
}

/// Combines a rejected question answer with a subsequent persistence failure.
fn question_restore_error(
    send_error: &ApiSessionError,
    restore_error: &impl std::fmt::Display,
) -> ApiSessionError {
    ApiSessionError::Operation(format!(
        "{send_error}; failed to restore session questions: {restore_error}"
    ))
}

/// Validates that a structured answer set exactly matches the current
/// persisted questions.
fn validate_question_answers(
    questions: &[QuestionItem],
    answers: &[QuestionAnswer],
) -> Result<(), ApiSessionError> {
    if questions.is_empty() {
        return Err(ApiSessionError::Operation(
            "Session has no questions to answer".to_string(),
        ));
    }

    if questions.len() != answers.len() {
        return Err(ApiSessionError::Operation(format!(
            "Expected {} question answers, received {}",
            questions.len(),
            answers.len()
        )));
    }

    for (question_index, (question, answer)) in questions.iter().zip(answers).enumerate() {
        if question.text != answer.question {
            return Err(ApiSessionError::Operation(format!(
                "Question answer {} is stale",
                question_index + 1
            )));
        }
        if answer.answer.trim().is_empty() {
            return Err(ApiSessionError::Operation(format!(
                "Question answer {} is empty",
                question_index + 1
            )));
        }
    }

    Ok(())
}

/// Converts complete persistence rows into the public session aggregate.
fn build_api_session(
    row: SessionRow,
    message_rows: Vec<SessionMessageRow>,
    queued_messages: Vec<String>,
) -> Result<ag_session::Session, ApiSessionError> {
    let project_id = row.project_id.ok_or_else(|| {
        ApiSessionError::InvalidData(format!("session `{}` has no project", row.id))
    })?;
    let status = row
        .status
        .parse::<SessionStatus>()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?;
    let reasoning_level = row
        .reasoning_level_override
        .as_deref()
        .and_then(|value| value.parse::<ReasoningLevel>().ok())
        .unwrap_or_default();
    let speed_mode = row.speed_mode.parse::<SpeedMode>().unwrap_or_default();
    let response_style = row
        .response_style
        .parse::<ResponseStyle>()
        .unwrap_or_default();
    let messages = message_rows
        .into_iter()
        .map(api_message_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let questions = api_questions_from_json(row.questions.as_deref(), &row.id)?;
    let review_request = row
        .review_request
        .map(api_review_request_from_row)
        .transpose()?;
    let draft_prompt = (row.is_draft && !row.prompt.is_empty()).then_some(row.prompt);

    Ok(ag_session::Session {
        created_at: row.created_at,
        draft_prompt,
        id: SessionId::from(row.id),
        messages,
        published_upstream_ref: row.published_upstream_ref,
        questions,
        queued_messages,
        review_request,
        settings: SessionSettings {
            agent: parse_persisted_session_agent_model(Some(&row.agent), &row.model),
            base_branch: row.base_branch,
            is_draft: row.is_draft,
            project_id,
            reasoning_level,
            response_style,
            speed_mode,
        },
        status,
        title: row.title,
        updated_at: row.updated_at,
    })
}

/// Converts one persisted transcript row into its shared typed model.
fn api_message_from_row(row: SessionMessageRow) -> Result<SessionMessage, ApiSessionError> {
    let kind = row.kind.parse::<SessionMessageKind>().map_err(|error| {
        ApiSessionError::InvalidData(format!(
            "session message at position {}: {error}",
            row.position
        ))
    })?;

    Ok(SessionMessage::new(row.position, kind, row.content))
}

/// Parses current and legacy persisted clarification-question payloads.
fn api_questions_from_json(
    raw_json: Option<&str>,
    session_id: &str,
) -> Result<Vec<QuestionItem>, ApiSessionError> {
    let Some(raw_json) = raw_json.filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    if let Ok(questions) = serde_json::from_str::<Vec<QuestionItem>>(raw_json) {
        return Ok(questions);
    }

    serde_json::from_str::<Vec<String>>(raw_json)
        .map(|questions| questions.into_iter().map(QuestionItem::new).collect())
        .map_err(|error| {
            ApiSessionError::InvalidData(format!(
                "session `{session_id}` has invalid questions: {error}"
            ))
        })
}

/// Converts persisted joined forge metadata into its shared typed model.
fn api_review_request_from_row(
    row: SessionReviewRequestRow,
) -> Result<ReviewRequest, ApiSessionError> {
    let forge_kind = row
        .forge_kind
        .parse()
        .map_err(ApiSessionError::InvalidData)?;
    let state = row
        .state
        .parse::<ReviewRequestState>()
        .map_err(ApiSessionError::InvalidData)?;

    Ok(ReviewRequest {
        last_refreshed_at: row.last_refreshed_at,
        summary: ag_session::ReviewRequestSummary {
            display_id: row.display_id,
            forge_kind,
            source_branch: row.source_branch,
            state,
            status_summary: row.status_summary,
            target_branch: row.target_branch,
            title: row.title,
            web_url: row.web_url,
        },
    })
}

/// Preserves stable not-found semantics while translating host app errors.
pub(super) fn api_error_from_app(error: AppError) -> ApiSessionError {
    match error {
        AppError::Session(error) => api_error_from_session(error),
        other => ApiSessionError::Operation(other.to_string()),
    }
}

/// Preserves stable not-found semantics while translating session errors.
fn api_error_from_session(error: SessionError) -> ApiSessionError {
    match error {
        SessionError::NotFound => ApiSessionError::NotFound,
        other => ApiSessionError::Operation(other.to_string()),
    }
}

#[cfg(test)]
#[path = "session_api_test.rs"]
mod tests;
