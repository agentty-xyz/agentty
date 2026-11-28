//! Host-enforced remote-control scope: a paired device reaches every project
//! the host lists and only the sessions on those projects' pages, and only
//! the operations remote control serves, checked in the foreground turn that
//! serves each request.

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_session::test_support::graph_proposal_state;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, CreateTaskRequest,
    DurableMessageRequest, DurableMessageVisibility, GenerateGraphProposalRequest,
    GraphProposalState, MessageImage, ProjectScope, ReasoningLevel,
    SessionError as ApiSessionError, SessionGraphAction, SessionGraphActionRequest,
    SessionGraphsRequest, SessionId, SessionService, SessionSettingsChange,
    StartExecutionGraphRequest, test_support as model_fixture,
};
use ag_store::MockGraphRepository;
use tempfile::TempDir;

use super::support::{entry_ids, terminal_list, viewed_session_ids};
use crate::app::App;
use crate::app::execution_graph::tests::graph;
use crate::app::test_support::{
    MockSyncMainRunner, add_listed_project, hide_project, insert_loadable_session,
};
use crate::domain::session::Status;
use crate::domain::session_order::ARCHIVE_PAGE_SIZE;
use crate::test_support::{new_test_app, session_fixture};

/// A boxed request whose result says whether the host returned something.
type Request = Pin<Box<dyn Future<Output = Result<bool, ApiSessionError>> + Send>>;

/// Builds one [`Request`] from its own service and session identity.
type RequestCall = fn(SessionService, SessionId) -> Request;

/// Requests whose own handler answers a session no loaded page holds as
/// missing before it reads or changes anything else. For a session that no
/// page holds, the scope check cannot be told apart from these handlers.
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
/// to a paired device: in a project the switcher does not offer, off the
/// active page of the active project, or in a project the switcher offered
/// until it was hidden.
const UNLISTED_PLACEMENTS: [&str; 3] = ["other project", "not loaded", "hidden project"];

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
        let project_id = app.active_project_id();

        for (operation, request) in session_requests(&remote, project_id, "hidden") {
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
        assert_eq!(
            app.sessions.state().viewed_project_ids(),
            Vec::<i64>::new(),
            "{placement}: no page of an unlisted project loads"
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

            session_requests(&app.session_service(), app.active_project_id(), "hidden")
                .into_iter()
                .map(|(operation, _)| operation)
                .collect::<Vec<_>>()
        };

        for (index, operation) in operations.into_iter().enumerate() {
            // Arrange
            let (mut app, _directory) = unlisted_session_app(placement).await;
            let api = app.session_service();
            let (_, request) =
                session_requests(&api, app.active_project_id(), "hidden").swap_remove(index);

            // Act
            let result = app.drive_session_request(request).await;

            // Assert
            // The same request from the session API, which the scope does
            // not limit, gets past the answer the remote request gets,
            // except where its own handler serves only loaded sessions.
            if ANSWERED_AS_MISSING_BY_THEIR_HANDLER.contains(&operation) {
                assert_eq!(result, missing(operation), "{placement}: {operation}");
            } else {
                assert_ne!(result, missing(operation), "{placement}: {operation}");
            }
        }
    }
}

