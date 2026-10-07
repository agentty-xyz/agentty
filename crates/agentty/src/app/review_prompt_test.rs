use std::fmt::Write as _;
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
use ag_session::{AgentKind, AgentModel, FocusedReviewStatus};
use ag_store::Database;
use ag_worker::{MockRunClient, RunClient};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::{
    MAX_REVIEW_PROVIDER_CALLS, REVIEW_CONCURRENCY, cross_file_review, file_headers, merge,
    reduce_review, split_candidates, submit,
};
use crate::app::diff_prompt::PROMPT_BUDGET;
use crate::app::review::ReviewProgress;
use crate::app::review_resume::ReviewResumeClient;
use crate::infra::review_deadline::ReviewDeadlineClient;

fn request() -> OneShotRequest {
    OneShotRequest {
        activity_tx: None,
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

fn accounted(request: &OneShotRequest, mut submission: OneShotSubmission) -> OneShotSubmission {
    if let Some(count) = super::reduction_candidate_count(&request.prompt) {
        let mut review: FocusedReview =
            serde_json::from_str(&submission.response.answer).expect("fixture review");
        review.candidate_decisions = (0..count)
            .map(|candidate_index| ag_protocol::FocusedReviewDecision {
                candidate_index,
                reason: "Fixture source verifies the disposition".into(),
                suggestion_index: (!review.suggestions.is_empty())
                    .then_some(candidate_index.min(review.suggestions.len().saturating_sub(1))),
            })
            .collect();
        submission.response.answer = serde_json::to_string(&review).expect("fixture response");
    }

    submission
}

fn render(diff: &str, context: &str) -> String {
    let fence = diff_fence(diff);
    format!("{context}\n{fence}diff\n{diff}\n{fence}")
}

#[tokio::test]
async fn unaudited_reduction_cannot_silently_remove_a_discovered_risk() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().times(3).returning(|request| {
        if request.prompt.starts_with("Reduce review:") {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
                stats: SessionStats::default(),
            });
        }
        Ok(response())
    });

    // Act
    let text = submit(
        &client,
        request(),
        "diff --git a/x.rs b/x.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "",
        |diff, history, _| Ok(render(diff, history)),
        |_| {},
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("Consolidation must account for all 1 candidates"));
    assert!(text.contains("Retained findings have not been reconciled"));
}

#[tokio::test]
async fn omitted_suggestion_index_preserves_the_discovered_candidates() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().times(3).returning(|request| {
        if request.prompt.starts_with("Reduce review:") {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[],"candidate_decisions":[{"candidate_index":0,"reason":"Verified source"}]}"#),
                stats: SessionStats::default(),
            });
        }
        Ok(response())
    });

    // Act
    let text = submit(
        &client,
        request(),
        "diff --git a/x.rs b/x.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "",
        |diff, history, _| Ok(render(diff, history)),
        |_| {},
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("suggestion_index"));
    assert!(text.contains("Retained findings have not been reconciled"));
}

#[tokio::test]
async fn unlinked_consolidation_output_preserves_the_discovered_candidates() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().times(3).returning(|request| {
        if request.prompt.starts_with("Reduce review:") {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[{"details":"Unlinked replacement","severity":"high"}],"candidate_decisions":[{"candidate_index":0,"suggestion_index":null,"reason":"Rejected input"}]}"#),
                stats: SessionStats::default(),
            });
        }
        Ok(response())
    });

    // Act
    let text = submit(
        &client,
        request(),
        "diff --git a/x.rs b/x.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "",
        |diff, history, _| Ok(render(diff, history)),
        |_| {},
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(!text.contains("Unlinked replacement"));
    assert!(text.contains("Consolidation must link every output finding"));
    assert_eq!(
        FocusedReviewStatus::for_text(&text),
        FocusedReviewStatus::Partial
    );
}

