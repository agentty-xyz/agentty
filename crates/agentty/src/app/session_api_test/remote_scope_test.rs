//! Host-enforced remote-control scope: a paired device reaches only the
//! sessions the host lists and only the operations remote control serves,
//! checked in the foreground turn that serves each request.

use std::fs;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ag_git::MockGitClient;
use ag_session::test_support::graph_proposal_state;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, CreateTaskRequest,
    DurableMessageRequest, DurableMessageVisibility, GenerateGraphProposalRequest,
    GraphProposalState, MessageImage, ProjectScope, SessionError as ApiSessionError,
    SessionGraphAction, SessionGraphActionRequest, SessionGraphsRequest, SessionId, SessionService,
    SessionSettingsChange, StartExecutionGraphRequest, test_support as model_fixture,
};
use ag_store::MockGraphRepository;
use tempfile::TempDir;

use crate::app::App;
use crate::app::execution_graph::tests::graph;
use crate::app::test_support::{add_listed_project, insert_loadable_session};
use crate::domain::session::Status;
use crate::test_support::{new_test_app, session_fixture};

/// A boxed request whose result says whether the host returned something.
type Request = Pin<Box<dyn Future<Output = Result<bool, ApiSessionError>> + Send>>;

/// Builds one [`Request`] from its own service and session identity.
type RequestCall = fn(SessionService, SessionId) -> Request;

/// Requests whose own handler answers a session the host has not loaded as
/// missing before it reads or changes anything else. A paired device is
/// listed exactly the active page's sessions, so no session off every page
/// can tell the scope check apart from these handlers; a session on a viewed
/// page gets past them.
const ANSWERED_AS_MISSING_BY_THEIR_HANDLER: [&str; 10] = [
    "read diff",
    "read files",
    "send message",
    "send message with images",
    "cancel",
    "stop turn",
    "rebase",
    "settings",
    "merge",
    "publish",
];

/// Delivery identity of the graph proposal request each table sends.
const PROPOSAL_ID: &str = "remote-proposal";

/// Task of the graph proposal request each table sends.
const PROPOSAL_TASK: &str = "Plan it";

/// Where the stored session `hidden` sits so that the host does not list it
/// to a paired device: in another project, on no page, or on the page of a
/// listed project that a session-API caller views.
const UNLISTED_PLACEMENTS: [&str; 3] = ["other project", "not loaded", "viewed page"];

#[tokio::test]
async fn remote_requests_for_an_unlisted_session_answer_as_missing_and_change_nothing() {
    for placement in UNLISTED_PLACEMENTS {
        // Arrange
        let (mut app, _directory) = unlisted_session_app(placement).await;
        // Any graph read would fail the test: the mock expects none.
        app.services
            .set_graph_repository(Arc::new(MockGraphRepository::new()));
        let remote = app.remote_session_service();
        let loaded_sessions = app.sessions.sessions().len();

        for (operation, request) in session_requests(&remote, &app, "hidden") {
            // Act
            let result = app.drive_session_request(request).await;

            // Assert
            assert_eq!(result, missing(operation), "{placement}: {operation}");
        }
        assert_eq!(
            app.sessions.sessions().len(),
            loaded_sessions,
            "{placement}"
        );
        assert_eq!(app.pending_session_creations.len(), 0, "{placement}");
        assert_eq!(stored_status(&app, "hidden").await, "Review", "{placement}");
        assert_eq!(message_count(&app, "hidden").await, 0, "{placement}");
        assert_eq!(
            app.services
                .db()
                .graph_proposals()
                .latest("hidden")
                .await
                .expect("proposal read")
                .map(|state| (state.input, state.version.number)),
            Some((stored_proposal().input, 1)),
            "{placement}"
        );
    }
}

