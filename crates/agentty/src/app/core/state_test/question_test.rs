use std::path::PathBuf;
use std::sync::Arc;

use super::support::{
    test_app_viewing_reconcile_session, test_prompt_mode_snapshot, test_turn_applied_state,
};
use crate::app::core::event::AppEvent;
use crate::domain::input::InputState;
use crate::domain::question::{QuestionItem, QuestionProgress};
use crate::domain::session::{SessionDiffState, SessionId, SessionStats, Status};
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn reconcile_open_session_question_mode_enters_question_mode_from_view() {
    // Arrange — a viewed session reached `Question` status with pending
    // questions, but the view was never flipped into the clarification panel
    // (for example the live projection was missed while an overlay was open).
    let pending_questions = vec![
        QuestionItem::with_options("Need a target branch?", vec!["main".to_string()]),
        QuestionItem::new("Need integration tests?"),
    ];
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        pending_questions.clone(),
        "session-question-reconcile",
    )
    .await;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            questions: ref mode_questions,
            current_index: 0,
            ..
        } if session_id == "session-1" && mode_questions == &pending_questions
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_ignores_non_question_status() {
    // Arrange — the viewed session is in `Review`, not awaiting a question.
    let mut app =
        test_app_viewing_reconcile_session(Status::Review, Vec::new(), "session-review-reconcile")
            .await;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert — the view is preserved.
    assert!(matches!(
        app.mode,
        AppMode::View { ref session_id, .. } if session_id == "session-1"
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_ignores_non_view_modes() {
    // Arrange — a `Question` session exists, but the user is on the list, not
    // viewing that session, so the panel must not steal focus.
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        vec![QuestionItem::new("Need integration tests?")],
        "session-list-reconcile",
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert — the list stays active.
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_reloads_detail_at_most_once_when_still_empty() {
    // Arrange — a viewed session reports `Question` status but carries no
    // questions in the snapshot, and no persisted detail exists to reload, so
    // the reconciliation cannot open the panel.
    let mut app =
        test_app_viewing_reconcile_session(Status::Question, Vec::new(), "session-question-empty")
            .await;

    // Act — run two reconciliations to emulate two consecutive render cycles
    // while the session stays stuck without questions.
    app.reconcile_open_session_question_mode().await;
    let attempted_after_first = app.question_reconcile_reload_attempted.clone();
    app.reconcile_open_session_question_mode().await;

    // Assert — the first pass records the stuck session so the second cycle
    // short-circuits before reloading detail again, and the view is preserved
    // because no questions became available.
    assert_eq!(attempted_after_first.as_deref(), Some("session-1"));
    assert_eq!(
        app.question_reconcile_reload_attempted.as_deref(),
        Some("session-1")
    );
    assert!(matches!(
        app.mode,
        AppMode::View { ref session_id, .. } if session_id == "session-1"
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_clears_reload_guard_when_leaving_view() {
    // Arrange — a stuck `Question` view records the reload guard, then the user
    // navigates back to the list.
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        Vec::new(),
        "session-question-guard-reset",
    )
    .await;
    app.reconcile_open_session_question_mode().await;
    assert_eq!(
        app.question_reconcile_reload_attempted.as_deref(),
        Some("session-1")
    );

    // Act — leave the session view and reconcile again.
    app.mode = AppMode::List;
    app.reconcile_open_session_question_mode().await;

    // Assert — the guard is cleared so a later legitimate transition reloads.
    assert!(app.question_reconcile_reload_attempted.is_none());
}

#[tokio::test]
async fn restore_prompt_progress_retains_snapshot_for_question_session() {
    // Arrange
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        vec![QuestionItem::new("Continue?")],
        "question-prompt-progress",
    )
    .await;
    let session_id = SessionId::from("session-1");
    app.save_prompt_progress(test_prompt_mode_snapshot(session_id.clone()));

    // Act
    let restored = app.restore_prompt_progress(&session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.contains_key(&session_id));
}

#[tokio::test]
async fn apply_app_events_agent_response_switches_view_mode_to_question_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-question-view"),
        ));
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: None,
    };
    let expected_questions = vec![
        QuestionItem::with_options(
            "Need a target branch?",
            vec!["main".to_string(), "develop".to_string()],
        ),
        QuestionItem::with_options(
            "Need integration tests?",
            vec!["Yes".to_string(), "No".to_string()],
        ),
    ];
    let turn_applied_state = test_turn_applied_state(
        vec![
            QuestionItem::with_options(
                "Need a target branch?",
                vec!["main".to_string(), "develop".to_string()],
            ),
            QuestionItem::with_options(
                "Need integration tests?",
                vec!["Yes".to_string(), "No".to_string()],
            ),
        ],
        Vec::new(),
        SessionStats::default(),
    );

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            ref questions,
            ref responses,
            current_index: 0,
            ref input,
            selected_option_index: Some(0),
            ..
        } if session_id == "session-1"
            && questions == &expected_questions
            && responses.is_empty()
            && input.text().is_empty()
    ));
}

