use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;

use ag_contracts::{
    ExecutionPolicy, MockAgentChannel, PermissionMode, ReasoningLevel, SessionStats, SpeedMode,
    TurnRequest, TurnResult,
};
use ag_git::MockGitClient;
use ag_protocol::{
    AgentResponse, ProtocolRequestProfile, QuestionItem, TurnPrompt, TurnPromptAttachment,
};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph,
    GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionProfile, GraphInputBinding,
    GraphNode, GraphOutput, GraphPostProcessing, GraphResultState, GraphWorkspaceAccess, SessionId,
    StartExecutionGraphRequest,
};
use ag_worker::SessionRunClient;
use tokio::sync::{Notify, mpsc, watch};

use crate::app::App;
use crate::app::service::MockSessionRunFactory;
use crate::app::session::SessionManager;
use crate::domain::session::{SessionHandles, Status};
use crate::infra::db::{AppRepositories, StoredGraphResult};
use crate::infra::fs::MockFsClient;
use crate::test_support::{
    SessionFixtureBuilder, new_test_app_with_clients, session_manager_with_handles,
    test_app_clients,
};

#[path = "execution_graph_driver_test.rs"]
mod driver;

#[path = "execution_graph_boundary_test.rs"]
mod boundary;

#[tokio::test]
async fn background_utilities_preserve_host_overlap_and_exclude_graph_writers() {
    for permission in [PermissionMode::ReadOnly, PermissionMode::AutoEdit] {
        // Arrange
        let (app, _directory) = crate::test_support::new_test_app().await;
        let environment = app
            .services
            .graph_resources
            .environment(&EnvId::SessionWorktree("sess".into()));
        let held = environment.clone().write_owned().await;
        let background_readers = app
            .services
            .graph_resources
            .background_readers(&EnvId::SessionWorktree("sess".into()));
        let background_writer = background_readers.clone().write_owned().await;
        assert!(app.services.try_environment_write("sess").is_err());
        let mut run_client = ag_worker::MockRunClient::new();
        let checked_environment = environment.clone();
        let checked_background = background_readers.clone();
        run_client.expect_submit().once().returning(move |_| {
            if permission.is_read_only() {
                assert!(checked_background.clone().try_write_owned().is_err());
                assert!(checked_environment.clone().try_write_owned().is_ok());
            } else {
                assert!(checked_environment.clone().try_read_owned().is_err());
            }

            Ok(ag_contracts::OneShotSubmission {
                response: AgentResponse::plain("result"),
                stats: SessionStats::default(),
            })
        });
        let client = app
            .services
            .background_run_client("sess", Arc::new(run_client));
        let mut wake = app.services.graph_wake.subscribe();
        let request = ag_contracts::OneShotRequest {
            activity_tx: None,
            child_pid: None,
            execution_policy: ExecutionPolicy::default(),
            folder: "workspace".into(),
            harness: "codex".into(),
            model: "gpt-6.1-sol".into(),
            permission_mode: permission,
            prompt: "detached work".into(),
            provider_call_budget: None,
            reasoning_level: ReasoningLevel::default(),
            request_kind: ag_contracts::AgentRequestKind::UtilityPrompt,
            speed_mode: SpeedMode::default(),
        };
        let work = client.submit(request);
        tokio::pin!(work);

        // Act / Assert: graph writers exclude every detached utility, while
        // background readers allow ordinary session mutations such as sync.
        let waiting = tokio::select! {
            biased;
            _ = &mut work => false,
            () = std::future::ready(()) => true,
        };
        assert!(waiting, "utility must wait for Env release");
        drop(held);
        drop(background_writer);
        work.await.expect("utility finishes after release");
        assert!(wake.has_changed().expect("wake channel"));
        wake.borrow_and_update();
        let merge_lease = app
            .services
            .try_environment_write("sess")
            .expect("merge lease");
        assert!(environment.clone().try_read_owned().is_err());
        drop(merge_lease);
        assert!(wake.has_changed().expect("merge release wakes dispatch"));
        assert!(environment.try_write_owned().is_ok());
        assert!(background_readers.try_write_owned().is_ok());
    }
}