#[tokio::test]
async fn only_the_scope_check_keeps_remote_requests_from_the_unlisted_session() {
    for placement in UNLISTED_PLACEMENTS {
        let operations = {
            let (app, _directory) = unlisted_session_app(placement).await;

            session_requests(&app.session_service(), &app, "hidden")
                .into_iter()
                .map(|(operation, _)| operation)
                .collect::<Vec<_>>()
        };

        for (index, operation) in operations.into_iter().enumerate() {
            // Arrange
            let (mut app, _directory) = unlisted_session_app(placement).await;
            let api = app.session_service();
            let (_, request) = session_requests(&api, &app, "hidden").swap_remove(index);

            // Act
            let result = app.drive_session_request(request).await;

            // Assert
            // The same request from the session API, which the scope does
            // not limit, gets past the answer the remote request gets,
            // except where its own handler serves only loaded sessions.
            if placement != "viewed page"
                && ANSWERED_AS_MISSING_BY_THEIR_HANDLER.contains(&operation)
            {
                assert_eq!(result, missing(operation), "{placement}: {operation}");
            } else {
                assert_ne!(result, missing(operation), "{placement}: {operation}");
            }
        }
    }
}

#[tokio::test]
async fn api_and_terminal_callers_still_reach_sessions_the_host_does_not_list() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let project_id = app.active_project_id();
    insert_session(&app, "hidden", project_id).await;
    let services = [
        ("api", app.session_service()),
        ("terminal", app.session_view_service()),
    ];

    for (caller, service) in services {
        // Act
        let session = app
            .drive_session_request(service.get_session(&SessionId::from("hidden")))
            .await;
        let graphs = app
            .drive_session_request(service.session_graphs(graphs_request("hidden")))
            .await;
        let proposal = app
            .drive_session_request(service.get_graph_proposal("missing"))
            .await;

        // Assert
        assert!(
            session.expect("session read").is_some(),
            "{caller} reads the stored session"
        );
        assert_eq!(graphs.expect("graph read").graphs, Vec::new(), "{caller}");
        assert_eq!(proposal, Ok(None), "{caller}");
    }
}

#[tokio::test]
async fn remote_control_is_refused_reads_and_mutations_it_does_not_serve() {
    // Arrange
    let (mut app, _directory) = listed_session_app().await;
    let remote = app.remote_session_service();
    let api = app.session_service();
    let session_id = SessionId::from("sess");

    // Act
    let reads = app
        .drive_session_request(async {
            [
                remote.get_graph_proposal("proposal").await.map(|_| ()),
                remote.get_execution_graph("graph").await.map(|_| ()),
                remote.get_agent_task_history("task").await.map(|_| ()),
                remote.list_tasks(&session_id).await.map(|_| ()),
                remote.get_task("task").await.map(|_| ()),
            ]
        })
        .await;
    let mutations = app
        .drive_session_request(async {
            [
                remote.approve_graph_proposal("proposal", 1).await,
                remote
                    .start_execution_graph(StartExecutionGraphRequest {
                        graph: graph(),
                        provider_call_limits: [("calls".into(), 1)].into(),
                    })
                    .await,
                remote
                    .submit_durable_message(
                        &session_id,
                        DurableMessageRequest {
                            message: "Durable turn".into(),
                            operation_id: "operation".into(),
                            visibility: DurableMessageVisibility::Visible,
                        },
                    )
                    .await,
                remote
                    .create_task(
                        &session_id,
                        CreateTaskRequest {
                            objective: "Outcome".into(),
                            task_id: "task".into(),
                        },
                    )
                    .await,
                remote.complete_task("task").await,
            ]
        })
        .await;
    let tasks = app.drive_session_request(api.list_tasks(&session_id)).await;

    // Assert
    assert_eq!(reads, std::array::from_fn(|_| not_served()));
    assert_eq!(mutations, std::array::from_fn(|_| not_served()));
    assert_eq!(tasks, Ok(Vec::new()));
    assert_eq!(message_count(&app, "sess").await, 0);
}