#[tokio::test]
async fn remote_requests_reach_a_session_of_another_listed_project() {
    let operations = {
        let (app, _directory, project_id) = listed_project_session_app().await;

        session_requests(&app.remote_session_service(), project_id, "listed1")
            .into_iter()
            .map(|(operation, _)| operation)
            .collect::<Vec<_>>()
    };

    for (index, operation) in operations.into_iter().enumerate() {
        // Arrange
        let (mut app, _directory, project_id) = listed_project_session_app().await;
        let remote = app.remote_session_service();
        let (_, request) = session_requests(&remote, project_id, "listed1").swap_remove(index);
        let terminal_before = terminal_list(&app);

        // Act
        let result = app.drive_session_request(request).await;

        // Assert
        // Every request a device makes for the session gets past the scope
        // check, which loads the session's page in the turn that serves it,
        // and the terminal's list stays as it was.
        assert_ne!(result, missing(operation), "{operation}");
        assert!(
            viewed_session_ids(&app, project_id)
                .iter()
                .any(|session_id| session_id == "listed1"),
            "{operation}"
        );
        assert_eq!(terminal_list(&app), terminal_before, "{operation}");
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
async fn remote_creation_and_sync_reach_every_listed_project_and_no_other() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let active_project_id = app.active_project_id();
    let listed_project_id = add_listed_project(&mut app, &directory, "listed").await;
    let hidden_project_id = add_listed_project(&mut app, &directory, "hidden").await;
    hide_project(&mut app, hidden_project_id).await;
    insert_loadable_session(&app, "listed1", Status::Review, listed_project_id).await;
    let started_dirs = record_project_syncs(&mut app);
    let remote = app.remote_session_service();
    let terminal_before = terminal_list(&app);
    let request = |project_id, inherit_from_session_id: Option<&str>| CreateSessionRequest {
        inherit_from_session_id: inherit_from_session_id.map(SessionId::from),
        mode: CreateSessionMode::Draft,
        project_id,
    };

    // Act
    let created = app
        .drive_session_request(remote.create_session(request(listed_project_id, None)))
        .await;
    let inherited = app
        .drive_session_request(remote.create_session(request(listed_project_id, Some("listed1"))))
        .await;
    let inherited_elsewhere = app
        .drive_session_request(remote.create_session(request(active_project_id, Some("listed1"))))
        .await;
    let synced = app
        .drive_session_request(remote.sync_project_for(ProjectScope::Project(listed_project_id)))
        .await;
    let mut unlisted = Vec::new();
    for project_id in [hidden_project_id, hidden_project_id + 100] {
        unlisted.push(
            app.drive_session_request(remote.create_session(request(project_id, None)))
                .await
                .map(|_| ()),
        );
        unlisted.push(
            app.drive_session_request(remote.sync_project_for(ProjectScope::Project(project_id)))
                .await,
        );
    }

    // Assert
    let created = created.expect("a listed project takes a device's new session");
    let inherited = inherited.expect("a device may inherit from a session it lists");
    for (label, session_id) in [("created", &created), ("inherited", &inherited)] {
        let row = app
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .expect("session read")
            .expect("created session");
        assert_eq!(row.project_id, Some(listed_project_id), "{label}");
    }
    let mut viewed = viewed_session_ids(&app, listed_project_id);
    viewed.sort_unstable();
    let mut expected_viewed = vec![
        "listed1".to_string(),
        created.to_string(),
        inherited.to_string(),
    ];
    expected_viewed.sort_unstable();
    assert_eq!(viewed, expected_viewed);
    // A creation inherits only within the source session's own project.
    assert_eq!(
        inherited_elsewhere,
        Err(ApiSessionError::Operation(format!(
            "Session `listed1` belongs to project `{listed_project_id}`, not `{active_project_id}`"
        )))
    );
    assert_eq!(synced, Ok(()));
    assert_eq!(
        *started_dirs.lock().expect("started syncs"),
        vec![directory.path().join("listed")]
    );
    assert_eq!(unlisted, vec![Err(ApiSessionError::NotFound); 4]);
    assert_eq!(terminal_list(&app), terminal_before);
    assert_eq!(
        app.sessions.state().viewed_project_ids(),
        vec![listed_project_id]
    );
}

#[tokio::test]
async fn remote_project_reads_reach_every_listed_project_and_load_its_page() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let home_project_id = add_listed_project(&mut app, &directory, "home").await;
    let viewed_project_id = add_listed_project(&mut app, &directory, "viewed").await;
    let unloaded_project_id = add_listed_project(&mut app, &directory, "unloaded").await;
    let hidden_project_id = add_listed_project(&mut app, &directory, "hidden").await;
    insert_loadable_session(&app, "sess", Status::Review, home_project_id).await;
    insert_loadable_session(&app, "viewed1", Status::Review, viewed_project_id).await;
    insert_loadable_session(&app, "unload1", Status::Review, unloaded_project_id).await;
    insert_loadable_session(&app, "hidden1", Status::Review, hidden_project_id).await;
    hide_project(&mut app, hidden_project_id).await;
    fs::write(directory.path().join("viewed/marker.txt"), "marker").expect("marker file");
    app.switch_project(home_project_id)
        .await
        .expect("project switch");
    let terminal_before = terminal_list(&app);
    let remote = app.remote_session_service();
    let viewed = ProjectScope::Project(viewed_project_id);

    // Act
    let projects = app
        .drive_session_request(remote.list_projects())
        .await
        .expect("a device lists the projects");
    let viewed_sessions = app
        .drive_session_request(remote.project_sessions(viewed))
        .await
        .expect("a listed project lists its sessions");
    let viewed_host = app
        .drive_session_request(remote.host_context_for(viewed))
        .await
        .expect("a listed project is described");
    let viewed_files = app
        .drive_session_request(remote.list_project_files_for(viewed))
        .await
        .expect("a listed project's checkout lists");
    let active_sessions = app
        .drive_session_request(remote.project_sessions(ProjectScope::Active))
        .await
        .expect("the active project lists");
    let mut listed = Vec::new();
    for session_id in ["sess", "viewed1", "unload1", "hidden1"] {
        let session_id = SessionId::from(session_id);
        listed.push(
            app.drive_session_request(remote.is_session_listed(&session_id))
                .await,
        );
    }
    let hidden = ProjectScope::Project(hidden_project_id);
    let hidden_reads = [
        app.drive_session_request(remote.project_sessions(hidden))
            .await
            .map(|_| ()),
        app.drive_session_request(remote.host_context_for(hidden))
            .await
            .map(|_| ()),
        app.drive_session_request(remote.list_project_files_for(hidden))
            .await
            .map(|_| ()),
    ];

    // Assert
    let listed_projects = projects
        .iter()
        .map(|project| (project.id, project.is_active))
        .collect::<Vec<_>>();
    for project in [
        (home_project_id, true),
        (viewed_project_id, false),
        (unloaded_project_id, false),
    ] {
        assert!(listed_projects.contains(&project), "{listed_projects:?}");
    }
    assert!(
        listed_projects
            .iter()
            .all(|(project_id, _)| *project_id != hidden_project_id),
        "{listed_projects:?}"
    );
    assert_eq!(viewed_sessions.project_id, Some(viewed_project_id));
    assert_eq!(
        viewed_sessions
            .status
            .map(|status| (status.project_name, status.branch)),
        Some(("viewed".to_string(), Some("main".to_string())))
    );
    assert_eq!(entry_ids(&viewed_sessions.entries), vec!["viewed1"]);
    assert_eq!(viewed_host.project_id, viewed_project_id);
    assert_eq!(viewed_host.project_name, "viewed");
    assert!(viewed_files.iter().any(|entry| entry.path == "marker.txt"));
    // Every answer names its project, the active one's included.
    assert_eq!(active_sessions.project_id, Some(home_project_id));
    assert!(active_sessions.status.is_some());
    assert_eq!(entry_ids(&active_sessions.entries), vec!["sess"]);
    assert_eq!(listed, vec![Ok(true), Ok(true), Ok(true), Ok(false)]);
    assert_eq!(
        hidden_reads,
        std::array::from_fn(|_| Err(ApiSessionError::NotFound))
    );
    // A device's request loads the page of the listed project it names,
    // and of a listed session's project, and of no hidden project.
    assert_eq!(
        app.sessions.state().viewed_project_ids(),
        vec![viewed_project_id, unloaded_project_id]
    );
    assert_eq!(app.active_project_id(), home_project_id);
    assert_eq!(terminal_list(&app), terminal_before);
}