#[tokio::test]
async fn forty_source_fragments_complete_both_passes_and_consolidation() {
    // Arrange
    let mut diff = String::new();
    for index in 0..40 {
        write!(
            diff,
            "diff --git a/file-{index}.rs b/file-{index}.rs\n--- a/file-{index}.rs\n+++ \
             b/file-{index}.rs\n@@ -0,0 +1,5000 @@\n{}",
            "+change\n".repeat(5000)
        )
        .expect("fixture diff");
    }
    let calls = Arc::new(Mutex::new([0_usize; 4]));
    let observed = calls.clone();
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        let phase = if request.request_kind == AgentRequestKind::UtilityPrompt {
            0
        } else if request.prompt.starts_with("Cross-file review:") {
            2
        } else if request.prompt.starts_with("Reduce review:") {
            3
        } else {
            1
        };
        observed.lock().expect("calls")[phase] += 1;
        if phase == 0 {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("All changed modules affect the same workflow."),
                stats: SessionStats::default(),
            });
        }
        Ok(accounted(&request, response()))
    });

    // Act
    let text = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, history, _| Ok(render(diff, history)),
        |_| {},
    )
    .await
    .expect("complete review");

    // Assert
    let calls = calls.lock().expect("calls");
    assert_eq!(calls[1], 40);
    assert_eq!(calls[2], 40);
    assert_eq!(calls[3], 1);
    assert!(calls.iter().sum::<usize>() <= MAX_REVIEW_PROVIDER_CALLS);
    assert_eq!(
        FocusedReviewStatus::for_text(&text),
        FocusedReviewStatus::Ready
    );
    assert!(text.contains("Processed files: 40/40"));
    assert!(text.contains("Shared cross-file context was condensed"));
}

#[tokio::test]
async fn boundary_fragments_share_changes_that_only_interact_across_files() {
    // Arrange
    let diff = ["CALLER_CHANGED", "CALLEE_CHANGED"]
        .map(|name| {
            format!(
                "diff --git a/{name}.rs b/{name}.rs\n--- a/{name}.rs\n+++ b/{name}.rs\n@@ -0,0 \
                 +1,5001 @@\n+{name}();\n{}",
                "+change\n".repeat(5000)
            )
        })
        .join("");
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        request.provider_call_budget.as_ref().expect("shared budget").consume()?;
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            return Ok(OneShotSubmission { response: AgentResponse::plain("CALLER_CHANGED.rs calls CALLEE_CHANGED.rs; their changed contracts must agree."), stats: SessionStats::default() });
        }
        if request.prompt.starts_with("Cross-file review:") || request.prompt.starts_with("Reduce review:") {
            assert!(request.prompt.contains("CALLER_CHANGED"));
            assert!(request.prompt.contains("CALLEE_CHANGED"));
            return Ok(accounted(&request, OneShotSubmission {
                response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[{"severity":"high","details":"CALLER_CHANGED and CALLEE_CHANGED disagree on the shared contract"}]}"#),
                stats: SessionStats::default(),
            }));
        }
        Ok(OneShotSubmission { response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#), stats: SessionStats::default() })
    });

    // Act
    let text = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, history, _| Ok(render(diff, history)),
        |_| {},
    )
    .await
    .expect("cross-file review");

    // Assert
    assert!(text.contains("CALLER_CHANGED and CALLEE_CHANGED disagree"));
    assert!(text.contains("Processed files: 2/2"));
    assert_eq!(
        FocusedReviewStatus::for_text(&text),
        FocusedReviewStatus::Ready
    );
}

