use std::sync::Arc;

use ag_contracts::{
    AgentRequestKind, ExecutionPolicy, OneShotRequest, OneShotSubmission, PermissionMode,
    ProviderCallBudget, ReasoningLevel, SpeedMode,
};
use ag_protocol::{TurnPrompt, TurnPromptAttachment, TurnPromptContentPart};
use ag_worker::{MockRunClient, RunClient};

use super::{GraphEffectClient, graph_prompt, intersect_permissions};
use crate::app::execution_graph::tests::run_request;

#[test]
fn graph_images_disambiguate_reused_reply_markers_without_rewriting_json_data() {
    // Arrange
    let mut request = run_request();
    let input =
        serde_json::json!({"literal": "[Task image #1]", "request": "Use [Image #1]"}).to_string();
    request.dispatch.node.request = TurnPrompt::from_agent_data(input.clone());
    request.dispatch.node.request.attachments = vec![TurnPromptAttachment {
        local_image_path: "initial.png".into(),
        placeholder: "[Image #1]".into(),
    }];
    let replies = vec![TurnPromptAttachment {
        local_image_path: "revision.png".into(),
        placeholder: "[Image #1]".into(),
    }];

    // Act
    let prompt = graph_prompt(&request, &replies);
    let paths = prompt
        .content_parts()
        .into_iter()
        .filter_map(|part| match part {
            TurnPromptContentPart::Attachment(image) => {
                Some(image.local_image_path.to_str().expect("path"))
            }
            TurnPromptContentPart::Text(_) | TurnPromptContentPart::OrphanAttachment(_) => None,
        })
        .collect::<Vec<_>>();

    // Assert
    assert!(prompt.text.contains(&input));
    assert_eq!(paths, ["initial.png", "revision.png"]);
    assert_eq!(prompt.attachments[0].placeholder, "[#Task image #1]");
    assert_eq!(prompt.attachments[1].placeholder, "[Task image #2]");
}

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
