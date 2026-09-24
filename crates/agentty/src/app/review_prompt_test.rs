use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::{
    AgentRequestKind, OneShotError, OneShotRequest, OneShotSubmission, PermissionMode,
    ProviderCallBudget, ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::{
    AgentResponse, FocusedReview, FocusedReviewSeverity, FocusedReviewSuggestion, diff_fence,
};
use ag_session::{AgentKind, AgentModel};
use ag_worker::{MockRunClient, RunClient};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::{
    REVIEW_CONCURRENCY, cross_file_review, file_headers, merge, reduce_review, split_candidates,
    submit,
};
use crate::app::diff_prompt::{MAX_PROVIDER_CALLS, PROMPT_BUDGET};
use crate::app::review::ReviewProgress;
use crate::infra::review_deadline::ReviewDeadlineClient;

fn request() -> OneShotRequest {
    OneShotRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        harness: (AgentKind::Claude).to_string(),
        child_pid: None,
        folder: PathBuf::from("."),
        model: AgentModel::ClaudeSonnet5.as_str().to_string(),
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

/// Holds a provider call open until the test explicitly completes it.
struct PendingReview {
    request: OneShotRequest,
    reply: oneshot::Sender<Result<OneShotSubmission, OneShotError>>,
}

struct ControlledReviewClient {
    requests: mpsc::UnboundedSender<PendingReview>,
}

#[async_trait]
impl RunClient for ControlledReviewClient {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        let (reply, response) = oneshot::channel();
        self.requests
            .send(PendingReview { request, reply })
            .expect("test observer");

        response.await.expect("test completes every provider call")
    }
}

#[tokio::test]
async fn expired_review_deadline_does_not_admit_worker_submissions() {
    // Arrange
    let mut worker = MockRunClient::new();
    worker.expect_submit().never();
    let client = ReviewDeadlineClient::new(&worker, Duration::ZERO);

    // Act
    let error = client
        .submit(request())
        .await
        .expect_err("expired deadline");

    // Assert
    assert!(error.to_string().contains("deadline exceeded"));
}

#[tokio::test]
async fn deadline_keeps_finished_findings_and_reports_unreviewed_fragments() {
    // Arrange
    let (requests, mut pending) = mpsc::unbounded_channel();
    let client = ControlledReviewClient { requests };
    let client = ReviewDeadlineClient::new(&client, Duration::from_millis(250));
    let diff = "x".repeat(250_000);

    // Act
    let review = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, context| Ok(render(diff, context)),
        |_| {},
    );
    let observe = async {
        let first = pending.recv().await.expect("first batch");
        let second = pending.recv().await.expect("second batch");
        let third = pending.recv().await.expect("third batch");
        first.reply.send(Ok(response())).expect("first result");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(second.reply.is_closed());
        assert!(third.reply.is_closed());
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(review, observe)
    })
    .await
    .expect("review finishes within the total deadline");

    // Assert
    let text = result.expect("partial review");
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("Partial review: 1 batches completed; 5 fragments remain unreviewed"));
    assert!(text.contains("deadline exceeded"));
    assert!(pending.try_recv().is_err());
}

