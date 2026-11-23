//! Programmatic session orchestration facade and host backend port.

use std::sync::Arc;

use ag_protocol::{TurnPrompt, TurnPromptAttachment};
use async_trait::async_trait;

use crate::error::SessionError;
use crate::model::{ReviewRequest, Session, SessionId, SessionListEntry, SessionSummary};
use crate::{
    AcceptTaskPlanRequest, AgentModel, AgentSelection, CreateTaskRequest, ReasoningLevel,
    ResponseStyle, SpeedMode, SubmitTaskRequest, Task,
};

/// Creation strategy for a new session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CreateSessionMode {
    /// Creates a regular session with an eagerly materialized worktree.
    #[default]
    Regular,
    /// Creates a draft whose worktree is materialized on first send.
    Draft,
    /// Creates a regular session that runs on the in-process
    /// [`crate::AgentKind::Harness`] with its default model.
    Harness,
}

/// Explicit input for creating one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSessionRequest {
    /// Existing session whose launch settings should be copied.
    ///
    /// When absent, the host resolves the owning project's current defaults.
    pub inherit_from_session_id: Option<SessionId>,
    /// Determines whether the session is regular, deferred, or harness-backed.
    pub mode: CreateSessionMode,
    /// Project that owns the new session.
    pub project_id: i64,
}

/// One structured response to a persisted clarification question.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionAnswer {
    /// User response paired with `question`.
    pub answer: String,
    /// Exact persisted question text being answered.
    pub question: String,
}

/// Structured input for resuming one session from clarification questions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerQuestionsRequest {
    /// Ordered question and answer pairs for the current question set.
    pub answers: Vec<QuestionAnswer>,
    /// Local images referenced by `[Image #n]` placeholders in `answers`,
    /// delivered with the reply turn like prompt attachments. The caller
    /// keeps the files when the request fails, so it can retry with them.
    pub attachments: Vec<TurnPromptAttachment>,
}

/// Idempotent turn submitted to one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableMessageRequest {
    /// Agent-facing prompt.
    pub message: String,
    /// Stable operation identifier reused when delivery is retried.
    pub operation_id: String,
    /// Whether the machine-authored prompt is shown in the human transcript.
    pub visibility: DurableMessageVisibility,
}

/// Transcript treatment for one durable prompt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DurableMessageVisibility {
    /// Deliver generated context without displaying it as user chat.
    #[default]
    Hidden,
    /// Display the submitted instruction in session history.
    Visible,
}

/// Operation a host may choose to serve to frontends that check
/// [`SessionService::supports`] first, such as remote control.
///
/// A frontend offers an operation only when the host reports it, so adding a
/// variant never exposes an operation that an existing host has not opted
/// into.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum SessionOperation {
    /// Creating a session in the active project through
    /// [`SessionBackend::create_session`].
    CreateSession,
    /// Describing the active project through
    /// [`SessionBackend::host_context`].
    HostContext,
    /// Merging a review-ready session through
    /// [`SessionBackend::merge_session`].
    MergeSession,
    /// Reading a session's changes through
    /// [`SessionBackend::get_session_diff`].
    SessionDiff,
    /// Listing the files and directories in the active project's checkout
    /// through [`SessionBackend::list_project_files`], for drafting a new
    /// session before it has a worktree.
    ProjectFiles,
    /// Listing the files and directories in a session's worktree through
    /// [`SessionBackend::list_session_files`].
    SessionFiles,
    /// Stopping a running turn through [`SessionBackend::stop_turn`].
    StopTurn,
    /// Rebasing a session branch onto its base branch through
    /// [`SessionBackend::rebase_session`].
    RebaseSession,
    /// Changing a session's model, reasoning, style, or speed through
    /// [`SessionBackend::update_session_settings`].
    SessionSettings,
    /// Publishing a session to its existing branch, or under its default name
    /// the first time, and creating or refreshing its review request through
    /// [`SessionBackend::create_review_request`].
    PublishSession,
    /// Reporting the active project's branch, its distance from upstream,
    /// and its latest sync through [`SessionBackend::project_status`].
    ProjectStatus,
    /// Syncing the active project's branch with its upstream through
    /// [`SessionBackend::sync_project`].
    SyncProject,
    /// Reading a session's graph proposal and execution graphs through
    /// [`SessionBackend::session_graphs`], and summarizing them in
    /// [`Session::graph`].
    Graphs,
}

