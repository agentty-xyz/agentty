//! Agentty adapter for the frontend-neutral `ag-session` programmatic API.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use ag_contracts::{ReasoningLevel, ResponseStyle, SpeedMode};
use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    DurableMessageVisibility, HostContext, HostProject, HostProjectStatus, HostProjectSync,
    HostProjectSyncPhase, HostSessionMode, MessageImage, ProjectScope, ProjectSessions,
    QuestionAnswer, ReviewRequest, ReviewRequestState, SessionBackend,
    SessionError as ApiSessionError, SessionFileEntry, SessionId, SessionMessage,
    SessionMessageKind, SessionOperation, SessionService, SessionSettings, SessionSettingsChange,
    SessionStatus, parse_persisted_session_agent_model,
};
use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::app::command_gateway::{CommandInitiator, RemoteScope};
use crate::app::graph_node_reply::QUESTIONS_NOT_PENDING;
use crate::app::graph_question::GraphAnswerOutcome;
use crate::app::message_admission::MessagePlan;
use crate::app::project::ProjectContext;
use crate::app::prompt_intent::PromptModelChange;
use crate::app::review::ReviewCacheEntry;
use crate::app::session::{
    SessionCreationSettings, SessionGitStatus, SessionManager, SessionPage,
    migrate_session_off_retired_model,
};
use crate::app::session_creation::PreparedSessionCreation;
use crate::app::task::TaskService;
use crate::app::{
    App, AppError, AppEvent, LocalQuery, ProjectQuery, ProjectSyncPhase, ProjectSyncStatus,
    SessionError, SessionQuery, SessionRuntimeCommand, SessionRuntimeHandle, review, spawn,
};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, HarnessModelStatus};
use crate::domain::composer::PromptAttachment;
use crate::domain::file_entry::FileEntry;
use crate::domain::question::question_answer_message;
use crate::domain::session::Session;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment};
use crate::infra::clipboard_image::persist_message_image;
use crate::infra::db::{SessionMessageRow, SessionReviewRequestRow, SessionRow};

/// Rejection of answers to a session that asks no questions now.
const NO_QUESTIONS_TO_ANSWER: &str = "Session has no questions to answer";

/// Rejection of answers the graph work that asked refused for now; the
/// session's chat says why.
const GRAPH_ANSWERS_NOT_TAKEN: &str =
    "The graph work did not take the answers; the chat says why. Try again.";