#[tokio::test]
async fn graph_launch_rejects_missing_budgets_and_unsupported_profiles_before_persistence() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let service = app.session_service();
    for scenario in [
        "budget",
        "fresh-attachments",
        "fresh-writer",
        "invalid",
        "missing-session",
        "ineligible",
    ] {
        let mut graph = graph();
        let mut limits = BTreeMap::from([("calls".into(), 1)]);
        match scenario {
            "budget" => limits.clear(),
            "fresh-attachments" | "fresh-writer" => {
                graph.nodes.truncate(1);
                graph.nodes[0].execution.context = GraphContext::Fresh;
                graph.nodes[0].execution.response_profile = ProtocolRequestProfile::UtilityPrompt;
                graph.nodes[0].outputs[0].schema = ProtocolRequestProfile::UtilityPrompt;
                if scenario == "fresh-writer" {
                    graph.nodes[0].request.attachments.clear();
                    graph.nodes[0].definition.read_only = false;
                    graph.nodes[0].execution.workspace_access =
                        GraphWorkspaceAccess::ExclusiveWrite;
                }
            }
            "invalid" => graph.nodes.clear(),
            "missing-session" => {
                graph.session_id = "missing".into();
                for node in &mut graph.nodes {
                    node.env_id = EnvId::SessionWorktree("missing".into());
                }
            }
            _ => {
                *app.sessions.session_handles()["sess"]
                    .status
                    .lock()
                    .expect("status") = Status::Done;
            }
        }

        // Act
        let result = app
            .drive_session_request(service.start_execution_graph(StartExecutionGraphRequest {
                graph,
                provider_call_limits: limits,
            }))
            .await;

        // Assert
        assert!(result.is_err(), "{scenario}");
        assert!(
            app.services
                .db()
                .graphs()
                .load_graph("graph", None)
                .await
                .expect("snapshot")
                .is_none()
        );
    }
}

#[tokio::test]
async fn graph_handoffs_reject_stale_or_conflicting_claims_before_worker_delivery() {
    for scenario in [
        "utility",
        "env",
        "node",
        "receipt",
        "running",
        "missing-graph",
        "missing-session",
        "status",
        "stack",
    ] {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        let mut request = claimed_handoff_request(&mut app, scenario == "stack").await;
        match scenario {
            "utility" => {
                request.dispatch.record.execution = ag_session::GraphExecutionRef::UtilityRun {
                    run_id: "utility".into(),
                }
            }
            "env" => request.folder = "other-workspace".into(),
            "node" => request.dispatch.node.objective.push_str("changed"),
            "receipt" => request.dispatch.record.node_id = "missing".into(),
            "running" => {
                let operation_id = task_operation_id(&request.dispatch.record.execution);
                app.services
                    .db()
                    .operations()
                    .mark_session_operation_running(operation_id)
                    .await
                    .expect("running");
            }
            "missing-graph" => request.dispatch.record.graph_id = "missing".into(),
            "missing-session" => {
                *app.sessions = session_manager_with_handles(Vec::new(), HashMap::new());
            }
            "status" => {
                *app.sessions.session_handles()["sess"]
                    .status
                    .lock()
                    .expect("status") = Status::Done;
            }
            _ => {}
        }
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        // Act
        app.sessions
            .enqueue_claimed_graph_task(
                &app.services,
                request,
                tokio_util::sync::CancellationToken::new(),
                response_tx,
            )
            .await;

        // Assert
        assert!(
            response_rx.await.expect("completion").is_err(),
            "{scenario}"
        );
    }
}