/// Settings to change on one session; `None` keeps a setting as it is.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionSettingsChange {
    /// Model to switch to; the session's own agent runs it, as in the
    /// terminal's `/model`.
    pub model: Option<AgentModel>,
    /// Reasoning level to use for later turns.
    pub reasoning_level: Option<ReasoningLevel>,
    /// Response style to use for later turns.
    pub response_style: Option<ResponseStyle>,
    /// Response speed to use for later turns.
    pub speed_mode: Option<SpeedMode>,
}

/// One file a session's diff changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDiffFile {
    /// Lines the file's patch adds.
    pub added_lines: u64,
    /// Lines the file's patch removes.
    pub deleted_lines: u64,
    /// Path before a rename, or `None` when the path did not change.
    pub old_path: Option<String>,
    /// The file's unified diff, including its `diff --git` header.
    pub patch: String,
    /// Path after the change; a deleted file keeps its old path.
    pub path: String,
    /// How the change affects the file.
    pub status: SessionDiffFileStatus,
}

/// How a session's diff changes one file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionDiffFileStatus {
    /// The file is new.
    Added,
    /// The file is removed.
    Deleted,
    /// The file's contents or mode changed.
    Modified,
    /// The file moved, possibly with changes.
    Renamed,
}

/// One file or directory in a session's worktree or the active project's
/// checkout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFileEntry {
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Path relative to the listed root, with `/` separators.
    pub path: String,
}

/// The project a host creates new sessions in, and how it creates them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostContext {
    /// Branch new sessions start from.
    pub base_branch: String,
    /// Model a new session in the first of `session_modes` starts with: the
    /// harness model when only Harness sessions are offered, otherwise the
    /// regular agent's default.
    pub default_model: AgentModel,
    /// Agents and models a session can switch to now, in display order.
    /// A session switches only among the entries of its own agent, as the
    /// terminal's `/model` offers them; each gives the model's id and name.
    pub models: Vec<AgentSelection>,
    /// Identifier of the active project, the only one that accepts new
    /// sessions.
    pub project_id: i64,
    /// Display name of the active project.
    pub project_name: String,
    /// Creation modes the host can serve now, in display order, each with
    /// the agent and model its new sessions start with.
    pub session_modes: Vec<HostSessionMode>,
}

/// One creation mode a host serves, and how its new sessions start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSessionMode {
    /// Agent and model a new session in this mode starts with.
    pub agent: AgentSelection,
    /// Creation mode.
    pub mode: CreateSessionMode,
    /// Reasoning level a new session in this mode starts with.
    pub reasoning_level: ReasoningLevel,
    /// Response style a new session in this mode starts with.
    pub response_style: ResponseStyle,
    /// Response-speed preference a new session in this mode starts with.
    pub speed_mode: SpeedMode,
}

/// The host's active project as a session list shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProjectStatus {
    /// Commits the branch is ahead of and behind its upstream, in that
    /// order, as of the host's latest fetch; `None` when the host has not
    /// compared them.
    pub ahead_behind: Option<(u32, u32)>,
    /// Branch the project's checkout is on, which new sessions start from;
    /// `None` when it has none, such as a detached `HEAD`.
    pub branch: Option<String>,
    /// Display name of the active project.
    pub project_name: String,
    /// Latest explicit sync of this project, while the host still shows it.
    pub sync: Option<HostProjectSync>,
}

/// One explicit sync of the active project's branch with its upstream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProjectSync {
    /// Branch the sync updates.
    pub branch: String,
    /// How far the sync has come.
    pub phase: HostProjectSyncPhase,
}

/// How far an explicit project sync has come.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostProjectSyncPhase {
    /// The host is pulling, rebasing, or pushing.
    Running,
    /// An agent is resolving the conflicts of this many files.
    ResolvingConflicts {
        /// Files the rebase left in conflict.
        conflicted_file_count: usize,
    },
    /// The branch matches its upstream.
    Complete {
        /// Commits pulled from upstream, when the host counted them.
        pulled_commits: Option<u32>,
        /// Local commits pushed upstream, when the host counted them.
        pushed_commits: Option<u32>,
        /// Files whose conflicts the agent resolved.
        resolved_conflict_count: usize,
    },
    /// The host did not start the sync, such as while a merge runs or the
    /// checkout has uncommitted changes.
    Blocked {
        /// Why the sync could not start.
        message: String,
    },
    /// Git, authentication, or conflict resolution failed.
    Failed {
        /// Why the sync failed.
        message: String,
    },
}

