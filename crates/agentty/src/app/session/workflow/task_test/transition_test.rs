use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_agent::MockOneShotClient;
use ag_git::{GitError, MockGitClient};
use tokio::sync::mpsc;

use super::super::{
    AUTO_COMMIT_ERROR_TRUNCATED_SECTION_MARKER, AutoCommitOutcome,
    SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER, SessionTaskService,
    append_agentty_coauthor_trailer, compact_commit_error_for_assist,
};
use super::support::{commit_fallback_transcript, insert_review_session, one_shot_submission};
use crate::app::AppEvent;
use crate::app::assist::AssistContext;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::session::COMMITTING_PROGRESS_LABEL;
use crate::domain::session_message::SessionTranscript;

#[tokio::test]
/// Verifies commit helper failure appends a commit error message without
/// invoking real git or agent subprocesses.
async fn test_handle_auto_commit_appends_commit_error_from_mock_git_client() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Err(GitError::OutputParse("commit failed".to_string())) }));
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .once()
        .returning(|_| Err(agent::OneShotError::new("commit failed")));
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(mock_git_client),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    let outcome = SessionTaskService::handle_auto_commit(context).await;

    // Assert
    let output_text = transcript
        .lock()
        .ok()
        .and_then(|buffer| buffer.replay_text())
        .unwrap_or_default();
    assert!(output_text.contains("[Commit Error] commit failed"));
    assert!(matches!(outcome, AutoCommitOutcome::Failed));
}

#[tokio::test]
/// Verifies persistent index contention ends auto-commit with recovery
/// guidance and clears progress without asking an agent to repair it.
async fn test_handle_auto_commit_stops_on_index_lock() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_diff()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("+pending change".to_string()) }));
    mock_git_client
        .expect_has_commits_since()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .times(1)
        .returning(|_, _, _, _| {
            Box::pin(async {
                Err(GitError::CommandFailed {
                    command: "git add -A".to_string(),
                    stderr: "fatal: Unable to create '.git/worktrees/session/index.lock': File \
                             exists."
                        .to_string(),
                })
            })
        });
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(|request| {
            assert!(
                request
                    .prompt
                    .contains("Generate the canonical session commit message")
            );

            Ok(one_shot_submission("Preserve pending changes", 0, 0))
        });
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: PathBuf::from("project"),
        git_client: Arc::new(mock_git_client),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    let outcome = SessionTaskService::handle_auto_commit(context).await;

    // Assert
    assert!(matches!(outcome, AutoCommitOutcome::Failed));
    let messages = database
        .sessions()
        .load_session_messages("session-id")
        .await
        .expect("persisted messages should load");
    assert_eq!(messages.len(), 1);
    let message = &messages[0].content;
    assert!(message.contains("[Commit Error] Auto-commit blocked by a Git index lock"));
    assert!(message.contains("confirm it is stale before removing it"));
    assert!(message.contains("left the lock and your changes intact"));
    assert!(message.contains("git add -A: fatal: Unable to create"));
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.contains(&AppEvent::SessionProgressUpdated {
        progress_message: None,
        session_id: "session-id".into(),
    }));
}

#[tokio::test]
/// Verifies a missing configured hook emits an advisory after a successful
/// normal commit instead of turning the commit into a failure.
async fn test_handle_auto_commit_warns_when_pre_commit_hook_is_missing() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok::<_, GitError>(false) }));
    mock_git_client
        .expect_diff()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
    mock_git_client
        .expect_has_commits_since()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok::<_, GitError>(true) }));
    mock_git_client
        .expect_head_commit_message()
        .times(1)
        .returning(|_| Box::pin(async { Ok(Some("Update project".to_string())) }));
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .times(1)
        .returning(|_, _, _, _| Box::pin(async { Ok::<_, GitError>(()) }));
    mock_git_client
        .expect_head_short_hash()
        .times(1)
        .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
    mock_git_client
        .expect_check_pre_commit_hook_ready()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Err(GitError::PreCommitHookMissing {
                    config_file: ".pre-commit-config.yaml".to_string(),
                })
            })
        });
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(|_| Ok(one_shot_submission("Update project", 0, 0)));
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(mock_git_client),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    let outcome = SessionTaskService::handle_auto_commit(context).await;

    // Assert
    assert!(matches!(outcome, AutoCommitOutcome::Committed(_)));
    let output_text = transcript
        .lock()
        .ok()
        .and_then(|buffer| buffer.replay_text())
        .unwrap_or_default();
    assert!(output_text.contains("[Commit Warning]"));
    assert!(output_text.contains("prek install"));
    assert!(output_text.contains("pre-commit install"));
    assert!(output_text.contains("will become an error in a future release"));
    assert!(!output_text.contains("[Commit Error]"));
}

