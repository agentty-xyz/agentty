use std::num::NonZeroUsize;
use std::sync::Arc;

use ag_contracts::{MockOneShotClient, OneShotSubmission, PermissionMode, SessionStats};
use ag_protocol::{AgentResponse, ProtocolRequestProfile, TurnPrompt, TurnPromptAttachment};
use ag_session::{GraphContext, GraphExecutionRef};

use crate::graph::support_test::dispatch;
use crate::graph::{GraphNodeRunner, GraphRunRequest, GraphUtilityRunner};
use crate::run::mock::MockRunRepository;
use crate::{HeartbeatClock, RunWorker};

#[tokio::test]
async fn malformed_dispatches_are_rejected_before_utility_submission() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_close().once().return_const(());
    let worker = Arc::new(RunWorker::with_client(
        Arc::new(client),
        Arc::new(MockRunRepository::new()),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let runner = GraphUtilityRunner::new(worker.clone());
    let mut conversation = dispatch();
    conversation.node.execution.context = GraphContext::TaskConversation {
        task_id: "task".into(),
    };
    let mut task = dispatch();
    task.record.execution = GraphExecutionRef::TaskAttempt {
        attempt_id: "attempt".into(),
        operation_id: "operation".into(),
        task_id: "task".into(),
    };
    let mut unsupported_schema = dispatch();
    unsupported_schema.node.execution.response_profile = ProtocolRequestProfile::SessionTurn;

    for (dispatch, expected) in [
        (conversation, "Unsupported utility graph node"),
        (task, "Utility node requires a utility run"),
        (unsupported_schema, "Unsupported utility response schema"),
    ] {
        // Act
        let result = runner
            .run(GraphRunRequest {
                dispatch,
                folder: "workspace".into(),
                permission_mode: PermissionMode::ReadOnly,
                provider_call_budget: None,
            })
            .await;

        // Assert
        assert_eq!(result.expect_err("invalid dispatch").to_string(), expected);
    }
    assert!(!runner.manages_environment(&dispatch().node));
    worker.shutdown().await;
}

#[tokio::test]
async fn fresh_utility_nodes_send_request_images_after_their_data() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client
        .expect_submit_cancellable()
        .once()
        .withf(|request, _| {
            request.attachments
                == [TurnPromptAttachment {
                    local_image_path: "design.png".into(),
                    placeholder: "[Task image #1]".into(),
                }]
                && request
                    .prompt
                    .ends_with("Evidence [Image #1]\n\nTask input attachment 1: [Task image #1]")
        })
        .returning(|_, _| {
            Ok(OneShotSubmission {
                response: AgentResponse::plain("Seen"),
                stats: SessionStats::default(),
            })
        });
    client.expect_close().once().return_const(());
    let mut repository = MockRunRepository::new();
    repository
        .expect_claim_queued()
        .once()
        .return_const(Ok(true));
    repository
        .expect_transition()
        .times(2)
        .returning(|_, _, _| Ok(()));
    let worker = Arc::new(RunWorker::with_client(
        Arc::new(client),
        Arc::new(repository),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let runner = GraphUtilityRunner::new(worker.clone());
    let mut dispatch = dispatch();
    dispatch.node.request = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            local_image_path: "design.png".into(),
            placeholder: "[Image #1]".into(),
        }],
        ..TurnPrompt::from_agent_data("Evidence [Image #1]".into())
    };
    let supported = runner.supports(&dispatch.node);

    // Act
    let result = runner
        .run(GraphRunRequest {
            dispatch,
            folder: "workspace".into(),
            permission_mode: PermissionMode::ReadOnly,
            provider_call_budget: None,
        })
        .await;
    worker.shutdown().await;

    // Assert
    assert!(supported);
    assert_eq!(result.expect("utility answer"), r#"{"answer":"Seen"}"#);
}