/// Host implementation boundary for session persistence and workflows.
///
/// The trait is object-safe so agent loops and background workflows can hold a
/// programmatic session capability without depending on a concrete frontend.
#[async_trait]
pub trait SessionBackend: Send + Sync {
    /// Reports whether the host serves `operation` to frontends that ask
    /// first. Hosts serve none unless they opt in.
    fn supports(&self, operation: SessionOperation) -> bool {
        let _ = operation;

        false
    }

    /// Reserves generation without waiting for model completion.
    async fn generate_graph_proposal(
        &self,
        request: crate::GenerateGraphProposalRequest,
    ) -> Result<(), SessionError> {
        let _ = request;
        Err(SessionError::Operation(
            "Host does not support graph proposals".into(),
        ))
    }

    /// Reads retained state without generating or launching work.
    async fn get_graph_proposal(
        &self,
        id: &str,
    ) -> Result<Option<crate::GraphProposalState>, SessionError> {
        let _ = id;
        Err(SessionError::Operation(
            "Host does not support graph proposals".into(),
        ))
    }

    /// Applies an explicit action to the exact inspected version.
    async fn act_on_graph_proposal(
        &self,
        id: &str,
        version: u32,
        action: crate::GraphProposalAction,
    ) -> Result<(), SessionError> {
        let _ = (id, version, action);
        Err(SessionError::Operation(
            "Host does not support graph proposals".into(),
        ))
    }

