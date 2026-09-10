use ag_session::QuestionAnswer;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::tempdir;

use super::super::{
    NO_ANSWER, handle_chat_focus_key, structured_question_answers, sync_question_at_mention_state,
};
use super::support::{TEST_TERMINAL_SIZE, handle, question_mode_with_options};
use crate::app::AppEvent;
use crate::domain::input::InputState;
use crate::domain::question::{QuestionItem, default_option_index};
use crate::domain::session::{Session, SessionId, SessionRole, SessionSize, SessionStats, Status};
use crate::domain::transient_message::TransientMessageStore;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::PromptAtMentionState;
use crate::ui::RenderCacheStore;

#[tokio::test]
async fn test_handle_chat_focus_preserves_question_state_for_enter_and_escape() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();
    if let AppMode::Question { focus, .. } = &mut app.mode {
        *focus = ChatFocus::Chat;
    }

    // Act
    for key in [KeyCode::Enter, KeyCode::Esc] {
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(key, KeyModifiers::NONE),
        )
        .await;
    }

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Chat,
            current_index: 0,
            ref responses,
            ..
        } if responses.is_empty()
    ));
}

#[tokio::test]
async fn test_question_delayed_lookup_result_respects_dismissal() {
    for dismissal in [
        None,
        Some(KeyCode::Esc),
        Some(KeyCode::Tab),
        Some(KeyCode::Enter),
    ] {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            at_mention_state,
            input,
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
            *input = InputState::with_text("@src".to_string());
            *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
        }
        let entries = vec![crate::domain::file_entry::FileEntry {
            is_dir: false,
            path: "src/lib.rs".to_string(),
        }];
        let lookup_root = app.at_mention_lookup_root("session-id");
        let delayed_result = AppEvent::AtMentionEntriesLoaded {
            entries: entries.clone(),
            session_id: "session-id".into(),
        };

        // Act: deliver the queued load after the dismissal key was handled.
        if let Some(code) = dismissal {
            handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(code, KeyModifiers::NONE),
            )
            .await;
        }
        app.apply_app_events(delayed_result).await;

        // Assert: late results may populate the cache, but cannot reopen
        // the picker.
        assert_eq!(
            app.sessions.at_mention_index_for_root(&lookup_root),
            Some(entries.clone())
        );
        let expected_entries = dismissal.is_none().then_some(entries);
        assert!(matches!(
            &app.mode,
            AppMode::Question {
                at_mention_state,
                input,
                focus: ChatFocus::Input,
                ..
            } if input.text() == "@src"
                && at_mention_state.as_ref().map(|state| &state.all_entries)
                    == expected_entries.as_ref()
        ));
    }
}

#[tokio::test]
async fn test_d_key_in_chat_focus_opens_diff_with_question_snapshot() {
    // Arrange — question mode with chat focused in a git worktree
    // session that has a non-empty diff.

    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-diff-question";

    // Set up a session with a real temp dir (no git repo, so diff will
    // produce an error message — which counts as non-empty content).
    let session_dir = tempdir().expect("failed to create session dir");
    app.sessions.push_session(Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: session_dir.path().to_path_buf(),
        follow_up_tasks: Vec::new(),
        id: session_id.into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            crate::domain::agent::AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: String::new(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: crate::domain::agent::SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::Question,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    });

    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: session_id.into(),
        questions: vec![QuestionItem {
            options: vec!["A".to_string()],
            text: "Pick one".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Chat,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: Some(0),
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    )
    .await;

    // Assert — transitioned to diff loading with a question snapshot.
    assert!(matches!(
        app.mode,
        AppMode::DiffLoading {
            ref session_id,
            restore: Some(_),
            ..
        } if session_id == "session-diff-question"
    ));
}

#[tokio::test]
async fn test_question_empty_lookup_selection_closes_without_submitting() {
    for code in [
        KeyCode::Tab,
        KeyCode::Enter,
        KeyCode::Char('\r'),
        KeyCode::Char('\n'),
    ] {
        for entries in [
            Vec::new(),
            vec![crate::domain::file_entry::FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            }],
        ] {
            // Arrange
            let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
            app.mode = question_mode_with_options();
            if let AppMode::Question {
                at_mention_state,
                input,
                selected_option_index,
                ..
            } = &mut app.mode
            {
                *selected_option_index = None;
                *input = InputState::with_text("@missing".to_string());
                *at_mention_state = Some(PromptAtMentionState::new(entries));
            }

            // Act
            handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(code, KeyModifiers::NONE),
            )
            .await;

            // Assert
            assert!(matches!(
                &app.mode,
                AppMode::Question {
                    at_mention_state: None,
                    current_index: 0,
                    focus: ChatFocus::Input,
                    input,
                    responses,
                    ..
                } if input.text() == "@missing" && responses.is_empty()
            ));
        }
    }
}