#[tokio::test]
async fn remote_creation_reaches_the_host_only_for_the_active_project() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let listed_project_id = add_listed_project(&mut app, &directory, "listed").await;
    let remote = app.remote_session_service();
    let request = |project_id| CreateSessionRequest {
        inherit_from_session_id: None,
        mode: CreateSessionMode::Regular,
        project_id,
    };

    // Act
    let in_listed = app
        .drive_session_request(remote.create_session(request(listed_project_id)))
        .await;
    let in_unknown = app
        .drive_session_request(remote.create_session(request(999)))
        .await;
    let in_active = app
        .drive_session_request(remote.create_session(request(app.active_project_id())))
        .await;

    // Assert
    assert_eq!(
        in_listed,
        Err(ApiSessionError::Operation(format!(
            "Project `{listed_project_id}` is not active"
        )))
    );
    assert_eq!(
        in_unknown,
        Err(ApiSessionError::Operation(
            "Project `999` is not active".to_string()
        ))
    );
    // The host's own checks answer, so the scope check let it through.
    assert_eq!(
        in_active,
        Err(ApiSessionError::Operation(
            "Git branch is required to create a session".to_string()
        ))
    );
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
}

#[tokio::test]
async fn remote_syncs_and_inheriting_creations_reach_only_the_active_project() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let active_project_id = app.active_project_id();
    let listed_project_id = add_listed_project(&mut app, &directory, "listed").await;
    insert_loadable_session(&app, "sess", Status::Review, active_project_id).await;
    app.refresh_sessions_now().await;
    let remote = app.remote_session_service();
    let api = app.session_service();
    let listed = ProjectScope::Project(listed_project_id);
    let inheriting = CreateSessionRequest {
        inherit_from_session_id: Some(SessionId::from("sess")),
        mode: CreateSessionMode::Draft,
        project_id: listed_project_id,
    };

    // Act
    let synced = app
        .drive_session_request(remote.sync_project_for(listed))
        .await;
    let created = app
        .drive_session_request(remote.create_session(inheriting.clone()))
        .await;
    let api_created = app
        .drive_session_request(api.create_session(inheriting))
        .await;

    // Assert
    // A listed project the session API reaches stays out of a paired
    // device's reach, whichever way it names the project.
    assert_eq!(
        synced,
        Err(ApiSessionError::Operation(format!(
            "Project `{listed_project_id}` is not active"
        )))
    );
    let foreign_source = Err(ApiSessionError::Operation(format!(
        "Session `sess` belongs to project `{active_project_id}`, not `{listed_project_id}`"
    )));
    assert_eq!(created, foreign_source);
    assert_eq!(api_created, foreign_source);
    assert!(app.project_sync_statuses.is_empty());
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
    assert_eq!(app.sessions.sessions().len(), 1);
}

#[tokio::test]
async fn remote_project_reads_reach_only_the_active_project_and_load_no_page() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let home_project_id = add_listed_project(&mut app, &directory, "home").await;
    let viewed_project_id = add_listed_project(&mut app, &directory, "viewed").await;
    let unloaded_project_id = add_listed_project(&mut app, &directory, "unloaded").await;
    insert_loadable_session(&app, "sess", Status::Review, home_project_id).await;
    insert_loadable_session(&app, "viewed1", Status::Review, viewed_project_id).await;
    insert_loadable_session(&app, "unload1", Status::Review, unloaded_project_id).await;
    app.switch_project(home_project_id)
        .await
        .expect("project switch");
    app.ensure_project_page(viewed_project_id)
        .await
        .expect("listed page should load");
    let remote = app.remote_session_service();
    let viewed = ProjectScope::Project(viewed_project_id);
    let inactive = Err(ApiSessionError::Operation(format!(
        "Project `{viewed_project_id}` is not active"
    )));

    // Act
    let projects = app.drive_session_request(remote.list_projects()).await;
    let viewed_sessions = app
        .drive_session_request(remote.project_sessions(viewed))
        .await;
    let viewed_host = app
        .drive_session_request(remote.host_context_for(viewed))
        .await;
    let viewed_files = app
        .drive_session_request(remote.list_project_files_for(viewed))
        .await;
    let active_sessions = app
        .drive_session_request(remote.project_sessions(ProjectScope::Active))
        .await
        .expect("active project should list");
    let mut listed = Vec::new();
    for session_id in ["sess", "viewed1", "unload1"] {
        let session_id = SessionId::from(session_id);
        listed.push(
            app.drive_session_request(remote.is_session_listed(&session_id))
                .await,
        );
    }
    let loaded_after_reads = app.sessions.state().viewed_project_ids();
    app.switch_project(viewed_project_id)
        .await
        .expect("project switch");

    // Assert
    // The terminal reaches only loaded pages, so it loads no other page.
    assert_eq!(
        app.drive_session_request(
            app.session_view_service()
                .project_sessions(ProjectScope::Project(unloaded_project_id))
        )
        .await,
        Err(ApiSessionError::NotFound)
    );
    assert_eq!(projects, Err(not_served().expect_err("refusal")));
    assert_eq!(viewed_sessions.map(|_| ()), inactive);
    assert_eq!(viewed_host.map(|_| ()), inactive);
    assert_eq!(viewed_files.map(|_| ()), inactive);
    assert_eq!(active_sessions.project_id, Some(home_project_id));
    assert_eq!(
        active_sessions
            .entries
            .iter()
            .map(|entry| entry.summary.id.as_str())
            .collect::<Vec<_>>(),
        vec!["sess"]
    );
    assert_eq!(listed, vec![Ok(true), Ok(false), Ok(false)]);
    // A paired device loads no page of another project, and its reads of
    // the active project lease no page, so a switch drops that page.
    assert_eq!(loaded_after_reads, vec![viewed_project_id]);
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
}