    /// Reads durable progress without launching or retrying work.
    async fn get_execution_graph(
        &self,
        graph_id: &str,
    ) -> Result<Option<crate::ExecutionGraphProgress>, SessionError> {
        let _ = graph_id;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Reads a session's graphs, its latest proposal, and the selected
    /// graph's live progress and outputs, without launching or changing any
    /// work.
    async fn session_graphs(
        &self,
        request: crate::SessionGraphsRequest,
    ) -> Result<crate::SessionGraphs, SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Closes graph admission durably, then signals its owned workers.
    async fn cancel_execution_graph(
        &self,
        request: crate::GraphControlRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Explicitly restarts dispatch without replaying admitted executions.
    async fn resume_execution_graph(
        &self,
        request: crate::ResumeExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Records an explicit increase of one budget's provider calls, then
    /// releases nodes waiting for that credit.
    async fn extend_execution_graph_budget(
        &self,
        request: crate::ExtendGraphBudgetRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Authorizes one new attempt against an exact failed/question execution.
    async fn continue_execution_graph_node(
        &self,
        request: crate::ContinueGraphNodeRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Saves and starts a static graph. Existing admitted nodes are never
    /// replayed. Named budgets are fixed at first launch and their spending is
    /// recorded durably, so relaunching never replenishes them.
    async fn start_execution_graph(
        &self,
        request: StartExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Records explicit acceptance of an exact graph output and wakes ready
    /// dependents. Model output must never call this trusted host boundary.
    async fn accept_execution_graph_result(
        &self,
        reference: crate::GraphResultRef,
    ) -> Result<(), SessionError> {
        let _ = reference;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }
    /// Admits a role-bound attempt with its own isolated conversation.
    async fn submit_agent_task(
        &self,
        session_id: &SessionId,
        request: crate::SubmitAgentTaskRequest,
    ) -> Result<(), SessionError> {
        let _ = (session_id, request);

        Err(SessionError::Operation(
            "Host does not support role-aware tasks".to_string(),
        ))
    }

    /// Reads isolated task responses without exposing host storage types.
    async fn get_agent_task_history(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::TaskAgentHistory>, SessionError> {
        let _ = task_id;

        Err(SessionError::Operation(
            "Host does not support role-aware tasks".to_string(),
        ))
    }

    /// Explicitly accepts a standalone Planner response by producing version.
    async fn accept_agent_plan(
        &self,
        _session_id: &SessionId,
        _request: AcceptTaskPlanRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support plan acceptance".into(),
        ))
    }

    /// Persists an outcome without execution. Older hosts may omit support.
    async fn create_task(
        &self,
        _session_id: &SessionId,
        _request: CreateTaskRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Lists a session's tasks, including unattempted and completed outcomes.
    async fn list_tasks(&self, _session_id: &SessionId) -> Result<Vec<Task>, SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Admits an explicit task attempt. Older hosts may omit task support.
    async fn submit_task(
        &self,
        _session_id: &SessionId,
        _request: SubmitTaskRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Loads the desired outcome and retained attempt history.
    async fn get_task(&self, _task_id: &str) -> Result<Option<Task>, SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Explicitly accepts the task outcome after all attempts have settled.
    async fn complete_task(&self, _task_id: &str) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Describes the active project new sessions are created in. Older hosts
    /// may omit it.
    async fn host_context(&self) -> Result<HostContext, SessionError> {
        Err(SessionError::Operation(
            "Host does not describe its project".to_string(),
        ))
    }

    /// Reports the active project's branch, its distance from upstream, and
    /// its latest explicit sync. Older hosts may omit it.
    async fn project_status(&self) -> Result<HostProjectStatus, SessionError> {
        Err(SessionError::Operation(
            "Host does not report its project status".to_string(),
        ))
    }

    /// Starts or queues a sync of the active project's branch with its
    /// upstream: a pull with rebase, then a push, with agent-assisted
    /// conflict resolution. The host acknowledges admission and reports
    /// progress through [`Self::project_status`]. Older hosts may omit
    /// support.
    async fn sync_project(&self) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support syncing its project".to_string(),
        ))
    }

    /// Creates one session and returns its stable identifier.
    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError>;

    /// Loads one complete session aggregate, including settings and messages.
    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError>;

    /// Loads the files a session changes against its base branch, or `None`
    /// when the host has not loaded the session. Hosts that do not serve
    /// diffs report it as an error.
    async fn get_session_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionDiffFile>>, SessionError> {
        let _ = session_id;

        Err(SessionError::Operation(
            "Host does not serve session diffs".to_string(),
        ))
    }

    /// Lists the files and directories in a session's worktree, directories
    /// first, or `None` when the host has not loaded the session. Hosts that
    /// do not serve file listings report it as an error.
    async fn list_session_files(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionFileEntry>>, SessionError> {
        let _ = session_id;

        Err(SessionError::Operation(
            "Host does not serve session files".to_string(),
        ))
    }

    /// Lists the files and directories in the active project's checkout,
    /// directories first. Hosts that do not serve project file listings
    /// report it as an error.
    async fn list_project_files(&self) -> Result<Vec<SessionFileEntry>, SessionError> {
        Err(SessionError::Operation(
            "Host does not serve project files".to_string(),
        ))
    }

    /// Lists the host's loaded sessions in its display order. Older hosts
    /// may omit listing support.
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        Err(SessionError::Operation(
            "Host does not support session listing".to_string(),
        ))
    }

    /// Lists the host's loaded sessions in its display order, with the
    /// figures a dense list shows. Hosts that do not provide them return
    /// [`Self::list_sessions`] without stats.
    async fn list_session_entries(&self) -> Result<Vec<SessionListEntry>, SessionError> {
        let summaries = self.list_sessions().await?;

        Ok(summaries.into_iter().map(SessionListEntry::from).collect())
    }

    /// Sends one text message, starting, resuming, or queueing as appropriate.
    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError>;

    /// Submits a durable turn without entering the lossy live-chat
    /// queue used while an ordinary user turn is active.
    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), SessionError>;

    /// Answers the complete current clarification-question set.
    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError>;

    /// Cancels one session through the host lifecycle workflow.
    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError>;

    /// Stops the session's running turn and returns the session to review,
    /// keeping its worktree and changes. Older hosts may omit support.
    async fn stop_turn(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let _ = session_id;

        Err(SessionError::Operation(
            "Host does not support stopping a turn".to_string(),
        ))
    }

    /// Starts or queues a rebase of the session branch onto its base
    /// branch; a conflict starts an agent-assisted resolution turn. Older
    /// hosts may omit support.
    async fn rebase_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let _ = session_id;

        Err(SessionError::Operation(
            "Host does not support syncing a session branch".to_string(),
        ))
    }

    /// Changes the session's settings for later turns. A model or speed
    /// change waits while the session still has accepted work. Older hosts
    /// may omit support.
    async fn update_session_settings(
        &self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), SessionError> {
        let _ = (session_id, change);

        Err(SessionError::Operation(
            "Host does not support changing session settings".to_string(),
        ))
    }

    /// Requests merge processing for one review-ready session.
    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError>;

    /// Queues publication of one session branch and creates or refreshes its
    /// review request.
    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError>;
}

/// Stable programmatic facade for session lifecycle operations.
#[derive(Clone)]
pub struct SessionService {
    backend: Arc<dyn SessionBackend>,
}

/// Explicit static-graph launch, separate from ordinary chat.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct StartExecutionGraphRequest {
    /// Validated immutable snapshot to save before admitting nodes.
    pub graph: crate::ExecutionGraph,
    /// Provider calls shared by nodes naming the same budget, including
    /// repairs. These limits cannot be replenished by redelivering an
    /// active launch.
    pub provider_call_limits: std::collections::BTreeMap<String, usize>,
}

impl SessionService {
    /// Generates a proposal through the host's bounded worker workflow.
    ///
    /// # Errors
    /// Returns admission, permissions, or persistence errors.
    pub async fn generate_graph_proposal(
        &self,
        request: crate::GenerateGraphProposalRequest,
    ) -> Result<(), SessionError> {
        self.backend.generate_graph_proposal(request).await
    }