#[tokio::test]
async fn reviews_three_batches_concurrently_and_merges_in_input_order() {
    // Arrange
    let (requests, mut pending) = mpsc::unbounded_channel();
    let client = ControlledReviewClient { requests };
    let diff = "x".repeat(250_000);

    // Act
    let review = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, context| Ok(render(diff, context)),
        |_| {},
    );
    let observe = async {
        for wave in 0..2 {
            let mut calls = Vec::new();
            for _ in 0..REVIEW_CONCURRENCY {
                calls.push(pending.recv().await.expect("concurrent batch"));
            }
            assert!(pending.try_recv().is_err(), "concurrency is bounded");
            for (index, call) in calls.into_iter().enumerate().rev() {
                assert!(!call.request.prompt.starts_with("Cross-file review:"));
                call.reply.send(Ok(OneShotSubmission {
                    response: AgentResponse::plain(serde_json::json!({
                        "project_impact": [],
                        "suggestions": [{"severity": "high", "details": format!("Finding {}.", wave * REVIEW_CONCURRENCY + index)}],
                    }).to_string()),
                    stats: SessionStats::default(),
                })).expect("batch remains active");
                if index > 0 {
                    tokio::task::yield_now().await;
                    assert!(pending.try_recv().is_err(), "wait for the entire wave");
                }
            }
        }
        let cross_file = pending.recv().await.expect("cross-file pass");
        assert!(cross_file.request.prompt.starts_with("Cross-file review:"));
        for index in 0..6 {
            assert!(
                cross_file
                    .request
                    .prompt
                    .contains(&format!("Finding {index}."))
            );
        }
        cross_file
            .reply
            .send(Ok(response()))
            .expect("cross-file reply");
        let reduction = pending.recv().await.expect("final reduction");
        assert!(reduction.request.prompt.starts_with("Reduce review:"));
        let positions: Vec<_> = (0..6)
            .map(|index| {
                reduction
                    .request
                    .prompt
                    .find(&format!("Finding {index}."))
                    .expect("all batch findings reach reduction")
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(reduction.request.prompt.contains("Preserved finding."));
        reduction
            .reply
            .send(Ok(OneShotSubmission {
                response: AgentResponse::plain(
                    serde_json::json!({
                        "project_impact": [],
                        "suggestions": (0..6).map(|index| serde_json::json!({
                            "severity": "high", "details": format!("Finding {index}.")
                        })).collect::<Vec<_>>()
                    })
                    .to_string(),
                ),
                stats: SessionStats::default(),
            }))
            .expect("reduction reply");
    };
    let (text, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(review, observe)
    })
    .await
    .expect("concurrent calls must not deadlock");

    // Assert
    let text = text.expect("complete review");
    let positions: Vec<_> = (0..6)
        .map(|index| {
            text.find(&format!("Finding {index}."))
                .expect("finding preserved")
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(!text.contains("Partial review"));
    assert!(pending.try_recv().is_err());
}

#[tokio::test]
async fn failed_batch_drains_running_peers_and_stops_queued_work() {
    // Arrange
    let (requests, mut pending) = mpsc::unbounded_channel();
    let client = ControlledReviewClient { requests };
    let diff = "x".repeat(250_000);

    // Act
    let review = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, context| Ok(render(diff, context)),
        |_| {},
    );
    let observe = async {
        let first = pending.recv().await.expect("first batch");
        let second = pending.recv().await.expect("second batch");
        let third = pending.recv().await.expect("third batch");
        first
            .reply
            .send(Err(OneShotError::new("network timeout")))
            .expect("failure reply");
        tokio::task::yield_now().await;
        assert!(!second.reply.is_closed(), "failure must not cancel peers");
        second.reply.send(Ok(response())).expect("second reply");
        third.reply.send(Ok(response())).expect("third reply");
    };
    let (text, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(review, observe)
    })
    .await
    .expect("running peers must finish");

    // Assert
    let text = text.expect("partial review");
    assert!(text.contains("Partial review: 2 batches completed; 4 fragments remain unreviewed"));
    assert!(text.contains("network timeout"));
    assert!(text.contains("Preserved finding."));
    assert!(
        pending.try_recv().is_err(),
        "queued batches and cross-file pass must not start"
    );
}

