//! Shortcut availability tests for list, settings, session view, and diff
//! help actions.

use super::{
    DiffReviewCommentHelp, PROMPT_IMAGE_PASTE_SHORTCUT_LABEL, ViewActionAvailability,
    ViewHelpState, ViewSessionState, diff_actions, project_list_actions, session_list_actions,
    session_view_state, settings_actions, view_actions, view_actions_with_review_request,
};
use crate::domain::session::{PublishBranchAction, ReviewWatchToggle, Status};
use crate::test_support::SessionFixtureBuilder;

#[test]
fn test_project_list_actions_include_explicit_registration_and_discovery() {
    // Arrange
    // Act
    let actions = project_list_actions();

    // Assert
    assert!(
        actions
            .iter()
            .any(|action| action.key == "a" && action.popup_label == "Add project by path")
    );
    assert!(actions.iter().any(|action| action.key == "d"));
}

#[test]
fn test_settings_actions_exclude_new_session_shortcut() {
    // Arrange
    // Act
    let actions = settings_actions();

    // Assert
    assert!(!actions.iter().any(|action| action.key == "a"));
}

#[test]
fn test_session_list_actions_include_new_session_shortcut() {
    // Arrange
    // Act
    let actions = session_list_actions(false, false);

    // Assert
    assert!(actions.iter().any(|action| action.key == "a"));
}

#[test]
fn test_session_list_actions_include_switch_project_shortcut() {
    // Arrange
    // Act
    let actions = session_list_actions(false, false);

    // Assert
    assert!(
        actions
            .iter()
            .any(|action| action.key == "p" && action.popup_label == "Switch project")
    );
}

#[test]
fn test_session_list_actions_include_cancel_and_open_for_selected_session() {
    // Arrange
    // Act
    let actions = session_list_actions(true, true);

    // Assert
    assert!(actions.iter().any(|action| action.key == "c"));
    assert!(actions.iter().any(|action| action.key == "Enter"));
}

#[test]
fn test_session_list_actions_hide_enter_without_openable_session() {
    // Arrange
    // Act
    let actions = session_list_actions(false, false);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "j/k"));
}

#[test]
fn test_view_actions_in_progress_shows_stop_sync_and_queue_and_hides_open_and_commands() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::InProgress,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(actions.iter().any(|action| action.key == "r"));
    assert!(
        actions
            .iter()
            .any(|action| action.key == "Enter" && action.popup_label == "Queue message")
    );
    assert!(!actions.iter().any(|action| action.key == "/"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(actions.iter().any(|action| action.key == "d"));
}

#[test]
fn in_progress_help_offers_send_now_only_for_steerable_queued_chat() {
    // Arrange
    let state = |can_steer_queued_messages, session_state| ViewHelpState {
        can_fork_session: ViewActionAvailability::Disabled,
        can_merge_session_branch: ViewActionAvailability::Disabled,
        can_open_worktree: ViewActionAvailability::Disabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages,
        publish_pull_request_action: None,
        session_state,
    };
    let has_send_now = |state| {
        view_actions(state)
            .iter()
            .any(|action| action.key == "Ctrl+s" && action.footer_label == "send now")
    };

    // Act / Assert
    assert!(has_send_now(state(
        ViewActionAvailability::Enabled,
        ViewSessionState::InProgress
    )));
    assert!(!has_send_now(state(
        ViewActionAvailability::Disabled,
        ViewSessionState::InProgress
    )));
    assert!(!has_send_now(state(
        ViewActionAvailability::Enabled,
        ViewSessionState::Review
    )));
}

#[test]
fn test_view_actions_hide_open_when_worktree_is_unavailable() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Disabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "o"));
}

#[test]
fn test_view_actions_show_diff_in_every_session_state() {
    for session_state in [
        ViewSessionState::Done,
        ViewSessionState::Canceled,
        ViewSessionState::Merged,
        ViewSessionState::InProgress,
        ViewSessionState::Rebasing,
        ViewSessionState::MergeQueue,
        ViewSessionState::NewSession,
        ViewSessionState::Review,
        ViewSessionState::AgentReview,
        ViewSessionState::Interactive,
    ] {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            can_steer_queued_messages: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(
            actions.iter().any(|action| action.key == "d"),
            "{session_state:?} should offer the diff action"
        );
    }
}

#[test]
fn merged_view_actions_keep_diff_and_hide_mutating_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Enabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Merged,
    };

    // Act
    let actions = view_actions(state);
    let session = SessionFixtureBuilder::new().status(Status::Merged).build();
    let session_state = session_view_state(&session);

    // Assert
    assert_eq!(session_state, ViewSessionState::Merged);
    assert!(actions.iter().any(|action| action.key == "d"));
    for key in ["Enter", "/", "o", "p", "f", "F", "m", "r", "s", "c"] {
        assert!(!actions.iter().any(|action| action.key == key));
    }
}