#[tokio::test]
async fn apply_app_events_agent_response_clears_saved_question_progress() {
    // Arrange — stale partial answers saved from the previous question set
    // must not survive a new turn result for the session.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-progress-clear"),
        ));
    app.question_progress.insert(
        "session-1".into(),
        QuestionProgress {
            current_index: 1,
            input: InputState::default(),
            responses: vec!["Old answer".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("New question?")],
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(app.question_progress.is_empty());
}

#[tokio::test]
/// Verifies reducer-applied turn projections clear stale questions and add
/// token deltas to cached session stats.
async fn apply_app_events_agent_response_updates_questions_and_token_usage() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-stats-view"));
    session.questions = vec![QuestionItem::new("Old question?")];
    session.stats.input_tokens = 5;
    session.stats.output_tokens = 8;
    app.sessions.push_session(session);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 13,
                output_tokens: 21,
            },
        ),
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].questions,
        [] as [ag_protocol::QuestionItem; 0]
    );
    assert_eq!(app.sessions.sessions()[0].stats.input_tokens, 18);
    assert_eq!(app.sessions.sessions()[0].stats.output_tokens, 29);
}

#[tokio::test]
async fn enter_question_mode_restores_saved_progress() {
    // Arrange — progress saved by a previous `q` exit from question mode.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let questions = vec![
        QuestionItem::with_options("First?", vec!["Yes".to_string(), "No".to_string()]),
        QuestionItem::new("Second?"),
    ];
    app.question_progress.insert(
        "session-restore".into(),
        QuestionProgress {
            current_index: 1,
            input: InputState::with_text("draft answer".to_string()),
            responses: vec!["Yes".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.enter_question_mode("session-restore", questions);

    // Assert — resumes at the second question with the saved answer, and
    // the stored entry is consumed.
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            current_index: 1,
            responses,
            input,
            selected_option_index: None,
            session_id,
            ..
        } if responses == &vec!["Yes".to_string()]
            && input.text() == "draft answer"
            && session_id == "session-restore"
    ));
    assert!(app.question_progress.is_empty());
}

#[tokio::test]
async fn enter_question_mode_discards_progress_for_changed_question_list() {
    // Arrange — saved progress no longer matches the question list.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let questions = vec![QuestionItem::with_options(
        "Only question?",
        vec!["Yes".to_string()],
    )];
    app.question_progress.insert(
        "session-stale".into(),
        QuestionProgress {
            current_index: 2,
            input: InputState::default(),
            responses: vec!["One".to_string(), "Two".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.enter_question_mode("session-stale", questions);

    // Assert — starts fresh at the first question with its first option
    // highlighted.
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            current_index: 0,
            responses,
            selected_option_index: Some(0),
            ..
        } if responses.is_empty()
    ));
}
