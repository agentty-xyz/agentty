use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use app::sync;
use tempfile::tempdir;

use super::super::{App, UpdateStatus};
use super::support::{install_mock_git_client, known_session_diff_stats, test_turn_applied_state};
use crate::app;
use crate::app::core::event::{AppEvent, AppEventBatch};
use crate::app::session_state::SessionGitStatus;
use crate::domain::agent::{AgentCliInfo, AgentKind, AgentModel, AgentSelection};
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    SESSION_DATA_DIR, SessionDiffState, SessionHandles, SessionId, SessionSize, SessionStats,
    Status,
};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn test_switch_project_updates_active_git_upstream_reference() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let second_project_path = second_project_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("feature/footer-bar".to_string()) }));
    mock_git_client
        .expect_current_upstream_reference()
        .once()
        .returning(|_| Box::pin(async { Ok("origin/feature/footer-bar".to_string()) }));
    mock_git_client
        .expect_find_git_repo_root()
        .times(0..)
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert_eq!(app.git_branch(), Some("feature/footer-bar"));
    assert_eq!(app.git_upstream_ref(), Some("origin/feature/footer-bar"));
}

#[tokio::test]
async fn session_git_status_targets_use_detected_session_branch_name_when_available() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    app.sessions.push_session(review_session);
    app.sessions.replace_session_branch_names(HashMap::from([(
        SessionId::from("session-1"),
        "agentty/session-".to_string(),
    )]));

    // Act
    let targets = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "agentty/session-".to_string(),
            session_id: "session-1".into(),
        }]
    );
}

#[test]
/// Verifies that `UpdateStatusChanged` events update the event batch so
/// the reducer can apply the latest update progress state.
fn app_event_batch_collect_event_stores_update_status() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::InProgress {
            version: "v1.0.0".to_string(),
        },
    });
    event_batch.collect_event(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::Complete {
            version: "v1.0.0".to_string(),
        },
    });

    // Assert
    assert_eq!(
        event_batch.update_status,
        Some(UpdateStatus::Complete {
            version: "v1.0.0".to_string()
        })
    );
}

#[test]
/// Verifies that `AgentCliVersionsUpdated` events keep the latest
/// completed version snapshot in one reducer batch.
fn app_event_batch_collect_event_stores_agent_cli_versions() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string()),
        )],
    });
    event_batch.collect_event(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("0.139.0".to_string()),
        )],
    });

    // Assert
    assert_eq!(
        event_batch.agent_cli_updates,
        Some(vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("0.139.0".to_string())
        )])
    );
}

