use std::sync::Arc;

use ag_contracts::{AgentError, MockAgentChannel};
use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, TaskAgentAssignment,
    TaskAgentContext,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::request;
use crate::{RuntimeConfig, SessionRunClient};

#[test]
fn rejects_invalid_task_context_ids_before_runtime_construction() {
    // Arrange
    for id in ["", "../session", "provider/thread", "not-a-uuid"] {
        let context = context(id);

        // Act
        let result = SessionRunClient::for_task_context(&context, &RuntimeConfig::default());

        // Assert
        assert!(
            matches!(result, Err(AgentError::Runtime(message)) if message == "Task context ID must be a UUID")
        );
    }
}

#[tokio::test]
async fn pre_canceled_task_cleans_its_context_without_touching_ordinary_chat() {
    // Arrange
    let context = context("AAAAAAAA-0000-0000-0000-000000000001");
    let mut client = SessionRunClient::for_task_context(&context, &RuntimeConfig::default())
        .expect("valid task context");
    let mut channel = MockAgentChannel::new();
    channel.expect_run_turn().never();
    channel
        .expect_shutdown_session()
        .once()
        .withf(|id| id == "task-aaaaaaaa-0000-0000-0000-000000000001")
        .returning(|_| Box::pin(async { Ok(()) }));
    client.runtime = ag_runtime::SessionRuntime::from_channel(Arc::new(channel));
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    // Act
    let result = client
        .submit(request(), mpsc::unbounded_channel().0, cancellation)
        .await;

    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
    assert_eq!(client.session_id, "owner");
}

fn context(id: &str) -> TaskAgentContext {
    TaskAgentContext {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: BuiltinAgentDefinition::Planner,
        },
        id: id.to_string(),
        session_id: "owner".into(),
        task_id: "caller-task".to_string(),
    }
}

#[tokio::test]
async fn active_task_cancellation_shuts_down_the_task_runtime() {
    // Arrange
    let context = context("aaaaaaaa-0000-0000-0000-000000000001");
    let mut client = SessionRunClient::for_task_context(&context, &RuntimeConfig::default())
        .expect("valid task context");
    let (started_tx, started_rx) = oneshot::channel();
    let mut started_tx = Some(started_tx);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .withf(|id, _, _| id == "task-aaaaaaaa-0000-0000-0000-000000000001")
        .returning(move |_, _, _| {
            started_tx
                .take()
                .expect("one turn")
                .send(())
                .expect("waiting test");
            Box::pin(std::future::pending())
        });
    channel
        .expect_shutdown_session()
        .once()
        .withf(|id| id == "task-aaaaaaaa-0000-0000-0000-000000000001")
        .returning(|_| Box::pin(async { Ok(()) }));
    client.runtime = ag_runtime::SessionRuntime::from_channel(Arc::new(channel));
    let cancellation = CancellationToken::new();
    let cancel_turn = cancellation.clone();

    // Act
    let (result, ()) = tokio::join!(
        client.submit(request(), mpsc::unbounded_channel().0, cancellation),
        async move {
            started_rx.await.expect("turn started");
            cancel_turn.cancel();
        },
    );

    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
}