#[tokio::test]
/// Verifies auto-commit reports clean-worktree no-op commits as transient
/// workflow notices without appending to the transcript.
async fn test_handle_auto_commit_reports_when_no_changes_exist() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok::<_, GitError>(true) }));
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(mock_git_client),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(MockOneShotClient::new()),
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    let outcome = SessionTaskService::handle_auto_commit(context).await;

    // Assert
    let output_text = transcript
        .lock()
        .ok()
        .and_then(|buffer| buffer.replay_text())
        .unwrap_or_default();
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(!output_text.contains("[Commit] No changes to commit."));
    assert!(events.contains(&AppEvent::SessionProgressUpdated {
        progress_message: Some(COMMITTING_PROGRESS_LABEL.to_string()),
        session_id: "session-id".into(),
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::SessionWorkflowNoticeUpdated {
            notice,
            session_id,
        } if session_id.as_str() == "session-id"
            && notice == "[Commit] No changes to commit."
    )));
    assert!(events.contains(&AppEvent::SessionProgressUpdated {
        progress_message: None,
        session_id: "session-id".into(),
    }));
    assert!(matches!(outcome, AutoCommitOutcome::NoChanges));
}

#[tokio::test]
async fn commit_generation_summarizes_oversized_diff_and_existing_message() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(|request| {
        assert!(request.prompt.len() <= 60_000);
        assert_eq!(request.permission_mode, agent::PermissionMode::ReadOnly);
        if request.prompt.starts_with("Summarize") {
            return Ok(one_shot_submission(
                "Preserve changes to all affected files",
                0,
                0,
            ));
        }
        assert!(request.prompt.contains("Summarized input"));
        Ok(one_shot_submission("Handle large changes", 0, 0))
    });

    // Act
    let message = SessionTaskService::generate_session_commit_message_with_client(
        Path::new("."),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            ReasoningLevel::Medium,
            crate::domain::agent::SpeedMode::Normal,
        ),
        &"+change\n".repeat(160_000),
        Some(&"previous message\n".repeat(10_000)),
        &client,
        true,
        false,
    )
    .await
    .expect("operation should succeed");

    // Assert
    assert_eq!(
        message,
        format!("Handle large changes\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}")
    );
}

#[tokio::test]
/// Auto-commit assistance preserves the retained runtime PID on success
/// and failure, even when a one-shot client clears its cancellation slot.
async fn test_commit_assist_preserves_retained_runtime_accounting() {
    // Arrange
    for assist_fails in [false, true] {
        let database = AppRepositories::in_memory().await.expect("db should open");
        insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
        let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
        let child_pid = Arc::new(Mutex::new(Some(4242)));
        let mut one_shot_client = MockOneShotClient::new();
        one_shot_client
            .expect_submit()
            .times(1)
            .returning(move |request| {
                assert!(request.prompt.contains("commit failed"));
                assert!(
                    request.child_pid.is_none(),
                    "isolated runtime must not receive the session PID slot"
                );
                if assist_fails {
                    Err(agent::OneShotError::new("assist failed"))
                } else {
                    Ok(one_shot_submission("Fixed the commit failure", 0, 0))
                }
            });
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let context = AssistContext {
            app_event_tx,
            child_pid: Arc::clone(&child_pid),
            db: database,
            folder: PathBuf::from("project"),
            git_client: Arc::new(MockGitClient::new()),
            id: "session-id".to_string(),
            one_shot_client: Arc::new(one_shot_client),
            session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            session_update_versions: Arc::default(),
            transcript: Arc::clone(&transcript),
        };

        // Act
        let result =
            SessionTaskService::run_commit_assist_for_error(&context, "commit failed").await;

        // Assert
        assert_eq!(result.is_err(), assist_fails);
        assert_eq!(*child_pid.lock().expect("retained runtime PID"), Some(4242));
        let replay_text = transcript.lock().expect("transcript lock").replay_text();
        if assist_fails {
            assert!(replay_text.is_none());
        } else {
            assert_eq!(replay_text.as_deref(), Some("Fixed the commit failure\n\n"));
        }
    }
}

#[tokio::test]
/// Verifies plain-text one-shot output is rejected for session commit
/// message generation after both the original parse and the
/// protocol-repair retry fail.
async fn test_generate_session_commit_message_with_client_rejects_submission_error() {
    // Arrange
    let temp_directory = tempfile::tempdir().expect("failed to create temp dir");
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().returning(|_| {
        Err(agent::OneShotError::new(
            "One-shot agent output did not match the required JSON schema\nresponse:\nRefactor \
             agent prompt and protocol handling",
        ))
    });

    // Act
    let error = SessionTaskService::generate_session_commit_message_with_client(
        temp_directory.path(),
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
            ReasoningLevel::Low,
            crate::domain::agent::SpeedMode::Normal,
        ),
        "diff --git a/a.rs b/a.rs",
        None,
        &one_shot_client,
        false,
        false,
    )
    .await
    .expect_err("plain-text one-shot commit message should fail");

    // Assert
    assert!(
        error
            .to_string()
            .contains("did not match the required JSON schema")
    );
    assert!(
        error
            .to_string()
            .contains("response:\nRefactor agent prompt and protocol handling")
    );
}