#[test]
fn app_event_batch_collect_event_keeps_latest_same_session_updates() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::SessionModelUpdated {
        session_id: "session-a".into(),
        session_agent: AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini38Flash),
    });
    event_batch.collect_event(AppEvent::SessionModelUpdated {
        session_id: "session-a".into(),
        session_agent: AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro),
    });
    event_batch.collect_event(AppEvent::SessionProgressUpdated {
        progress_message: Some("first".to_string()),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionProgressUpdated {
        progress_message: Some("second".to_string()),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionDiffStatsUpdated {
        diff_stats: known_session_diff_stats(1, 2, SessionSize::S),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionDiffStatsUpdated {
        diff_stats: known_session_diff_stats(8, 13, SessionSize::L),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
        generation: 1,
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
        generation: 2,
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionUpdated {
        session_id: "session-a".into(),
        version: 1,
    });
    event_batch.collect_event(AppEvent::SessionUpdated {
        session_id: "session-a".into(),
        version: 2,
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-a".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("first question")],
            Vec::new(),
            SessionStats::default(),
        ),
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-a".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("second question")],
            Vec::new(),
            SessionStats::default(),
        ),
    });

    // Assert
    assert_eq!(
        event_batch.session_model_updates.get("session-a"),
        Some(&AgentSelection::new(
            AgentKind::Gemini,
            AgentModel::Gemini31Pro
        ))
    );
    assert_eq!(
        event_batch.session_progress_updates.get("session-a"),
        Some(&Some("second".to_string()))
    );
    assert_eq!(
        event_batch.session_diff_stats_updates.get("session-a"),
        Some(&known_session_diff_stats(8, 13, SessionSize::L))
    );
    assert_eq!(
        event_batch.session_update_versions.get("session-a"),
        Some(&2)
    );
    assert_eq!(
        event_batch
            .session_title_generation_finished
            .get("session-a"),
        Some(&2)
    );
    assert_eq!(event_batch.session_ids.len(), 1);
    assert_eq!(
        event_batch
            .applied_turns
            .get("session-a")
            .map(|turn_applied_state| turn_applied_state.questions.clone()),
        Some(vec![QuestionItem::new("second question")])
    );
}

#[tokio::test]
/// Verifies that the reducer applies `UpdateStatusChanged` events to
/// `App.update_status`.
async fn apply_app_events_update_status_changed_updates_app_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    assert!(app.update_status().is_none());
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::InProgress {
            version: "v2.0.0".to_string(),
        },
    })
    .await;

    // Assert
    assert_eq!(
        app.update_status().cloned(),
        Some(UpdateStatus::InProgress {
            version: "v2.0.0".to_string()
        })
    );
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies that completed CLI version events replace startup loading
/// rows and request a redraw.
async fn apply_app_events_agent_cli_versions_updates_app_services() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.services
        .replace_available_agent_clis(vec![AgentCliInfo::loading(AgentKind::Claude)]);
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string()),
        )],
    })
    .await;

    // Assert
    assert_eq!(
        app.services.available_agent_clis(),
        vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string())
        )]
    );
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies workflow notices append to in-memory session state without
/// changing persisted transcript messages.
async fn apply_app_events_session_workflow_notice_updates_session_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    session.id = "session-1".into();
    session.status = Status::Review;
    session.transcript = Some(crate::test_support::assistant_transcript(
        "assistant output",
    ));
    app.sessions.push_session(session);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);
    app.services
        .event_sender()
        .send(AppEvent::SessionWorkflowNoticeUpdated {
            notice: "[Merge] Successfully merged wt/session-1 into main".to_string(),
            session_id: "session-1".into(),
        })
        .expect("queued workflow notice should send");
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::SessionWorkflowNoticeUpdated {
        notice: "[Commit] No changes to commit.".to_string(),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "session-1")
        .expect("session should exist");
    assert_eq!(
        session
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
            .map(|message| message.body.text()),
        Some(
            "[Commit] No changes to commit.\n\n[Merge] Successfully merged wt/session-1 into main"
        )
    );
    assert!(app.pending_session_diff_requests.is_empty());
    assert_eq!(
        session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .as_deref(),
        Some("assistant output\n\n")
    );
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies orchestration progress updates the board without transcript noise.
async fn apply_app_events_orchestration_progress_updates_board_snapshot() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/controller"));
    session.id = "controller".into();
    app.sessions.push_session(session);

    // Act
    app.apply_app_events(AppEvent::SessionOrchestrationProgressUpdated {
        progress: Some("Working... Protocol: running".to_string()),
        session_id: "controller".into(),
    })
    .await;

    // Assert
    let controller = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "controller")
        .expect("controller should remain loaded");
    assert_eq!(
        controller.orchestration_progress.as_deref(),
        Some("Working... Protocol: running")
    );
    assert!(
        controller
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::Orchestration)
            .is_none()
    );

    // Act
    app.apply_app_events(AppEvent::SessionOrchestrationProgressUpdated {
        progress: None,
        session_id: "controller".into(),
    })
    .await;

    // Assert
    assert!(
        app.sessions
            .sessions()
            .iter()
            .find(|session| session.id == "controller")
            .is_some_and(|session| session.orchestration_progress.is_none())
    );
}

#[tokio::test]
/// Verifies stale `SessionUpdated` versions do not re-arm redraw when the
/// reducer has already applied that handle snapshot.
async fn apply_app_events_session_updated_same_version_keeps_redraw_clean() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 7,
    })
    .await;
    app.clear_redraw();
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 7,
    })
    .await;

    // Assert
    assert!(!app.needs_redraw());
}

#[tokio::test]
/// Verifies that one combined git-status event updates the in-memory
/// session snapshot cache.
async fn apply_app_events_git_status_updated_updates_project_and_session_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-git-status"),
        ));

    // Act
    app.apply_app_events(AppEvent::GitStatusUpdated {
        generation: app.sync_handle.current_generation(),
        session_statuses: HashMap::from([(
            SessionId::from("session-1"),
            SessionGitStatus {
                base_status: Some((4, 2)),
                has_merge_conflict: Some(true),
                remote_status: Some((1, 0)),
            },
        )]),
        status: Some((1, 3)),
    })
    .await;

    // Assert
    assert_eq!(app.git_status_info(), Some((1, 3)));
    assert_eq!(
        app.sessions
            .render_parts()
            .session_git_statuses
            .get("session-1"),
        Some(&SessionGitStatus {
            base_status: Some((4, 2)),
            has_merge_conflict: Some(true),
            remote_status: Some((1, 0)),
        })
    );
}

#[tokio::test]
/// Verifies stale git-status snapshots do not overwrite the current sync
/// generation.
async fn apply_app_events_git_status_updated_ignores_stale_generation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.publish_sync_context_for_refresh();
    let stale_generation = app.sync_handle.current_generation().saturating_sub(1);

    // Act
    app.apply_app_events(AppEvent::GitStatusUpdated {
        generation: stale_generation,
        session_statuses: HashMap::new(),
        status: Some((9, 9)),
    })
    .await;

    // Assert
    assert_eq!(app.git_status_info(), None);
    assert!(app.sessions.render_parts().session_git_statuses.is_empty());
}

