use super::{
    current_page_messages, rotating_message, session_chat_messages, session_list_messages,
};
use crate::app::Tab;
use crate::presentation::app_mode::{AppMode, DiffFocus, DiffLineComments};
use crate::presentation::help_action;
use crate::presentation::help_action::{ViewActionAvailability, ViewHelpState, ViewSessionState};

#[test]
fn rotating_message_cycles_through_messages() {
    // Arrange
    let fyi_messages = ["First", "Second", "Third"];

    // Act
    let zero = rotating_message(&fyi_messages, 0);
    let one = rotating_message(&fyi_messages, 1);
    let wrapped = rotating_message(&fyi_messages, 4);

    // Assert
    assert_eq!(zero, Some("First"));
    assert_eq!(one, Some("Second"));
    assert_eq!(wrapped, Some("Second"));
}

#[test]
fn rotating_message_returns_none_for_empty_set() {
    // Arrange
    let fyi_messages: [&str; 0] = [];

    // Act
    let selected_message = rotating_message(&fyi_messages, 2);

    // Assert
    assert_eq!(selected_message, None);
}

#[test]
fn current_page_messages_returns_session_list_guidance_for_sessions_tab() {
    // Arrange
    let mode = AppMode::List;

    // Act
    let page_fyis = current_page_messages(Tab::Sessions, &mode);

    // Assert
    assert_eq!(page_fyis, Some(session_list_messages()));
}

#[test]
fn current_page_messages_returns_session_chat_guidance_for_view_mode() {
    // Arrange
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };

    // Act
    let page_fyis = current_page_messages(Tab::Sessions, &mode);

    // Assert
    assert_eq!(page_fyis, Some(session_chat_messages()));
}

#[test]
fn current_page_messages_skips_non_session_pages_and_diff_mode() {
    // Arrange
    let list_mode = AppMode::List;
    let diff_mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: crate::presentation::app_mode::DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };

    // Act
    let settings_page_fyis = current_page_messages(Tab::Settings, &list_mode);
    let diff_page_fyis = current_page_messages(Tab::Sessions, &diff_mode);

    // Assert
    assert_eq!(settings_page_fyis, None);
    assert_eq!(diff_page_fyis, None);
}

#[test]
fn terminal_continuation_fyi_matches_help_actions() {
    // Arrange
    let terminal_states = [ViewSessionState::Done, ViewSessionState::Canceled];

    // Act
    let continuation_keys = terminal_states.map(|session_state| {
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Disabled,
            can_merge_session_branch: ViewActionAvailability::Disabled,
            can_mutate_session_branch: ViewActionAvailability::Disabled,
            can_open_worktree: ViewActionAvailability::Disabled,
            can_rebase_session_branch: ViewActionAvailability::Disabled,
            can_show_diff: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Disabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state,
        };

        help_action::view_actions(state)
            .into_iter()
            .find(|action| action.popup_label == "Continue in new session")
            .map(|action| action.key)
            .expect("terminal session should expose a continuation action")
    });
    let expected_message = format!(
        "Done and canceled sessions can continue in a fresh draft with {}.",
        continuation_keys[0]
    );
    let continuation_messages = session_chat_messages()
        .iter()
        .filter(|message| message.contains("continue in a fresh draft"))
        .copied()
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(continuation_keys[0], continuation_keys[1]);
    assert_eq!(continuation_messages, [expected_message.as_str()]);
}