#[tokio::test]
async fn partial_boundary_pass_keeps_both_original_and_new_findings() {
    // Arrange
    let database = completed_boundary_review_database()
        .await
        .expect("completed review");
    let old_audit = database
        .sessions()
        .load_completed_review_audit("session")
        .await
        .expect("old audit");
    assert_eq!(old_audit.len(), 1);
    let diff = ["first", "later"]
        .map(|name| {
            format!(
                "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -0,0 +1,5000 @@\n{}",
                "+change\n".repeat(5000)
            )
        })
        .join("");
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        assert!(!request.prompt.starts_with("Reduce review:"));
        if request.prompt.starts_with("Cross-file review:") {
            if request.prompt.contains("diff --git a/later") {
                return Err(OneShotError::new("boundary unavailable"));
            }
            return Ok(OneShotSubmission { response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[{"severity":"high","details":"New boundary finding"}]}"#), stats: SessionStats::default() });
        }
        Ok(response())
    });
    let request_id = uuid::Uuid::new_v4();
    let client = ReviewResumeClient::new(Arc::new(client), &database, "session", 1, "", request_id)
        .await
        .expect("replacement review");

    // Act
    let text = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, history, _| Ok(render(diff, history)),
        |_| {},
    )
    .await
    .expect("retained boundary results");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("New boundary finding"));
    assert_eq!(
        FocusedReviewStatus::for_text(&text),
        FocusedReviewStatus::Partial
    );
    assert!(text.contains("Partial review: independent boundary pass;"));
    assert!(text.contains("boundary unavailable"));
    assert!(text.contains("Processed files: 1/2. Unfinished files: 1."));

    // Act: persist the generated status through the production boundary.
    assert!(
        database
            .sessions()
            .update_session_focused_review_for_generation(
                "session",
                &request_id.to_string(),
                FocusedReviewStatus::for_text(&text),
                Some("1".into()),
                Some(text),
            )
            .await
            .expect("partial persistence")
    );

    // Assert: successful discovery calls remain resumable and the previous
    // completed audit is untouched.
    let checkpoints: Vec<(String, String)> = sqlx::query_as(
        "SELECT request, answer FROM session_review_fragment WHERE session_id = 'session'",
    )
    .fetch_all(database.pool())
    .await
    .expect("retained checkpoints");
    assert!(
        checkpoints
            .iter()
            .any(|(_, answer)| answer.contains("Preserved finding."))
    );
    assert!(
        checkpoints
            .iter()
            .any(|(request, answer)| request.contains("Cross-file review:")
                && answer.contains("New boundary finding"))
    );
    assert_eq!(
        database
            .sessions()
            .load_completed_review_audit("session")
            .await
            .expect("retained audit"),
        old_audit
    );
}

/// Seeds an older completed audit before a replacement review starts.
async fn completed_boundary_review_database() -> Result<Database, ag_store::DbError> {
    let database = Database::open_in_memory().await?;
    let project = database.projects().upsert_project("project", None).await?;
    let sessions = database.sessions();
    sessions
        .insert_session("session", "model", "main", "Review", project)
        .await?;
    sessions
        .begin_review_generation("session", "old-inputs", "old")
        .await?;
    sessions
        .save_review_fragment("session", "old-inputs", "old", "discovery", "old candidate")
        .await?;
    assert!(
        sessions
            .update_session_focused_review_for_generation(
                "session",
                "old",
                FocusedReviewStatus::Ready,
                Some("0".into()),
                Some("Old review".into()),
            )
            .await?
    );

    Ok(database)
}

#[test]
fn reduction_metadata_is_scoped_to_host_generated_prompts() {
    // Arrange / Act / Assert
    assert_eq!(
        super::reduction_candidate_count("Reduce review: 2 candidates.\nVerify"),
        Some(2)
    );
    for prompt in [
        "Review source",
        "Reduce review: two candidates.\nVerify",
        "Reduce review: 2 candidates.",
    ] {
        assert_eq!(super::reduction_candidate_count(prompt), None);
    }
}

