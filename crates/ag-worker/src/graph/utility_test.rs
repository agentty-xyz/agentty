use std::num::NonZeroUsize;
use std::sync::Arc;

use ag_contracts::{MockOneShotClient, PermissionMode};
use ag_protocol::ProtocolRequestProfile;
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
    worker.shutdown().await;
}
