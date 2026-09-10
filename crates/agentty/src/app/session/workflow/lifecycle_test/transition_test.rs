use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_agent as agent;
use ag_agent::{AgentRequestKind, MockOneShotClient, OneShotClient};
use ag_git as git;
use ag_protocol::AgentResponse;
use tokio::sync::{Notify, mpsc};

use super::super::{
    ClaimedSessionTitleGenerationTaskInput, SESSION_TITLE_CONTEXT_TRUNCATION_MARKER,
    SESSION_TITLE_GENERATION_MAX_ATTEMPTS, SessionTitleGenerationContext,
    TitleGenerationTaskCompletion,
};
use super::support::{
    DelayedTitleClient, load_persisted_session_row, mock_title_client, provisional_title_database,
    session_manager_with_sessions, session_with_id, title_generation_task_input,
};
use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::session::{SessionId, Status};
use crate::infra::db::AppRepositories;
use crate::infra::fs;

#[tokio::test]
/// Ensures a title-generation claim failure completes tracking without
/// starting a provider request.
async fn test_spawn_session_title_generation_task_handles_claim_failure() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let one_shot_client: Arc<dyn OneShotClient> = Arc::new(MockOneShotClient::new());
    let mut input = title_generation_task_input(
        app_event_tx,
        database,
        one_shot_client,
        "review the project",
    );
    input.tracked_generation = Some(7);

    // Act
    let title_generation_task = SessionManager::spawn_session_title_generation_task(input).await;

    // Assert
    assert!(title_generation_task.is_none());
    assert!(matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::SessionTitleGenerationFinished {
            generation: 7,
            session_id,
        }) if session_id == "session-id"
    ));
}

#[test]
fn previous_is_no_op_when_no_sessions_present() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(Vec::new());

    // Act
    session_manager.previous();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), None);
}

#[test]
fn session_at_returns_session_by_index_or_none_for_out_of_range() {
    // Arrange
    let session_manager = session_manager_with_sessions(vec![
        session_with_id("session-a", Status::InProgress),
        session_with_id("session-b", Status::Review),
    ]);

    // Act / Assert
    assert_eq!(
        session_manager
            .session_at(0)
            .map(|session| session.id.as_str()),
        Some("session-a")
    );
    assert_eq!(
        session_manager
            .session_at(1)
            .map(|session| session.id.as_str()),
        Some("session-b")
    );
    assert!(session_manager.session_at(99).is_none());
}

#[test]
/// Ensures single-line title responses are normalized and accepted.
fn test_parse_generated_session_title_accepts_plain_title() {
    // Arrange
    let response_content = "Refine session startup flow";

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(
        parsed_title,
        Some("Refine session startup flow".to_string())
    );
}

#[test]
/// Ensures protocol-wrapped plain answer lines are accepted.
fn test_parse_generated_session_title_accepts_protocol_answer_plain_text() {
    // Arrange
    let response_content = r#"{"answer":"Polish title parsing","questions":[]}"#;

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(parsed_title, Some("Polish title parsing".to_string()));
}

#[test]
/// Ensures plain-text responses with extra lines keep only the first
/// non-empty title line.
fn test_parse_generated_session_title_uses_first_nonempty_line_for_multiline_response() {
    // Arrange
    let response_content = "Polish title parsing\nExtra detail that should be ignored";

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(parsed_title, Some("Polish title parsing".to_string()));
}

#[test]
/// Ensures `Title:` prefixes are normalized before persistence.
fn test_parse_generated_session_title_normalizes_title_prefix() {
    // Arrange
    let response_content = "Title: \"Polish merge queue behavior\"";

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(
        parsed_title,
        Some("Polish merge queue behavior".to_string())
    );
}

#[test]
/// Ensures progress-gerund output is rejected as status prose.
fn test_parse_generated_session_title_rejects_progress_prefix() {
    // Arrange
    let response_content = "Checking commit-message constraints";

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(parsed_title, None);
}

#[test]
/// Ensures overlong model prose is rejected instead of being truncated
/// into a misleading generated title.
fn test_parse_generated_session_title_rejects_overlong_candidate() {
    // Arrange
    let response_content =
        "Refine session title generation for utility outputs that are unexpectedly verbose";

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(parsed_title, None);
}

#[test]
fn session_id_for_index_returns_owned_id_or_none_for_out_of_range() {
    // Arrange
    let session_manager =
        session_manager_with_sessions(vec![session_with_id("session-a", Status::InProgress)]);

    // Act / Assert
    assert_eq!(
        session_manager.session_id_for_index(0),
        Some("session-a".into())
    );
    assert!(session_manager.session_id_for_index(1).is_none());
}