#[tokio::test]
async fn graph_writers_refresh_active_siblings_before_stack_admission() {
    for live_status in [Status::InProgress, Status::Rebasing] {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        let request = claimed_handoff_request(&mut app, true).await;
        app.sessions.sessions_mut()[1].status = Status::Review;
        *app.sessions.session_handles()["child"]
            .status
            .lock()
            .expect("status") = live_status;
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        // Act
        app.sessions
            .enqueue_claimed_graph_task(
                &app.services,
                request,
                tokio_util::sync::CancellationToken::new(),
                response_tx,
            )
            .await;

        // Assert
        assert!(
            response_rx
                .await
                .expect("completion")
                .expect_err("active sibling")
                .to_string()
                .contains("Graph writer requires an idle session stack")
        );
        assert_eq!(app.sessions.sessions()[1].status, live_status);
    }
}

#[tokio::test]
async fn task_graph_preserves_context_and_waits_for_acceptance_and_ordered_effects() {
    // Arrange
    let GraphScenario {
        mut app,
        directory: _directory,
        effects_entered,
        effects_release,
        mut observed_rx,
    } = GraphScenario::new().await;
    let db = app.services.db().clone();
    let _consumer = app.sessions.foreground_consumer();
    let service = app.session_service();
    let request = StartExecutionGraphRequest {
        graph: graph(),
        provider_call_limits: BTreeMap::from([("calls".into(), 2)]),
    };

    // Act: launch the reader; the acceptance gate keeps the writer unclaimed.
    app.drive_session_request(service.start_execution_graph(request.clone()))
        .await
        .expect("launch");
    let first = app
        .drive_session_request(async { observed_rx.recv().await.expect("first turn") })
        .await;
    let result = app
        .drive_session_request(wait_result(
            db.clone(),
            app.services.graph_wake.subscribe(),
            "first",
        ))
        .await;
    assert_reader_result(&mut app, &first, &request, &result).await;

    // Act: explicit acceptance wakes the writer, whose effects still own
    // settlement.
    app.drive_session_request(service.accept_execution_graph_result(result.result.reference))
        .await
        .expect("accept");
    let second = app
        .drive_session_request(async { observed_rx.recv().await.expect("second turn") })
        .await;
    effects_entered.notified().await;
    let task = db
        .tasks()
        .load_task("task-second")
        .await
        .expect("task")
        .expect("present");

    // Assert
    assert_eq!(
        task.attempts[0].status,
        ag_session::TaskAttemptStatus::Running
    );
    assert_eq!(second.permission_mode, PermissionMode::AutoEdit);
    assert!(second.prompt.agent_text().contains("result @decorator"));
    assert_eq!(
        db.graphs()
            .list_results("graph", "second", "response")
            .await
            .expect("results")
            .len(),
        0
    );

    // Act: finish effects and inspect the authoritative result and ordinary
    // chat.
    effects_release.notify_one();
    let second_result = app
        .drive_session_request(wait_result(
            db.clone(),
            app.services.graph_wake.subscribe(),
            "second",
        ))
        .await;
    assert_eq!(second_result.result.state, GraphResultState::Validated);
    assert_ordinary_context(&db).await;
    app.services.wait_for_cleanup_tasks(None).await;
}

async fn assert_ordinary_context(db: &AppRepositories) {
    let ordinary = db
        .sessions()
        .load_session("sess")
        .await
        .expect("session")
        .expect("present");

    // Assert
    assert_eq!(
        db.sessions()
            .get_session_provider_conversation_id("sess")
            .await
            .expect("ordinary provider")
            .as_deref(),
        Some("ordinary-provider")
    );
    assert_eq!(
        db.sessions()
            .get_session_instruction_conversation_id("sess")
            .await
            .expect("ordinary instructions")
            .as_deref(),
        Some("ordinary-instructions")
    );
    assert!(
        ordinary
            .questions
            .as_deref()
            .expect("ordinary questions")
            .contains("ordinary question")
    );
    assert_eq!(
        db.sessions()
            .load_session_messages("sess")
            .await
            .expect("messages")
            .len(),
        0
    );
    assert!(
        db.tasks()
            .load_task("task-second")
            .await
            .expect("task")
            .expect("present")
            .completed_at
            .is_none()
    );
}