    /// Queries pending, restored, or approved state without execution.
    ///
    /// # Errors
    /// Returns query delivery or persistence errors.
    pub async fn get_graph_proposal(
        &self,
        id: &str,
    ) -> Result<Option<crate::GraphProposalState>, SessionError> {
        self.backend.get_graph_proposal(id).await
    }

    /// Answers the complete question set on an exact proposal version.
    ///
    /// # Errors
    /// Returns stale-version, incomplete-answer, or generation-limit errors.
    pub async fn answer_graph_proposal(
        &self,
        id: &str,
        version: u32,
        answers: Vec<String>,
    ) -> Result<(), SessionError> {
        self.backend
            .act_on_graph_proposal(id, version, crate::GraphProposalAction::Answer(answers))
            .await
    }

    /// Regenerates within the original captured generation ceiling.
    ///
    /// # Errors
    /// Returns stale-version, approved-proposal, or exhausted-limit errors.
    pub async fn regenerate_graph_proposal(
        &self,
        id: &str,
        version: u32,
    ) -> Result<(), SessionError> {
        self.backend
            .act_on_graph_proposal(id, version, crate::GraphProposalAction::Regenerate)
            .await
    }

    /// Regenerates with user feedback, including its images, added to the
    /// generation history, within the original captured generation ceiling.
    ///
    /// # Errors
    /// Returns stale-version, empty-feedback, approved-proposal, or
    /// exhausted-limit errors.
    pub async fn revise_graph_proposal(
        &self,
        id: &str,
        version: u32,
        feedback: TurnPrompt,
    ) -> Result<(), SessionError> {
        self.backend
            .act_on_graph_proposal(id, version, crate::GraphProposalAction::Revise(feedback))
            .await
    }

    /// Approves the exact preview before dispatch. Redelivery never dispatches.
    ///
    /// # Errors
    /// Returns stale-version, resolution, permissions, or publication errors.
    pub async fn approve_graph_proposal(&self, id: &str, version: u32) -> Result<(), SessionError> {
        self.backend
            .act_on_graph_proposal(id, version, crate::GraphProposalAction::Approve)
            .await
    }

    /// Cancels a pending proposal and any active generating utility.
    ///
    /// # Errors
    /// Returns stale-version or already-approved errors.
    pub async fn cancel_graph_proposal(&self, id: &str, version: u32) -> Result<(), SessionError> {
        self.backend
            .act_on_graph_proposal(id, version, crate::GraphProposalAction::Cancel)
            .await
    }

    /// Reads graph/node progress and immutable execution history.
    ///
    /// # Errors
    /// Returns failed persistence or query delivery errors.
    pub async fn get_execution_graph(
        &self,
        graph_id: &str,
    ) -> Result<Option<crate::ExecutionGraphProgress>, SessionError> {
        self.backend.get_execution_graph(graph_id).await
    }