#[tokio::test]
async fn nested_size_retries_keep_source_order_in_complete_and_partial_reviews() {
    for fail_last_retry in [false, true] {
        // Arrange
        let diff = ['a', 'b', 'c', 'd', 'e', 'f']
            .into_iter()
            .map(|marker| {
                marker
                    .to_string()
                    .repeat(if marker < 'e' { 12_000 } else { 48_000 })
            })
            .collect::<String>();
        let mut client = MockRunClient::new();
        client.expect_submit().returning(move |request| {
            request.provider_call_budget.as_ref().expect("budget").consume()?;
            if request.prompt.starts_with("Reduce review:") {
                return Ok(OneShotSubmission {
                    response: AgentResponse::plain(serde_json::json!({
                        "project_impact": [],
                        "suggestions": (['a', 'b', 'c', 'd', 'e', 'f'].map(|marker| serde_json::json!({
                            "severity": "medium", "details": format!("Finding {marker}.")
                        })))
                    }).to_string()),
                    stats: SessionStats::default(),
                });
            }
            if request.prompt.starts_with("Cross-file review:") {
                let positions: Vec<_> = ['a', 'b', 'c', 'd', 'e', 'f']
                    .map(|marker| request.prompt.find(&format!("Finding {marker}."))
                        .expect("cross-file overview retains every finding"))
                    .into_iter().collect();
                assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
                return Ok(OneShotSubmission {
                    response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
                    stats: SessionStats::default(),
                });
            }
            let marker = request.prompt.chars().next().expect("source fragment");
            if marker < 'e' && request.prompt.len() > 12_000 {
                return Err(OneShotError::new("contextWindowExceeded"));
            }
            if fail_last_retry && marker == 'd' {
                return Err(OneShotError::new("provider unavailable"));
            }
            Ok(OneShotSubmission {
                response: AgentResponse::plain(serde_json::json!({
                    "project_impact": [],
                    "suggestions": [{"severity": "medium", "details": format!("Finding {marker}.")}],
                }).to_string()),
                stats: SessionStats::default(),
            })
        });

        // Act
        let text = submit(
            &client,
            request(),
            &diff,
            "",
            |diff, _| Ok(diff.into()),
            |_| {},
        )
        .await
        .expect("retain completed findings");

        // Assert
        let positions: Vec<_> = ['a', 'b', 'c', 'd', 'e', 'f']
            .into_iter()
            .filter(|marker| !fail_last_retry || *marker != 'd')
            .map(|marker| {
                text.find(&format!("Finding {marker}."))
                    .expect("finding preserved")
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(text.contains("Partial review"), fail_last_retry);
    }
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
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        assert_eq!(request.request_kind, AgentRequestKind::FocusedReview);
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(request.model, AgentModel::ClaudeSonnet5.as_str());
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
        |_| {},
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
            .expect("final reduction")
            .starts_with("Reduce review:")
    );
    assert_eq!(text.matches("Preserved finding.").count(), 1);
    assert!(!text.contains("Partial review"));
}

#[tokio::test]
async fn splits_fencing_overhead_and_provider_size_rejections_without_summaries() {
    // Arrange
    let mut client = MockRunClient::new();
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
        |_| {},
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
    let mut client = MockRunClient::new();
    client.expect_submit().times(3).returning(move |_| {
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
    let text = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, context| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("Partial review: 2 batches completed"));
    assert!(text.contains("diff --git a/last b/last"));
    assert!(text.contains("network timeout"));
    assert!(text.contains("Press `f` to retry"));
    let suggestions = ag_session::review_suggestions(&text).expect("actionable finding");
    assert!(!suggestions.contains("Partial review"));
}

#[tokio::test]
async fn failed_cross_file_pass_preserves_all_batch_findings() {
    // Arrange
    let mut client = MockRunClient::new();
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
        |_| {},
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("cross-file pass failed: network timeout"));
}

#[tokio::test]
async fn final_reduction_replaces_candidates_and_normalizes_the_complete_review() {
    // Arrange
    for no_supported_findings in [false, true] {
        let mut client = MockRunClient::new();
        client.expect_submit().returning(move |request| {
            request.provider_call_budget.as_ref().expect("shared budget").consume()?;
            if request.prompt.starts_with("Reduce review:") {
                assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
                assert_eq!(request.request_kind, AgentRequestKind::FocusedReview);
                assert_eq!(request.reasoning_level, ReasoningLevel::Medium);
                assert_eq!(request.model, AgentModel::ClaudeSonnet5.as_str());
                assert!(request.prompt.contains("Accepted decision"));
                assert!(request.prompt.contains("diff --git a/source b/source"));
                assert!(request.prompt.contains("Preserved finding."));
                assert!(request.prompt.contains("Cross-file candidate."));
                let suggestions = if no_supported_findings {
                    serde_json::json!([])
                } else {
                    serde_json::json!([
                        {"severity": "medium", "details": "Reassessed risk."},
                        {"severity": "high", "details": "Consolidated risk."},
                        {"severity": "high", "details": "Consolidated risk."}
                    ])
                };
                return Ok(OneShotSubmission {
                    response: AgentResponse::plain(serde_json::json!({
                        "project_impact": ["Coherent impact."],
                        "suggestions": suggestions
                    }).to_string()),
                    stats: SessionStats::default(),
                });
            }
            if request.prompt.starts_with("Cross-file review:") {
                return Ok(OneShotSubmission {
                    response: AgentResponse::plain(
                        r#"{"project_impact":[],"suggestions":[{"severity":"medium","details":"Cross-file candidate."}]}"#,
                    ),
                    stats: SessionStats::default(),
                });
            }
            Ok(response())
        });
        let progress = Mutex::new(Vec::new());

        // Act
        let text = submit(
            &client,
            request(),
            &format!("diff --git a/source b/source\n{}", "x".repeat(90_000)),
            "Accepted decision",
            |diff, context| Ok(render(diff, context)),
            |update| progress.lock().expect("progress").push(update),
        )
        .await
        .expect("consolidated review");

        // Assert
        assert!(text.contains("Coherent impact."));
        assert!(!text.contains("Original changes reviewed."));
        assert!(!text.contains("Preserved finding."));
        assert!(!text.contains("Cross-file candidate."));
        assert!(!text.contains("Partial review"));
        if no_supported_findings {
            assert!(ag_session::review_suggestions(&text).is_none());
        } else {
            assert_eq!(text.matches("Consolidated risk.").count(), 1);
            assert!(
                text.find("Consolidated risk.").expect("high finding")
                    < text.find("Reassessed risk.").expect("medium finding")
            );
        }
        assert!(
            progress
                .lock()
                .expect("progress")
                .ends_with(&[ReviewProgress::CrossFile, ReviewProgress::Reducing])
        );
    }
}

#[tokio::test]
async fn failed_or_invalid_reduction_keeps_candidates_with_coverage_notice() {
    // Arrange
    for failure in ["network timeout", "contextWindowExceeded", "invalid", ""] {
        let mut client = MockRunClient::new();
        client.expect_submit().returning(move |request| {
            if request.prompt.starts_with("Reduce review:") {
                if failure == "invalid" || failure.is_empty() {
                    return Ok(OneShotSubmission {
                        response: AgentResponse::plain(failure),
                        stats: SessionStats::default(),
                    });
                }
                return Err(OneShotError::new(failure));
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
            |_| {},
        )
        .await
        .expect("fallback review");

        // Assert
        assert!(text.contains("Preserved finding."));
        assert!(text.contains("final consolidation failed:"));
        assert!(text.contains("Retained findings have not been reconciled."));
        assert!(text.contains("Press `f` to retry"));
        let suggestions = ag_session::review_suggestions(&text).expect("actionable fallback");
        assert!(!suggestions.contains("Partial review"));
    }
}

#[tokio::test]
async fn final_reduction_uses_the_existing_review_deadline() {
    // Arrange
    let (requests, mut pending) = mpsc::unbounded_channel();
    let client = ControlledReviewClient { requests };
    let client = ReviewDeadlineClient::new(&client, Duration::from_millis(250));
    let diff = "x".repeat(90_000);

    // Act
    let review = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, context| Ok(render(diff, context)),
        |_| {},
    );
    let observe = async {
        for _ in 0..3 {
            pending
                .recv()
                .await
                .expect("batch or cross-file pass")
                .reply
                .send(Ok(response()))
                .expect("completed candidate");
        }
        let reduction = pending.recv().await.expect("reduction");
        assert!(reduction.request.prompt.starts_with("Reduce review:"));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(reduction.reply.is_closed());
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(review, observe)
    })
    .await
    .expect("deadline completes the review");

    // Assert
    let text = result.expect("fallback review");
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("final consolidation failed:"));
    assert!(text.contains("deadline exceeded"));
    assert!(pending.try_recv().is_err());
}

