use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent::{
    AgentKind, AgentModel, AgentRequestKind, MockOneShotClient, OneShotError, OneShotRequest,
    OneShotSubmission, PermissionMode, ReasoningLevel, SessionStats, SpeedMode, diff_fence,
};
use ag_protocol::{AgentResponse, FocusedReview, FocusedReviewSeverity, FocusedReviewSuggestion};

use super::{file_headers, merge, submit};
use crate::app::diff_prompt::{MAX_PROVIDER_CALLS, PROMPT_BUDGET};

fn request() -> OneShotRequest {
    OneShotRequest {
        agent_kind: AgentKind::Claude,
        child_pid: None,
        folder: PathBuf::from("."),
        model: AgentModel::ClaudeSonnet5,
        permission_mode: PermissionMode::ReadOnly,
        prompt: String::new(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::Medium,
        request_kind: AgentRequestKind::FocusedReview,
        speed_mode: SpeedMode::Normal,
    }
}

fn response() -> OneShotSubmission {
    OneShotSubmission {
        response: AgentResponse::plain(
            r#"{"project_impact":["Original changes reviewed."],"suggestions":[{"severity":"high","details":"Preserved finding."}]}"#,
        ),
        stats: SessionStats::default(),
    }
}

fn render(diff: &str, context: &str) -> String {
    let fence = diff_fence(diff);
    format!("{context}\n{fence}diff\n{diff}\n{fence}")
}

#[tokio::test]
async fn batches_original_unicode_diff_then_checks_cross_file_interactions() {
    // Arrange
    let diff = format!(
        "diff --git a/source b/source\n{}\nTAIL",
        "+🦀\n".repeat(30_000)
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let prompts = Arc::clone(&seen);
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        assert_eq!(request.request_kind, AgentRequestKind::FocusedReview);
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(request.model, AgentModel::ClaudeSonnet5);
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        assert!(request.prompt.contains("Accepted decision"));
        prompts.lock().expect("prompts").push(request.prompt);
        Ok(response())
    });

    // Act
    let text = submit(
        &client,
        request(),
        &diff,
        "Accepted decision",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("review");

    // Assert
    let prompts = seen.lock().expect("prompts");
    assert!(prompts.len() > 2);
    assert_eq!(
        prompts
            .iter()
            .map(|prompt| prompt.matches('🦀').count())
            .sum::<usize>(),
        30_000
    );
    assert_eq!(
        prompts
            .iter()
            .filter(|prompt| prompt.contains("TAIL"))
            .count(),
        1
    );
    assert!(
        prompts
            .last()
            .expect("cross-file pass")
            .starts_with("Cross-file review:")
    );
    assert_eq!(text.matches("Preserved finding.").count(), 1);
    assert!(!text.contains("Partial review"));
}

#[tokio::test]
async fn splits_fencing_overhead_and_provider_size_rejections_without_summaries() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        if request.prompt.len() > 5_000 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        Ok(response())
    });

    // Act
    let text = submit(
        &client,
        request(),
        &"`".repeat(25_000),
        "",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(!text.contains("Partial review"));
}

#[tokio::test]
async fn failed_later_batch_retains_findings_and_identifies_unreviewed_files() {
    // Arrange
    let mut calls = 0;
    let mut client = MockOneShotClient::new();
    client.expect_submit().times(2).returning(move |_| {
        calls += 1;
        if calls == 2 {
            return Err(OneShotError::new("network timeout"));
        }
        Ok(response())
    });
    let diff = format!(
        "diff --git a/first b/first\n{}diff --git a/last b/last\n{}",
        "+first\n".repeat(8_000),
        "+last\n".repeat(8_000)
    );

    // Act
    let text = submit(&client, request(), &diff, "", |diff, context| {
        Ok(render(diff, context))
    })
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("Partial review: 1 batches completed"));
    assert!(text.contains("diff --git a/last b/last"));
    assert!(text.contains("network timeout"));
    assert!(text.contains("Press `f` to retry"));
    let suggestions = ag_session::review_suggestions(&text).expect("actionable finding");
    assert!(!suggestions.contains("Partial review"));
}

#[tokio::test]
async fn failed_cross_file_pass_preserves_all_batch_findings() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(|request| {
        if request.prompt.starts_with("Cross-file review:") {
            return Err(OneShotError::new("network timeout"));
        }
        Ok(response())
    });

    // Act
    let text = submit(
        &client,
        request(),
        &"x".repeat(90_000),
        "",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("cross-file pass failed: network timeout"));
}