#[async_trait]
impl SessionBackend for SessionRuntimeHandle {
    /// Reports the operations the host serves. Naming another listed project
    /// ([`SessionOperation::Projects`]) is served to every caller but paired
    /// devices, whose handle reaches only the active project.
    fn supports(&self, operation: SessionOperation) -> bool {
        if operation == SessionOperation::Projects {
            return self.serves_other_projects();
        }

        matches!(
            operation,
            SessionOperation::CreateSession
                | SessionOperation::HostContext
                | SessionOperation::MergeSession
                | SessionOperation::ProjectFiles
                | SessionOperation::SessionDiff
                | SessionOperation::SessionFiles
                | SessionOperation::StopTurn
                | SessionOperation::RebaseSession
                | SessionOperation::SessionSettings
                | SessionOperation::PublishSession
                | SessionOperation::ProjectStatus
                | SessionOperation::SyncProject
                | SessionOperation::Graphs
                | SessionOperation::GraphActions
                | SessionOperation::GenerateGraphProposal
                | SessionOperation::MessageImages
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

    async fn session_graphs(
        &self,
        request: ag_session::SessionGraphsRequest,
    ) -> Result<ag_session::SessionGraphs, ApiSessionError> {
        SessionRuntimeHandle::session_graphs(self, request).await
    }

    async fn act_on_session_graph(
        &self,
        request: ag_session::SessionGraphActionRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::act_on_session_graph(self, request).await
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

    async fn list_projects(&self) -> Result<Vec<HostProject>, ApiSessionError> {
        SessionRuntimeHandle::list_projects(self).await
    }

    async fn host_context(&self) -> Result<HostContext, ApiSessionError> {
        SessionRuntimeHandle::host_context_for(self, ProjectScope::Active).await
    }

    async fn host_context_for(&self, scope: ProjectScope) -> Result<HostContext, ApiSessionError> {
        SessionRuntimeHandle::host_context_for(self, scope).await
    }

    async fn project_status(&self) -> Result<HostProjectStatus, ApiSessionError> {
        SessionRuntimeHandle::project_status(self).await
    }

    async fn project_sessions(
        &self,
        scope: ProjectScope,
    ) -> Result<ProjectSessions, ApiSessionError> {
        SessionRuntimeHandle::project_sessions(self, scope).await
    }

    async fn sync_project(&self) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::sync_project_for(self, ProjectScope::Active).await
    }

    async fn sync_project_for(&self, scope: ProjectScope) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::sync_project_for(self, scope).await
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

    async fn list_project_files(
        &self,
    ) -> Result<Vec<ag_session::SessionFileEntry>, ApiSessionError> {
        SessionRuntimeHandle::list_project_files_for(self, ProjectScope::Active).await
    }

    async fn list_project_files_for(
        &self,
        scope: ProjectScope,
    ) -> Result<Vec<ag_session::SessionFileEntry>, ApiSessionError> {
        SessionRuntimeHandle::list_project_files_for(self, scope).await
    }

    async fn list_sessions(&self) -> Result<Vec<ag_session::SessionSummary>, ApiSessionError> {
        SessionRuntimeHandle::list_sessions(self).await
    }

    async fn list_session_entries(
        &self,
    ) -> Result<Vec<ag_session::SessionListEntry>, ApiSessionError> {
        SessionRuntimeHandle::list_session_entries(self).await
    }

    async fn is_session_listed(&self, session_id: &SessionId) -> Result<bool, ApiSessionError> {
        SessionRuntimeHandle::is_session_listed(self, session_id).await
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::send_message(self, session_id, message).await
    }

    async fn send_message_with_images(
        &self,
        session_id: &SessionId,
        message: String,
        images: Vec<MessageImage>,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::send_message_with_images(self, session_id, message, images).await
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

    /// Uses the same mailbox for paired remote-control devices, whose
    /// requests the host limits to the sessions it lists and the operations
    /// remote control serves.
    pub(crate) fn remote_session_service(&self) -> SessionService {
        SessionService::new(Arc::new(
            self.sessions
                .handle()
                .with_initiator(crate::app::command_gateway::CommandInitiator::Remote),
        ))
    }

    /// Drives one local API request while processing the actor commands ahead
    /// of it.
    ///
    /// Background callers rely on the terminal event loop to drive the same
    /// mailbox. Foreground callers use this helper so awaiting their own
    /// response never deadlocks the foreground executor. Pending creation also
    /// pumps reducer events so its background completion can acknowledge the
    /// request; other commands retain their normal snapshot ordering. Each
    /// pumped event ends with the remote revision check the foreground cycle
    /// makes.
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
                    // This loop stands in for the foreground cycle until the
                    // request finishes.
                    self.publish_reported_state_changes();
                }
            }
        }
    }

    /// Routes mutations to shared admission and keeps reads on the query path.
    ///
    /// Each read's variant decides its remote-control scope: a paired
    /// device's project read reaches only the active project, its session
    /// read only a session the host lists for the active project, and its
    /// local reads are refused. A session-API read renews the lease of the
    /// page it reads.
    pub(crate) async fn apply_session_runtime_command(&mut self, command: SessionRuntimeCommand) {
        match command {
            SessionRuntimeCommand::Project { initiator, query } => {
                match self.remote_refusal(initiator, query.remote_scope()) {
                    Some(refusal) => query.refuse(refusal),
                    None => self.answer_project_query(initiator, query).await,
                }
            }
            SessionRuntimeCommand::Session {
                initiator,
                query,
                session_id,
            } => {
                let scope = RemoteScope::Session(&session_id);
                if self.remote_refusal(initiator, scope).is_some() {
                    query.answer_unlisted();
                } else {
                    self.renew_session_page_lease(initiator, &session_id);
                    self.answer_session_query(session_id, query).await;
                }
            }
            SessionRuntimeCommand::Local { initiator, query } => {
                match self.remote_refusal(initiator, RemoteScope::NotServed) {
                    Some(refusal) => query.refuse(refusal),
                    None => self.answer_local_query(query).await,
                }
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
            SessionRuntimeCommand::Mutation(command) => {
                let _ = self.admit_session_command(command).await;
                // A mutation may write only the database, such as a
                // transcript or a task, which the foreground fingerprint
                // does not hold.
                self.publish_session_change();
            }
        }
    }

    /// Answers one project read that `initiator` may make.
    async fn answer_project_query(&mut self, initiator: CommandInitiator, query: ProjectQuery) {
        match query {
            ProjectQuery::HostContext { response_tx, scope } => {
                let _ = response_tx.send(self.api_host_context(initiator, scope).await);
            }
            ProjectQuery::List { response_tx, scope } => {
                let _ = response_tx.send(self.api_project_sessions(initiator, scope).await);
            }
            ProjectQuery::ListProjectFiles { response_tx, scope } => {
                match self.api_project_root(initiator, scope).await {
                    Ok((project_id, root)) => {
                        self.start_api_project_files(project_id, root, response_tx);
                    }
                    Err(error) => {
                        let _ = response_tx.send(Err(error));
                    }
                }
            }
            ProjectQuery::ListProjects { response_tx } => {
                let _ = response_tx.send(Ok(self.api_projects()));
            }
            ProjectQuery::ProjectStatus { response_tx } => {
                let _ = response_tx.send(Ok(self.api_project_status()));
            }
            ProjectQuery::SessionListed {
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.api_session_listed(initiator, &session_id).await);
            }
        }
    }

    /// Lists the projects the host lists, in its switcher's
    /// most-recently-opened order, with the active project marked. An
    /// active project the switcher does not offer comes first, with its
    /// count taken from its loaded page.
    pub(super) fn api_projects(&self) -> Vec<HostProject> {
        let active_project_id = self.active_project_id();
        let mut projects = self
            .projects
            .mru_project_items()
            .into_iter()
            .map(|project_item| HostProject {
                active_session_count: project_item.active_session_count,
                id: project_item.project.id,
                is_active: project_item.project.id == active_project_id,
                name: project_item.project.display_label(),
            })
            .collect::<Vec<_>>();
        if !projects.iter().any(|project| project.is_active) {
            let active_session_count = self
                .sessions
                .sessions()
                .iter()
                .filter(|session| {
                    !matches!(
                        session.status,
                        SessionStatus::Done
                            | SessionStatus::Canceled
                            | SessionStatus::Queued
                            | SessionStatus::Merging
                    )
                })
                .count();
            projects.insert(
                0,
                HostProject {
                    active_session_count: u32::try_from(active_session_count).unwrap_or(u32::MAX),
                    id: active_project_id,
                    is_active: true,
                    name: self.projects.project_name().to_string(),
                },
            );
        }

        projects
    }

    /// Lists `scope`'s sessions and reads its status in the same turn, so
    /// both describe the project the answer names.
    async fn api_project_sessions(
        &mut self,
        initiator: CommandInitiator,
        scope: ProjectScope,
    ) -> Result<ProjectSessions, ApiSessionError> {
        let project_id = self.resolve_project_scope(initiator, scope).await?;
        let context = self
            .projects
            .context(project_id)
            .ok_or(ApiSessionError::NotFound)?;
        let page = self
            .sessions
            .state()
            .page_for_project(project_id)
            .ok_or(ApiSessionError::NotFound)?;

        Ok(ProjectSessions {
            entries: api_session_entries(page),
            project_id: Some(project_id),
            status: Some(self.api_project_status_for(context)),
        })
    }

    /// Reports whether the host lists `session_id`: whether it is on the
    /// loaded page of a listed project. A session of a listed project whose
    /// page is not loaded has that page loaded first. A paired device only
    /// reaches this for a session on the active page.
    async fn api_session_listed(
        &mut self,
        initiator: CommandInitiator,
        session_id: &SessionId,
    ) -> Result<bool, ApiSessionError> {
        if self.sessions.session_for_id(session_id).is_none() {
            let project_id = self
                .services
                .db()
                .sessions()
                .load_session_project_id(session_id)
                .await
                .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
            let Some(project_id) = project_id.filter(|project_id| {
                *project_id != self.active_project_id() && self.projects.is_listed(*project_id)
            }) else {
                return Ok(false);
            };
            self.ensure_project_page(project_id).await?;
        }
        let is_listed = self.sessions.session_for_id(session_id).is_some();
        self.renew_session_page_lease(initiator, session_id);

        Ok(is_listed)
    }

    /// Answers one read of `session_id` that its caller may make.
    async fn answer_session_query(&mut self, session_id: SessionId, query: SessionQuery) {
        match query {
            SessionQuery::Get { response_tx } => {
                let _ = response_tx.send(self.get_api_session(&session_id).await);
            }
            SessionQuery::GetDiff { response_tx } => {
                self.start_api_session_diff(&session_id, response_tx);
            }
            SessionQuery::GetGraphs {
                graph_id,
                node_id,
                response_tx,
            } => {
                let request = ag_session::SessionGraphsRequest {
                    graph_id,
                    node_id,
                    session_id,
                };
                let _ = response_tx.send(self.api_session_graphs(request).await);
            }
            SessionQuery::ListFiles { response_tx } => {
                self.start_api_session_files(&session_id, response_tx);
            }
        }
    }

    /// Answers one graph, proposal, or task read that its caller may make.
    async fn answer_local_query(&self, query: LocalQuery) {
        match query {
            LocalQuery::GetAgentTaskHistory {
                response_tx,
                task_id,
            } => {
                let _ = response_tx.send(self.load_api_task_history(&task_id).await);
            }
            LocalQuery::GetExecutionGraph {
                graph_id,
                response_tx,
            } => {
                let _ = response_tx.send(self.execution_graph_progress(&graph_id).await);
            }
            LocalQuery::GetGraphProposal { id, response_tx } => {
                let result = self
                    .services
                    .db()
                    .graph_proposals()
                    .get(&id, None)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()));
                let _ = response_tx.send(result);
            }
            LocalQuery::GetTask {
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
            LocalQuery::ListTasks {
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
        }
    }

    /// Collects what a session snapshot reports from the loaded `session`
    /// rather than its stored row, with its shown focused review as
    /// `focused_review` reports it. Remote clients compare the same details,
    /// so every live input of a snapshot is listed here once.
    pub(super) fn live_session_details<FocusedReview>(
        &self,
        session: &Session,
        focused_review: impl Fn(
            &HashMap<SessionId, ReviewCacheEntry>,
            &Session,
        ) -> Option<FocusedReview>,
    ) -> LiveSessionDetails<FocusedReview> {
        LiveSessionDetails {
            focused_review: focused_review(&self.review_cache, session),
            progress: session.status_line(self.session_progress_message(&session.id)),
            queued_actions: session.queued_action_labels(),
            queued_messages: session.queued_message_texts(),
        }
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

    /// Resolves `scope` for a project file listing from `initiator` into
    /// the project and its checkout.
    async fn api_project_root(
        &mut self,
        initiator: CommandInitiator,
        scope: ProjectScope,
    ) -> Result<(i64, PathBuf), ApiSessionError> {
        let project_id = self.resolve_project_scope(initiator, scope).await?;
        let root = self
            .projects
            .context(project_id)
            .ok_or(ApiSessionError::NotFound)?
            .working_dir()
            .to_path_buf();

        Ok((project_id, root))
    }

    /// Answers one programmatic project file-listing query: lists `root`,
    /// the checkout of `project_id` and the root a new session's draft looks
    /// up `@` mentions in, from the `@`-mention index when it already caches
    /// that root and otherwise on a blocking worker.
    fn start_api_project_files(
        &mut self,
        project_id: i64,
        root: PathBuf,
        response_tx: oneshot::Sender<Result<Vec<SessionFileEntry>, ApiSessionError>>,
    ) {
        if let Some(entries) = self.sessions.at_mention_index_for_root(&root) {
            let _ = response_tx.send(Ok(api_file_entries(entries)));

            return;
        }
        // The loader names a session only in its failure log.
        let log_id = SessionId::from(format!("project-{project_id}"));
        spawn::spawn_detached(async move {
            let entries = TaskService::load_at_mention_entries(root, &log_id).await;
            let _ = response_tx.send(Ok(api_file_entries(entries)));
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
    ///
    /// A request may name any listed project. Another project than the
    /// active one has its page loaded, so the new session joins that page
    /// rather than the terminal's list, and everything it starts from is
    /// that project's own: its checkout must be free of its sync, its base
    /// branch is the one checked out there now, and its default settings
    /// are its stored ones. A paired device's request for another project
    /// was refused before it got here; one that inherits from a session the
    /// host lists to it names that session's project, the active one, or is
    /// refused here before any page loads.
    pub(super) async fn prepare_api_session_creation(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<PreparedSessionCreation, ApiSessionError> {
        self.validate_api_session_request(&request)?;
        let inherited_settings = self
            .inherited_creation_settings(
                request.inherit_from_session_id.as_ref(),
                request.project_id,
            )
            .await?;
        if request.project_id != self.active_project_id() {
            self.ensure_project_page(request.project_id).await?;
        }
        self.ensure_project_checkout_available(request.project_id)
            .map_err(api_error_from_app)?;

        let (base_branch_override, creation_settings) = inherited_settings
            .map_or((None, None), |inherited| {
                (Some(inherited.base_branch), Some(inherited.settings))
            });
        let base_branch = self
            .api_creation_base_branch(request.project_id, base_branch_override)
            .await?;
        let runs_on_harness = request.mode == CreateSessionMode::Harness;
        match request.mode {
            CreateSessionMode::Draft => {
                let session_id = self
                    .sessions
                    .create_draft_session_for_project_with_settings(
                        &self.services,
                        request.project_id,
                        &base_branch,
                        creation_settings,
                    )
                    .await
                    .map_err(api_error_from_session)?;

                return Ok(PreparedSessionCreation::Persisted(session_id));
            }
            CreateSessionMode::Regular | CreateSessionMode::Harness => {}
        }
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
            base_branch,
            project_id: request.project_id,
            settings,
        })
    }

    /// Describes `scope`'s project for `initiator`, with its base branch
    /// and default model, and the creation modes
    /// [`Self::validate_api_session_request`] accepts now, each with that
    /// project's own default settings.
    async fn api_host_context(
        &mut self,
        initiator: CommandInitiator,
        scope: ProjectScope,
    ) -> Result<HostContext, ApiSessionError> {
        let project_id = self.resolve_project_scope(initiator, scope).await?;
        let context = self
            .projects
            .context(project_id)
            .ok_or(ApiSessionError::NotFound)?;
        let base_branch = context
            .git_branch()
            .map(str::to_string)
            .ok_or_else(git_branch_required)?;
        let project_name = context.name().to_string();
        let HostInputs {
            harness_model,
            has_agent_cli,
            models,
        } = self.api_host_inputs();
        let settings = self
            .sessions
            .resolve_session_creation_settings(&self.services, project_id, None)
            .await
            .map_err(api_error_from_session)?;
        let mut session_modes = Vec::new();
        if has_agent_cli {
            session_modes.push(HostSessionMode {
                agent: settings.agent,
                mode: CreateSessionMode::Regular,
                reasoning_level: settings.reasoning_level,
                response_style: settings.response_style,
                speed_mode: settings.speed_mode,
            });
        }
        // Harness sessions start at normal speed, as creation sets them.
        if let Some(harness_model) = harness_model {
            session_modes.push(HostSessionMode {
                agent: AgentSelection::new(AgentKind::Harness, harness_model),
                mode: CreateSessionMode::Harness,
                reasoning_level: settings.reasoning_level,
                response_style: settings.response_style,
                speed_mode: SpeedMode::Normal,
            });
        }
        // A host that offers only Harness sessions starts them on the harness
        // model, not on the regular agent's default.
        let default_model = session_modes
            .first()
            .map_or(settings.agent, |session_mode| session_mode.agent)
            .model();

        Ok(HostContext {
            base_branch,
            default_model,
            models,
            project_id,
            project_name,
            session_modes,
        })
    }

    /// Collects what [`Self::api_host_context`] reads from host memory for
    /// every project; each project's branch and name are in its status.
    /// Remote clients compare the same inputs, so each is listed here once.
    pub(super) fn api_host_inputs(&self) -> HostInputs {
        HostInputs {
            harness_model: self.services.harness_availability().default_model(),
            has_agent_cli: self.services.has_agent_cli(),
            models: self.api_session_models(),
        }
    }

    /// Reports the active project's branch, its ahead and behind counts from
    /// the latest background fetch, and the explicit sync the status bar
    /// shows for it. A sync of another project is left out.
    pub(super) fn api_project_status(&self) -> HostProjectStatus {
        self.api_project_status_for(self.projects.active_context())
    }

    /// Reports the status of the project `context` describes, the active
    /// project or one whose page is loaded: its branch, its ahead and behind
    /// counts, and its latest explicit sync. A viewed project reports the
    /// branch detected when its page loaded and no counts, which only the
    /// active project's background fetch reports.
    pub(super) fn api_project_status_for(&self, context: &ProjectContext) -> HostProjectStatus {
        HostProjectStatus {
            ahead_behind: context.git_status(),
            branch: context.git_branch().map(str::to_string),
            project_name: context.name().to_string(),
            sync: self
                .project_sync_statuses
                .get(&context.id())
                .map(api_project_sync),
        }
    }

    /// Starts a sync of `scope`'s branch through the same workflow as the
    /// terminal's `s`, and reports a sync that the project policy refused
    /// to start.
    ///
    /// Another listed project has its page loaded first, as every request
    /// naming it does, and syncs in its own checkout; see
    /// [`Self::start_sync_main_for`]. A paired device's request for a
    /// project other than the active one was refused before it got here.
    ///
    /// # Errors
    /// Returns [`ApiSessionError::NotFound`] for a project the host does not
    /// list, or an operation error when its page cannot be loaded or the
    /// sync is blocked.
    pub(super) async fn sync_api_project(
        &mut self,
        scope: ProjectScope,
    ) -> Result<(), ApiSessionError> {
        let project_id = match scope {
            ProjectScope::Active => self.active_project_id(),
            ProjectScope::Project(project_id) => project_id,
        };
        if project_id == self.active_project_id() {
            self.start_sync_main();
        } else {
            self.ensure_project_page(project_id).await?;
            self.start_sync_main_for(project_id).await?;
        }
        let blocked_message = self
            .project_sync_statuses
            .get(&project_id)
            .and_then(|status| match &status.phase {
                ProjectSyncPhase::Blocked { message } => Some(message),
                _ => None,
            });
        if let Some(message) = blocked_message {
            return Err(ApiSessionError::Operation(format!(
                "Sync is blocked: {message}"
            )));
        }

        Ok(())
    }

    /// Lists the agents and models a session can switch to now: every model
    /// of each available agent CLI, then the harness's ready listed models, or
    /// its fixed catalog before any are listed. A session switches only among
    /// its own agent's entries, as the terminal's `/model` offers them.
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

        models
    }

    /// Changes one session's settings through the terminal's own setting
    /// workflows, after checking the whole change.
    ///
    /// Nothing is written until the whole change passes the checks the
    /// terminal makes by construction: the session's status shows the
    /// composer, where `/model` and the other setting commands are reachable;
    /// a model is one `/model` offers for the chosen agent, the session's own
    /// unless the change names another together with a model; and fast
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
        let agent_kind = change.agent.unwrap_or_else(|| current_agent.kind());
        if change.model.is_none() && agent_kind != current_agent.kind() {
            return Err(ApiSessionError::Operation(format!(
                "Choose a model for agent `{agent_kind}`"
            )));
        }
        if let Some(model) = change.model
            && !self
                .api_session_models()
                .into_iter()
                .any(|offered| offered.kind() == agent_kind && offered.model() == model)
        {
            return Err(ApiSessionError::Operation(format!(
                "Model `{}` is not available to session `{session_id}` on agent `{agent_kind}`",
                model.as_str()
            )));
        }
        let selected_agent = change
            .model
            .map(|model| AgentSelection::new(agent_kind, model));
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

    /// Checks that `request` names a listed project, answering any other
    /// as missing, and that a backend can run the session it asks for.
    fn validate_api_session_request(
        &self,
        request: &CreateSessionRequest,
    ) -> Result<(), ApiSessionError> {
        if !self.projects.is_listed(request.project_id) {
            return Err(ApiSessionError::NotFound);
        }
        if request.mode == CreateSessionMode::Harness
            && !self.services.harness_availability().is_available()
        {
            return Err(ApiSessionError::Operation(
                "Harness sessions need a runnable model: a local Ollama or OpenAI-compatible \
                 server serving a model, or a hosted model whose API key is stored or exported, \
                 such as `MODEL_API_KEY`."
                    .to_string(),
            ));
        }
        // Without agent CLIs, regular and draft sessions start on the harness.
        if !self.services.has_agent_cli() && !self.services.harness_availability().is_available() {
            return Err(ApiSessionError::Operation(
                "No supported backend CLI found on `PATH`.".to_string(),
            ));
        }
        Ok(())
    }

    /// Loads one complete session aggregate from persistence plus live queue
    /// state, and the summary of its latest graph proposal.
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
        let live_session = self.sessions.session_for_id(session_id);
        let LiveSessionDetails {
            focused_review,
            progress,
            queued_actions,
            queued_messages,
        } = live_session.map_or_default(|session| {
            self.live_session_details(session, review::shown_focused_review)
        });
        let graph = self.api_graph_summary(session_id).await;

        let live_status = live_session.map(|session| session.status);
        let session = build_api_session(row, message_rows, queued_messages)?;
        // A running focused review marks the session `AgentReview` only in
        // memory; the row keeps `Review` or `Watching`. Report it as the
        // session list does, and otherwise trust the row, which the worker
        // updates before the live state hears of it.
        let status = match (session.status, live_status) {
            (SessionStatus::Review | SessionStatus::Watching, Some(SessionStatus::AgentReview)) => {
                SessionStatus::AgentReview
            }
            (persisted_status, _) => persisted_status,
        };
        // As in the terminal's question panel, the session's graph work asks
        // its questions only while the turn asks none. A failed lookup leaves
        // them out; a chat reply still answers them.
        let graph_questions = if session.questions.is_empty() {
            self.shown_graph_questions(session_id, status)
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let (questions, graph_question_target) = match graph_questions {
            Some(pending) => (pending.questions, Some(pending.target.key())),
            None => (session.questions, None),
        };

        Ok(Some(ag_session::Session {
            focused_review,
            graph,
            graph_question_target,
            progress,
            questions,
            queued_actions,
            status,
            ..session
        }))
    }

    /// Sends one validated API message through the existing session workflow.
    ///
    /// `images` are stored in the session's prompt-image directory, as pasted
    /// images are, and attached as `[Image #n]` in their order; they are
    /// removed again when the session does not take the message, unless a
    /// staged draft whose start failed still refers to them. Graph work
    /// waiting for the session's chat receives the message first, as it does
    /// for terminal messages. Accepting a graph reply acknowledges that it
    /// was recorded, not that its decision finished.
    pub(super) async fn send_api_message(
        &mut self,
        session_id: &SessionId,
        message: String,
        images: Vec<MessageImage>,
    ) -> Result<(), ApiSessionError> {
        if message.trim().is_empty() && images.is_empty() {
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
        let prompt = TurnPrompt {
            attachments: self.persist_api_message_images(session_id, images).await?,
            ..TurnPrompt::from_text(message)
        };
        let mut image_paths: Vec<PathBuf> = prompt.local_image_paths().cloned().collect();
        let result = self
            .deliver_api_prompt(session_id, prompt, status, plan)
            .await;
        if result.is_err() {
            // A staged draft keeps its images when its start fails, so the
            // retried start still finds them.
            let draft_attachments = self
                .sessions
                .session_for_id(session_id)
                .map_or_default(|session| session.draft_attachments.as_slice());
            image_paths.retain(|path| {
                !draft_attachments
                    .iter()
                    .any(|attachment| &attachment.local_image_path == path)
            });
            SessionManager::cleanup_prompt_attachment_paths(self.services.fs_client(), image_paths)
                .await;
        }

        result
    }

    /// Stores the images of one API message in the session's prompt-image
    /// directory and returns them as attachments numbered from 1, removing
    /// the ones already stored when one fails.
    async fn persist_api_message_images(
        &self,
        session_id: &SessionId,
        images: Vec<MessageImage>,
    ) -> Result<Vec<TurnPromptAttachment>, ApiSessionError> {
        let fs_client = self.services.fs_client();
        let clock = self.services.clock();
        let mut attachments = Vec::with_capacity(images.len());
        for (index, image) in images.into_iter().enumerate() {
            let attachment_number = index + 1;
            match persist_message_image(
                fs_client.as_ref(),
                clock.as_ref(),
                session_id.as_str(),
                attachment_number,
                image,
            )
            .await
            {
                Ok(local_image_path) => attachments.push(TurnPromptAttachment {
                    local_image_path,
                    placeholder: PromptAttachment::placeholder_for(attachment_number),
                }),
                Err(error) => {
                    let stored = attachments
                        .into_iter()
                        .map(|attachment| attachment.local_image_path)
                        .collect();
                    SessionManager::cleanup_prompt_attachment_paths(fs_client, stored).await;

                    return Err(ApiSessionError::Operation(format!(
                        "Cannot store the message's images: {error}"
                    )));
                }
            }
        }

        Ok(attachments)
    }

    /// Delivers one API prompt as a graph reply or through `plan`.
    async fn deliver_api_prompt(
        &mut self,
        session_id: &SessionId,
        prompt: TurnPrompt,
        status: SessionStatus,
        plan: MessagePlan,
    ) -> Result<(), ApiSessionError> {
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
            .session_for_id(session_id)
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
    ///
    /// Answers that name a `graph_question_target` answer the graph work's
    /// questions instead, as the snapshot reports them, and only while the
    /// turn asks none. Answers without one answer only the turn's questions.
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
        if let Some(target) = request.graph_question_target.clone() {
            // The turn's own questions hide the graph work's.
            if !questions.is_empty() {
                return Err(ApiSessionError::Operation(
                    QUESTIONS_NOT_PENDING.to_string(),
                ));
            }

            return Box::pin(self.answer_api_graph_questions(session_id, &target, request)).await;
        }
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

    /// Sends a complete answer set to the graph work whose questions the
    /// session's snapshot reports now, in the ordinary clarification reply
    /// format, as the terminal's question panel does, only while that work
    /// is still the one `target` names.
    async fn answer_api_graph_questions(
        &mut self,
        session_id: &SessionId,
        target: &str,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        let status = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?
            .status;
        // Answers entered for other graph work never reach the work pending
        // now, even when it asks the same questions.
        let pending = self
            .shown_graph_questions(session_id, status)
            .await
            .map_err(ApiSessionError::Operation)?
            .filter(|pending| pending.target.key() == target)
            .ok_or_else(|| ApiSessionError::Operation(QUESTIONS_NOT_PENDING.to_string()))?;
        validate_question_answers(&pending.questions, &request.answers)?;
        let outcome = self
            .send_graph_answers(
                session_id,
                &pending.target,
                &request.answers,
                request.attachments,
            )
            .await;

        match outcome {
            GraphAnswerOutcome::Sent => Ok(()),
            GraphAnswerOutcome::Stale => Err(ApiSessionError::Operation(
                QUESTIONS_NOT_PENDING.to_string(),
            )),
            GraphAnswerOutcome::Retry => Err(ApiSessionError::Operation(
                GRAPH_ANSWERS_NOT_TAKEN.to_string(),
            )),
        }
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

    /// Resolves the base branch a new session of `project_id` starts from:
    /// `base_branch_override`, inherited from a source session, or else the
    /// project's branch. The active project's branch is the one its status
    /// tracks; another project's is detected in its checkout now.
    async fn api_creation_base_branch(
        &self,
        project_id: i64,
        base_branch_override: Option<String>,
    ) -> Result<String, ApiSessionError> {
        if let Some(base_branch) = base_branch_override {
            return Ok(base_branch);
        }
        let base_branch = if project_id == self.active_project_id() {
            self.projects.git_branch().map(str::to_string)
        } else {
            self.detect_viewed_project_branch(project_id).await
        };

        base_branch.ok_or_else(git_branch_required)
    }

    /// Attempts to register a newly persisted session on its project's
    /// loaded page before acknowledging creation, scheduling a refresh retry
    /// when loading is temporarily unavailable.
    pub(super) async fn finish_api_session_creation(&mut self, session_id: &str) {
        if self.sessions.session_for_id(session_id).is_some() {
            return;
        }

        let _ = self
            .sessions
            .register_created_session(&self.services, session_id, &self.projects)
            .await;
        if self.sessions.session_for_id(session_id).is_some() {
            return;
        }

        self.services.emit_app_event(AppEvent::RefreshSessions);
    }
}

/// What a session snapshot reports from the loaded session rather than its
/// stored row, with the shown focused review as `FocusedReview`: the review
/// itself for the snapshot, or only its identity where it is compared.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct LiveSessionDetails<FocusedReview> {
    /// Focused review the session shows.
    pub(super) focused_review: Option<FocusedReview>,
    /// Line describing the session's current work.
    pub(super) progress: Option<String>,
    /// Labels of the workflow actions waiting behind the active turn.
    pub(super) queued_actions: Vec<String>,
    /// Messages waiting behind the active turn.
    pub(super) queued_messages: Vec<String>,
}

impl<FocusedReview> Default for LiveSessionDetails<FocusedReview> {
    /// Reports no live detail, as for a session the host has not loaded.
    fn default() -> Self {
        Self {
            focused_review: None,
            progress: None,
            queued_actions: Vec::new(),
            queued_messages: Vec::new(),
        }
    }
}

/// What the host context reads from host memory for every project. Its
/// creation defaults come from stored settings, and its project's branch and
/// name are in the project's status.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct HostInputs {
    /// Model new Harness sessions start on, while one is available.
    harness_model: Option<AgentModel>,
    /// Whether any agent CLI can run regular sessions.
    has_agent_cli: bool,
    /// Agents and models a session can switch to.
    models: Vec<AgentSelection>,
}

/// Launch settings loaded from one existing session.
struct InheritedCreationSettings {
    base_branch: String,
    settings: SessionCreationSettings,
}

/// Summarizes the sessions on one loaded page in their display order, with
/// the diff size, model, timer, branch, and review figures the session list
/// and the session footer show. A viewed page holds no git statuses, so its
/// entries report no branch distances or merge conflicts.
pub(super) fn api_session_entries(page: &SessionPage) -> Vec<ag_session::SessionListEntry> {
    let git_statuses = page.session_git_statuses();

    page.sessions()
        .iter()
        .map(|session| {
            let git_status = git_statuses.get(&session.id).copied().unwrap_or_default();

            ag_session::SessionListEntry {
                stats: Some(api_session_list_stats(session, git_status)),
                summary: ag_session::SessionSummary {
                    id: session.id.clone(),
                    status: session.status,
                    title: session.title.clone(),
                    updated_at: session.updated_at,
                },
            }
        })
        .collect()
}

/// Collects the list figures of one loaded session, with the branch
/// distances that the background Git poll last reported for it.
fn api_session_list_stats(
    session: &Session,
    git_status: SessionGitStatus,
) -> ag_session::SessionListStats {
    ag_session::SessionListStats {
        added_lines: session.stats.added_lines,
        base_ahead_behind: git_status.base_status,
        deleted_lines: session.stats.deleted_lines,
        has_merge_conflict: git_status.has_merge_conflict.unwrap_or(false),
        in_progress_started_at: session.in_progress_started_at,
        in_progress_total_seconds: session.in_progress_total_seconds,
        is_published: session.published_upstream_ref.is_some(),
        model: session.agent.model(),
        reasoning_level: session.effective_reasoning_level(),
        review_request: session
            .review_request
            .as_ref()
            .map(|review_request| review_request.summary.clone()),
        size_label: session.size.label().to_string(),
        upstream_ahead_behind: git_status.remote_status,
    }
}

/// Refuses to describe or create in a project whose branch is not detected.
fn git_branch_required() -> ApiSessionError {
    ApiSessionError::Operation("Git branch is required to create a session".to_string())
}

/// Converts the status bar's project sync into its programmatic form.
fn api_project_sync(status: &ProjectSyncStatus) -> HostProjectSync {
    let phase = match &status.phase {
        ProjectSyncPhase::Running => HostProjectSyncPhase::Running,
        ProjectSyncPhase::ResolvingConflicts {
            conflicted_file_count,
        } => HostProjectSyncPhase::ResolvingConflicts {
            conflicted_file_count: *conflicted_file_count,
        },
        ProjectSyncPhase::Complete {
            pulled_commits,
            pushed_commits,
            resolved_conflict_count,
        } => HostProjectSyncPhase::Complete {
            pulled_commits: *pulled_commits,
            pushed_commits: *pushed_commits,
            resolved_conflict_count: *resolved_conflict_count,
        },
        ProjectSyncPhase::Blocked { message } => HostProjectSyncPhase::Blocked {
            message: message.clone(),
        },
        ProjectSyncPhase::Failed { message } => HostProjectSyncPhase::Failed {
            message: message.clone(),
        },
    };

    HostProjectSync {
        branch: status.context.default_branch.clone(),
        phase,
    }
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
            NO_QUESTIONS_TO_ANSWER.to_string(),
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
    // Like the terminal's draft preview, a started draft stages nothing.
    let draft_prompt = (row.is_draft && status == SessionStatus::Draft && !row.prompt.is_empty())
        .then_some(row.prompt);

    Ok(ag_session::Session {
        created_at: row.created_at,
        draft_prompt,
        focused_review: None,
        graph: None,
        graph_question_target: None,
        id: SessionId::from(row.id),
        messages,
        progress: None,
        published_upstream_ref: row.published_upstream_ref,
        questions,
        queued_actions: Vec::new(),
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