#[tokio::test]
async fn the_terminal_reaches_only_project_pages_already_loaded() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let viewed_project_id = add_listed_project(&mut app, &directory, "viewed").await;
    let unloaded_project_id = add_listed_project(&mut app, &directory, "unloaded").await;
    app.ensure_project_page(viewed_project_id)
        .await
        .expect("listed page should load");
    let terminal = app.session_view_service();

    // Act
    let viewed = app
        .drive_session_request(terminal.project_sessions(ProjectScope::Project(viewed_project_id)))
        .await
        .map(|listing| listing.project_id);
    let unloaded = app
        .drive_session_request(
            terminal.project_sessions(ProjectScope::Project(unloaded_project_id)),
        )
        .await
        .map(|listing| listing.project_id);

    // Assert
    // Unlike a paired device, the terminal loads no page by naming it.
    assert_eq!(viewed, Ok(Some(viewed_project_id)));
    assert_eq!(unloaded, Err(ApiSessionError::NotFound));
    assert_eq!(
        app.sessions.state().viewed_project_ids(),
        vec![viewed_project_id]
    );
}

#[tokio::test]
async fn a_device_leases_the_pages_it_reads_and_reloads_one_dropped_while_idle() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let home_project_id = add_listed_project(&mut app, &directory, "home").await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "home1", Status::Review, home_project_id).await;
    app.switch_project(home_project_id)
        .await
        .expect("project switch");
    let remote = app.remote_session_service();

    // Act
    let while_active = read_session_id(&mut app, &remote, "home1").await;
    // The device read the terminal's page, so a switch keeps it loaded.
    app.switch_project(other_project_id)
        .await
        .expect("project switch");
    let after_switch = app.sessions.state().viewed_project_ids();
    let now = app.services.clock().now_instant();
    app.evict_idle_viewed_pages(now + Duration::from_mins(11));
    let after_idle = app.sessions.state().viewed_project_ids();
    let after_eviction = read_session_id(&mut app, &remote, "home1").await;

    // Assert
    let home1 = Ok(Some(SessionId::from("home1")));
    assert_eq!(while_active, home1);
    assert_eq!(after_switch, vec![home_project_id]);
    assert_eq!(after_idle, Vec::<i64>::new());
    // A device idle past eviction gets the page loaded again, not an error.
    assert_eq!(after_eviction, home1);
    assert_eq!(
        app.sessions.state().viewed_project_ids(),
        vec![home_project_id]
    );
}