#[test]
fn test_structured_question_answers_pairs_all_responses() {
    // Arrange
    let questions = vec![
        QuestionItem {
            options: vec!["main".to_string(), "develop".to_string()],
            text: "Need target?".to_string(),
        },
        QuestionItem {
            options: vec!["Yes".to_string(), "No".to_string()],
            text: "Need tests?".to_string(),
        },
    ];
    let responses = vec!["main".to_string(), NO_ANSWER.to_string()];

    // Act
    let answers = structured_question_answers(&questions, &responses);

    // Assert
    assert_eq!(
        answers,
        [
            QuestionAnswer {
                answer: "main".to_string(),
                question: "Need target?".to_string(),
            },
            QuestionAnswer {
                answer: NO_ANSWER.to_string(),
                question: "Need tests?".to_string(),
            },
        ]
    );
}

#[tokio::test]
async fn test_chat_focus_key_ignores_non_question_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::List;

    // Act
    let is_consumed = handle_chat_focus_key(
        &mut app,
        &RenderCacheStore::default(),
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
    );

    // Assert
    assert!(!is_consumed);
    assert!(matches!(app.mode, AppMode::List));
}

#[test]
fn test_default_option_index_returns_none_for_question_without_predefined_options() {
    // Arrange — without predefined options the UI starts directly in
    // free-text input mode.
    let questions = vec![QuestionItem {
        options: Vec::new(),
        text: "Type something?".to_string(),
    }];

    // Act & Assert
    assert_eq!(default_option_index(&questions, 0), None);
}

#[tokio::test]
async fn test_question_enter_encodings_submit_without_lookup() {
    for code in [KeyCode::Enter, KeyCode::Char('\r'), KeyCode::Char('\n')] {
        for (selected_option, draft, expected) in [
            (Some(1), "ignored draft", "Option B"),
            (None, "typed answer", "typed answer"),
            (None, "", NO_ANSWER),
        ] {
            // Arrange
            let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
            app.mode = question_mode_with_options();
            if let AppMode::Question {
                input,
                questions,
                selected_option_index,
                ..
            } = &mut app.mode
            {
                *input = InputState::with_text(draft.to_string());
                *selected_option_index = selected_option;
                questions.push(QuestionItem::new("Anything else?"));
            }

            // Act
            handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(code, KeyModifiers::NONE),
            )
            .await;

            // Assert
            assert!(matches!(
                &app.mode,
                AppMode::Question {
                    current_index: 1,
                    input,
                    responses,
                    focus: ChatFocus::Input,
                    ..
                } if responses == &vec![expected.to_string()] && input.is_empty()
            ));
        }
    }
}

