use std::time::Duration;

use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, SessionMessage,
    SessionMessageKind, SessionStatus, SessionTranscript, StartPlanThenImplementRequest,
};
use tempfile::TempDir;

use crate::app::App;
use crate::domain::session::SessionId;
use crate::infra::db::Database;
use crate::presentation::app_mode::AppMode;
use crate::presentation::task_chain_graph::TaskChainNodeStatus;
use crate::test_support::{SessionFixtureBuilder, new_test_app, test_app_clients};
use crate::ui::session_output_assembly;

#[tokio::test]
async fn visible_chain_session_loads_its_latest_graph_for_the_update_version() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let session_id = visible_session(&mut app, Some("workflow")).await;
    app.services
        .db()
        .tasks()
        .start_task_chain(&session_id, &launch_request(), "agent_task")
        .await
        .expect("durable chain");

    // Act
    app.refresh_task_chain_graph_if_needed();
    app.refresh_task_chain_graph_if_needed();
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.task_chain_graphs.graph(&session_id).is_none() {
            let event = app.next_app_event().await.expect("graph load event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("graph load guard");

    // Assert
    let graph = app
        .task_chain_graphs
        .graph(&session_id)
        .expect("loaded graph");
    assert_eq!(graph.chain_id, "workflow");
    assert_eq!(
        graph
            .nodes
            .iter()
            .map(|node| (node.agent, node.status))
            .collect::<Vec<_>>(),
        [
            (
                BuiltinAgentDefinition::Planner,
                TaskChainNodeStatus::Working
            ),
            (
                BuiltinAgentDefinition::Implementer,
                TaskChainNodeStatus::Waiting
            ),
        ]
    );
    assert!(
        !app.task_chain_graphs
            .needs_load(&session_id, 0, app.services.clock().now_instant())
    );
}

#[tokio::test]
async fn failed_graph_load_settles_without_a_graph() {
    // Arrange
    let directory = TempDir::new().expect("app directory");
    let database = Database::in_memory().await.expect("database");
    let mut app = App::new_with_clients(
        directory.path().into(),
        directory.path().into(),
        None,
        database.clone(),
        test_app_clients(),
    )
    .await
    .expect("app");
    let session_id = visible_session(&mut app, Some("workflow")).await;
    let previous = session_output_assembly::tests::task_chain_graph("workflow");
    app.task_chain_graphs.begin_load(&session_id, u64::MAX);
    app.task_chain_graphs.complete_load(
        &session_id,
        u64::MAX,
        Ok(Some(previous.clone())),
        app.services.clock().now_instant(),
    );
    database.pool().close().await;

    // Act: the reducer may drain the completion inside another event batch,
    // so wait on the settled cache state rather than one event.
    app.refresh_task_chain_graph_if_needed();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !app.task_chain_graphs.needs_load(
            &session_id,
            u64::MAX,
            app.services.clock().now_instant(),
        ) {
            let event = app.next_app_event().await.expect("graph load event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("graph load guard");

    // Assert: the failed load kept the previous snapshot.
    assert_eq!(app.task_chain_graphs.graph(&session_id), Some(&previous));
}

#[tokio::test]
async fn ordinary_visible_session_resolves_without_loading_task_chains() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let session_id = visible_session(&mut app, None).await;

    // Act
    app.refresh_task_chain_graph_if_needed();

    // Assert
    assert_eq!(app.task_chain_graphs.graph(&session_id), None);
    assert!(
        !app.task_chain_graphs
            .needs_load(&session_id, 0, app.services.clock().now_instant())
    );
}

#[tokio::test]
async fn hidden_sessions_never_start_graph_loads() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let session_id = visible_session(&mut app, Some("workflow")).await;
    app.mode = AppMode::List;

    // Act
    app.refresh_task_chain_graph_if_needed();

    // Assert
    assert!(
        app.task_chain_graphs
            .needs_load(&session_id, 0, app.services.clock().now_instant())
    );
}

async fn visible_session(app: &mut App, graph_chain_id: Option<&str>) -> SessionId {
    let session_id = SessionId::from("chain-session");
    app.services
        .db()
        .sessions()
        .insert_session(
            &session_id,
            "gpt-6.1-sol",
            "main",
            "Review",
            app.active_project_id(),
        )
        .await
        .expect("persisted session");
    let mut session = SessionFixtureBuilder::new()
        .id(session_id.clone())
        .status(SessionStatus::Review)
        .build();
    let mut messages = vec![SessionMessage::new(
        0,
        SessionMessageKind::UserPrompt,
        "Add search",
    )];
    if let Some(chain_id) = graph_chain_id {
        messages.push(SessionMessage::new(
            1,
            SessionMessageKind::AgentGraph,
            chain_id,
        ));
    }
    session.transcript = Some(SessionTranscript::new(messages));
    app.sessions.push_session(session);
    app.mode = AppMode::View {
        scroll_offset: None,
        session_id: session_id.clone(),
    };

    session_id
}

fn launch_request() -> StartPlanThenImplementRequest {
    StartPlanThenImplementRequest {
        attachments: Vec::new(),
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "workflow".into(),
        planner_attempt_id: "initial".into(),
        user_request: "Add search".into(),
    }
}