#[tokio::test]
async fn remote_control_reads_a_session_the_host_lists() {
    // Arrange
    let (mut app, _directory) = listed_session_app().await;
    let remote = app.remote_session_service();
    let session_id = SessionId::from("sess");

    // Act
    let session = app
        .drive_session_request(remote.get_session(&session_id))
        .await;
    let graphs = app
        .drive_session_request(remote.session_graphs(graphs_request("sess")))
        .await;
    let files = app
        .drive_session_request(remote.list_session_files(&session_id))
        .await;
    let project = app.drive_session_request(remote.project_status()).await;

    // Assert
    assert_eq!(
        session.expect("session read").map(|session| session.id),
        Some(session_id)
    );
    assert_eq!(graphs.expect("graph read").graphs, Vec::new());
    assert_eq!(files, Ok(Some(Vec::new())));
    assert!(project.is_ok(), "project reads serve every caller");
}

#[tokio::test]
async fn remote_requests_queued_before_a_project_switch_no_longer_reach_the_session() {
    // Arrange
    let (mut app, directory) = listed_session_app().await;
    let other_project_id = add_project(&app, &directory, "other").await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .returning(|_| Box::pin(async { None }));
    app.services.set_git_client(Arc::new(git_client));
    let _consumer = app.sessions.foreground_consumer();
    let remote = app.remote_session_service();
    let listed = app.drive_session_request(remote.list_sessions()).await;
    let read = tokio::spawn({
        let remote = remote.clone();

        async move { remote.get_session(&SessionId::from("sess")).await }
    });
    let read_command = app.sessions.next_command().await;
    // Its handler reads the stored row, not the loaded session, so only the
    // scope check keeps it from the session after the switch.
    let answer = tokio::spawn(async move {
        remote
            .answer_questions(
                &SessionId::from("sess"),
                AnswerQuestionsRequest {
                    answers: Vec::new(),
                    attachments: Vec::new(),
                    graph_question_target: None,
                },
            )
            .await
    });
    let answer_command = app.sessions.next_command().await;

    // Act
    app.switch_project(other_project_id)
        .await
        .expect("project switch");
    app.apply_session_runtime_command(read_command).await;
    app.apply_session_runtime_command(answer_command).await;

    // Assert
    assert!(
        listed
            .expect("session list")
            .iter()
            .any(|summary| summary.id.as_str() == "sess")
    );
    assert_eq!(read.await.expect("read completes"), Ok(None));
    assert_eq!(
        answer.await.expect("answer completes"),
        Err(ApiSessionError::NotFound)
    );
    assert_eq!(message_count(&app, "sess").await, 0);
}