    /// Reads a session's graphs as an inspector shows them: every graph,
    /// the latest proposal with its preview, and the selected graph with
    /// its live progress and the selected node's outputs.
    ///
    /// # Errors
    /// Rejects a `graph_id` of another session, and returns failed
    /// persistence or query delivery errors.
    pub async fn session_graphs(
        &self,
        request: crate::SessionGraphsRequest,
    ) -> Result<crate::SessionGraphs, SessionError> {
        self.backend.session_graphs(request).await
    }

    /// Permanently cancels this graph without canceling unrelated session work.
    ///
    /// # Errors
    /// Rejects missing/stale graphs and failed durable cancellation.
    pub async fn cancel_execution_graph(
        &self,
        request: crate::GraphControlRequest,
    ) -> Result<(), SessionError> {
        self.backend.cancel_execution_graph(request).await
    }

    /// Resumes dispatch explicitly; failed nodes still require a new action.
    ///
    /// # Errors
    /// Rejects canceled/stale graphs, graphs without recorded launch limits,
    /// or ineligible sessions.
    pub async fn resume_execution_graph(
        &self,
        request: crate::ResumeExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        self.backend.resume_execution_graph(request).await
    }

    /// Adds provider calls to one launched budget through a recorded,
    /// idempotent extension; the launch limits stay unchanged.
    ///
    /// # Errors
    /// Rejects stale, completed, or canceled graphs, unknown budgets,
    /// amounts outside `1..=ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS`,
    /// conflicting action identities, and failed persistence.
    pub async fn extend_execution_graph_budget(
        &self,
        request: crate::ExtendGraphBudgetRequest,
    ) -> Result<(), SessionError> {
        self.backend.extend_execution_graph_budget(request).await
    }

    /// Retries a failed node or answers its exact settled clarification set.
    ///
    /// # Errors
    /// Rejects stale sources, partial answers, conflicting action identities,
    /// canceled graphs, and failed persistence or delivery.
    pub async fn continue_execution_graph_node(
        &self,
        request: crate::ContinueGraphNodeRequest,
    ) -> Result<(), SessionError> {
        self.backend.continue_execution_graph_node(request).await
    }

    /// Starts an explicit static graph without waiting for model execution.
    ///
    /// # Errors
    /// Rejects unsupported profiles, stale/conflicting identities, missing
    /// budgets, ineligible sessions, and failed persistence or delivery.
    pub async fn start_execution_graph(
        &self,
        request: StartExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        self.backend.start_execution_graph(request).await
    }

    /// Accepts one exact validated output and releases its acceptance gate.
    ///
    /// # Errors
    /// Rejects stale or unvalidated references and failed persistence.
    pub async fn accept_execution_graph_result(
        &self,
        reference: crate::GraphResultRef,
    ) -> Result<(), SessionError> {
        self.backend.accept_execution_graph_result(reference).await
    }

    /// Accepts a standalone Planner version from durable task history.
    ///
    /// # Errors
    /// Returns an error for stale/ineligible versions or failed persistence.
    pub async fn accept_agent_plan(
        &self,
        session_id: &SessionId,
        request: AcceptTaskPlanRequest,
    ) -> Result<(), SessionError> {
        self.backend.accept_agent_plan(session_id, request).await
    }

    /// Creates an owned session capability backed by a shared host handle.
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self { backend }
    }

    /// Admits an isolated role-bound attempt, acknowledging durable admission.
    /// Identical redeliveries do not execute again. Task completion is
    /// explicit.
    ///
    /// # Errors
    /// Returns an error for invalid input, unaccepted implementation plans,
    /// conflicting identity, or a session unable to accept work.
    pub async fn submit_agent_task(
        &self,
        session_id: &SessionId,
        request: crate::SubmitAgentTaskRequest,
    ) -> Result<(), SessionError> {
        self.backend.submit_agent_task(session_id, request).await
    }