struct GraphScenario {
    app: App,
    directory: tempfile::TempDir,
    effects_entered: Arc<Notify>,
    effects_release: Arc<Notify>,
    observed_rx: mpsc::UnboundedReceiver<TurnRequest>,
}

impl GraphScenario {
    async fn new() -> Self {
        let (observed_tx, observed_rx) = mpsc::unbounded_channel::<TurnRequest>();
        let effects_entered = Arc::new(Notify::new());
        let effects_release = Arc::new(Notify::new());
        let mut factory = MockSessionRunFactory::new();
        factory.expect_create().once().returning(|id, _| {
            let mut channel = MockAgentChannel::new();
            channel
                .expect_shutdown_session()
                .returning(|_| Box::pin(async { Ok(()) }));
            SessionRunClient::from_channel(id.to_string(), Arc::new(channel))
        });
        factory.expect_create_graph_task().times(2).returning(
            move |context_id, session_id, kind| {
                assert!(uuid::Uuid::parse_str(context_id).is_ok());
                assert_eq!(session_id.as_str(), "sess");
                assert_eq!(kind, AgentKind::Codex);
                let mut channel = MockAgentChannel::new();
                let observed = observed_tx.clone();
                channel
                    .expect_run_turn()
                    .once()
                    .returning(move |_, request, _| {
                        request
                            .provider_call_budget
                            .as_ref()
                            .expect("graph budget")
                            .consume()
                            .expect("reserved provider call");
                        observed.send(request).expect("observe task");
                        Box::pin(async {
                            Ok(TurnResult {
                                assistant_message: AgentResponse::plain("result @decorator"),
                                context_reset: false,
                                harness_report: None,
                                input_tokens: 3,
                                output_tokens: 5,
                                provider_conversation_id: Some("isolated-provider".into()),
                            })
                        })
                    });
                channel
                    .expect_shutdown_session()
                    .once()
                    .returning(|_| Box::pin(async { Ok(()) }));
                Ok(SessionRunClient::from_channel(
                    context_id.into(),
                    Arc::new(channel),
                ))
            },
        );
        let clients = test_app_clients().with_session_run_factory(Arc::new(factory));
        let (mut app, directory) = new_test_app_with_clients(clients).await;
        install_graph_environment(&mut app, &effects_entered, &effects_release);
        seed_session(&mut app.sessions, app.services.db()).await;
        *app.sessions.session_handles()["sess"]
            .status
            .lock()
            .expect("status") = Status::Draft;
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at("sess", "Draft", 0)
            .await
            .expect("prepared draft");

        Self {
            app,
            directory,
            effects_entered,
            effects_release,
            observed_rx,
        }
    }
}

fn install_graph_environment(
    app: &mut App,
    effects_entered: &Arc<Notify>,
    effects_release: &Arc<Notify>,
) {
    let mut fs = MockFsClient::new();
    fs.expect_is_dir().returning(|_| true);
    app.services.set_fs_client(Arc::new(fs));
    let mut git = MockGitClient::new();
    git.expect_detect_git_info()
        .returning(|_| Box::pin(async { Some("wt/sess".into()) }));
    git.expect_main_checkout_working_tree()
        .returning(|_| Box::pin(async { Ok(None) }));
    git.expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    let entered = effects_entered.clone();
    let release = effects_release.clone();
    git.expect_is_worktree_clean().once().returning(move |_| {
        let entered = entered.clone();
        let release = release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(true)
        })
    });
    app.services.set_git_client(Arc::new(git));
}