#[tokio::test]
async fn a_device_request_keeps_its_page_ahead_of_older_ones_under_the_bound() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let mut project_ids = Vec::new();
    for index in 0..5 {
        project_ids
            .push(add_listed_project(&mut app, &directory, &format!("project{index}")).await);
    }
    insert_loadable_session(&app, "first1", Status::Review, project_ids[0]).await;
    for project_id in &project_ids[..4] {
        app.ensure_project_page(*project_id)
            .await
            .expect("listed page should load");
    }
    let remote = app.remote_session_service();

    // Act
    let read = app
        .drive_session_request(remote.get_session(&SessionId::from("first1")))
        .await;
    app.ensure_project_page(project_ids[4])
        .await
        .expect("listed page should load");

    // Assert
    // The device's read renewed the first page, so the second, now the
    // least recently requested, leaves for the fifth.
    assert!(read.expect("session read").is_some());
    assert_eq!(
        app.sessions.state().viewed_project_ids(),
        vec![
            project_ids[0],
            project_ids[2],
            project_ids[3],
            project_ids[4]
        ]
    );
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
async fn remote_requests_queued_before_a_terminal_switch_still_reach_the_session() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let home_project_id = add_listed_project(&mut app, &directory, "home").await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "home1", Status::Review, home_project_id).await;
    app.switch_project(home_project_id)
        .await
        .expect("project switch");
    let _consumer = app.sessions.foreground_consumer();
    let remote = app.remote_session_service();
    let read = tokio::spawn({
        let remote = remote.clone();

        async move { remote.get_session(&SessionId::from("home1")).await }
    });
    let read_command = app.sessions.next_command().await;
    let change = tokio::spawn(async move {
        remote
            .update_session_settings(
                &SessionId::from("home1"),
                SessionSettingsChange {
                    reasoning_level: Some(ReasoningLevel::Low),
                    ..SessionSettingsChange::default()
                },
            )
            .await
    });
    let change_command = app.sessions.next_command().await;

    // Act
    app.switch_project(other_project_id)
        .await
        .expect("project switch");
    app.apply_session_runtime_command(read_command).await;
    app.apply_session_runtime_command(change_command).await;

    // Assert
    // Reach does not depend on the terminal's project: the session's
    // project is still listed, so its page loads again for the request.
    assert_eq!(
        read.await
            .expect("read completes")
            .map(|session| session.map(|session| session.id)),
        Ok(Some(SessionId::from("home1")))
    );
    assert_eq!(change.await.expect("change completes"), Ok(()));
    assert_eq!(
        app.sessions
            .session_for_id("home1")
            .and_then(|session| session.reasoning_level_override),
        Some(ReasoningLevel::Low)
    );
    assert_eq!(app.active_project_id(), other_project_id);
}

