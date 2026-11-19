//! Approval decisions: which calls ask, panicking handlers, and replacement
//! arguments checked against the tool's parameters.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Approval, ApprovalHandler, ApprovalRequest, Approved, Decision};
use crate::tool::{ToolDefinition, ToolEffects};
use crate::turn::TurnError;

/// Returns `decision`, or panics when it is `None`.
struct Fixed(Option<Decision>);

#[async_trait]
impl ApprovalHandler for Fixed {
    async fn decide(&self, _request: &ApprovalRequest) -> Decision {
        match &self.0 {
            Some(decision) => decision.clone(),
            None => std::panic::resume_unwind(Box::new("approval bug")),
        }
    }
}

fn approval(decision: Option<Decision>) -> Approval {
    Approval::new(Arc::new(Fixed(decision)), Duration::from_secs(1))
}

fn definition(parameters: Value) -> ToolDefinition {
    ToolDefinition::new("deploy", "Deploy a target.", parameters).expect("valid definition")
}

fn request() -> ApprovalRequest {
    ApprovalRequest {
        arguments: json!({"target": "prod"}),
        call_id: "deploy_0".to_string(),
        effects: ToolEffects::Executes,
        run: None,
        session: None,
        tool: "deploy".to_string(),
    }
}

/// Returns the display of a tool failure, or panics for another result.
fn tool_failure(result: Result<Approved, TurnError>) -> String {
    let error = result.expect_err("expected a tool failure");
    assert!(matches!(error, TurnError::ToolFailed { .. }), "{error:?}");

    error.to_string()
}

#[test]
fn only_calls_that_change_state_wait_for_a_decision() {
    // Arrange
    let effects = [
        ToolEffects::ReadOnly,
        ToolEffects::Writes,
        ToolEffects::Executes,
    ];

    // Act
    let required: Vec<bool> = effects.into_iter().map(Approval::requires).collect();

    // Assert
    assert_eq!(required, [false, true, true]);
}

#[tokio::test]
async fn allow_and_deny_answer_the_call() {
    // Arrange
    let parameters = definition(json!({"type": "object"}));
    let deny = Decision::Deny {
        feedback: "no".to_string(),
    };

    // Act
    let allowed = approval(Some(Decision::Allow))
        .decide(&parameters, request())
        .await;
    let denied = approval(Some(deny)).decide(&parameters, request()).await;

    // Assert
    assert_eq!(
        allowed.expect("allowed"),
        Approved::Run(json!({"target": "prod"}))
    );
    assert_eq!(denied.expect("denied"), Approved::Denied("no".to_string()));
}

#[tokio::test]
async fn a_panicking_handler_fails_the_call_as_a_tool_failure() {
    // Act
    let result = approval(None)
        .decide(&definition(json!({"type": "object"})), request())
        .await;

    // Assert
    assert_eq!(
        tool_failure(result),
        "tool `deploy` failed: the approval handler panicked"
    );
}

#[tokio::test]
async fn replacement_arguments_must_match_a_usable_parameters_schema() {
    // Arrange
    let strict = definition(json!({
        "type": "object",
        "properties": {"target": {"type": "string"}},
        "required": ["target"]
    }));
    let unusable = definition(json!({"type": "object", "properties": {"target": {"type": 5}}}));
    let oversized = json!({"target": "x".repeat(3 * 1024 * 1024)});

    // Act
    let valid = approval(Some(Decision::AllowWith(json!({"target": "staging"}))))
        .decide(&strict, request())
        .await;
    let violation = approval(Some(Decision::AllowWith(json!({"target": 5}))))
        .decide(&strict, request())
        .await;
    let too_large = approval(Some(Decision::AllowWith(oversized)))
        .decide(&strict, request())
        .await;
    let no_schema = approval(Some(Decision::AllowWith(json!({}))))
        .decide(&unusable, request())
        .await;

    // Assert
    assert_eq!(
        valid.expect("valid replacement"),
        Approved::Replaced(json!({"target": "staging"}))
    );
    assert!(tool_failure(violation).starts_with(
        "tool `deploy` failed: approved arguments do not match the tool's parameters: /target: "
    ));
    assert_eq!(
        tool_failure(too_large),
        "tool `deploy` failed: approved arguments do not match the tool's parameters: they are \
         too large"
    );
    assert!(
        tool_failure(no_schema)
            .starts_with("tool `deploy` failed: approved arguments do not match the tool's"),
    );
}
