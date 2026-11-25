//! Fork session snapshot tests that verify source-specific state is reset.

use ag_contracts::test_support::token_usage;
use ag_contracts::{TurnUsage, UsageCheckpoint, UsagePurpose};
use ag_session::{FocusedReviewStatus, ResponseStyle};
use sqlx::SqlitePool;

use crate::AppRepositories;
use crate::review::SessionReviewRequestRow;
use crate::session::{ForkSessionSnapshot, SessionRow, SessionTurnMetadata};
use crate::test_support::review_request_fixture;

#[tokio::test]
async fn test_fork_session_snapshot_resets_source_specific_state() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let (source_reset_row, source_review_request) =
        seed_fork_snapshot_source(&database, &pool).await;
    database
        .sessions()
        .update_session_response_style("source-session", ResponseStyle::Detailed)
        .await
        .expect("source response style should persist");

    // Act
    database
        .sessions()
        .fork_session_snapshot(ForkSessionSnapshot {
            new_session_id: "fork-session",
            source_session_id: "source-session",
            status: "Review",
        })
        .await
        .expect("failed to fork session snapshot");

    // Assert
    let session_rows = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let source_row = session_rows
        .iter()
        .find(|session_row| session_row.id == "source-session")
        .expect("missing source session row");
    let fork_row = session_rows
        .iter()
        .find(|session_row| session_row.id == "fork-session")
        .expect("missing forked session row");
    let fork_reset_row = load_fork_reset_row(&pool, "fork-session").await;
    let fork_review_request = database
        .reviews()
        .load_session_review_request("fork-session")
        .await
        .expect("failed to load fork review request");
    assert_source_reset_state(
        source_row,
        &source_reset_row,
        source_review_request.as_ref(),
    );
    assert_fork_reset_state(fork_row, &fork_reset_row, fork_review_request.as_ref());
    assert_eq!(fork_row.response_style, ResponseStyle::Detailed.as_str());
}

/// Session columns that must be reset when snapshotting a fork.
struct ForkResetRow {
    app_server_instruction_provider_conversation_id: Option<String>,
    focused_review_diff_hash: Option<String>,
    focused_review_text: Option<String>,
    in_progress_started_at: Option<i64>,
    in_progress_total_seconds: i64,
    input_tokens: i64,
    is_draft: bool,
    merged_commit_hash: Option<String>,
    output_tokens: i64,
    provider_conversation_id: Option<String>,
    published_upstream_ref: Option<String>,
    questions: Option<String>,
    usage_checkpoint: Option<String>,
}

/// Loads reset-sensitive fork columns that are not exposed by public
/// session row projections.
async fn load_fork_reset_row(pool: &SqlitePool, session_id: &str) -> ForkResetRow {
    sqlx::query_as!(
        ForkResetRow,
        r#"
SELECT app_server_instruction_provider_conversation_id,
       focused_review_diff_hash,
       focused_review_text,
       in_progress_started_at,
       in_progress_total_seconds,
       input_tokens,
       is_draft AS "is_draft: bool",
       merged_commit_hash,
       output_tokens,
       provider_conversation_id,
       published_upstream_ref,
       questions,
       usage_checkpoint
FROM session
WHERE id = ?
"#,
        session_id
    )
    .fetch_one(pool)
    .await
    .expect("failed to load fork reset row")
}