#[test]
/// Ensures byte truncation never splits a multibyte UTF-8 character.
fn test_truncate_session_title_context_preserves_utf8_boundaries() {
    // Arrange
    let max_bytes = SESSION_TITLE_CONTEXT_TRUNCATION_MARKER.len() + 2;
    let value = "€".repeat(max_bytes);

    // Act
    let truncated = SessionManager::truncate_session_title_context(&value, max_bytes);

    // Assert
    assert_eq!(truncated, SESSION_TITLE_CONTEXT_TRUNCATION_MARKER);
    assert!(truncated.len() <= max_bytes);
    assert!(truncated.is_char_boundary(truncated.len()));
}

#[test]
fn selected_session_returns_currently_selected_session_or_none() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(vec![
        session_with_id("session-a", Status::InProgress),
        session_with_id("session-b", Status::Review),
    ]);

    // Act / Assert
    assert!(session_manager.selected_session().is_none());

    session_manager.state.table_state.select(Some(1));
    assert_eq!(
        session_manager
            .selected_session()
            .map(|session| session.id.clone()),
        Some("session-b".into())
    );
}

#[test]
fn next_advances_selection_to_next_grouped_row() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(vec![
        session_with_id("session-active-1", Status::InProgress),
        session_with_id("session-active-2", Status::Review),
    ]);
    session_manager.state.table_state.select(Some(0));

    // Act
    session_manager.next();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), Some(1));
}

#[test]
/// Ensures case, punctuation, and single-line layout changes cannot turn
/// request text into an authoritative generated title.
fn test_generated_session_title_copy_detection_normalizes_request_text() {
    // Arrange
    let context = SessionTitleGenerationContext {
        current_title: String::new(),
        latest_request: "Review the project, please.".to_string(),
        original_request: "Background context only.".to_string(),
    };

    // Act
    let latest_request_copy = SessionManager::is_generated_session_title_request_copy(
        "REVIEW THE PROJECT PLEASE",
        &context,
    );
    let original_request_copy = SessionManager::is_generated_session_title_request_copy(
        "Background context only",
        &context,
    );
    let distinct_title =
        SessionManager::is_generated_session_title_request_copy("Assess project quality", &context);
    let empty_title = SessionManager::is_normalized_title_copy("", &context.latest_request);
    let context_with_current_title = SessionTitleGenerationContext {
        current_title: "Stable session title".to_string(),
        latest_request: context.latest_request,
        original_request: context.original_request,
    };
    let current_title_copy = SessionManager::is_generated_session_title_request_copy(
        "STABLE SESSION TITLE!",
        &context_with_current_title,
    );

    // Assert
    assert!(latest_request_copy);
    assert!(original_request_copy);
    assert!(current_title_copy);
    assert!(!distinct_title);
    assert!(!empty_title);
}

#[test]
/// Ensures a copied line from a multiline clarification payload is
/// rejected even though it is not equal to the full request.
fn test_generated_session_title_copy_detection_checks_each_request_line() {
    // Arrange
    let context = SessionTitleGenerationContext {
        current_title: "Stabilize session titles".to_string(),
        latest_request: "Clarifications:\nUse all session context!".to_string(),
        original_request: "Stabilize session title generation".to_string(),
    };

    // Act
    let is_copy = SessionManager::is_generated_session_title_request_copy(
        "use all session context",
        &context,
    );

    // Assert
    assert!(is_copy);
}

#[tokio::test]
async fn test_cleanup_session_worktree_resources_collects_cleanup_errors() {
    // Arrange
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_remove_dir_all()
        .once()
        .returning(|_| {
            Box::pin(async {
                Err(fs::FsError::Io(std::io::Error::other(
                    "simulated directory cleanup failure",
                )))
            })
        });
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_remove_worktree()
        .once()
        .returning(|_| {
            Box::pin(async {
                Err(git::GitError::CommandFailed {
                    command: "git worktree remove".to_string(),
                    stderr: "simulated worktree removal failure".to_string(),
                })
            })
        });
    mock_git_client
        .expect_delete_branch()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Err(git::GitError::CommandFailed {
                    command: "git branch -D".to_string(),
                    stderr: "simulated branch deletion failure".to_string(),
                })
            })
        });

    // Act
    let cleanup_errors = SessionManager::cleanup_session_worktree_resources(
        Arc::new(mock_fs_client),
        Arc::new(mock_git_client),
        PathBuf::from("/tmp/session"),
        "wt/session-id".to_string(),
        Some(PathBuf::from("/tmp/repo")),
        true,
    )
    .await;

    // Assert
    assert_eq!(cleanup_errors.len(), 3);
    assert!(
        cleanup_errors
            .iter()
            .any(|message| message.contains("failed to remove worktree"))
    );
    assert!(
        cleanup_errors
            .iter()
            .any(|message| message.contains("failed to delete branch"))
    );
    assert!(
        cleanup_errors
            .iter()
            .any(|message| message.contains("failed to remove worktree directory"))
    );
}