#[tokio::test]
async fn reduction_respects_remaining_budget_and_never_summarizes_oversized_candidates() {
    // Arrange
    for oversized in [false, true] {
        let mut client = MockRunClient::new();
        client.expect_submit().returning(move |request| {
            let budget = request.provider_call_budget.as_ref().expect("budget");
            budget.consume()?;
            assert!(!request.prompt.starts_with("Reduce review:"));
            if request.request_kind == AgentRequestKind::UtilityPrompt {
                return Ok(OneShotSubmission {
                    response: AgentResponse::plain("Bounded cross-file overview."),
                    stats: SessionStats::default(),
                });
            }
            if request.prompt.starts_with("Cross-file review:") {
                if !oversized {
                    while budget.consume().is_ok() {}
                }
                return Ok(response());
            }
            Ok(OneShotSubmission {
                response: AgentResponse::plain(serde_json::json!({
                    "project_impact": [if oversized { "Impact. ".repeat(20_000) } else { "Impact.".into() }],
                    "suggestions": [{"severity": "medium", "details": "Original batch finding."}]
                }).to_string()),
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
            |_| {},
        )
        .await
        .expect("fallback review");

        // Assert
        assert!(text.contains("Original batch finding."));
        assert!(text.contains("Preserved finding."));
        assert!(text.contains("final consolidation failed:"));
        if oversized {
            assert_eq!(text.matches("Impact.").count(), 20_000);
            assert!(text.contains("review prompt budget"));
        } else {
            assert!(text.contains("provider call limit reached"));
        }
    }
}

#[tokio::test]
async fn reduction_render_failure_does_not_submit_a_worker_request() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    let review = FocusedReview {
        project_impact: Vec::new(),
        suggestions: Vec::new(),
    };

    // Act
    let error = reduce_review(
        &client,
        &request(),
        &review,
        "original diff",
        &mut String::new(),
        &|_, _| Err(OneShotError::new("render failed")),
        &ProviderCallBudget::new(1),
    )
    .await
    .expect_err("render error");

    // Assert
    assert_eq!(error.to_string(), "render failed");
}

#[tokio::test]
async fn exhausted_shared_budget_preserves_completed_batches() {
    // Arrange
    let mut client = MockRunClient::new();
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
        |_| {},
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
        let mut client = MockRunClient::new();
        client
            .expect_submit()
            .once()
            .returning(move |_| Err(OneShotError::new(diagnostic)));

        // Act
        let error = submit(
            &client,
            request(),
            "tiny",
            "",
            |diff, context| Ok(render(diff, context)),
            |_| {},
        )
        .await
        .expect_err("no review");

        // Assert
        assert_eq!(error.to_string(), diagnostic);
    }
}

#[tokio::test]
async fn render_and_invalid_response_errors_propagate() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().once().returning(|_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain("invalid"),
            stats: SessionStats::default(),
        })
    });

    // Act
    let invalid = submit(
        &client,
        request(),
        "tiny",
        "",
        |diff, context| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect_err("invalid review");
    let render_error = submit(
        &client,
        request(),
        "tiny",
        "",
        |_, _| Err(OneShotError::new("render")),
        |_| {},
    )
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
    let mut client = MockRunClient::new();
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
        |_| {},
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
    let mut client = MockRunClient::new();
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
        if request.prompt.starts_with("Reduce review:") {
            assert_eq!(request.prompt.matches("Large batch impact.").count(), 1000);
            assert!(request.prompt.contains("Original batch finding."));
            assert!(request.prompt.contains("Preserved finding."));
            return Err(OneShotError::new("reduction unavailable"));
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
        |_| {},
    )
    .await
    .expect("review");

    // Assert
    assert!(text.contains("Original batch finding."));
    assert!(text.contains("Preserved finding."));
    assert_eq!(text.matches("Large batch impact.").count(), 1000);
    assert!(text.contains("final consolidation failed: reduction unavailable"));
}

