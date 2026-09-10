use std::fs;

use super::support::{test_app_viewing_reconcile_session, test_prompt_mode_snapshot};
use crate::app::core::event::{AppEvent, AppEventBatch};
use crate::domain::agent::AgentModel;
use crate::domain::file_entry::FileEntry;
use crate::domain::session::{SessionId, Status};
use crate::domain::session_message::SessionMessageKind;

#[tokio::test]
async fn session_chat_history_loads_persisted_transcript_for_unloaded_session() {
    // Arrange
    let (app, _base_dir) = crate::test_support::new_test_app().await;
    let session_id = "unloaded-review-history";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gpt56Sol.as_str(),
            "main",
            "Review",
            app.active_project_id(),
        )
        .await
        .expect("failed to insert unloaded review session");
    app.services
        .db()
        .sessions()
        .append_session_message(
            session_id,
            SessionMessageKind::UserPrompt,
            "Keep the accepted tradeoff",
        )
        .await
        .expect("failed to persist review prompt");
    app.services
        .db()
        .sessions()
        .append_session_message(
            session_id,
            SessionMessageKind::AssistantAnswer,
            "The accepted tradeoff remains in place.",
        )
        .await
        .expect("failed to persist review answer");
    assert!(app.sessions.session_for_id(session_id).is_none());

    // Act
    let session_chat_history = app.session_chat_history(session_id).await;

    // Assert
    assert_eq!(
        session_chat_history.as_deref(),
        Some(" › Keep the accepted tradeoff\n\nThe accepted tradeoff remains in place.\n\n")
    );
}

#[tokio::test]
async fn at_mention_lookup_root_uses_nearest_materialized_stacked_ancestor() {
    // Arrange
    let (mut app, temp_dir) = crate::test_support::new_test_app().await;
    let ancestor_folder = temp_dir.path().join("materialized-ancestor");
    fs::create_dir(&ancestor_folder).expect("failed to create ancestor folder");
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("ancestor-session")
            .folder(ancestor_folder.clone())
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("unmaterialized-parent")
            .folder(temp_dir.path().join("missing-parent"))
            .parent_session_id(Some(SessionId::from("ancestor-session")))
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("unmaterialized-child")
            .folder(temp_dir.path().join("missing-child"))
            .parent_session_id(Some(SessionId::from("unmaterialized-parent")))
            .build(),
    );

    // Act
    let lookup_root = app.at_mention_lookup_root("unmaterialized-child");

    // Assert
    assert_eq!(lookup_root, ancestor_folder);
}

#[tokio::test]
async fn at_mention_lookup_root_falls_back_for_cyclic_parent_chain() {
    // Arrange
    let (mut app, temp_dir) = crate::test_support::new_test_app().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("first-session")
            .folder(temp_dir.path().join("missing-first"))
            .parent_session_id(Some(SessionId::from("second-session")))
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("second-session")
            .folder(temp_dir.path().join("missing-second"))
            .parent_session_id(Some(SessionId::from("first-session")))
            .build(),
    );

    // Act
    let lookup_root = app.at_mention_lookup_root("first-session");

    // Assert
    assert_eq!(lookup_root, app.working_dir());
}

#[test]
fn app_event_batch_collect_event_keeps_latest_at_mention_entries_update() {
    // Arrange
    let mut event_batch = AppEventBatch::default();
    let first_entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];
    let second_entries = vec![FileEntry {
        is_dir: true,
        path: "crates".to_string(),
    }];

    // Act
    event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
        entries: first_entries,
        session_id: "session-1".into(),
    });
    event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
        entries: second_entries.clone(),
        session_id: "session-1".into(),
    });

    // Assert
    assert_eq!(
        event_batch
            .at_mention_entries_updates
            .get("session-1")
            .cloned(),
        Some(second_entries)
    );
}

#[tokio::test]
async fn app_event_applies_at_mention_entries_to_session_runtime() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    let session_id = SessionId::from("missing-session");
    let lookup_root = app.at_mention_lookup_root(&session_id);
    let entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];

    // Act
    app.apply_app_events(AppEvent::AtMentionEntriesLoaded {
        entries: entries.clone(),
        session_id,
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.at_mention_index_for_root(&lookup_root),
        Some(entries)
    );
}

#[tokio::test]
async fn restore_prompt_progress_returns_false_without_saved_snapshot() {
    // Arrange
    let mut app = test_app_viewing_reconcile_session(
        Status::Review,
        Vec::new(),
        "session-without-prompt-progress",
    )
    .await;

    // Act
    let restored = app.restore_prompt_progress("session-1").await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.is_empty());
}

#[tokio::test]
async fn restore_prompt_progress_retains_snapshot_when_session_is_missing() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = SessionId::from("missing-session");
    app.save_prompt_progress(test_prompt_mode_snapshot(session_id.clone()));

    // Act
    let restored = app.restore_prompt_progress(&session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.contains_key(&session_id));
}

#[tokio::test]
async fn restore_prompt_progress_retains_snapshot_while_stack_reply_is_blocked() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let parent_session_id = SessionId::from("parent-session");
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(parent_session_id.clone())
            .status(Status::Review)
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("active-child")
            .parent_session_id(Some(parent_session_id.clone()))
            .status(Status::InProgress)
            .build(),
    );
    app.save_prompt_progress(test_prompt_mode_snapshot(parent_session_id.clone()));

    // Act
    let restored = app.restore_prompt_progress(&parent_session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.contains_key(&parent_session_id));
}