#[tokio::test]
async fn remote_requests_queued_before_their_project_leaves_the_list_are_refused() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "other1", Status::Review, other_project_id).await;
    let _consumer = app.sessions.foreground_consumer();
    let remote = app.remote_session_service();
    let listed = app
        .drive_session_request(remote.is_session_listed(&SessionId::from("other1")))
        .await;
    // Each request already passed `ag-remote`'s own check, which saw the
    // project listed.
    let read = tokio::spawn({
        let remote = remote.clone();

        async move { remote.get_session(&SessionId::from("other1")).await }
    });
    let read_command = app.sessions.next_command().await;
    let message = tokio::spawn({
        let remote = remote.clone();

        async move {
            remote
                .send_message(&SessionId::from("other1"), "Hello".to_string())
                .await
        }
    });
    let message_command = app.sessions.next_command().await;
    let sessions = tokio::spawn({
        let remote = remote.clone();

        async move {
            remote
                .project_sessions(ProjectScope::Project(other_project_id))
                .await
        }
    });
    let sessions_command = app.sessions.next_command().await;
    let sync = tokio::spawn(async move {
        remote
            .sync_project_for(ProjectScope::Project(other_project_id))
            .await
    });
    let sync_command = app.sessions.next_command().await;

    // Act
    hide_project(&mut app, other_project_id).await;
    for command in [
        read_command,
        message_command,
        sessions_command,
        sync_command,
    ] {
        app.apply_session_runtime_command(command).await;
    }

    // Assert
    assert_eq!(listed, Ok(true));
    assert_eq!(read.await.expect("read completes"), Ok(None));
    assert_eq!(
        message.await.expect("message completes"),
        Err(ApiSessionError::NotFound)
    );
    assert_eq!(
        sessions
            .await
            .expect("listing completes")
            .map(|listing| listing.project_id),
        Err(ApiSessionError::NotFound)
    );
    assert_eq!(
        sync.await.expect("sync completes"),
        Err(ApiSessionError::NotFound)
    );
    assert_eq!(message_count(&app, "other1").await, 0);
    assert!(app.project_sync_statuses.is_empty());
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
}

#[tokio::test]
async fn a_remote_request_for_a_session_the_host_cannot_check_is_refused() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let remote = app.remote_session_service();
    let session_id = SessionId::from("unloaded");
    pool.close().await;

    // Act
    let change = app
        .drive_session_request(
            remote.update_session_settings(&session_id, SessionSettingsChange::default()),
        )
        .await;
    let read = app
        .drive_session_request(remote.get_session(&session_id))
        .await;
    let listed = app
        .drive_session_request(remote.is_session_listed(&session_id))
        .await;

    // Assert
    // Without the session's project, the host cannot tell that it lists
    // the session, so nothing reaches it.
    assert!(
        matches!(change, Err(ApiSessionError::Operation(_))),
        "{change:?}"
    );
    assert_eq!(read, Ok(None));
    assert_eq!(listed, Ok(false));
}

