use std::time::Duration;

use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, SessionMessage,
    SessionMessageKind, SessionStatus, SessionTranscript, StartPlanThenImplementRequest,
};

use crate::app::App;
use crate::domain::session::SessionId;
use crate::presentation::app_mode::AppMode;
use crate::presentation::task_chain_graph::TaskChainNodeStatus;
use crate::test_support::{SessionFixtureBuilder, new_test_app};

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
    assert!(!app.task_chain_graphs.needs_load(&session_id, 0));
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
    assert!(!app.task_chain_graphs.needs_load(&session_id, 0));
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
    assert!(app.task_chain_graphs.needs_load(&session_id, 0));
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
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "workflow".into(),
        planner_attempt_id: "initial".into(),
        user_request: "Add search".into(),
    }
}