#[test]
fn test_view_actions_rebasing_shows_queue_publish_and_hides_open_and_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Rebasing,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(
        actions
            .iter()
            .any(|action| { action.key == "Enter" && action.popup_label == "Queue message" })
    );
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(actions.iter().any(|action| action.key == "d"));
}

#[test]
fn test_view_actions_merge_queue_hides_worktree_shortcuts_and_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::MergeQueue,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(actions.iter().any(|action| action.key == "d"));
}

#[test]
fn test_view_actions_review_shows_diff() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "d"));
    assert!(actions.iter().any(|action| action.key == "f"));
    assert!(
        actions
            .iter()
            .any(|action| action.key == "p" && action.footer_label == "PR")
    );
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "o"));
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "F"));
    assert!(actions.iter().any(|action| {
        action.key == "/"
            && action.footer_label == "commands menu"
            && action.popup_label == "Open commands menu"
    }));
    assert!(!actions.iter().any(|action| action.key == "S-Tab"));
    assert!(
        actions
            .iter()
            .any(|action| action.key == "f" && action.popup_label == "Focused review")
    );
}

#[test]
fn test_view_actions_review_hides_fork_when_unavailable() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Disabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "F"));
}

#[test]
fn test_view_actions_review_hides_merge_when_merge_gate_is_disabled_but_keeps_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Disabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "m"));
    assert!(actions.iter().any(|action| action.key == "r"));
    assert!(actions.iter().any(|action| action.key == "/"));
}

#[test]
fn test_view_actions_agent_review_shows_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::AgentReview,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "d"));
    assert!(actions.iter().any(|action| action.key == "f"));
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_actions_interactive_hides_review_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Interactive,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "f"));
    assert!(!actions.iter().any(|action| action.key == "r"));
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "/"));
}

#[test]
fn test_view_actions_new_session_shows_add_draft_and_start() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Enabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::NewSession,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(
        actions
            .iter()
            .any(|action| action.key == "Enter" && action.footer_label == "add draft")
    );
    assert!(actions.iter().any(|action| {
        action.key == PROMPT_IMAGE_PASTE_SHORTCUT_LABEL && action.footer_label == "paste image"
    }));
    assert!(actions.iter().any(|action| action.key == "/"));
    assert!(actions.iter().any(|action| action.key == "s"));
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_actions_done_shows_continue_and_hides_edit_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Done,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| {
        action.key == "c"
            && action.footer_label == "continue"
            && action.popup_label == "Continue in new session"
    }));
    assert!(!actions.iter().any(|action| action.key == "p"));
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "d"));
    assert!(!actions.iter().any(|action| action.key == "f"));
    assert!(!actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_actions_canceled_shows_continue_without_edit_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Canceled,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| {
        action.key == "c"
            && action.footer_label == "continue"
            && action.popup_label == "Continue in new session"
    }));
    assert!(!actions.iter().any(|action| action.key == "p"));
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(!actions.iter().any(|action| action.key == "t"));
}

#[test]
fn test_view_actions_terminal_sessions_hide_linked_review_comments() {
    // Arrange
    let terminal_states = [ViewSessionState::Done, ViewSessionState::Canceled];

    for session_state in terminal_states {
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            can_steer_queued_messages: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state,
        };

        // Act
        let full_actions = view_actions_with_review_request(state, true, None);

        // Assert
        assert_eq!(
            full_actions
                .iter()
                .filter(|action| action.key == "c")
                .count(),
            1
        );
        assert!(full_actions.iter().any(|action| {
            action.key == "c" && action.popup_label == "Continue in new session"
        }));
        assert!(
            !full_actions
                .iter()
                .any(|action| action.popup_label == "Show review comments")
        );
    }
}

#[test]
fn test_view_actions_non_terminal_review_comments_follow_availability() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let available_actions = view_actions_with_review_request(state, true, None);
    let unavailable_actions = view_actions_with_review_request(state, false, None);

    // Assert
    assert!(
        available_actions
            .iter()
            .any(|action| action.popup_label == "Show review comments")
    );
    assert!(
        !unavailable_actions
            .iter()
            .any(|action| action.popup_label == "Show review comments")
    );
}

#[test]
fn test_view_actions_review_hides_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
}