#[tokio::test]
async fn test_question_at_mention_loads_parent_worktree_entries_for_stacked_session() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let parent_session_id = SessionId::from("parent-session");
    let child_session_id = SessionId::from("child-session");
    let parent_folder = base_dir.path().join("parent-worktree");
    let expected_path = "parent_question_target.txt";
    std::fs::create_dir_all(&parent_folder).expect("failed to create parent worktree");
    std::fs::write(parent_folder.join(expected_path), "parent")
        .expect("failed to write parent worktree file");
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(parent_session_id.clone())
            .folder(parent_folder)
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(child_session_id.clone())
            .folder(base_dir.path().join("missing-child-worktree"))
            .parent_session_id(Some(parent_session_id))
            .status(Status::Question)
            .build(),
    );
    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("@parent".to_string()),
        questions: vec![QuestionItem::new("Which file?")],
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: child_session_id.clone(),
    };

    // Act
    sync_question_at_mention_state(&mut app);
    let next_event = loop {
        let next_event =
            tokio::time::timeout(std::time::Duration::from_secs(1), app.next_app_event())
                .await
                .expect("at-mention event should arrive")
                .expect("at-mention event channel should stay open");
        if matches!(next_event, AppEvent::AtMentionEntriesLoaded { .. }) {
            break next_event;
        }
    };

    // Assert
    assert!(matches!(
        next_event,
        AppEvent::AtMentionEntriesLoaded {
            entries,
            session_id,
        } if session_id == child_session_id
            && entries.contains(&crate::domain::file_entry::FileEntry {
                is_dir: false,
                path: expected_path.to_string(),
            })
    ));
}

#[tokio::test]
async fn test_question_lookup_selects_file_without_submitting_or_changing_focus() {
    for selection_key in [
        KeyCode::Tab,
        KeyCode::Enter,
        KeyCode::Char('\r'),
        KeyCode::Char('\n'),
    ] {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            at_mention_state,
            input,
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
            *input = InputState::with_text("Use @src/pending".to_string());
            input.cursor = "Use @src".chars().count();
            *at_mention_state = Some(PromptAtMentionState::new(vec![
                crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/first.rs".to_string(),
                },
                crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/second.rs".to_string(),
                },
            ]));
        }

        // Act
        for code in [KeyCode::Down, KeyCode::Up, KeyCode::Down, selection_key] {
            handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(code, KeyModifiers::NONE),
            )
            .await;
        }

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Question {
                at_mention_state: None,
                current_index: 0,
                focus: ChatFocus::Input,
                input,
                responses,
                selected_option_index: None,
                ..
            } if input.text() == "Use @src/second.rs " && responses.is_empty()
        ));
    }
}

#[tokio::test]
async fn test_question_lookup_modified_enter_inserts_newline() {
    for (code, modifiers) in [
        (KeyCode::Enter, KeyModifiers::ALT),
        (KeyCode::Enter, KeyModifiers::SHIFT),
        (KeyCode::Char('\r'), KeyModifiers::ALT),
        (KeyCode::Char('\r'), KeyModifiers::SHIFT),
        (KeyCode::Char('\n'), KeyModifiers::ALT),
        (KeyCode::Char('\n'), KeyModifiers::SHIFT),
        (KeyCode::Char('\r'), KeyModifiers::CONTROL),
        (KeyCode::Char('\n'), KeyModifiers::CONTROL),
        (KeyCode::Char('j'), KeyModifiers::CONTROL),
        (KeyCode::Char('m'), KeyModifiers::CONTROL),
    ] {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            at_mention_state,
            input,
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
            *input = InputState::with_text("@src".to_string());
            *at_mention_state = Some(PromptAtMentionState::new(vec![
                crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/lib.rs".to_string(),
                },
            ]));
        }

        // Act
        handle(&mut app, TEST_TERMINAL_SIZE, KeyEvent::new(code, modifiers)).await;

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Question {
                at_mention_state: None,
                current_index: 0,
                focus: ChatFocus::Input,
                input,
                responses,
                ..
            } if input.text() == "@src\n" && responses.is_empty()
        ));
    }
}

#[tokio::test]
async fn test_store_question_response_defaults_to_first_option_on_next_question() {
    // Arrange — free-text mode on first question, next has options.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "missing-session".into(),
        questions: vec![
            QuestionItem {
                options: vec!["Foo".to_string()],
                text: "First question?".to_string(),
            },
            QuestionItem {
                options: vec!["Alpha".to_string(), "Beta".to_string()],
                text: "Pick one?".to_string(),
            },
        ],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("answer".to_string()),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            current_index: 1,
            selected_option_index: Some(0),
            ..
        }
    ));
}