#[test]
fn previous_wraps_to_last_selectable_row_when_at_first_row() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(vec![
        session_with_id("session-active", Status::InProgress),
        session_with_id("session-archive", Status::Done),
    ]);
    session_manager.state.table_state.select(Some(0));

    // Act
    session_manager.previous();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), Some(1));
}

#[test]
fn session_index_for_id_returns_index_or_none_for_unknown_session() {
    // Arrange
    let session_manager = session_manager_with_sessions(vec![
        session_with_id("session-a", Status::InProgress),
        session_with_id("session-b", Status::Review),
    ]);

    // Act / Assert
    assert_eq!(session_manager.session_index_for_id("session-a"), Some(0));
    assert_eq!(session_manager.session_index_for_id("session-b"), Some(1));
    assert!(session_manager.session_index_for_id("missing").is_none());
}

#[tokio::test]
/// Ensures an authoritative title rejects a delayed generated candidate.
async fn test_title_generation_ignores_candidate_invalidated_while_running() {
    // Arrange
    let (database, _pool) = provisional_title_database("Background context only.").await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let one_shot_client: Arc<dyn OneShotClient> = Arc::new(DelayedTitleClient {
        release: Arc::clone(&release),
    });
    let input = title_generation_task_input(
        app_event_tx,
        database.clone(),
        one_shot_client,
        "review the project",
    );
    let title_generation_task = SessionManager::spawn_session_title_generation_task(input)
        .await
        .expect("title generation should start");

    // Act
    database
        .sessions()
        .update_session_title("session-id", "Authoritative commit title")
        .await
        .expect("failed to persist authoritative title");
    release.notify_one();
    title_generation_task
        .await
        .expect("title generation task should finish");
    let persisted_session = load_persisted_session_row(&database).await;

    // Assert
    assert_eq!(
        persisted_session.title.as_deref(),
        Some("Authoritative commit title")
    );
    assert!(app_event_rx.try_recv().is_err());
}

#[test]
fn previous_starts_at_first_selectable_row_when_no_prior_selection() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(vec![
        session_with_id("session-active", Status::InProgress),
        session_with_id("session-archive", Status::Done),
    ]);

    // Act
    session_manager.previous();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), Some(0));
}

#[test]
fn next_starts_at_first_selectable_row_when_no_prior_selection() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(vec![
        session_with_id("session-active", Status::InProgress),
        session_with_id("session-archive", Status::Done),
    ]);

    // Act
    session_manager.next();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), Some(0));
}

#[test]
fn previous_moves_selection_back_one_grouped_row() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(vec![
        session_with_id("session-active-1", Status::InProgress),
        session_with_id("session-active-2", Status::Review),
    ]);
    session_manager.state.table_state.select(Some(1));

    // Act
    session_manager.previous();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), Some(0));
}

#[test]
fn next_is_no_op_when_no_sessions_present() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(Vec::new());

    // Act
    session_manager.next();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), None);
}

#[tokio::test]
/// Ensures a claimed task completes its tracking event without calling a
/// provider when persisted session context disappears.
async fn test_claimed_title_generation_finishes_when_session_context_is_missing() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().times(0);
    let input = ClaimedSessionTitleGenerationTaskInput {
        app_event_tx,
        db: database,
        folder: PathBuf::from("/tmp/session"),
        latest_request: "Latest request".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        reasoning_level: ReasoningLevel::Low,
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        session_id: SessionId::from("missing-session"),
        speed_mode: SpeedMode::Normal,
        title_generation: 1,
        tracked_completion: Some(TitleGenerationTaskCompletion {
            generation: 7,
            session_id: SessionId::from("missing-session"),
        }),
    };

    // Act
    SessionManager::run_claimed_session_title_generation_task(input).await;

    // Assert
    assert!(matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::SessionTitleGenerationFinished {
            generation: 7,
            session_id,
        }) if session_id == "missing-session"
    ));
}