async fn assert_reader_result(
    app: &mut App,
    first: &TurnRequest,
    request: &StartExecutionGraphRequest,
    result: &StoredGraphResult,
) {
    let db = app.services.db().clone();
    let graph = &request.graph;
    let service = app.session_service();
    let pending = db
        .graphs()
        .list_executions("graph", graph.revision)
        .await
        .expect("executions");

    // Assert
    assert_eq!(first.model, "gpt-6.1-sol");
    assert_eq!(first.permission_mode, PermissionMode::ReadOnly);
    assert_eq!(first.agent_profile.current(), Some("Captured instructions"));
    assert_eq!(first.continuation.provider_conversation_id(), None);
    assert_eq!(first.continuation.replay_transcript(), None);
    assert_eq!(first.prompt.attachments, graph.nodes[0].request.attachments);
    assert!(first.prompt.text.contains("\"src/main.rs\""));
    assert_eq!(pending.len(), 1);
    assert_eq!(result.result.state, GraphResultState::Validated);
    let history = db
        .graphs()
        .load_task_turn(&pending[0].execution)
        .await
        .expect("history")
        .expect("observed response");
    assert_eq!(
        history.provider_conversation_id.as_deref(),
        Some("isolated-provider")
    );
    app.drive_session_request(service.start_execution_graph(request.clone()))
        .await
        .expect("idempotent launch");
    let mut conflicting = request.clone();
    conflicting.provider_call_limits.insert("calls".into(), 3);
    assert!(
        app.drive_session_request(service.start_execution_graph(conflicting))
            .await
            .is_err()
    );

    let mut stale = result.result.reference.clone();
    stale.graph_revision = NonZeroU64::new(2).expect("revision");
    assert!(
        app.drive_session_request(service.accept_execution_graph_result(stale))
            .await
            .is_err()
    );
    let mut missing = result.result.reference.clone();
    missing.graph_id = "missing".into();
    assert!(
        app.drive_session_request(service.accept_execution_graph_result(missing))
            .await
            .is_err()
    );
}

async fn claimed_handoff_request(app: &mut App, writer: bool) -> ag_worker::GraphRunRequest {
    let mut graph = graph();
    graph.nodes.truncate(1);
    if writer {
        graph.nodes[0].definition.read_only = false;
        graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
        let mut sessions = vec![
            SessionFixtureBuilder::new()
                .id("sess")
                .folder(PathBuf::from("workspace"))
                .build(),
        ];
        sessions.push(
            SessionFixtureBuilder::new()
                .id("child")
                .parent_session_id(Some("sess".into()))
                .status(Status::InProgress)
                .build(),
        );
        *app.sessions = session_manager_with_handles(
            sessions,
            HashMap::from([
                ("sess".into(), SessionHandles::new(Status::Review)),
                ("child".into(), SessionHandles::new(Status::InProgress)),
            ]),
        );
    }
    app.services
        .db()
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("save");
    let dispatch = claimed_dispatch(
        app.services
            .db()
            .graph_dispatch()
            .claim(
                "graph",
                graph.revision,
                "first",
                "aaaaaaaa-0000-0000-0000-000000000001",
                std::path::Path::new("workspace"),
            )
            .await
            .expect("claim"),
    );
    ag_worker::GraphRunRequest {
        dispatch,
        folder: "workspace".into(),
        permission_mode: PermissionMode::ReadOnly,
        provider_call_budget: None,
    }
}

pub(crate) fn claimed_dispatch(admission: ag_worker::GraphAdmission) -> ag_worker::GraphDispatch {
    match admission {
        ag_worker::GraphAdmission::Claimed(dispatch) => Some(*dispatch),
        ag_worker::GraphAdmission::Blocked | ag_worker::GraphAdmission::Existing(_) => None,
    }
    .expect("new graph dispatch")
}

pub(crate) fn task_operation_id(execution: &ag_session::GraphExecutionRef) -> &str {
    match execution {
        ag_session::GraphExecutionRef::TaskAttempt { operation_id, .. } => {
            Some(operation_id.as_str())
        }
        ag_session::GraphExecutionRef::UtilityRun { .. } => None,
    }
    .expect("task execution")
}