#[tokio::test]
async fn single_batch_verifies_candidates_and_repairs_captured_source_citations() {
    // Arrange
    let diff = "diff --git a/src/x.rs b/src/x.rs\n--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1 +1 \
                @@\n-old();\n+run();\n";
    let supported = serde_json::json!({
        "details": "src/x.rs:999:3: Supported risk", "severity": "high",
        "evidence": {
            "correction": "Validate input", "end_line": 999,
            "existing_code": "run();", "impact": "Invalid input executes",
            "path": "src/x.rs", "side": "new", "start_line": 999,
            "trigger": "An invalid request"
        }
    });
    let mut calls = 0;
    let mut client = MockRunClient::new();
    client.expect_submit().times(3).returning(move |request| {
        calls += 1;
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        let mut suggestions = vec![supported.clone()];
        if calls == 1 {
            assert!(!request.prompt.starts_with("Reduce review:"));
            suggestions
                .push(serde_json::json!({"details":"Unsupported candidate", "severity":"medium"}));
        } else if calls == 2 {
            assert!(request.prompt.starts_with("Cross-file review:"));
            assert!(request.prompt.contains("+run();"));
            suggestions.clear();
        } else {
            assert!(request.prompt.starts_with("Reduce review:"));
            assert!(request.prompt.contains("Independently verify every"));
            assert!(request.prompt.contains("Unsupported candidate"));
            assert!(request.prompt.contains("\"existing_code\":\"run();\""));
        }
        Ok(accounted(
            &request,
            OneShotSubmission {
                response: AgentResponse::plain(
                    serde_json::json!({"project_impact":[], "suggestions":suggestions}).to_string(),
                ),
                stats: SessionStats::default(),
            },
        ))
    });

    // Act
    let text = submit(
        &client,
        request(),
        diff,
        "",
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect("verified review");

    // Assert
    assert!(text.contains("`src/x.rs:1`"));
    assert!(text.contains("Supported risk"));
    assert!(!text.contains("Unsupported candidate"));
    assert!(!text.contains("999"));
    assert!(text.contains("src/x.rs:1: Supported risk"));
    assert!(!text.contains("Partial review"));
    assert!(text.contains("Processed files: 1/1. Unfinished files: 0. Unanchored findings: 0."));
    assert!(
        ag_session::review_suggestions(&text)
            .expect("apply contract")
            .contains("Correction: Validate input")
    );
}

#[tokio::test]
async fn single_batch_verification_failure_preserves_candidates_as_partial() {
    // Arrange
    let mut calls = 0;
    let mut client = MockRunClient::new();
    client.expect_submit().times(3).returning(move |request| {
        calls += 1;
        if calls == 3 {
            return Err(OneShotError::new("verification unavailable"));
        }
        Ok(accounted(&request, response()))
    });
    let diff = "diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+new\n";

    // Act
    let text = submit(
        &client,
        request(),
        diff,
        "",
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect("retained candidates");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(text.contains("Partial review: all 1 batches completed"));
    assert!(text.contains("verification unavailable"));
    assert!(text.contains("Processed files: 1/1. Unfinished files: 0. Unanchored findings: 1."));
}

#[tokio::test]
async fn coverage_includes_spaced_metadata_paths_and_unresolved_identities() {
    // Arrange
    let known = "diff --git a/x.rs b/x.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let cases = [
        (
            "diff --git a/check script.sh b/check script.sh\nold mode 100644\nnew mode 100755\n",
            "2/2",
            None,
        ),
        (
            "diff --git a/image asset.png b/image asset.png\nBinary files a/image asset.png and \
             b/image asset.png differ\n",
            "2/2",
            None,
        ),
        ("diff --git malformed\n", "1/2", Some(1)),
        ("diff --git malformed\ndiff --git invalid\n", "1/3", Some(2)),
        ("", "1/1", None),
    ];

    // Act / Assert
    for (additional, ratio, unresolved) in cases {
        let mut client = MockRunClient::new();
        client
            .expect_submit()
            .times(3)
            .returning(|request| Ok(accounted(&request, response())));
        let text = submit(
            &client,
            request(),
            &format!("{known}{additional}"),
            "",
            |diff, context, _| Ok(render(diff, context)),
            |_| {},
        )
        .await
        .expect("review");
        assert!(text.contains(&format!("Processed files: {ratio}. Unfinished files: 0.")));
        if let Some(count) = unresolved {
            assert!(text.contains(&format!("Unresolved file identities: {count}")));
            assert!(text.contains("file processing coverage is incomplete"));
        } else {
            assert!(!text.contains("Unresolved file identities"));
        }
    }
}

#[tokio::test]
async fn file_coverage_does_not_count_an_unfinished_file_as_processed() {
    // Arrange
    let diff = ["first", "later"]
        .map(|name| {
            format!(
                "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -0,0 +1,5000 @@\n{}",
                "+added\n".repeat(5000)
            )
        })
        .join("");
    let mut client = MockRunClient::new();
    client.expect_submit().times(2).returning(|request| {
        if request.prompt.contains("a/later") {
            return Err(OneShotError::new("batch unavailable"));
        }
        Ok(accounted(&request, response()))
    });

    // Act
    let text = submit(
        &client,
        request(),
        &diff,
        "",
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains("Processed files: 1/2. Unfinished files: 1."));
    assert!(text.contains("Unreviewed diff headers:\ndiff --git a/later b/later"));
    assert!(text.contains("Preserved finding."));
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

async fn complete_shared_boundary_pass(
    pending: &mut mpsc::UnboundedReceiver<PendingReview>,
    count: usize,
) {
    let overview = format!("All {count} changed fragments share a workflow.");
    let mut boundary_calls = 0;
    while boundary_calls < count {
        let boundary = pending.recv().await.expect("boundary batch");
        if boundary.request.request_kind == AgentRequestKind::UtilityPrompt {
            boundary
                .reply
                .send(Ok(OneShotSubmission {
                    response: AgentResponse::plain(overview.clone()),
                    stats: SessionStats::default(),
                }))
                .expect("shared overview reply");
            continue;
        }
        assert!(boundary.request.prompt.starts_with("Cross-file review:"));
        assert!(boundary.request.prompt.contains(&overview));
        assert!(!boundary.request.prompt.contains("Finding 0."));
        boundary_calls += 1;
        boundary
            .reply
            .send(Ok(OneShotSubmission {
                response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
                stats: SessionStats::default(),
            }))
            .expect("boundary reply");
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
        |diff, context, _| Ok(render(diff, context)),
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
        |diff, context, _| Ok(render(diff, context)),
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
        complete_shared_boundary_pass(&mut pending, 6).await;
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
        assert!(!reduction.request.prompt.contains("Preserved finding."));
        reduction
            .reply
            .send(Ok(accounted(
                &reduction.request,
                OneShotSubmission {
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
                },
            )))
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
        |diff, context, _| Ok(render(diff, context)),
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
                return Ok(accounted(&request, OneShotSubmission {
                    response: AgentResponse::plain(serde_json::json!({
                        "project_impact": [],
                        "suggestions": (['a', 'b', 'c', 'd', 'e', 'f'].map(|marker| serde_json::json!({
                            "severity": "medium", "details": format!("Finding {marker}.")
                        })))
                    }).to_string()),
                    stats: SessionStats::default(),
                }));
            }
            if request.prompt.starts_with("Cross-file review:") {
                assert!(!request.prompt.contains("Finding a."));
                assert!(request.prompt.contains(&"a".repeat(100)) || request.prompt.contains(&"b".repeat(100)) || request.prompt.contains(&"c".repeat(100)) || request.prompt.contains(&"d".repeat(100)) || request.prompt.contains(&"e".repeat(100)) || request.prompt.contains(&"f".repeat(100)));
                return Ok(accounted(&request, OneShotSubmission {
                    response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
                    stats: SessionStats::default(),
                }));
            }
            let marker = request.prompt.chars().next().expect("source fragment");
            if marker < 'e' && request.prompt.len() > 12_000 {
                return Err(OneShotError::new("contextWindowExceeded"));
            }
            if fail_last_retry && marker == 'd' {
                return Err(OneShotError::new("provider unavailable"));
            }
            Ok(accounted(&request, OneShotSubmission {
                response: AgentResponse::plain(serde_json::json!({
                    "project_impact": [],
                    "suggestions": [{"severity": "medium", "details": format!("Finding {marker}.")}],
                }).to_string()),
                stats: SessionStats::default(),
            }))
        });

        // Act
        let text = submit(
            &client,
            request(),
            &diff,
            "",
            |diff, _, _| Ok(diff.into()),
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
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(request.model, AgentModel::ClaudeSonnet5.as_str());
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        if request.request_kind == AgentRequestKind::UtilityPrompt {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("Changed source fragments share a workflow."),
                stats: SessionStats::default(),
            });
        }
        assert_eq!(request.request_kind, AgentRequestKind::FocusedReview);
        assert!(request.prompt.contains("Accepted decision"));
        prompts
            .lock()
            .expect("prompts")
            .push(request.prompt.clone());
        Ok(accounted(&request, response()))
    });

    // Act
    let text = submit(
        &client,
        request(),
        &diff,
        "Accepted decision",
        |diff, context, _| Ok(render(diff, context)),
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
        60_000
    );
    assert_eq!(
        prompts
            .iter()
            .filter(|prompt| prompt.contains("TAIL"))
            .count(),
        2
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
        if request.prompt.len() > 7_500 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        Ok(accounted(&request, response()))
    });

    // Act
    let text = submit(
        &client,
        request(),
        &"`".repeat(32_768),
        "",
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect("review");

    // Assert
    assert!(text.contains("Preserved finding."));
    assert!(!text.contains("Partial review"), "{text}");
}