#[tokio::test]
async fn a_session_that_left_its_page_answers_remote_requests_as_missing() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    let archived_ids = (0..=ARCHIVE_PAGE_SIZE)
        .map(|index| format!("done{index}"))
        .collect::<Vec<_>>();
    for session_id in &archived_ids {
        insert_loadable_session(&app, session_id, Status::Done, other_project_id).await;
    }
    insert_loadable_session(&app, "deleted1", Status::Review, other_project_id).await;
    let remote = app.remote_session_service();
    let listed_before = app
        .drive_session_request(remote.is_session_listed(&SessionId::from("deleted1")))
        .await;
    let on_page = viewed_session_ids(&app, other_project_id);
    let past_window = archived_ids
        .iter()
        .filter(|session_id| !on_page.contains(session_id))
        .cloned()
        .collect::<Vec<_>>();
    app.services
        .db()
        .sessions()
        .delete_session("deleted1")
        .await
        .expect("session should delete");
    app.refresh_viewed_pages().await;

    // The archive window holds one page; the oldest archived session is
    // past it.
    assert_eq!(past_window.len(), 1);
    let leaving = [
        ("archived past the window", past_window[0].as_str()),
        ("deleted", "deleted1"),
    ];

    for (label, session_id) in leaving {
        let session_id = SessionId::from(session_id);

        // Act
        let listed = app
            .drive_session_request(remote.is_session_listed(&session_id))
            .await;
        let read = app
            .drive_session_request(remote.get_session(&session_id))
            .await;
        let change = app
            .drive_session_request(
                remote.update_session_settings(&session_id, SessionSettingsChange::default()),
            )
            .await;

        // Assert
        assert_eq!(listed, Ok(false), "{label}");
        assert_eq!(read, Ok(None), "{label}");
        assert_eq!(change, Err(ApiSessionError::NotFound), "{label}");
    }
    assert_eq!(listed_before, Ok(true));
}

/// Every request a paired device can make that names `session_id`, labelled
/// `read ...` when the host answers a missing session with nothing. A
/// creation inheriting from the session names `project_id`.
fn session_requests(
    remote: &SessionService,
    project_id: i64,
    session_id: &str,
) -> Vec<(&'static str, Request)> {
    let id = SessionId::from(session_id);
    let creation = boxed({
        let remote = remote.clone();
        let id = id.clone();

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
        (
            "document",
            request(|remote, id| {
                boxed(async move { remote.get_session_document(&id).await.map(|_| true) })
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
        "hidden project" => {
            let project_id = add_listed_project(&mut app, &directory, "hidden").await;
            insert_loadable_session(&app, "hidden", Status::Review, project_id).await;
            app.ensure_project_page(project_id)
                .await
                .expect("listed page should load");
            hide_project(&mut app, project_id).await;
            assert!(app.sessions.session_for_id("hidden").is_none());
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

/// Builds an app whose listed project, not the active one, stores the
/// loadable review session `listed1`, with no page of it loaded yet, and
/// returns that project's id.
async fn listed_project_session_app() -> (App, TempDir, i64) {
    let (mut app, directory) = new_test_app().await;
    let project_id = add_listed_project(&mut app, &directory, "listed").await;
    insert_loadable_session(&app, "listed1", Status::Review, project_id).await;
    assert!(app.sessions.state().viewed_page(project_id).is_none());

    (app, directory, project_id)
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

/// Lets `app` start project syncs without running Git, and records the
/// checkout each one starts in.
fn record_project_syncs(app: &mut App) -> Arc<Mutex<Vec<PathBuf>>> {
    let started_dirs = Arc::new(Mutex::new(Vec::new()));
    let mut sync_main_runner = MockSyncMainRunner::new();
    sync_main_runner.expect_start_sync_main().returning({
        let started_dirs = Arc::clone(&started_dirs);

        move |_, _, _, sync_context| {
            started_dirs
                .lock()
                .expect("started syncs")
                .push(sync_context.working_dir);
        }
    });
    app.sync_main_runner = Arc::new(sync_main_runner);

    started_dirs
}

/// Reads `session_id` through `remote` and returns the id of the session
/// the host answered with.
async fn read_session_id(
    app: &mut App,
    remote: &SessionService,
    session_id: &str,
) -> Result<Option<SessionId>, ApiSessionError> {
    app.drive_session_request(remote.get_session(&SessionId::from(session_id)))
        .await
        .map(|session| session.map(|session| session.id))
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

/// Registers an openable project directory named `name`, which the project
/// switcher does not offer.
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