    /// Reads the task's own responses. Attempt status remains authoritative.
    ///
    /// # Errors
    /// Returns an error when the host cannot load the isolated conversation.
    pub async fn get_agent_task_history(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::TaskAgentHistory>, SessionError> {
        self.backend.get_agent_task_history(task_id).await
    }

    /// Persists a task with zero attempts in an existing session, regardless
    /// of its execution status. Does not start work or change session state.
    /// Identical creation requests preserve the task and its history.
    ///
    /// # Errors
    /// Returns an error for empty inputs, conflicting identity, a missing
    /// session, or a host that cannot persist tasks.
    pub async fn create_task(
        &self,
        session_id: &SessionId,
        request: CreateTaskRequest,
    ) -> Result<(), SessionError> {
        self.backend.create_task(session_id, request).await
    }

    /// Lists all tasks owned by a session in creation order, including their
    /// attempt histories in one snapshot. Equal timestamps retain insertion
    /// order. Returns an empty list for a missing session or one without tasks.
    ///
    /// # Errors
    /// Returns an error when the host cannot read tasks.
    pub async fn list_tasks(&self, session_id: &SessionId) -> Result<Vec<Task>, SessionError> {
        self.backend.list_tasks(session_id).await
    }

    /// Admits one task attempt through the existing serialized session worker.
    /// Creates the task atomically if it does not already exist.
    /// Acceptance does not wait for execution or imply task completion.
    /// Identical deliveries are acknowledged without running again.
    ///
    /// # Errors
    /// Returns an error for conflicting identity, an unavailable host, a
    /// completed task, or a session that cannot accept a new durable turn.
    pub async fn submit_task(
        &self,
        session_id: &SessionId,
        request: SubmitTaskRequest,
    ) -> Result<(), SessionError> {
        self.backend.submit_task(session_id, request).await
    }

    /// Loads a task and all its attempts from durable storage.
    ///
    /// # Errors
    /// Returns an error when the host cannot read tasks.
    pub async fn get_task(&self, task_id: &str) -> Result<Option<Task>, SessionError> {
        self.backend.get_task(task_id).await
    }

    /// Explicitly accepts the outcome. Successful model execution alone never
    /// completes a task. Repeated completion retains its original timestamp.
    ///
    /// # Errors
    /// Returns an error if the task is missing, an attempt is unfinished, or
    /// the host cannot persist completion.
    pub async fn complete_task(&self, task_id: &str) -> Result<(), SessionError> {
        self.backend.complete_task(task_id).await
    }

    /// Reports whether the host serves `operation` to frontends that ask
    /// first.
    #[must_use]
    pub fn supports(&self, operation: SessionOperation) -> bool {
        self.backend.supports(operation)
    }

    /// Describes the active project new sessions are created in.
    ///
    /// # Errors
    /// Returns an error when the host does not describe its project or
    /// cannot load its defaults.
    pub async fn host_context(&self) -> Result<HostContext, SessionError> {
        self.backend.host_context().await
    }

    /// Reports the active project's branch, its distance from upstream, and
    /// its latest explicit sync.
    ///
    /// # Errors
    /// Returns an error when the host does not report its project status.
    pub async fn project_status(&self) -> Result<HostProjectStatus, SessionError> {
        self.backend.project_status().await
    }

    /// Starts or queues a sync of the active project's branch with its
    /// upstream. The host acknowledges admission; [`Self::project_status`]
    /// reports the progress and the outcome.
    ///
    /// # Errors
    /// Returns an error when the host cannot start the sync now, such as
    /// while a merge runs, or does not support syncing its project.
    pub async fn sync_project(&self) -> Result<(), SessionError> {
        self.backend.sync_project().await
    }

    /// Creates one session and returns its stable identifier.
    ///
    /// # Errors
    /// Returns an error when the host cannot create the requested session.
    pub async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.backend.create_session(request).await
    }

    /// Loads one complete session aggregate by identifier.
    ///
    /// # Errors
    /// Returns an error when persisted data cannot be loaded or decoded.
    pub async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Session>, SessionError> {
        self.backend.get_session(session_id).await
    }

    /// Loads the files one session changes against its base branch, or
    /// `None` when the host has not loaded the session.
    ///
    /// # Errors
    /// Returns an error when the host does not serve diffs or cannot compute
    /// or load this one.
    pub async fn get_session_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionDiffFile>>, SessionError> {
        self.backend.get_session_diff(session_id).await
    }

