//! `App` prompt unit tests for chat history, slash options, at-mentions, and
//! saved prompt progress.

use std::fs;
use std::time::Duration;

use ag_session::test_support as model_fixture;
use tokio::time::{Instant, timeout_at};

use super::support::{test_app_viewing_reconcile_session, test_prompt_mode_snapshot};
use crate::app::App;
use crate::app::core::event::{AppEvent, AppEventBatch};
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputState;
use crate::domain::session::{SessionId, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::presentation::app_mode::AppMode;
use crate::presentation::prompt::{PromptAtMentionState, PromptGraphState};

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
            model_fixture::CODEX_MODEL.as_str(),
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
async fn prompt_slash_state_offers_graph_by_default() {
    // Arrange
    let (app, _base_dir) = crate::test_support::new_test_app().await;

    // Act
    let slash_state = app.prompt_slash_state("session");

    // Assert
    assert_eq!(slash_state.graph, PromptGraphState::Available);
}

#[tokio::test]
async fn at_mention_lookup_root_uses_materialized_session_folder() {
    // Arrange
    let (mut app, temp_dir) = crate::test_support::new_test_app().await;
    let session_folder = temp_dir.path().join("materialized-session");
    fs::create_dir(&session_folder).expect("failed to create session folder");
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("materialized-session")
            .folder(session_folder.clone())
            .build(),
    );

    // Act
    let lookup_root = app.at_mention_lookup_root("materialized-session");

    // Assert
    assert_eq!(lookup_root, session_folder);
}

#[tokio::test]
async fn at_mention_lookup_root_falls_back_for_unmaterialized_session() {
    // Arrange
    let (mut app, temp_dir) = crate::test_support::new_test_app().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("unmaterialized-session")
            .folder(temp_dir.path().join("missing-session"))
            .build(),
    );

    // Act
    let lookup_root = app.at_mention_lookup_root("unmaterialized-session");
    let missing_session_root = app.at_mention_lookup_root("missing-session");

    // Assert
    assert_eq!(lookup_root, app.working_dir());
    assert_eq!(missing_session_root, app.working_dir());
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
async fn request_visible_at_mention_descriptions_applies_descriptions_to_open_lookup() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    let session_id = SessionId::from("missing-session");
    let lookup_root = app.at_mention_lookup_root(&session_id);
    fs::write(lookup_root.join("lib.rs"), "//! Library root.\n")
        .expect("failed to write described file");
    app.mode = prompt_mode_with_lookup(session_id, "@lib");

    // Act
    app.request_visible_at_mention_descriptions();
    app.request_visible_at_mention_descriptions();
    let descriptions_event = wait_for_at_mention_descriptions_event(&mut app).await;
    app.apply_single_app_event(descriptions_event).await;

    // Assert
    let active = app
        .mode
        .active_at_mention_mut()
        .expect("prompt lookup should stay open");
    assert_eq!(
        active.state.file_description("lib.rs"),
        Some("Library root.")
    );
}

#[tokio::test]
async fn app_event_ignores_at_mention_descriptions_for_another_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = prompt_mode_with_lookup(SessionId::from("open-session"), "@lib");

    // Act
    app.apply_single_app_event(AppEvent::AtMentionDescriptionsLoaded {
        descriptions: vec![("lib.rs".to_string(), Some("Library root.".to_string()))],
        session_id: SessionId::from("other-session"),
    })
    .await;

    // Assert
    let active = app
        .mode
        .active_at_mention_mut()
        .expect("prompt lookup should stay open");
    assert_eq!(active.state.file_description("lib.rs"), None);
}

#[tokio::test]
async fn request_visible_at_mention_descriptions_waits_for_an_active_query() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = SessionId::from("missing-session");
    app.request_visible_at_mention_descriptions();
    app.mode = prompt_mode_with_lookup(session_id, "lib");

    // Act
    app.request_visible_at_mention_descriptions();

    // Assert
    let active = app
        .mode
        .active_at_mention_mut()
        .expect("prompt lookup should stay open");
    assert_eq!(
        active.state.take_description_requests("lib", 10),
        vec!["lib.rs".to_string()]
    );
}

/// Builds prompt mode with `input_text` and an open lookup over `lib.rs`.
fn prompt_mode_with_lookup(session_id: SessionId, input_text: &str) -> AppMode {
    let mut snapshot = test_prompt_mode_snapshot(session_id);
    snapshot.input = InputState::with_text(input_text.to_string());
    snapshot.at_mention_state = Some(PromptAtMentionState::new(vec![FileEntry {
        is_dir: false,
        path: "lib.rs".to_string(),
    }]));

    snapshot.into_prompt_mode()
}

/// Waits for the next description load and skips unrelated startup events.
async fn wait_for_at_mention_descriptions_event(app: &mut App) -> AppEvent {
    let deadline = Instant::now() + Duration::from_secs(1);

    loop {
        let next_event = timeout_at(deadline, app.next_app_event())
            .await
            .expect("description event should arrive")
            .expect("app event channel closed unexpectedly");

        if matches!(next_event, AppEvent::AtMentionDescriptionsLoaded { .. }) {
            return next_event;
        }
    }
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