#[tokio::test]
async fn failed_later_batch_retains_findings_and_identifies_unreviewed_files() {
    // Arrange
    let mut calls = 0;
    let mut client = MockRunClient::new();
    client.expect_submit().times(3).returning(move |request| {
        calls += 1;
        if calls == 2 {
            return Err(OneShotError::new("network timeout"));
        }
        Ok(accounted(&request, response()))
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
        |diff, context, _| Ok(render(diff, context)),
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
        Ok(accounted(&request, response()))
    });

    // Act
    let text = submit(
        &client,
        request(),
        &"x".repeat(90_000),
        "",
        |diff, context, _| Ok(render(diff, context)),
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
            if request.request_kind == AgentRequestKind::UtilityPrompt {
                return Ok(OneShotSubmission {
                    response: AgentResponse::plain("Source changes share a caller contract."),
                    stats: SessionStats::default(),
                });
            }
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
                return Ok(accounted(&request, OneShotSubmission {
                    response: AgentResponse::plain(serde_json::json!({
                        "project_impact": ["Coherent impact."],
                        "suggestions": suggestions
                    }).to_string()),
                    stats: SessionStats::default(),
                }));
            }
            if request.prompt.starts_with("Cross-file review:") {
                return Ok(accounted(&request, OneShotSubmission {
                    response: AgentResponse::plain(
                        r#"{"project_impact":[],"suggestions":[{"severity":"medium","details":"Cross-file candidate."},{"severity":"medium","details":"Second boundary candidate."}]}"#,
                    ),
                    stats: SessionStats::default(),
                }));
            }
            Ok(accounted(&request, response()))
        });
        let progress = Mutex::new(Vec::new());

        // Act
        let text = submit(
            &client,
            request(),
            &format!("diff --git a/source b/source\n{}", "x".repeat(90_000)),
            "Accepted decision",
            |diff, context, _| Ok(render(diff, context)),
            |update| progress.lock().expect("progress").push(update),
        )
        .await
        .expect("consolidated review");

        // Assert
        assert!(text.contains("Coherent impact."));
        assert!(!text.contains("Original changes reviewed."));
        assert!(!text.contains("Preserved finding."));
        assert!(!text.contains("Cross-file candidate."));
        assert!(!text.contains("Second boundary candidate."));
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
            Ok(accounted(&request, response()))
        });

        // Act
        let text = submit(
            &client,
            request(),
            &"x".repeat(90_000),
            "",
            |diff, context, _| Ok(render(diff, context)),
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
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    );
    let observe = async {
        let mut completed_source_calls = 0;
        let reduction = loop {
            let call = pending.recv().await.expect("review call");
            if call.request.prompt.starts_with("Reduce review:") {
                break call;
            }
            let submission = if call.request.request_kind == AgentRequestKind::UtilityPrompt {
                OneShotSubmission {
                    response: AgentResponse::plain("Source fragments share a workflow."),
                    stats: SessionStats::default(),
                }
            } else {
                completed_source_calls += 1;
                response()
            };
            call.reply
                .send(Ok(submission))
                .expect("completed candidate");
        };
        assert_eq!(completed_source_calls, 4);
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
                if !oversized && request.prompt.contains("LAST SOURCE BYTE") {
                    while budget.consume().is_ok() {}
                }
                return Ok(accounted(&request, response()));
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
            &format!("{}LAST SOURCE BYTE", "x".repeat(90_000)),
            "",
            |diff, context, _| Ok(render(diff, context)),
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
        candidate_decisions: Vec::new(),
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
        &|_, _, _| Err(OneShotError::new("render failed")),
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
        .times(MAX_REVIEW_PROVIDER_CALLS)
        .returning(|request| {
            request
                .provider_call_budget
                .as_ref()
                .expect("budget")
                .consume()?;
            Ok(accounted(&request, response()))
        });

    // Act
    let text = submit(
        &client,
        request(),
        &"x".repeat(8_000_000),
        "",
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect("partial review");

    // Assert
    assert!(text.contains(&format!(
        "Partial review: {MAX_REVIEW_PROVIDER_CALLS} batches completed"
    )));
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
            |diff, context, _| Ok(render(diff, context)),
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
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect_err("invalid review");
    let render_error = submit(
        &client,
        request(),
        "tiny",
        "",
        |_, _, _| Err(OneShotError::new("render")),
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
        evidence: None,
        details: "High".to_string(),
        severity: FocusedReviewSeverity::High,
    };
    let medium = FocusedReviewSuggestion {
        evidence: None,
        details: "Medium".to_string(),
        severity: FocusedReviewSeverity::Medium,
    };
    let mut review = FocusedReview {
        candidate_decisions: Vec::new(),
        project_impact: vec!["Impact".to_string()],
        suggestions: vec![medium.clone()],
    };

    // Act
    merge(
        &mut review,
        FocusedReview {
            candidate_decisions: Vec::new(),
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
    client.expect_submit().times(5).returning(|request| {
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
        assert!(
            request.prompt.contains("ORIGINAL DIFF")
                || request.prompt.starts_with("Reduce review:")
        );
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
        |diff, context, _| Ok(render(diff, context)),
        |_| {},
    )
    .await
    .expect("review");

    // Assert
    assert!(text.contains("session history was summarized"));
    assert!(ag_session::review_suggestions(&text).is_none());
}

#[tokio::test]
async fn cross_file_pass_uses_bounded_original_source_without_discarding_findings() {
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
            assert!(request.prompt.contains(&"x".repeat(1000)));
            assert!(request.prompt.contains("Large batch impact retained"));
            assert!(request.prompt.contains("Shared cross-file context"));
            return Ok(accounted(&request, response()));
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
        |diff, context, _| Ok(render(diff, context)),
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
        Ok(accounted(&request, response()))
    });
    let progress = Mutex::new(Vec::new());
    // Act
    let result = submit(
        &client,
        request(),
        "original changes",
        &"history ".repeat(10_000),
        |diff, context, _| Ok(render(diff, context)),
        |update| progress.lock().expect("progress").push(update),
    )
    .await
    .expect("review");
    // Assert
    let progress = progress.lock().expect("progress");
    assert_eq!(progress.first(), Some(&ReviewProgress::SummarizingHistory));
    assert_eq!(progress.last(), Some(&ReviewProgress::Reducing));
    assert!(result.contains("Preserved finding."));
    assert!(result.contains("session history was summarized"));
}

#[tokio::test]
async fn cross_file_reduces_history_after_provider_size_rejection() {
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
        Ok(accounted(&request, response()))
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let mut context = "Accepted decision ".repeat(100);
    // Act
    let result = cross_file_review(
        &client,
        &request,
        "diff",
        &mut context,
        None,
        &|diff, context, _| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect("adaptive cross-file review");
    // Assert
    assert_eq!(result.review.suggestions[0].details, "Preserved finding.");
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
        Ok(accounted(
            &request,
            OneShotSubmission {
                response: AgentResponse::plain(
                    serde_json::json!({"project_impact":[],"suggestions": suggestions}).to_string(),
                ),
                stats: SessionStats::default(),
            },
        ))
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let review = FocusedReview {
        candidate_decisions: Vec::new(),
        project_impact: Vec::new(),
        suggestions: (0..4)
            .map(|id| FocusedReviewSuggestion {
                evidence: None,
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
        &|diff, context, _| Ok(render(diff, context)),
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
            candidate_decisions: Vec::new(),
            project_impact: (0..impact_count).map(|id| format!("impact-{id}")).collect(),
            suggestions: vec![
                FocusedReviewSuggestion {
                    evidence: None,
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
        Ok(accounted(&request, OneShotSubmission { response: AgentResponse::plain(serde_json::json!({"project_impact":[],"suggestions":suggestions}).to_string()), stats: SessionStats::default() }))
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let review = FocusedReview {
        candidate_decisions: Vec::new(),
        project_impact: Vec::new(),
        suggestions: (0..4)
            .map(|id| FocusedReviewSuggestion {
                evidence: None,
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
        &|diff, context, _| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect_err("no information loss");
    let cross_file = cross_file_review(
        &client,
        &request,
        "diff",
        &mut String::new(),
        None,
        &|diff, context, _| Ok(render(diff, context)),
        &budget,
    )
    .await
    .err()
    .expect("minimum input still rejected");
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
        Ok(accounted(&request, response()))
    });
    let mut request = request();
    let budget = ProviderCallBudget::new(64);
    request.provider_call_budget = Some(budget.clone());
    let review = super::parse_response(&response().response).expect("input candidate");
    let diff = format!("diff --git a/{} b/file", "long-name".repeat(250));
    let mut context = "Accepted decision ".repeat(180);
    // Act
    let result = reduce_review(
        &client,
        &request,
        &review,
        &diff,
        &mut context,
        &|diff, context, _| Ok(render(diff, context)),
        &budget,
    )
    .await
    .expect("adaptive reduction");
    // Assert
    assert_eq!(result.suggestions[0].details, "Preserved finding.");
    assert!(context.contains("[Summarized input;"));
}