/// Every request a paired device can make that names `session_id`, labelled
/// `read ...` when the host answers a missing session with nothing.
fn session_requests(
    remote: &SessionService,
    app: &App,
    session_id: &str,
) -> Vec<(&'static str, Request)> {
    let id = SessionId::from(session_id);
    let creation = boxed({
        let remote = remote.clone();
        let id = id.clone();
        let project_id = app.active_project_id();

        async move {
            remote
                .create_session(CreateSessionRequest {
                    inherit_from_session_id: Some(id),
                    mode: CreateSessionMode::Regular,
                    project_id,
                })
                .await
                .map(|_| true)
        }
    });
    let mut requests = session_reads(remote, &id);
    requests.extend(turn_mutations(remote, &id));
    requests.extend(workflow_mutations(remote, &id));
    requests.push(("create inheriting", creation));

    requests
}

/// Calls `call` with its own copies of `remote` and `id`.
fn owned_request(remote: &SessionService, id: &SessionId, call: RequestCall) -> Request {
    call(remote.clone(), id.clone())
}

/// The session reads, labelled as [`session_requests`] describes.
fn session_reads(remote: &SessionService, id: &SessionId) -> Vec<(&'static str, Request)> {
    let request = |call: RequestCall| owned_request(remote, id, call);

    vec![
        (
            "read session",
            request(|remote, id| {
                boxed(async move { remote.get_session(&id).await.map(|found| found.is_some()) })
            }),
        ),
        (
            "read diff",
            request(|remote, id| {
                boxed(async move {
                    remote
                        .get_session_diff(&id)
                        .await
                        .map(|found| found.is_some())
                })
            }),
        ),
        (
            "read files",
            request(|remote, id| {
                boxed(async move {
                    remote
                        .list_session_files(&id)
                        .await
                        .map(|found| found.is_some())
                })
            }),
        ),
        (
            "graphs",
            request(|remote, id| {
                boxed(async move {
                    remote
                        .session_graphs(graphs_request(id.as_str()))
                        .await
                        .map(|_| true)
                })
            }),
        ),
    ]
}

/// The mutations of a session's turns and settings.
fn turn_mutations(remote: &SessionService, id: &SessionId) -> Vec<(&'static str, Request)> {
    let request = |call: RequestCall| owned_request(remote, id, call);

    vec![
        (
            "send message",
            request(|remote, id| {
                boxed(async move { remote.send_message(&id, "Hello").await.map(|()| true) })
            }),
        ),
        (
            "send message with images",
            request(|remote, id| {
                boxed(async move {
                    let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
                    let image = MessageImage::new(png).expect("PNG image");

                    remote
                        .send_message_with_images(&id, "Hello", vec![image])
                        .await
                        .map(|()| true)
                })
            }),
        ),
        (
            "answer questions",
            request(|remote, id| {
                boxed(async move {
                    remote
                        .answer_questions(
                            &id,
                            AnswerQuestionsRequest {
                                answers: Vec::new(),
                                attachments: Vec::new(),
                                graph_question_target: None,
                            },
                        )
                        .await
                        .map(|()| true)
                })
            }),
        ),
        (
            "cancel",
            request(|remote, id| {
                boxed(async move { remote.cancel_session(&id).await.map(|()| true) })
            }),
        ),
        (
            "stop turn",
            request(|remote, id| boxed(async move { remote.stop_turn(&id).await.map(|()| true) })),
        ),
        (
            "rebase",
            request(|remote, id| {
                boxed(async move { remote.rebase_session(&id).await.map(|()| true) })
            }),
        ),
        (
            "settings",
            request(|remote, id| {
                boxed(async move {
                    remote
                        .update_session_settings(&id, SessionSettingsChange::default())
                        .await
                        .map(|()| true)
                })
            }),
        ),
        (
            "merge",
            request(|remote, id| {
                boxed(async move { remote.merge_session(&id).await.map(|()| true) })
            }),
        ),
    ]
}

/// The publication and graph mutations that name a session.
fn workflow_mutations(remote: &SessionService, id: &SessionId) -> Vec<(&'static str, Request)> {
    let request = |call: RequestCall| owned_request(remote, id, call);

    vec![
        (
            "publish",
            request(|remote, id| {
                boxed(async move { remote.create_review_request(&id).await.map(|_| true) })
            }),
        ),
        (
            "graph action",
            request(|remote, id| {
                boxed(async move {
                    remote
                        .act_on_session_graph(SessionGraphActionRequest {
                            action: SessionGraphAction::Cancel,
                            graph_id: "graph".into(),
                            session_id: id,
                            snapshot: "snapshot".into(),
                        })
                        .await
                        .map(|()| true)
                })
            }),
        ),
        (
            "graph proposal",
            request(|remote, id| {
                boxed(async move {
                    remote
                        .generate_graph_proposal(GenerateGraphProposalRequest::from_text(
                            PROPOSAL_ID.into(),
                            id,
                            PROPOSAL_TASK.into(),
                        ))
                        .await
                        .map(|()| true)
                })
            }),
        ),
    ]
}

