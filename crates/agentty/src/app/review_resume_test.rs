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
    request.prompt = "Reduce review: 1 candidates.\nVerify source".into();
    assert!(!ReviewResumeClient::cacheable(
        &request,
        r#"{"project_impact":[],"suggestions":[]}"#
    ));
    assert!(!ReviewResumeClient::cacheable(
        &request,
        r#"{"project_impact":[],"suggestions":[],"candidate_decisions":[{"candidate_index":0,"reason":"Verified source"}]}"#
    ));
    assert!(ReviewResumeClient::cacheable(
        &request,
        r#"{"project_impact":[],"suggestions":[],"candidate_decisions":[{"candidate_index":0,"suggestion_index":null,"reason":"The caller already validates input"}]}"#
    ));
    assert!(!ReviewResumeClient::cacheable(
        &request,
        r#"{"project_impact":[],"suggestions":[{"details":"Unlinked finding","severity":"high"}],"candidate_decisions":[{"candidate_index":0,"suggestion_index":null,"reason":"Rejected input"}]}"#
    ));
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
async fn invalid_dispositions_are_not_replayed_from_checkpoints() {
    // Arrange / Act / Assert
    for answer in [
        r#"{"project_impact":[],"suggestions":[],"candidate_decisions":[{"candidate_index":0,"reason":"Verified source"}]}"#,
        r#"{"project_impact":[],"suggestions":[{"details":"Unlinked finding","severity":"high"}],"candidate_decisions":[{"candidate_index":0,"suggestion_index":null,"reason":"Rejected input"}]}"#,
    ] {
        invalid_disposition_is_not_cached(answer).await;
    }
}

async fn invalid_disposition_is_not_cached(answer: &'static str) {
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
    worker.expect_submit().times(2).returning(move |_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain(answer),
            stats: SessionStats::default(),
        })
    });
    let worker = Arc::new(worker);
    let mut request = request();
    request.prompt = "Reduce review: 1 candidates.\nVerify source".into();

    // Act / Assert
    for _ in 0..2 {
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
        let result = client
            .submit(request.clone())
            .await
            .expect("provider response");
        assert_eq!(result.response.answer, answer);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_review_fragment")
        .fetch_one(database.pool())
        .await
        .expect("checkpoint count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn previously_cached_omitted_dispositions_are_replaced_with_a_valid_fresh_response() {
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
    let valid = r#"{"project_impact":[],"suggestions":[],"candidate_decisions":[{"candidate_index":0,"suggestion_index":null,"reason":"Verified source"}]}"#;
    let mut worker = MockRunClient::new();
    worker.expect_submit().times(2).returning(move |_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain(valid),
            stats: SessionStats {
                input_tokens: 7,
                ..SessionStats::default()
            },
        })
    });
    let worker = Arc::new(worker);
    let mut request = request();
    request.prompt = "Reduce review: 1 candidates.\nVerify source".into();
    let first = ReviewResumeClient::new(
        worker.clone(),
        &database,
        "session",
        1,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("first review");
    first.submit(request.clone()).await.expect("save call");
    // Simulate a checkpoint accepted by the older parser, preserving the real
    // request key instead of duplicating its encoding in this regression.
    let mut old_answer: serde_json::Value = serde_json::from_str(valid).expect("fixture review");
    old_answer["candidate_decisions"][0]
        .as_object_mut()
        .expect("decision")
        .remove("suggestion_index");
    sqlx::query("UPDATE session_review_fragment SET answer = ? WHERE session_id = 'session'")
        .bind(serde_json::json!({"Answer": old_answer.to_string()}).to_string())
        .execute(database.pool())
        .await
        .expect("older checkpoint");

    // Act
    let retry = ReviewResumeClient::new(
        worker,
        &database,
        "session",
        1,
        "history",
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("retry");
    let fresh = retry
        .submit(request.clone())
        .await
        .expect("fresh provider call");
    let replay = retry.submit(request).await.expect("valid replay");

    // Assert
    assert_eq!(fresh.response.answer, valid);
    assert_eq!(fresh.stats.input_tokens, 7);
    assert_eq!(replay.response.answer, valid);
    assert_eq!(replay.stats.input_tokens, 0);
}

#[tokio::test]
async fn completed_audit_preserves_discovery_and_group_scoped_rejection_reasons_after_retry() {
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
    let answer = r#"{"project_impact":[],"suggestions":[],"candidate_decisions":[{"candidate_index":0,"suggestion_index":null,"reason":"The caller already validates input"}]}"#;
    let discovery = r#"{"project_impact":[],"suggestions":[{"severity":"high","details":"bypassed input guard"},{"severity":"medium","details":"missing caller validation"}]}"#;
    let mut worker = MockRunClient::new();
    worker.expect_submit().times(3).returning(move |request| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain(if request.prompt.starts_with("Reduce review:") {
                answer
            } else {
                discovery
            }),
            stats: SessionStats::default(),
        })
    });
    let worker = Arc::new(worker);
    let mut request = request();
    request.prompt = "Reduce review: 1 candidates.\nCandidate: bypassed input guard".into();

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
    .expect("first review");
    first.submit(self::request()).await.expect("save discovery");
    first.submit(request.clone()).await.expect("save decisions");
    let mut second_group = request.clone();
    second_group.prompt =
        "Reduce review: 1 candidates.\nCandidate: missing caller validation".into();
    first.submit(second_group).await.expect("save other group");
    let request_id = uuid::Uuid::new_v4();
    let retry = ReviewResumeClient::new(worker, &database, "session", 1, "history", request_id)
        .await
        .expect("retry");
    let saved = retry.submit(request).await.expect("replayed accounting");
    database
        .sessions()
        .update_session_focused_review_for_generation(
            "session",
            &request_id.to_string(),
            FocusedReviewStatus::Ready,
            Some("1".into()),
            Some("## Review\n### Suggestions\n- None".into()),
        )
        .await
        .expect("completed persistence");

    // Assert
    assert_eq!(saved.response.answer, answer);
    let audit = database
        .sessions()
        .load_completed_review_audit("session")
        .await
        .expect("completed audit");
    assert_eq!(audit.len(), 3);
    let discovery_row = audit
        .iter()
        .find(|row| row.request.ends_with("original batch"))
        .expect("discovery call");
    let discovery_result: serde_json::Value =
        serde_json::from_str(&discovery_row.answer).expect("saved discovery");
    assert_eq!(discovery_result["Answer"], discovery);
    for candidate in ["bypassed input guard", "missing caller validation"] {
        let group = audit
            .iter()
            .find(|row| row.request.contains(&format!("Candidate: {candidate}")))
            .expect("scoped group");
        let result: serde_json::Value = serde_json::from_str(&group.answer).expect("saved call");
        let review: ag_protocol::FocusedReview =
            serde_json::from_str(result["Answer"].as_str().expect("answer")).expect("accounting");
        assert_eq!(review.candidate_decisions[0].candidate_index, 0);
        assert_eq!(review.candidate_decisions[0].suggestion_index, None);
        assert_eq!(
            review.candidate_decisions[0].reason,
            "The caller already validates input"
        );
        assert_eq!(group.generation, discovery_row.generation);
    }
    let checkpoints: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_review_fragment")
        .fetch_one(database.pool())
        .await
        .expect("retry cleanup");
    assert_eq!(checkpoints, 0);
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
            |diff, context, _| Ok(format!("{context}\n{diff}")),
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