#[test]
fn test_session_view_state_maps_agent_review_status() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::AgentReview)
        .build();

    // Act
    let state = session_view_state(&session);

    // Assert
    assert_eq!(state, ViewSessionState::AgentReview);
}

#[test]
fn test_read_only_detail_and_diff_action_groups_expose_expected_keys() {
    // Arrange, Act
    let diff_keys = diff_actions(true, DiffReviewCommentHelp::Reply)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        diff_keys,
        [
            "q",
            "j/k",
            "Enter/l",
            "f/h/Esc/Left",
            "c",
            "p",
            "J/K/Up/Down",
            "Shift+C",
            "Shift+V",
            "Alt/Shift+Enter",
            "Enter/Esc",
            "s",
            "Space",
            "Enter",
            "?"
        ]
    );
}

#[test]
fn test_read_only_diff_hides_inline_comment_actions() {
    // Arrange, Act
    let help_labels = diff_actions(false, DiffReviewCommentHelp::Hidden)
        .iter()
        .map(|action| action.footer_label)
        .collect::<Vec<_>>();

    // Assert
    assert!(help_labels.contains(&"open"));
    assert!(!help_labels.contains(&"open/comment"));
    assert!(!help_labels.contains(&"comment file"));
    assert!(!help_labels.contains(&"save comment"));
    assert!(!help_labels.contains(&"submit comments"));
}

#[test]
fn test_diff_actions_hide_review_comment_keys_without_linked_review_request() {
    // Arrange, Act
    let diff_keys = diff_actions(true, DiffReviewCommentHelp::Hidden)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();

    // Assert
    assert!(!diff_keys.contains(&"c"));
    assert!(!diff_keys.contains(&"Space"));
    assert!(!diff_keys.contains(&"Enter"));
    assert_eq!(diff_keys.last(), Some(&"?"));
}

#[test]
fn test_diff_actions_read_only_review_comments_keep_focus_and_hide_reply_keys() {
    // Arrange, Act
    let diff_keys = diff_actions(false, DiffReviewCommentHelp::ReadOnly)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();

    // Assert
    assert!(diff_keys.contains(&"c"));
    assert!(!diff_keys.contains(&"Space"));
    assert!(!diff_keys.contains(&"Enter"));
}

#[test]
fn test_diff_review_comment_help_requires_linked_comments_before_reply() {
    // Arrange, Act, Assert
    assert_eq!(
        DiffReviewCommentHelp::new(false, true),
        DiffReviewCommentHelp::Hidden
    );
    assert_eq!(
        DiffReviewCommentHelp::new(true, false),
        DiffReviewCommentHelp::ReadOnly
    );
    assert_eq!(
        DiffReviewCommentHelp::new(true, true),
        DiffReviewCommentHelp::Reply
    );
}

#[test]
fn test_list_actions_list_reverse_tab_cycling() {
    // Arrange, Act
    let tab_actions = [
        session_list_actions(false, false),
        project_list_actions(),
        settings_actions(),
    ];

    // Assert
    for actions in tab_actions {
        assert!(
            actions
                .iter()
                .any(|action| action.key == "Tab" && action.popup_label == "Next tab")
        );
        assert!(
            actions
                .iter()
                .any(|action| action.key == "Shift+Tab" && action.popup_label == "Previous tab")
        );
    }
}

#[test]
fn review_watch_action_follows_toggle_before_trailing_help_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Disabled,
        can_open_worktree: ViewActionAvailability::Disabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        can_steer_queued_messages: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let start_actions =
        view_actions_with_review_request(state, false, Some(ReviewWatchToggle::Start));
    let stop_actions =
        view_actions_with_review_request(state, false, Some(ReviewWatchToggle::Stop));
    let hidden_actions = view_actions_with_review_request(state, false, None);

    // Assert
    let start_index = start_actions
        .iter()
        .position(|action| action.key == "W")
        .expect("watch action");
    assert_eq!(
        start_actions[start_index].popup_label,
        "Watch review request and resolve checks, comments, and conflicts"
    );
    assert_eq!(start_actions[start_index - 1].key, "r");
    assert_eq!(start_actions.last().map(|action| action.key), Some("?"));
    assert!(
        stop_actions
            .iter()
            .any(|action| action.key == "W"
                && action.popup_label == "Stop watching review request")
    );
    assert!(hidden_actions.iter().all(|action| action.key != "W"));
    assert_eq!(start_actions.len(), hidden_actions.len() + 1);
}

#[test]
fn watching_sessions_share_review_view_actions() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::Watching)
        .build();

    // Act
    let session_state = session_view_state(&session);

    // Assert
    assert_eq!(session_state, ViewSessionState::Review);
}