#[tokio::test]
/// Ensures a later empty candidate does not invalidate an earlier usable
/// title generation that is still running.
async fn test_empty_candidate_preserves_delayed_title_generation() {
    // Arrange
    let (database, _pool) = provisional_title_database("Background context only.").await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let one_shot_client: Arc<dyn OneShotClient> = Arc::new(DelayedTitleClient {
        release: Arc::clone(&release),
    });
    let delayed_task =
        SessionManager::spawn_session_title_generation_task(title_generation_task_input(
            app_event_tx.clone(),
            database.clone(),
            one_shot_client,
            "review the project",
        ))
        .await
        .expect("actionable title generation should start");

    // Act
    let empty_candidate_task =
        SessionManager::spawn_session_title_generation_task(title_generation_task_input(
            app_event_tx,
            database.clone(),
            mock_title_client(""),
            "Additional context follows.",
        ))
        .await
        .expect("context-only title generation should start");
    empty_candidate_task
        .await
        .expect("empty title generation task should finish");
    release.notify_one();
    delayed_task
        .await
        .expect("delayed title generation task should finish");
    let persisted_session = load_persisted_session_row(&database).await;

    // Assert
    assert_eq!(
        persisted_session.title.as_deref(),
        Some("Assess project quality")
    );
    assert!(matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::RefreshSessions)
    ));
}

#[test]
fn next_wraps_to_first_selectable_row_after_last_row() {
    // Arrange
    let mut session_manager = session_manager_with_sessions(vec![
        session_with_id("session-active", Status::InProgress),
        session_with_id("session-archive", Status::Done),
    ]);
    session_manager.state.table_state.select(Some(1));

    // Act
    session_manager.next();

    // Assert
    assert_eq!(session_manager.state.table_state.selected(), Some(0));
}

#[tokio::test]
/// Ensures title generation returns normalized answer text from the
/// injected one-shot boundary.
async fn test_run_title_generation_command_returns_answer_text() {
    // Arrange
    let folder = PathBuf::from("/tmp/title-generation");
    let expected_folder = folder.clone();
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(move |request| {
            assert_eq!(request.agent_kind, AgentKind::Claude);
            assert_eq!(request.folder, expected_folder);
            assert_eq!(request.model, AgentModel::ClaudeSonnet5);
            assert_eq!(request.permission_mode, ag_agent::PermissionMode::ReadOnly);
            assert_eq!(request.prompt, "Generate a title");
            assert_eq!(request.reasoning_level, ReasoningLevel::Low);
            assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
            assert_eq!(request.speed_mode, SpeedMode::Fast);

            Ok(agent::OneShotSubmission {
                response: AgentResponse::plain("Refine session titles"),
                stats: agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    // Act
    let title = SessionManager::run_title_generation_command(
        &folder,
        "Generate a title",
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        ReasoningLevel::Low,
        "session-id",
        SpeedMode::Fast,
        &one_shot_client,
    )
    .await;

    // Assert
    assert_eq!(title.as_deref(), Some("Refine session titles"));
}

#[tokio::test]
/// Ensures a transient provider failure is retried once before returning
/// the usable title response.
async fn test_run_title_generation_command_retries_provider_failure() {
    // Arrange
    let mut one_shot_client = MockOneShotClient::new();
    let mut attempt = 0;
    one_shot_client
        .expect_submit()
        .times(SESSION_TITLE_GENERATION_MAX_ATTEMPTS)
        .returning(move |_| {
            attempt += 1;
            if attempt == 1 {
                return Err(agent::OneShotError::new("temporary provider failure"));
            }

            Ok(agent::OneShotSubmission {
                response: AgentResponse::plain("Stabilize session titles"),
                stats: agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    // Act
    let title = SessionManager::run_title_generation_command(
        Path::new("/tmp/title-generation"),
        "Generate a title",
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        ReasoningLevel::Low,
        "session-id",
        SpeedMode::Normal,
        &one_shot_client,
    )
    .await;

    // Assert
    assert_eq!(title.as_deref(), Some("Stabilize session titles"));
}

#[tokio::test]
/// Ensures exhausted title-provider retries leave the provisional title
/// available for a later turn.
async fn test_run_title_generation_command_returns_none_after_retry_exhaustion() {
    // Arrange
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(SESSION_TITLE_GENERATION_MAX_ATTEMPTS)
        .returning(|_| Err(agent::OneShotError::new("provider unavailable")));

    // Act
    let title = SessionManager::run_title_generation_command(
        Path::new("/tmp/title-generation"),
        "Generate a title",
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        ReasoningLevel::Low,
        "session-id",
        SpeedMode::Normal,
        &one_shot_client,
    )
    .await;

    // Assert
    assert_eq!(title, None);
}
