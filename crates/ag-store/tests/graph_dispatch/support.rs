use std::error::Error;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, ExecutionPolicy, OneShotClient, OneShotError, OneShotRequest,
    OneShotSubmission, PermissionMode, ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::{AgentResponse, ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph,
    GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionProfile, GraphExecutionRef,
    GraphInputBinding, GraphNode, GraphOutput, GraphPostProcessing, GraphWorkspaceAccess,
};
use ag_store::Database;
use ag_worker::{
    GraphAdmission, GraphDispatch, GraphDispatchRepository, GraphExecutionRecord, GraphNodeRunner,
    GraphRunRequest, RunClient, RunScope, RunState, RunWorker, in_scope,
};
use async_trait::async_trait;
use tokio::sync::{Barrier, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(super) struct ScannedAdmission {
    pub(super) repository: Arc<dyn GraphDispatchRepository>,
    pub(super) scanned: Notify,
}

#[async_trait]
impl GraphDispatchRepository for ScannedAdmission {
    async fn graph(&self, id: &str, revision: NonZeroU64) -> Result<ExecutionGraph, OneShotError> {
        self.repository.graph(id, revision).await
    }

    async fn claim(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        execution_id: &str,
        folder: &Path,
    ) -> Result<GraphAdmission, OneShotError> {
        if node_id == "probe" {
            self.scanned.notify_one();

            return Ok(GraphAdmission::Blocked);
        }

        self.repository
            .claim(graph_id, revision, node_id, execution_id, folder)
            .await
    }

    async fn finish(&self, dispatch: &GraphDispatch, body: &str) -> Result<(), OneShotError> {
        self.repository.finish(dispatch, body).await
    }

    async fn fail(&self, record: &GraphExecutionRecord, error: &str) -> Result<(), OneShotError> {
        self.repository.fail(record, error).await
    }
}

#[derive(Default)]
pub(super) struct BusyProvider {
    pub(super) provider: Provider,
    pub(super) release: Notify,
    entered: Notify,
}

impl BusyProvider {
    pub(super) async fn hold_capacity(
        &self,
        worker: Arc<RunWorker>,
    ) -> JoinHandle<Result<OneShotSubmission, OneShotError>> {
        let holder = tokio::spawn(in_scope(
            RunScope {
                session_id: Some("other-session".into()),
                ..RunScope::default()
            },
            async move {
                worker
                    .submit(OneShotRequest {
                        activity_tx: None,
                        child_pid: None,
                        execution_policy: ExecutionPolicy::default(),
                        folder: "other-workspace".into(),
                        harness: "codex".into(),
                        model: "gpt-6.1-sol".into(),
                        permission_mode: PermissionMode::ReadOnly,
                        prompt: "hold capacity".into(),
                        provider_call_budget: None,
                        reasoning_level: ReasoningLevel::default(),
                        request_kind: AgentRequestKind::UtilityPrompt,
                        speed_mode: SpeedMode::default(),
                    })
                    .await
            },
        ));
        self.entered.notified().await;

        holder
    }
}

#[async_trait]
impl OneShotClient for BusyProvider {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        if request.prompt == "hold capacity" {
            self.entered.notify_one();
            self.release.notified().await;

            return Ok(OneShotSubmission {
                response: AgentResponse::plain("released"),
                stats: SessionStats::default(),
            });
        }

        self.provider.submit(request).await
    }
}

pub(super) struct PausedAdmission {
    pub(super) committed: Notify,
    pub(super) release: Notify,
    repository: Arc<dyn GraphDispatchRepository>,
}

impl PausedAdmission {
    pub(super) fn new(repository: Arc<dyn GraphDispatchRepository>) -> Self {
        Self {
            committed: Notify::new(),
            release: Notify::new(),
            repository,
        }
    }
}

#[async_trait]
impl GraphDispatchRepository for PausedAdmission {
    async fn graph(&self, id: &str, revision: NonZeroU64) -> Result<ExecutionGraph, OneShotError> {
        self.repository.graph(id, revision).await
    }

    async fn claim(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        execution_id: &str,
        folder: &Path,
    ) -> Result<GraphAdmission, OneShotError> {
        let admission = self
            .repository
            .claim(graph_id, revision, node_id, execution_id, folder)
            .await?;
        if matches!(admission, GraphAdmission::Claimed(_)) {
            self.committed.notify_one();
            self.release.notified().await;
        }

        Ok(admission)
    }

    async fn finish(&self, dispatch: &GraphDispatch, body: &str) -> Result<(), OneShotError> {
        self.repository.finish(dispatch, body).await
    }

    async fn fail(&self, record: &GraphExecutionRecord, error: &str) -> Result<(), OneShotError> {
        self.repository.fail(record, error).await
    }
}

pub(super) struct PanickingRunner {
    pub(super) database: Database,
    pub(super) entered: Notify,
    pub(super) release: Notify,
    pub(super) running: bool,
}