async fn seed_session(manager: &mut SessionManager, db: &AppRepositories) {
    let project = db
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    db.sessions()
        .insert_session("sess", "gpt-6.1-sol", "main", "Review", project)
        .await
        .expect("session");
    db.sessions()
        .update_session_provider_conversation_id("sess", Some("ordinary-provider".into()))
        .await
        .expect("provider");
    db.sessions()
        .update_session_instruction_conversation_id("sess", Some("ordinary-instructions".into()))
        .await
        .expect("instructions");
    db.sessions()
        .update_session_questions(
            "sess",
            &serde_json::json!([QuestionItem::new("ordinary question")]).to_string(),
        )
        .await
        .expect("questions");
    *manager = session_manager_with_handles(
        vec![
            SessionFixtureBuilder::new()
                .id("sess")
                .folder(PathBuf::from("workspace"))
                .build(),
        ],
        HashMap::from([("sess".into(), SessionHandles::new(Status::Review))]),
    );
}

async fn wait_result(
    db: AppRepositories,
    mut wake: watch::Receiver<u64>,
    node: &str,
) -> StoredGraphResult {
    loop {
        wake.borrow_and_update();
        if let Some(result) = db
            .graphs()
            .list_results("graph", node, "response")
            .await
            .expect("results")
            .into_iter()
            .next()
        {
            return result;
        }
        wake.changed().await.expect("graph driver wakeup");
    }
}

pub(crate) fn graph() -> ExecutionGraph {
    let first = node("first");
    let mut second = node("second");
    second.definition.read_only = false;
    second.execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
    second.execution.post_processing = GraphPostProcessing::SessionWorkflow;
    second.dependencies.push(GraphDependency {
        node_id: "first".into(),
        gate: GraphCompletionGate::ResultAccepted {
            output_name: "response".into(),
        },
    });
    second.inputs.push(GraphInputBinding {
        name: "accepted-input".into(),
        output_name: "response".into(),
        producer_node_id: "first".into(),
    });

    ExecutionGraph {
        id: "graph".into(),
        nodes: vec![first, second],
        revision: NonZeroU64::MIN,
        session_id: SessionId::from("sess"),
    }
}

pub(crate) fn run_request() -> ag_worker::GraphRunRequest {
    ag_worker::GraphRunRequest {
        dispatch: ag_worker::GraphDispatch {
            inputs: Vec::new(),
            node: graph().nodes.remove(0),
            record: ag_worker::GraphExecutionRecord {
                execution: ag_session::GraphExecutionRef::TaskAttempt {
                    attempt_id: "attempt".into(),
                    operation_id: "operation".into(),
                    task_id: "task-first".into(),
                },
                graph_id: "graph".into(),
                graph_revision: NonZeroU64::MIN,
                inputs: Vec::new(),
                node_id: "first".into(),
            },
        },
        folder: "workspace".into(),
        permission_mode: PermissionMode::ReadOnly,
        provider_call_budget: None,
    }
}

fn node(id: &str) -> GraphNode {
    GraphNode {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        definition: AgentDefinition {
            description: "Test agent".into(),
            id: "custom-agent".into(),
            name: "Custom agent".into(),
            prompt: "Captured instructions".into(),
            read_only: true,
        },
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree("sess".into()),
        execution: GraphExecutionProfile {
            context: GraphContext::TaskConversation {
                task_id: format!("task-{id}"),
            },
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::SessionTurn,
            shared_budget_id: Some("calls".into()),
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: "Inspect the requested code".into(),
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: ProtocolRequestProfile::SessionTurn,
        }],
        request: TurnPrompt {
            text: "Inspect @src/main.rs [Image #1]".into(),
            attachments: vec![TurnPromptAttachment {
                local_image_path: "image.png".into(),
                placeholder: "[Image #1]".into(),
            }],
            ..TurnPrompt::from_text(String::new())
        },
    }
}