/// Seeds a forkable source session with every source-only field that the
/// snapshot insert is expected to clear.
async fn seed_fork_snapshot_source(
    database: &AppRepositories,
    pool: &SqlitePool,
) -> (ForkResetRow, Option<SessionReviewRequestRow>) {
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_draft_session(
            "source-session",
            "gpt-5.6-sol",
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert source session");

    seed_fork_snapshot_source_linkage(database).await;
    seed_fork_snapshot_source_timing(database, pool).await;

    let source_reset_row = load_fork_reset_row(pool, "source-session").await;
    let source_review_request = database
        .reviews()
        .load_session_review_request("source-session")
        .await
        .expect("failed to load source review request");

    (source_reset_row, source_review_request)
}

/// Persists source-only linkage and counters on the fork source row.
async fn seed_fork_snapshot_source_linkage(database: &AppRepositories) {
    database
        .sessions()
        .persist_session_turn_metadata(
            "source-session",
            &SessionTurnMetadata {
                harness_turn_report: None,
                instruction_conversation_id: None,
                model: "gpt-5.6-sol".to_string(),
                provider_conversation_id: None,
                questions_json: "[]".to_string(),
                review_comment_resolutions: Vec::new(),
                usage: TurnUsage {
                    checkpoint: Some(UsageCheckpoint::new("/worktree", "source-checkpoint")),
                    ..TurnUsage::default()
                },
            },
        )
        .await
        .expect("failed to persist applied agent_profile");
    database
        .sessions()
        .update_session_provider_conversation_id(
            "source-session",
            Some("provider-thread".to_string()),
        )
        .await
        .expect("failed to update provider conversation id");
    database
        .sessions()
        .update_session_instruction_conversation_id(
            "source-session",
            Some("instruction-thread".to_string()),
        )
        .await
        .expect("failed to update instruction conversation id");
    database
        .sessions()
        .update_session_questions("source-session", r#"["Need detail?"]"#)
        .await
        .expect("failed to update questions");
    database
        .sessions()
        .update_session_published_upstream_ref(
            "source-session",
            Some("origin/wt/source-session".to_string()),
        )
        .await
        .expect("failed to update published upstream ref");
    database
        .sessions()
        .update_session_merged_commit_hash("source-session", Some("merged123".to_string()))
        .await
        .expect("failed to update merged commit hash");
    database
        .sessions()
        .update_session_focused_review(
            "source-session",
            Some(FocusedReviewStatus::Ready),
            Some("diff123".to_string()),
            Some("Focused review text".to_string()),
        )
        .await
        .expect("failed to update focused review");
    database
        .usage()
        .upsert_session_usage(
            "source-session",
            UsagePurpose::Turn,
            &token_usage("gpt-5.6-sol", 11, 29),
        )
        .await
        .expect("failed to update token stats");
    database
        .sessions()
        .update_session_diff_stats(7, 3, true, "source-session", "S")
        .await
        .expect("failed to update source diff stats");
    database
        .reviews()
        .update_session_review_request("source-session", Some(review_request_fixture()))
        .await
        .expect("failed to update review request");
}

/// Persists active-work timing fields on the fork source row.
async fn seed_fork_snapshot_source_timing(database: &AppRepositories, pool: &SqlitePool) {
    database
        .sessions()
        .update_session_status_with_timing_at("source-session", "InProgress", 100)
        .await
        .expect("failed to open timing interval");
    sqlx::query!(
        r"
UPDATE session
SET in_progress_total_seconds = ?
WHERE id = ?
",
        75_i64,
        "source-session"
    )
    .execute(pool)
    .await
    .expect("failed to seed elapsed timing");
}

/// Asserts the fixture source row actually had source-only state before
/// the snapshot was taken.
fn assert_source_reset_state(
    source_row: &SessionRow,
    source_reset_row: &ForkResetRow,
    source_review_request: Option<&SessionReviewRequestRow>,
) {
    assert_eq!(source_row.added_lines, 7);
    assert_eq!(source_row.deleted_lines, 3);
    assert_eq!(source_row.has_diff, Some(true));
    assert_eq!(source_row.size, "S");
    assert!(source_reset_row.is_draft);
    assert_eq!(
        (
            source_reset_row.input_tokens,
            source_reset_row.output_tokens
        ),
        (11, 29)
    );
    assert_eq!(
        source_reset_row.usage_checkpoint.as_deref(),
        Some("source-checkpoint")
    );
    assert_eq!(
        source_reset_row.provider_conversation_id.as_deref(),
        Some("provider-thread")
    );
    assert_eq!(
        source_reset_row
            .app_server_instruction_provider_conversation_id
            .as_deref(),
        Some("instruction-thread")
    );
    assert_eq!(
        source_reset_row.published_upstream_ref.as_deref(),
        Some("origin/wt/source-session")
    );
    assert_eq!(
        source_reset_row.questions.as_deref(),
        Some(r#"["Need detail?"]"#)
    );
    assert_eq!(
        source_reset_row.merged_commit_hash.as_deref(),
        Some("merged123")
    );
    assert_eq!(
        source_reset_row.focused_review_diff_hash.as_deref(),
        Some("diff123")
    );
    assert_eq!(
        source_reset_row.focused_review_text.as_deref(),
        Some("Focused review text")
    );
    assert_eq!(source_reset_row.in_progress_started_at, Some(100));
    assert_eq!(source_reset_row.in_progress_total_seconds, 75);
    assert_eq!(
        source_review_request.map(|review_request| review_request.display_id.as_str()),
        Some("#42")
    );
}

/// Asserts the forked row kept durable snapshot state while clearing
/// source-only linkage.
fn assert_fork_reset_state(
    fork_row: &SessionRow,
    fork_reset_row: &ForkResetRow,
    fork_review_request: Option<&SessionReviewRequestRow>,
) {
    assert_eq!(fork_row.status, "Review");
    assert!(!fork_row.is_draft);
    assert_eq!(
        (fork_reset_row.input_tokens, fork_reset_row.output_tokens),
        (0, 0)
    );
    assert_eq!(fork_reset_row.usage_checkpoint, None);
    assert_eq!(fork_row.added_lines, 0);
    assert_eq!(fork_row.deleted_lines, 0);
    assert_eq!(fork_row.has_diff, None);
    assert_eq!(fork_row.size, "XS");
    assert_eq!(fork_row.questions, None);
    assert_eq!(fork_row.published_upstream_ref, None);
    assert_eq!(fork_row.review_request, None);
    assert_eq!(fork_reset_row.provider_conversation_id, None);
    assert_eq!(
        fork_reset_row.app_server_instruction_provider_conversation_id,
        None
    );
    assert_eq!(fork_reset_row.merged_commit_hash, None);
    assert_eq!(fork_reset_row.focused_review_diff_hash, None);
    assert_eq!(fork_reset_row.focused_review_text, None);
    assert_eq!(fork_reset_row.questions, None);
    assert_eq!(fork_reset_row.in_progress_started_at, None);
    assert_eq!(fork_reset_row.in_progress_total_seconds, 0);
    assert_eq!(fork_review_request, None);
}
