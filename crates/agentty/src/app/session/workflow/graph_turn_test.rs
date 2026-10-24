use std::sync::Arc;

use ag_contracts::{
    AgentRequestKind, ExecutionPolicy, OneShotRequest, OneShotSubmission, PermissionMode,
    ProviderCallBudget, ReasoningLevel, SpeedMode,
};
use ag_worker::{MockRunClient, RunClient};

use super::{GraphEffectClient, intersect_permissions};

#[test]
fn graph_permissions_never_exceed_captured_or_current_permissions() {
    // Arrange / Act / Assert
    for captured in PermissionMode::ALL {
        for current in PermissionMode::ALL {
            let expected = if captured.is_read_only() || current.is_read_only() {
                PermissionMode::ReadOnly
            } else if captured == PermissionMode::AutoEdit || current == PermissionMode::AutoEdit {
                PermissionMode::AutoEdit
            } else {
                PermissionMode::AutoEditAddressComments
            };
            assert_eq!(intersect_permissions(captured, current), expected);
        }
    }
}

#[tokio::test]
async fn effect_children_preserve_local_budgets_beneath_the_shared_graph_limit() {
    for local_limit in [None, Some(1)] {
        // Arrange
        let graph_budget = ProviderCallBudget::new(2);
        let local_budget = local_limit.map(ProviderCallBudget::new);
        let mut client = MockRunClient::new();
        client.expect_submit().once().returning(|request| {
            let budget = request.provider_call_budget.expect("graph ceiling");
            budget.consume().expect("one provider call");
            if budget.remaining() > 0 {
                budget.consume().expect("second graph call");
            }
            assert!(budget.consume().is_err());
            Ok(OneShotSubmission {
                response: ag_protocol::AgentResponse::plain("done"),
                stats: ag_contracts::SessionStats::default(),
            })
        });
        let client = GraphEffectClient {
            budget: graph_budget.clone(),
            client: Arc::new(client),
        };

        // Act
        client
            .submit(OneShotRequest {
                activity_tx: None,
                child_pid: None,
                execution_policy: ExecutionPolicy::default(),
                folder: "workspace".into(),
                harness: "codex".into(),
                model: "gpt-6.1-sol".into(),
                permission_mode: PermissionMode::ReadOnly,
                prompt: "commit metadata".into(),
                provider_call_budget: local_budget.clone(),
                reasoning_level: ReasoningLevel::default(),
                request_kind: AgentRequestKind::UtilityPrompt,
                speed_mode: SpeedMode::default(),
            })
            .await
            .expect("child execution");

        // Assert
        assert_eq!(graph_budget.remaining(), usize::from(local_limit.is_some()));
        if let Some(local) = local_budget {
            assert_eq!(local.remaining(), 0);
        }
    }
}