#[tokio::test]
async fn large_history_reports_preparation_and_keeps_review_reasoning() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            assert_eq!(request.reasoning_level, ReasoningLevel::Low);
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("Accepted decisions retained."),
                stats: SessionStats::default(),
            });
        }
        assert_eq!(request.reasoning_level, ReasoningLevel::Medium);
        Ok(response())
    });
    let progress = Mutex::new(Vec::new());
    // Act
    let result = submit(
        &client,
        request(),
        "original changes",
        &"history ".repeat(10_000),
        |diff, context| Ok(render(diff, context)),
        |update| progress.lock().expect("progress").push(update),
    )
    .await
    .expect("review");
    // Assert
    let progress = progress.lock().expect("progress");
    assert_eq!(progress.first(), Some(&ReviewProgress::SummarizingHistory));
    assert_eq!(
        progress.last(),
        Some(&ReviewProgress::Batches {
            completed: 1,
            total: 1
        })
    );
    assert!(result.contains("Preserved finding."));
    assert!(result.contains("session history was summarized"));
}

#[tokio::test]
async fn cross_file_retries_smaller_overviews_after_provider_size_rejection() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("Short evidence"),
                stats: SessionStats::default(),
            });
        }
        if request.prompt.len() > 2000 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        Ok(response())
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let review = FocusedReview {
        project_impact: vec!["impact ".repeat(1000)],
        suggestions: Vec::new(),
    };
    let mut context = "Accepted decision ".repeat(100);
    // Act
    let result = cross_file_review(
        &client,
        &request,
        &review,
        "diff",
        &mut context,
        &|diff, context| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect("adaptive cross-file review");
    // Assert
    assert_eq!(result.suggestions[0].details, "Preserved finding.");
    assert!(context.contains("[Summarized input;"));
}