#[async_trait]
impl GraphNodeRunner for PanickingRunner {
    fn supports(&self, _node: &GraphNode) -> bool {
        true
    }

    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError> {
        if self.running {
            match &request.dispatch.record.execution {
                GraphExecutionRef::UtilityRun { run_id } => {
                    self.database
                        .runs()
                        .transition(run_id, RunState::Running, None)
                        .await?;
                }
                GraphExecutionRef::TaskAttempt { operation_id, .. } => {
                    self.database
                        .operations()
                        .mark_session_operation_running(operation_id)
                        .await
                        .map_err(|error| OneShotError::new(error.to_string()))?;
                }
            }
        }
        self.entered.notify_one();
        self.release.notified().await;

        std::panic::resume_unwind(Box::new("graph runner panicked"));
    }
}

#[derive(Default)]
pub(super) struct Provider {
    pub(super) requests: Mutex<Vec<OneShotRequest>>,
    readers: Option<Barrier>,
    fail_second: bool,
}

impl Provider {
    pub(super) fn parallel_readers() -> Self {
        Self {
            readers: Some(Barrier::new(2)),
            ..Self::default()
        }
    }

    pub(super) fn failing_readers() -> Self {
        Self {
            fail_second: true,
            ..Self::parallel_readers()
        }
    }
}

#[async_trait]
impl OneShotClient for Provider {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let branch = !request.prompt.contains("Bound input");
        let fail = self.fail_second && request.prompt.ends_with("second");
        if let Some(budget) = &request.provider_call_budget {
            budget.consume()?;
        }
        let response = match request.request_kind {
            AgentRequestKind::FocusedReview => {
                AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#)
            }
            AgentRequestKind::ReviewMetadata => AgentResponse::plain(
                r#"{"title":"Review","description":"Evidence","is_title_change_significant":false}"#,
            ),
            _ => AgentResponse::plain("evidence"),
        };
        self.requests
            .lock()
            .map_err(|error| OneShotError::new(error.to_string()))?
            .push(request);
        if branch && let Some(barrier) = &self.readers {
            barrier.wait().await;
        }
        if fail {
            return Err(OneShotError::new("scripted branch failure"));
        }

        Ok(OneShotSubmission {
            response,
            stats: SessionStats::default(),
        })
    }
}

#[derive(Default)]
pub(super) struct CleanupProvider {
    pub(super) entered: Notify,
    pub(super) canceled: Notify,
    pub(super) release: Notify,
}

#[async_trait]
impl OneShotClient for CleanupProvider {
    async fn submit(&self, _request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        Err(OneShotError::new("requires cancellation-aware submission"))
    }

    async fn submit_cancellable(
        &self,
        _request: OneShotRequest,
        cancellation: CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        self.entered.notify_one();
        cancellation.cancelled().await;
        self.canceled.notify_one();
        self.release.notified().await;

        Err(OneShotError::new("canceled after cleanup"))
    }
}

pub(super) async fn assert_panicked_execution_failed(
    database: &Database,
) -> Result<(), Box<dyn Error>> {
    let (status, error, finished): (String, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT status, last_error, finished_at FROM agent_run UNION ALL SELECT status, \
         last_error, finished_at FROM session_operation",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(status, "failed");
    assert_eq!(error.as_deref(), Some("Graph node runner panicked"));
    assert!(finished.is_some());

    Ok(())
}

pub(super) async fn populate(database: &Database) -> Result<(), Box<dyn Error>> {
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;

    Ok(())
}

pub(super) fn graph() -> ExecutionGraph {
    let first = reader("first");
    let second = reader("second");
    let mut join = reader("join");
    for producer in [&first, &second] {
        join.dependencies.push(GraphDependency {
            node_id: producer.id.clone(),
            gate: GraphCompletionGate::ResultValidated {
                output_name: "response".into(),
            },
        });
        join.inputs.push(GraphInputBinding {
            name: producer.id.clone(),
            output_name: "response".into(),
            producer_node_id: producer.id.clone(),
        });
    }

    ExecutionGraph {
        id: "graph".into(),
        nodes: vec![first, second, join],
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
    }
}

fn reader(id: &str) -> GraphNode {
    GraphNode {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        definition: AgentDefinition {
            description: "Read evidence".into(),
            id: "reader".into(),
            name: "Reader".into(),
            prompt: "Inspect evidence".into(),
            read_only: true,
        },
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree("session".into()),
        execution: GraphExecutionProfile {
            context: GraphContext::Fresh,
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::UtilityPrompt,
            shared_budget_id: Some("budget".into()),
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: format!("Inspect {id}"),
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: ProtocolRequestProfile::UtilityPrompt,
        }],
        request: TurnPrompt::from_agent_data(id.into()),
    }
}