#[tokio::test]
/// Verifies blank commit-message protocol output falls back to the
/// continuity title and keeps auto-commit progressing.
async fn test_generate_session_commit_message_with_client_falls_back_for_blank_answer() {
    // Arrange
    let temp_directory = tempfile::tempdir().expect("failed to create temp dir");
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().returning(|request| {
        assert_eq!(request.reasoning_level, ReasoningLevel::XHigh);
        assert_eq!(request.speed_mode, crate::domain::agent::SpeedMode::Fast);

        Ok(one_shot_submission("", 0, 0))
    });

    // Act
    let generated_message = SessionTaskService::generate_session_commit_message_with_client(
        temp_directory.path(),
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
            ReasoningLevel::XHigh,
            crate::domain::agent::SpeedMode::Fast,
        ),
        "diff --git a/a.rs b/a.rs",
        Some("Keep session commit accurate\n\n- Preserve existing behavior"),
        &one_shot_client,
        false,
        false,
    )
    .await
    .expect("blank answer should fall back to continuity title");

    // Assert
    assert_eq!(generated_message, "Keep session commit accurate");
}

#[test]
/// Ensures commit error formatting normalizes output as bullet lines.
fn test_format_commit_error_for_display_returns_bulleted_lines() {
    // Arrange
    let commit_error = "line one\nline two";

    // Act
    let formatted = SessionTaskService::format_commit_error_for_display(commit_error);

    // Assert
    assert_eq!(formatted, "- line one\n- line two");
}

#[test]
/// Verifies long context-window overflow messages are compacted for
/// auto-commit assistance prompts.
fn test_compact_commit_error_for_assist_truncates_overflow_messages() {
    // Arrange
    let commit_error = "contextWindowExceeded\n".repeat(10_000);

    // Act
    let compacted = compact_commit_error_for_assist(&commit_error);

    // Assert
    assert!(compacted.len() < commit_error.len());
    assert!(compacted.contains(AUTO_COMMIT_ERROR_TRUNCATED_SECTION_MARKER));
}

#[test]
/// Verifies non-window-overflow messages are left unchanged.
fn test_compact_commit_error_for_assist_keeps_non_overflow_messages() {
    // Arrange
    let commit_error = "network timeout while pushing";

    // Act
    let compacted = compact_commit_error_for_assist(commit_error);

    // Assert
    assert_eq!(compacted, commit_error);
}

/// Covers provider rejection and failed large-diff reduction, using only
/// filenames and the user/assistant conversation on the successful fallback
/// attempt.
#[tokio::test]
async fn test_commit_session_changes_falls_back_to_files_and_chat() {
    for oversized_diff in [false, true] {
        // Arrange
        let mut git_client = MockGitClient::new();
        git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        git_client.expect_diff().times(1).returning(move |_, _| {
            Box::pin(async move {
                Ok("+DIFF_ONLY_SECRET\n".repeat(if oversized_diff { 10_000 } else { 1 }))
            })
        });
        git_client
            .expect_has_commits_since()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(false) }));
        git_client
            .expect_diff_changed_files()
            .times(1)
            .withf(|folder, base| folder == Path::new("project") && base == "main")
            .returning(|_, _| {
                Box::pin(async {
                    Ok(vec![
                        "src/old name.rs".into(),
                        "src/new.rs".into(),
                        "untracked.md".into(),
                    ])
                })
            });
        git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .withf(|_, _, message, strategy| {
                message == "Recover oversized commits"
                    && *strategy == ag_git::SingleCommitMessageStrategy::Replace
            })
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        git_client
            .expect_head_short_hash()
            .times(1)
            .returning(|_| Box::pin(async { Ok("abc123".into()) }));
        let mut client = MockOneShotClient::new();
        client
            .expect_submit()
            .times(if oversized_diff { 2..=64 } else { 1..=1 })
            .withf(|request| request.prompt.contains("DIFF_ONLY_SECRET"))
            .returning(move |request| {
                if oversized_diff {
                    assert!(request.prompt.contains("Summarize this fragment"));
                    request
                        .provider_call_budget
                        .expect("shared budget")
                        .consume()?;
                    return Ok(one_shot_submission("", 0, 0));
                }
                Err(agent::OneShotError::new("Input exceeds the maximum length"))
            });
        client
            .expect_submit()
            .times(1)
            .withf(|request| request.prompt.contains("Use only the changed file list"))
            .returning(|request| {
                assert_eq!(request.permission_mode, agent::PermissionMode::ReadOnly);
                assert_eq!(request.reasoning_level, ReasoningLevel::Low);
                assert!(
                    request
                        .prompt
                        .contains("Do not retrieve or inspect a Git diff")
                );
                for expected in [
                    "src/old name.rs",
                    "src/new.rs",
                    "untracked.md",
                    "Please recover oversized commits",
                    "Implemented commit fallback",
                ] {
                    assert!(request.prompt.contains(expected));
                }
                assert!(!request.prompt.contains("DIFF_ONLY_SECRET"));
                assert!(!request.prompt.contains("INTERNAL_NOTICE"));
                assert!(!request.prompt.contains("```diff"));
                Ok(one_shot_submission("Recover oversized commits", 0, 0))
            });

        // Act
        let outcome = SessionTaskService::commit_session_changes(
            &git_client,
            Path::new("project"),
            "main",
            (
                AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
                ReasoningLevel::Low,
                SpeedMode::Normal,
            ),
            &client,
            false,
            &commit_fallback_transcript(),
        )
        .await
        .expect("files and chat should recover commit generation");

        // Assert
        assert_eq!(outcome.commit_message, "Recover oversized commits");
        assert_eq!(outcome.commit_hash, "abc123");
    }
}