    /// Lists the files and directories in one session's worktree,
    /// directories first, or `None` when the host has not loaded the
    /// session.
    ///
    /// # Errors
    /// Returns an error when the host does not serve file listings or cannot
    /// read this one.
    pub async fn list_session_files(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionFileEntry>>, SessionError> {
        self.backend.list_session_files(session_id).await
    }

    /// Lists the files and directories in the active project's checkout,
    /// directories first.
    ///
    /// # Errors
    /// Returns an error when the host does not serve project file listings
    /// or cannot read the checkout.
    pub async fn list_project_files(&self) -> Result<Vec<SessionFileEntry>, SessionError> {
        self.backend.list_project_files().await
    }

    /// Lists the host's loaded sessions in its display order.
    ///
    /// # Errors
    /// Returns an error when the host cannot list sessions.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        self.backend.list_sessions().await
    }

    /// Lists the host's loaded sessions in its display order, with list
    /// stats when the host provides them.
    ///
    /// # Errors
    /// Returns an error when the host cannot list sessions.
    pub async fn list_session_entries(&self) -> Result<Vec<SessionListEntry>, SessionError> {
        self.backend.list_session_entries().await
    }

    /// Sends a text message to one session.
    ///
    /// # Errors
    /// Returns an error when the session cannot accept or enqueue the message.
    pub async fn send_message(
        &self,
        session_id: &SessionId,
        message: impl Into<String> + Send,
    ) -> Result<(), SessionError> {
        self.backend.send_message(session_id, message.into()).await
    }

    /// Submits one durable turn directly to the serialized worker.
    ///
    /// # Errors
    /// Returns an error when the session is busy or cannot accept the turn.
    pub async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        self.backend
            .submit_durable_message(session_id, request)
            .await
    }

    /// Answers the complete current clarification-question set.
    ///
    /// # Errors
    /// Returns an error when the answers are stale, incomplete, or cannot be
    /// enqueued as a follow-up turn.
    pub async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        self.backend.answer_questions(session_id, request).await
    }

    /// Cancels one session through its host lifecycle workflow.
    ///
    /// # Errors
    /// Returns an error when the session does not exist or cannot be canceled
    /// in its current state.
    pub async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.cancel_session(session_id).await
    }

    /// Stops the session's running turn and returns the session to review.
    /// Unlike [`Self::cancel_session`], the worktree and its changes stay;
    /// messages queued behind the turn are discarded.
    ///
    /// # Errors
    /// Returns an error when the session does not exist, has no running
    /// turn, or the host does not support stopping a turn.
    pub async fn stop_turn(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.stop_turn(session_id).await
    }

    /// Starts or queues a rebase of the session branch onto its base branch.
    /// The host acknowledges admission; a conflict starts an agent-assisted
    /// resolution turn.
    ///
    /// # Errors
    /// Returns an error when the session does not exist, cannot be rebased in
    /// its current state, or the host does not support syncing.
    pub async fn rebase_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.rebase_session(session_id).await
    }

    /// Changes the session's model, reasoning, style, or speed for later
    /// turns, where the terminal's composer could change them.
    ///
    /// The host refuses the whole change, applying none of it, when the
    /// session's status has no composer, the model is not one the session's
    /// agent offers, or fast mode does not suit the model the session will
    /// have. It then applies the model, reasoning, style, and speed in that
    /// order. Reasoning and style apply at once; a model or speed change
    /// waits while the session still has accepted work. Changes are not
    /// transactional: when one fails to apply, the ones before it stay
    /// applied.
    ///
    /// # Errors
    /// Returns an error when the session does not exist or its status has no
    /// composer, a model is not available to it, fast mode does not suit the
    /// resulting model, switching or persisting a setting fails, or the host
    /// does not support the change.
    pub async fn update_session_settings(
        &self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), SessionError> {
        self.backend
            .update_session_settings(session_id, change)
            .await
    }

    /// Requests merge processing for one session.
    ///
    /// # Errors
    /// Returns an error when the session is not mergeable or queueing fails.
    pub async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.merge_session(session_id).await
    }

    /// Queues publication of one session and creates or refreshes its review
    /// request.
    ///
    /// # Errors
    /// Returns an error when queueing, branch publication, forge access, or
    /// persistence fails.
    pub async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.backend.create_review_request(session_id).await
    }
}

#[cfg(test)]
#[path = "service_test.rs"]
mod tests;