#[tokio::test]
async fn exhausted_shared_budget_preserves_completed_batches() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client
        .expect_submit()
        .times(MAX_PROVIDER_CALLS)
        .returning(|request| {
            request
                .provider_call_budget
                .as_ref()
                .expect("budget")
                .consume()?;
            Ok(response())
        });

    // Act
    let text = submit(
        &client,
        request(),
        &"x".repeat(4_000_000),
        "",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Partial review: 64 batches completed"));
    assert!(text.contains("provider call limit reached"));
    assert!(text.contains("Preserved finding."));
}

#[tokio::test]
async fn errors_before_any_completed_review_propagate() {
    // Arrange
    for diagnostic in ["network timeout", "contextWindowExceeded"] {
        let mut client = MockOneShotClient::new();
        client
            .expect_submit()
            .once()
            .returning(move |_| Err(OneShotError::new(diagnostic)));

        // Act
        let error = submit(&client, request(), "tiny", "", |diff, context| {
            Ok(render(diff, context))
        })
        .await
        .expect_err("no review");

        // Assert
        assert_eq!(error.to_string(), diagnostic);
    }
}

#[tokio::test]
async fn render_and_invalid_response_errors_propagate() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_submit().once().returning(|_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain("invalid"),
            stats: SessionStats::default(),
        })
    });

    // Act
    let invalid = submit(&client, request(), "tiny", "", |diff, context| {
        Ok(render(diff, context))
    })
    .await
    .expect_err("invalid review");
    let render_error = submit(&client, request(), "tiny", "", |_, _| {
        Err(OneShotError::new("render"))
    })
    .await
    .expect_err("invalid template");

    // Assert
    assert!(invalid.to_string().contains("invalid structured output"));
    assert_eq!(render_error.to_string(), "render");
}

#[test]
fn merge_retains_unique_findings_with_high_severity_first() {
    // Arrange
    let high = FocusedReviewSuggestion {
        details: "High".to_string(),
        severity: FocusedReviewSeverity::High,
    };
    let medium = FocusedReviewSuggestion {
        details: "Medium".to_string(),
        severity: FocusedReviewSeverity::Medium,
    };
    let mut review = FocusedReview {
        project_impact: vec!["Impact".to_string()],
        suggestions: vec![medium.clone()],
    };

    // Act
    merge(
        &mut review,
        FocusedReview {
            project_impact: vec!["Impact".to_string(), "Another".to_string()],
            suggestions: vec![medium.clone(), high.clone()],
        },
    );

    // Assert
    assert_eq!(review.project_impact, ["Impact", "Another"]);
    assert_eq!(review.suggestions, [high, medium]);
}

#[test]
fn headers_keep_renames_and_deduplicate_continuations() {
    // Arrange
    let diff = "diff --git a/old b/new\n+hunk\ndiff --git a/old b/new\n+continued\ndiff --git \
                a/next b/next\n";

    // Act
    let headers = file_headers(diff);

    // Assert
    assert_eq!(headers, "diff --git a/old b/new\ndiff --git a/next b/next");
    assert!(file_headers("+continuation").contains("Continuation fragments"));
}

#[tokio::test]
async fn smaller_provider_limit_reduces_history_without_summarizing_diff() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_submit().times(3).returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            assert!(request.prompt.contains("Accepted decision"));
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("Accepted decision retained."),
                stats: SessionStats::default(),
            });
        }
        if request.prompt.len() > 2_000 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        assert!(request.prompt.contains("ORIGINAL DIFF"));
        Ok(OneShotSubmission {
            response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
            stats: SessionStats::default(),
        })
    });

    // Act
    let text = submit(
        &client,
        request(),
        "ORIGINAL DIFF",
        &"Accepted decision\n".repeat(200),
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("review");

    // Assert
    assert!(text.contains("session history was summarized"));
    assert!(ag_session::review_suggestions(&text).is_none());
}

#[tokio::test]
async fn cross_file_overview_is_bounded_without_discarding_batch_findings() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain(
                    "Large batch impact retained for cross-file inspection.",
                ),
                stats: SessionStats::default(),
            });
        }
        if request.prompt.starts_with("Cross-file review:") {
            assert!(request.prompt.contains("Large batch impact retained"));
            return Ok(response());
        }
        Ok(OneShotSubmission {
            response: AgentResponse::plain(
                serde_json::json!({
                    "project_impact": ["Large batch impact. ".repeat(1000)],
                    "suggestions": [{"severity": "medium", "details": "Original batch finding."}],
                })
                .to_string(),
            ),
            stats: SessionStats::default(),
        })
    });

    // Act
    let text = submit(
        &client,
        request(),
        &"x".repeat(90_000),
        "",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("review");

    // Assert
    assert!(text.contains("Original batch finding."));
    assert!(text.contains("Preserved finding."));
    assert_eq!(text.matches("Large batch impact.").count(), 1000);
}