#[tokio::test]
/// Verifies explicit git-status refresh events request an immediate
/// orchestrator pass instead of waiting for the periodic cadence.
async fn apply_app_events_refresh_git_status_requests_orchestrator_refresh() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(|dir| Box::pin(async move { Some(dir) }));
    mock_git_client
        .expect_fetch_remote()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(1)
        .returning(|_| {
            Box::pin(async { Ok(HashMap::from([("main".to_string(), Some((2_u32, 1_u32)))])) })
        });
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::RefreshGitStatus).await;
    let mut observed_events = vec![
        tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
            .await
            .expect("first app event should arrive")
            .expect("app event channel should remain open"),
    ];
    if !observed_events
        .iter()
        .any(|event| matches!(event, AppEvent::GitStatusUpdated { .. }))
    {
        let next_event = tokio::time::timeout(Duration::from_secs(1), app.next_app_event()).await;
        assert!(
            next_event.is_ok(),
            "git status refresh event should arrive after observed events: {observed_events:?}"
        );
        let next_event = next_event
            .expect("git status refresh timeout should be checked")
            .expect("app event channel should remain open");
        observed_events.push(next_event);
    }

    // Assert
    assert!(
        observed_events.contains(&AppEvent::GitStatusUpdated {
            generation: app.sync_handle.current_generation(),
            session_statuses: HashMap::new(),
            status: Some((2, 1)),
        }),
        "expected git status update among observed events: {observed_events:?}"
    );
}

#[tokio::test]
async fn apply_app_events_agent_response_keeps_list_mode_when_not_viewing_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("Need context?")],
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
/// Verifies one reducer tick preserves the latest turn projection while
/// accumulating token usage from multiple queued completions.
async fn apply_app_events_agent_response_batches_same_session_turns() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let event_sender = app.services.event_sender();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-batched-turns"),
        ));

    let first_turn = test_turn_applied_state(
        vec![QuestionItem::new("First question?")],
        Vec::new(),
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: 2,
            output_tokens: 3,
        },
    );
    let second_turn = test_turn_applied_state(
        vec![QuestionItem::new("Latest question?")],
        vec!["Capture reducer batching coverage."],
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: 5,
            output_tokens: 8,
        },
    );

    event_sender
        .send(AppEvent::AgentResponseReceived {
            session_id: "session-1".into(),
            turn_applied_state: second_turn,
        })
        .expect("queued event should send");

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: first_turn,
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].questions,
        vec![QuestionItem::new("Latest question?")]
    );
    assert_eq!(app.sessions.sessions()[0].stats.input_tokens, 7);
    assert_eq!(app.sessions.sessions()[0].stats.output_tokens, 11);
    assert_eq!(
        app.sessions.sessions()[0]
            .follow_up_tasks
            .iter()
            .map(|task| task.text.clone())
            .collect::<Vec<_>>(),
        vec!["Capture reducer batching coverage.".to_string()]
    );
}

#[tokio::test]
/// Verifies refresh keeps the active session view when merge cleanup has
/// removed the worktree just before `Done` persists.
async fn apply_app_events_refresh_keeps_viewed_merging_session_without_worktree() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-1",
            AgentModel::Gemini38Flash.as_str(),
            "main",
            &Status::Merging.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert merging session");

    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let session_folder = base_path.join("session-1");
    let mut viewed_session = crate::test_support::session_fixture_with_folder(session_folder);
    viewed_session.status = Status::Merging;
    app.sessions.push_session(viewed_session);
    app.sessions.session_handles_mut().insert(
        "session-1".into(),
        SessionHandles::new_with_transcript(
            Status::Merging,
            crate::test_support::assistant_transcript("Merging"),
        ),
    );
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: None,
    };

    // Act
    app.apply_app_events(AppEvent::RefreshSessions).await;

    // Assert
    assert!(
        app.sessions
            .sessions()
            .iter()
            .any(|session| session.id == "session-1" && session.status == Status::Merging)
    );
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id, ..
        } if session_id == "session-1"
    ));
}

#[tokio::test]
async fn apply_app_events_refresh_projects_reloads_project_active_session_count() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    fs::create_dir_all(base_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-active",
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert active session");

    let session_folder_name = "session-".chars().take(8).collect::<String>();
    let session_data_dir = base_path.join(session_folder_name).join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session dir");

    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    let initial_active_count = app
        .projects
        .render_parts()
        .project_items
        .iter()
        .find(|item| item.project.id == project_id)
        .map_or(0, |item| item.active_session_count);
    assert_eq!(initial_active_count, 1);

    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("session-active", &Status::Done.to_string(), 0)
        .await
        .expect("failed to update session status");

    // Act
    app.apply_app_events(AppEvent::RefreshProjects).await;

    // Assert
    let updated_active_count = app
        .projects
        .render_parts()
        .project_items
        .iter()
        .find(|item| item.project.id == project_id)
        .map_or(0, |item| item.active_session_count);
    assert_eq!(updated_active_count, 0);
}
