use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, OneShotError, OneShotRequest, OneShotSubmission, PermissionMode,
    ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::AgentResponse;
use ag_session::FocusedReviewStatus;
use ag_store::Database;
use ag_worker::{MockRunClient, RunClient};

use super::ReviewResumeClient;

fn request() -> OneShotRequest {
    OneShotRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        child_pid: None,
        folder: ".".into(),
        harness: "codex".into(),
        model: "model".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "original batch".into(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::Low,
        request_kind: AgentRequestKind::FocusedReview,
        speed_mode: SpeedMode::Normal,
    }
}

#[tokio::test]
async fn resumes_successful_calls_across_clients_and_invalidates_changed_input() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "model", "main", "Review", project)
        .await
        .expect("session");
    let mut worker = MockRunClient::new();
    worker.expect_submit().times(5).returning(|_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
            stats: SessionStats::default(),
        })
    });
    let worker = Arc::new(worker);
    // Act
    let first = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        1,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("review admission");
    first.submit(request()).await.expect("original");
    let retry = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        1,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("review admission");
    let cached = retry.submit(request()).await.expect("cached");
    let mut changed = request();
    changed.model = "another-model".into();
    retry.submit(changed).await.expect("new model");
    ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        2,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("review admission")
    .submit(request())
    .await
    .expect("new diff");
    ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        1,
        "changed history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("review admission")
    .submit(request())
    .await
    .expect("new history");
    database
        .sessions()
        .update_session_focused_review(
            "session",
            Some(FocusedReviewStatus::Ready),
            Some("1".into()),
            Some("## Review\n### Suggestions\n- None".into()),
        )
        .await
        .expect("persist completed review");
    ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        1,
        "changed history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("review admission")
    .submit(request())
    .await
    .expect("fresh regeneration");
    // Assert
    assert_eq!(
        cached.response.answer,
        r#"{"project_impact":[],"suggestions":[]}"#
    );
}

#[test]
fn only_valid_reviews_and_bounded_summaries_are_cached() {
    // Arrange
    let mut request = request();
    // Act / Assert
    assert!(!ReviewResumeClient::cacheable(&request, "invalid"));
    request.request_kind = AgentRequestKind::UtilityPrompt;
    request.prompt = "Keep answer within 8 UTF-8 bytes".into();
    assert!(ReviewResumeClient::cacheable(&request, "summary"));
    assert!(!ReviewResumeClient::cacheable(&request, "too long summary"));
    assert!(!ReviewResumeClient::cacheable(&request, " "));
    request.prompt = "Keep answer within invalid bytes".into();
    assert!(!ReviewResumeClient::cacheable(&request, "summary"));
    request.prompt = "Keep answer within ".into();
    assert!(!ReviewResumeClient::cacheable(&request, "summary"));
    request.request_kind = AgentRequestKind::AccountRead;
    assert!(!ReviewResumeClient::cacheable(&request, "summary"));
    request.request_kind = AgentRequestKind::UtilityPrompt;
    request.prompt = "ordinary utility".into();
    assert!(!ReviewResumeClient::cacheable(&request, "summary"));
}

#[tokio::test]
async fn budget_retries_advance_past_cached_size_rejections_and_completed_batches() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "model", "main", "Review", project)
        .await
        .expect("session");
    let successful = Mutex::new(HashSet::new());
    let mut worker = MockRunClient::new();
    worker.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        if request.prompt.len() > 8000 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        assert!(
            successful
                .lock()
                .expect("successful calls")
                .insert(request.prompt)
        );
        Ok(OneShotSubmission {
            response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
            stats: SessionStats::default(),
        })
    });
    let worker = Arc::new(worker);
    let mut diff = String::new();
    for id in 0..15_000 {
        writeln!(diff, "+original changed line {id:05}").expect("string write");
    }
    // Act
    let mut complete = false;
    for attempt in 0..10 {
        let client = ReviewResumeClient::new(
            worker.clone(),
            &database,
            "session",
            1,
            "history",
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("review admission");
        let text = crate::app::review_prompt::submit(
            &client,
            request(),
            &diff,
            "",
            |diff, context| Ok(format!("{context}\n{diff}")),
            |_| {},
        )
        .await
        .expect("review result");
        if attempt == 0 {
            assert_eq!(
                FocusedReviewStatus::for_text(&text),
                FocusedReviewStatus::Partial
            );
        }
        if FocusedReviewStatus::for_text(&text) == FocusedReviewStatus::Ready {
            complete = true;
            break;
        }
    }
    // Assert
    assert!(
        complete,
        "resumed calls must eventually cover the tail and final passes"
    );
}