#[tokio::test]
async fn hierarchical_reduction_reconciles_all_whole_candidates() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        if request.prompt.len() > 4000 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        let suggestions: Vec<_> = (0..4)
            .filter(|id| request.prompt.contains(&format!("risk-{id}")))
            .map(|id| serde_json::json!({"severity":"high","details": format!("risk-{id}")}))
            .collect();
        Ok(OneShotSubmission {
            response: AgentResponse::plain(
                serde_json::json!({"project_impact":[],"suggestions": suggestions}).to_string(),
            ),
            stats: SessionStats::default(),
        })
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let review = FocusedReview {
        project_impact: Vec::new(),
        suggestions: (0..4)
            .map(|id| FocusedReviewSuggestion {
                severity: FocusedReviewSeverity::High,
                details: format!("risk-{id} {}", "evidence ".repeat(180)),
            })
            .collect(),
    };
    // Act
    let result = reduce_review(
        &client,
        &request,
        &review,
        "diff",
        &mut String::new(),
        &|diff, context| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect("hierarchical review");
    // Assert
    assert_eq!(result.suggestions.len(), 4);
    for (id, suggestion) in result.suggestions.iter().enumerate() {
        assert_eq!(suggestion.details, format!("risk-{id}"));
    }
}

#[test]
fn candidate_splits_preserve_both_impact_and_finding_boundaries() {
    // Arrange
    for impact_count in [0, 1, 5] {
        let review = FocusedReview {
            project_impact: (0..impact_count).map(|id| format!("impact-{id}")).collect(),
            suggestions: vec![
                FocusedReviewSuggestion {
                    severity: FocusedReviewSeverity::High,
                    details: "risk".into()
                };
                3
            ],
        };
        // Act
        let (mut first, second) = split_candidates(review.clone());
        first.project_impact.extend(second.project_impact);
        first.suggestions.extend(second.suggestions);
        // Assert
        assert_eq!(first, review);
    }
}

#[tokio::test]
async fn irreducible_final_passes_stop_without_discarding_distinct_evidence() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        request.provider_call_budget.as_ref().expect("budget").consume()?;
        if request.prompt.len() > 4000 || request.prompt.starts_with("Cross-file review:") {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        let suggestions: Vec<_> = (0..4).filter(|id| request.prompt.contains(&format!("risk-{id}")))
            .map(|id| serde_json::json!({"severity":"high","details": format!("risk-{id} {}", "evidence ".repeat(180))})).collect();
        Ok(OneShotSubmission { response: AgentResponse::plain(serde_json::json!({"project_impact":[],"suggestions":suggestions}).to_string()), stats: SessionStats::default() })
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let review = FocusedReview {
        project_impact: Vec::new(),
        suggestions: (0..4)
            .map(|id| FocusedReviewSuggestion {
                severity: FocusedReviewSeverity::High,
                details: format!("risk-{id} {}", "evidence ".repeat(180)),
            })
            .collect(),
    };
    // Act
    let reduction = reduce_review(
        &client,
        &request,
        &review,
        "diff",
        &mut String::new(),
        &|diff, context| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect_err("no information loss");
    let cross_file = cross_file_review(
        &client,
        &request,
        &FocusedReview {
            project_impact: Vec::new(),
            suggestions: Vec::new(),
        },
        "diff",
        &mut String::new(),
        &|diff, context| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect_err("minimum input still rejected");
    // Assert
    assert!(
        reduction
            .to_string()
            .contains("without discarding evidence")
    );
    assert!(cross_file.to_string().contains("contextWindowExceeded"));
}

#[tokio::test]
async fn reduction_adapts_shared_context_and_headers_to_provider_limits() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("Short evidence"),
                stats: SessionStats::default(),
            });
        }
        if request.prompt.len() > 1800 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        Ok(response())
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let review = FocusedReview {
        project_impact: vec!["Impact".into()],
        suggestions: Vec::new(),
    };
    let diff = format!("diff --git a/{} b/file", "long-name".repeat(250));
    let mut context = "Accepted decision ".repeat(180);
    // Act
    let result = reduce_review(
        &client,
        &request,
        &review,
        &diff,
        &mut context,
        &|diff, context| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect("adaptive reduction");
    // Assert
    assert_eq!(result.suggestions[0].details, "Preserved finding.");
    assert!(context.contains("[Summarized input;"));
}