#[tokio::test]
/// Verifies repeated successful commits persist one copy of an unchanged
/// missing-hook warning in the session transcript.
async fn test_append_pre_commit_hook_warning_ignores_duplicate_advisory() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_check_pre_commit_hook_ready()
        .times(2)
        .returning(|_| {
            Box::pin(async {
                Err(GitError::PreCommitHookMissing {
                    config_file: ".pre-commit-config.yaml".to_string(),
                })
            })
        });
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(mock_git_client),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(MockOneShotClient::new()),
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    SessionTaskService::append_pre_commit_hook_warning(&context).await;
    SessionTaskService::append_pre_commit_hook_warning(&context).await;

    // Assert
    {
        let transcript = transcript
            .lock()
            .expect("transcript lock should not be poisoned");
        assert_eq!(transcript.messages().len(), 1);
        assert!(
            transcript.messages()[0]
                .content
                .contains("[Commit Warning]")
        );
    }

    let messages = database
        .sessions()
        .load_session_messages("session-id")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains("[Commit Warning]"));
}

/// Keeps earlier commit details available when the chat no longer describes
/// them.
#[tokio::test]
async fn test_commit_fallback_preserves_existing_message_continuity() {
    // Arrange
    let previous_message = "Preserve earlier work\n\n- Keep the earlier API behavior";
    let revised_message = format!("{previous_message}\n- Recover oversized commits");
    let expected_message = append_agentty_coauthor_trailer(&revised_message, true);
    let mut git_client = MockGitClient::new();
    git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    git_client
        .expect_diff()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("+DIFF_ONLY_SECRET".into()) }));
    git_client
        .expect_has_commits_since()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(true) }));
    git_client
        .expect_head_commit_message()
        .times(1)
        .returning(move |_| {
            Box::pin(async move {
                Ok(Some(append_agentty_coauthor_trailer(
                    previous_message,
                    true,
                )))
            })
        });
    git_client
        .expect_diff_changed_files()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(vec!["src/new.rs".into()]) }));
    let committed_message = expected_message.clone();
    git_client
        .expect_commit_all_preserving_single_commit()
        .times(1)
        .withf(move |_, _, message, strategy| {
            message == &committed_message
                && *strategy == ag_git::SingleCommitMessageStrategy::Replace
        })
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    git_client
        .expect_head_short_hash()
        .times(1)
        .returning(|_| Box::pin(async { Ok("abc123".into()) }));
    let mut client = MockOneShotClient::new();
    let mut sequence = mockall::Sequence::new();
    client
        .expect_submit()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(agent::OneShotError::new("Input exceeds the maximum length")));
    client
        .expect_submit()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |request| {
            assert!(request.prompt.contains(previous_message));
            assert!(request.prompt.contains("Preserve previously documented"));
            assert!(request.prompt.contains("src/new.rs"));
            assert!(request.prompt.contains("Implemented commit fallback"));
            assert!(!request.prompt.contains("DIFF_ONLY_SECRET"));
            assert!(
                !request
                    .prompt
                    .contains(SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER)
            );
            assert_eq!(request.permission_mode, agent::PermissionMode::ReadOnly);
            Ok(one_shot_submission(&revised_message, 0, 0))
        });

    // Act
    let outcome = SessionTaskService::commit_session_changes(
        &git_client,
        Path::new("project"),
        "main",
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            ReasoningLevel::Low,
            SpeedMode::Normal,
        ),
        &client,
        true,
        &commit_fallback_transcript(),
    )
    .await
    .expect("fallback should retain the previous commit details");

    // Assert
    assert_eq!(outcome.commit_message, expected_message);
}