#[tokio::test]
async fn checkpoint_failures_are_reported_and_invalid_answers_are_not_reused() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    let mut worker = MockRunClient::new();
    worker.expect_submit().times(2).returning(|_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain("invalid"),
            stats: SessionStats::default(),
        })
    });
    let worker = Arc::new(worker);
    let client = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        1,
        "",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("review admission");
    // Act / Assert
    client
        .submit(request())
        .await
        .expect("uncached invalid response");
    client
        .submit(request())
        .await
        .expect("retry invalid response");
    database.pool().close().await;
    assert!(client.submit(request()).await.is_err());
    assert!(
        ReviewResumeClient::new(
            worker.clone(),
            &database,
            "session",
            2,
            "",
            uuid::Uuid::new_v4()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn corrupted_checkpoints_and_failed_saves_surface_errors() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "model", "main", "Review", project)
        .await
        .expect("session");
    let mut worker = MockRunClient::new();
    worker.expect_submit().times(2).returning(|_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
            stats: SessionStats::default(),
        })
    });
    let worker = Arc::new(worker);
    let client = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        1,
        "",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("review admission");
    // Act
    client.submit(request()).await.expect("saved response");
    sqlx::query("UPDATE session_review_fragment SET answer = 'invalid'")
        .execute(database.pool())
        .await
        .expect("corrupt fixture");
    let corrupt = client.submit(request()).await.expect_err("corruption");
    database
        .sessions()
        .clear_review_fragments("session", &client.generation)
        .await
        .expect("clear fixture");
    sqlx::query(
        "CREATE TRIGGER reject_checkpoint BEFORE INSERT ON session_review_fragment BEGIN SELECT \
         RAISE(ABORT, 'checkpoint failure'); END",
    )
    .execute(database.pool())
    .await
    .expect("failing checkpoint fixture");
    let failed = client.submit(request()).await.expect_err("storage failure");
    // Assert
    assert!(corrupt.to_string().contains("Invalid review checkpoint"));
    assert!(failed.to_string().contains("checkpoint failure"));
}

#[tokio::test]
async fn delayed_older_submission_cannot_supersede_newer_checkpoints() {
    for (new_diff, new_history) in [(2, "history"), (1, "changed history")] {
        // Arrange: creation order differs from provider submission order.
        let database = Database::open_in_memory().await.expect("database");
        let project = database
            .projects()
            .upsert_project("project", None)
            .await
            .expect("project");
        database
            .sessions()
            .insert_session("session", "model", "main", "Review", project)
            .await
            .expect("session");
        let mut worker = MockRunClient::new();
        worker.expect_submit().times(3).returning(|_| {
            Ok(OneShotSubmission {
                response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
                stats: SessionStats::default(),
            })
        });
        let worker = Arc::new(worker);
        let older = ReviewResumeClient::new(
            worker.clone(),
            &database,
            "session",
            1,
            "history",
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("older admission");
        let newer = ReviewResumeClient::new(
            worker.clone(),
            &database,
            "session",
            new_diff,
            new_history,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("newer admission");
        let mut tail = request();
        tail.prompt = "later batch".into();

        // Act: the old review reaches its first submission after the new
        // review.
        newer
            .submit(request())
            .await
            .expect("newer first checkpoint");
        older.submit(request()).await.expect("delayed older result");
        newer
            .submit(tail.clone())
            .await
            .expect("newer subsequent checkpoint");
        drop(newer);
        let retry = ReviewResumeClient::new(
            worker.clone(),
            &database,
            "session",
            new_diff,
            new_history,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("resume interrupted newer review");
        retry
            .submit(request())
            .await
            .expect("reuse first checkpoint");
        retry
            .submit(tail)
            .await
            .expect("reuse subsequent checkpoint");

        // Assert: only the three original calls used the provider; no retry
        // repeats them.
        let generations: Vec<String> =
            sqlx::query_scalar("SELECT generation FROM session_review_fragment")
                .fetch_all(database.pool())
                .await
                .expect("retained checkpoints");
        assert_eq!(generations, [retry.generation.clone(), retry.generation]);
    }
}

#[tokio::test]
async fn invalidation_prevents_identical_inputs_from_reusing_or_restoring_old_evidence() {
    // Arrange: rebase can change worktree context without changing
    // diff/history.
    let database = Database::open_in_memory().await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "model", "main", "Review", project)
        .await
        .expect("session");
    let mut worker = MockRunClient::new();
    worker.expect_submit().times(3).returning(|_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain(r#"{"project_impact":[],"suggestions":[]}"#),
            stats: SessionStats::default(),
        })
    });
    let worker = Arc::new(worker);
    let older = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        42,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("original review");
    older
        .submit(request())
        .await
        .expect("pre-rebase checkpoint");

    // Act: explicitly invalidate, then activate the exact same inputs.
    database
        .sessions()
        .update_session_focused_review("session", None, None, None)
        .await
        .expect("invalidate context");
    let newer = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        42,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("post-rebase review");
    let mut late = request();
    late.prompt = "late pre-rebase batch".into();
    older
        .submit(late)
        .await
        .expect("old provider returns after reactivation");
    newer
        .submit(request())
        .await
        .expect("fresh provider call required");
    let retry = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        42,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("unchanged retry");
    retry
        .submit(request())
        .await
        .expect("reuse post-rebase evidence");

    // Assert: no pre-rebase answer is retained or replayed, and retry is free.
    let requests: Vec<String> = sqlx::query_scalar("SELECT request FROM session_review_fragment")
        .fetch_all(database.pool())
        .await
        .expect("retained evidence");
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].contains("late pre-rebase batch"));
}