fn boxed(request: impl Future<Output = Result<bool, ApiSessionError>> + Send + 'static) -> Request {
    Box::pin(request)
}

/// The stable refusal of an operation remote control does not serve.
fn not_served() -> Result<(), ApiSessionError> {
    Err(ApiSessionError::Operation(
        "Remote control does not serve this operation".to_string(),
    ))
}

fn graphs_request(session_id: &str) -> SessionGraphsRequest {
    SessionGraphsRequest {
        graph_id: None,
        node_id: None,
        session_id: session_id.into(),
    }
}

/// Builds an app that stores the session `hidden` at `placement`, one of
/// [`UNLISTED_PLACEMENTS`], without listing it.
///
/// The session already stores the graph proposal the tables ask for, which
/// its handler acknowledges by identity before it looks for the session.
async fn unlisted_session_app(placement: &str) -> (App, TempDir) {
    let (mut app, directory) = new_test_app().await;
    match placement {
        "other project" => {
            let project_id = add_project(&app, &directory, "other").await;
            insert_session(&app, "hidden", project_id).await;
        }
        "viewed page" => {
            let project_id = add_listed_project(&mut app, &directory, "viewed").await;
            insert_loadable_session(&app, "hidden", Status::Review, project_id).await;
            app.ensure_project_page(project_id)
                .await
                .expect("listed page should load");
            assert!(app.sessions.session_for_id("hidden").is_some());
        }
        _ => insert_session(&app, "hidden", app.active_project_id()).await,
    }
    app.services
        .db()
        .graph_proposals()
        .create(&stored_proposal())
        .await
        .expect("proposal should persist");

    (app, directory)
}

/// The proposal of `hidden` stored under the identity and task the tables'
/// proposal request carries.
fn stored_proposal() -> GraphProposalState {
    let mut state = graph_proposal_state(PROPOSAL_ID, "hidden");
    state.input = GenerateGraphProposalRequest::from_text(
        PROPOSAL_ID.into(),
        "hidden".into(),
        PROPOSAL_TASK.into(),
    );

    state
}

/// The answer to `operation` for a session that does not exist: nothing
/// for a `read ...`, not found otherwise.
fn missing(operation: &str) -> Result<bool, ApiSessionError> {
    if operation.starts_with("read ") {
        Ok(false)
    } else {
        Err(ApiSessionError::NotFound)
    }
}

/// Builds an app whose active project stores and lists the session `sess`.
async fn listed_session_app() -> (App, TempDir) {
    let (mut app, directory) = new_test_app().await;
    let project_id = app.active_project_id();
    insert_session(&app, "sess", project_id).await;
    app.sessions
        .push_session(session_fixture("sess", Status::Review));

    (app, directory)
}

async fn insert_session(app: &App, session_id: &str, project_id: i64) {
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("session should persist");
}

/// Registers an openable project directory named `name`.
async fn add_project(app: &App, directory: &TempDir, name: &str) -> i64 {
    let path = directory.path().join(name);
    fs::create_dir_all(path.join(".git")).expect("repository marker");

    app.services
        .db()
        .projects()
        .upsert_project(&path.to_string_lossy(), None)
        .await
        .expect("project should persist")
}

async fn stored_status(app: &App, session_id: &str) -> String {
    app.services
        .db()
        .sessions()
        .load_session(session_id)
        .await
        .expect("session read")
        .expect("stored session")
        .status
}

async fn message_count(app: &App, session_id: &str) -> usize {
    app.services
        .db()
        .sessions()
        .load_session_messages(session_id)
        .await
        .expect("messages read")
        .len()
}
