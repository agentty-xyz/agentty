use ag_session::SessionRole;

use super::{
    DiffFileCommentAvailability, DiffFooterContext, DiffLineCommentFooterState,
    PROMPT_IMAGE_PASTE_SHORTCUT_LABEL, ViewActionAvailability, ViewHelpState, ViewSessionState,
    diff_actions, diff_footer_actions, project_list_actions, session_list_actions,
    session_list_footer_actions, session_view_state, settings_actions, view_actions,
    view_actions_with_review_comments, view_footer_actions,
};
use crate::domain::session::{PublishBranchAction, Status};
use crate::presentation::app_mode::{DiffFocus, DiffSidebarFocus};
use crate::test_support::SessionFixtureBuilder;

#[test]
fn test_project_list_actions_exclude_new_session_shortcut() {
    // Arrange
    // Act
    let actions = project_list_actions();

    // Assert
    assert!(!actions.iter().any(|action| action.key == "a"));
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
fn test_session_list_actions_hide_enter_without_openable_session() {
    // Arrange
    // Act
    let actions = session_list_actions(false, false);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "j/k"));
}

#[test]
fn test_session_list_footer_actions_hides_non_critical_session_commands() {
    // Arrange

    // Act
    let actions = session_list_footer_actions(false, true);

    // Assert
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "a"));
    assert!(
        actions
            .iter()
            .any(|action| action.key == "p" && action.footer_label == "projects")
    );
    assert!(!actions.iter().any(|action| action.key == "d"));
    assert!(!actions.iter().any(|action| action.key == "c"));
    assert!(!actions.iter().any(|action| action.key == "Tab"));
}

#[test]
fn test_session_list_footer_actions_includes_cancel_when_session_is_cancelable() {
    // Arrange

    // Act
    let actions = session_list_footer_actions(true, true);

    // Assert
    assert!(actions.iter().any(|action| action.key == "c"));
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(!actions.iter().any(|action| action.key == "Tab"));
}

#[test]
fn test_view_actions_in_progress_shows_stop_and_sync_and_hides_open_and_edit_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::InProgress,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(actions.iter().any(|action| action.key == "r"));
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(!actions.iter().any(|action| action.key == "d"));
}

#[test]
fn test_view_actions_hide_open_when_worktree_is_unavailable() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Disabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "o"));
}

#[test]
fn test_view_actions_hide_diff_when_session_diff_is_empty() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Disabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "d"));
}

#[test]
fn merged_view_actions_keep_diff_and_hide_mutating_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Enabled,
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
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
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
    assert!(!actions.iter().any(|action| action.key == "d"));
}

#[test]
fn test_view_actions_merge_queue_hides_worktree_shortcuts_and_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::MergeQueue,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(!actions.iter().any(|action| action.key == "d"));
}

#[test]
fn test_view_actions_review_shows_diff() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
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
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "F"));
}

#[test]
fn test_view_actions_review_keeps_commands_when_stack_allows_reply() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Disabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "d"));
    assert!(actions.iter().any(|action| action.key == "f"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "o"));
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "/"));
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_review_actions_hide_commands_when_mutation_and_reply_are_blocked() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Disabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Disabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);
    let footer_actions = view_footer_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "/"));
    assert!(!footer_actions.iter().any(|action| action.key == "/"));
}

#[test]
fn test_view_actions_review_uses_merge_gate_separately_from_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Disabled,
        can_rebase_session_branch: ViewActionAvailability::Disabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
    assert!(actions.iter().any(|action| action.key == "/"));
}

#[test]
fn test_view_actions_agent_review_shows_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
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
fn test_view_actions_interactive_hides_diff() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Interactive,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "d"));
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
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Enabled,
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
fn test_view_actions_stacked_draft_shows_start_and_hides_merge_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Enabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::StackedDraft,
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
    assert!(!actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_actions_stacked_draft_hides_start_without_staged_drafts() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::StackedDraft,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(
        actions
            .iter()
            .any(|action| action.key == "Enter" && action.footer_label == "add draft")
    );
    assert!(!actions.iter().any(|action| action.key == "s"));
    assert!(!actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_actions_done_shows_continue_and_hides_edit_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
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
    assert!(!actions.iter().any(|action| action.key == "d"));
    assert!(!actions.iter().any(|action| action.key == "f"));
    assert!(!actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_footer_actions_review_shows_advanced_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "/"));
    assert!(actions.iter().any(|action| action.key == "o"));
    assert!(actions.iter().any(|action| action.key == "f"));
    assert!(actions.iter().any(|action| action.key == "F"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_footer_actions_review_hides_fork_when_unavailable() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Disabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "F"));
}

#[test]
fn test_view_footer_actions_review_keeps_commands_when_stack_allows_reply() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Disabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "o"));
    assert!(actions.iter().any(|action| action.key == "f"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "/"));
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_footer_actions_review_uses_merge_gate_separately_from_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Disabled,
        can_rebase_session_branch: ViewActionAvailability::Disabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
    assert!(actions.iter().any(|action| action.key == "/"));
}

#[test]
fn test_view_footer_actions_agent_review_shows_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::AgentReview,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "/"));
    assert!(actions.iter().any(|action| action.key == "o"));
    assert!(actions.iter().any(|action| action.key == "f"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(actions.iter().any(|action| action.key == "m"));
    assert!(actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_footer_actions_rebasing_shows_queue_publish_and_hides_open_and_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Rebasing,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(actions.iter().any(|action| action.key == "p"));
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(
        actions
            .iter()
            .any(|action| { action.key == "Enter" && action.footer_label == "queue message" })
    );
}

#[test]
fn test_view_footer_actions_new_session_keeps_edit_actions_grouped() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Enabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::NewSession,
    };

    // Act
    let actions = view_footer_actions(state);
    let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        &ordered_keys[..6],
        [
            "q",
            "Enter",
            "s",
            PROMPT_IMAGE_PASTE_SHORTCUT_LABEL,
            "/",
            "m"
        ]
    );
}

#[test]
fn test_view_footer_actions_stacked_draft_shows_start_and_hides_merge_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Enabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::StackedDraft,
    };

    // Act
    let actions = view_footer_actions(state);
    let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        &ordered_keys[..5],
        ["q", "Enter", "s", PROMPT_IMAGE_PASTE_SHORTCUT_LABEL, "/"]
    );
    assert!(actions.iter().any(|action| action.key == "Enter"));
    assert!(actions.iter().any(|action| action.key == "/"));
    assert!(actions.iter().any(|action| action.key == "s"));
    assert!(!actions.iter().any(|action| action.key == "m"));
    assert!(!actions.iter().any(|action| action.key == "r"));
}

#[test]
fn test_view_footer_actions_merge_queue_hides_worktree_shortcuts_and_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::MergeQueue,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(actions.iter().any(|action| action.key == "q"));
    assert!(actions.iter().any(|action| action.key == "j/k"));
}

#[test]
fn test_view_footer_actions_in_progress_shows_stop_and_sync() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::InProgress,
    };

    // Act
    let actions = view_footer_actions(state);
    let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

    // Assert
    assert_eq!(&ordered_keys[..4], ["q", "r", "Ctrl+c", "j/k"]);
}

#[test]
fn test_view_actions_canceled_shows_continue_without_edit_actions() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
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
fn test_view_footer_actions_done_shows_continue_before_scroll() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Done,
    };

    // Act
    let actions = view_footer_actions(state);
    let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

    // Assert
    assert_eq!(&ordered_keys[..4], ["q", "c", "j/k", "?"]);
}

#[test]
fn test_view_actions_terminal_sessions_hide_linked_review_comments() {
    // Arrange
    let terminal_states = [ViewSessionState::Done, ViewSessionState::Canceled];

    for session_state in terminal_states {
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_show_diff: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state,
        };

        // Act
        let full_actions = view_actions_with_review_comments(state, true);

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
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let available_actions = view_actions_with_review_comments(state, true);
    let unavailable_actions = view_actions_with_review_comments(state, false);

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
fn test_view_footer_actions_canceled_shows_continue_before_scroll() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Canceled,
    };

    // Act
    let actions = view_footer_actions(state);
    let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

    // Assert
    assert_eq!(&ordered_keys[..4], ["q", "c", "j/k", "?"]);
}

#[test]
fn test_view_footer_actions_in_progress_shows_stop_and_hides_open() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::InProgress,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(actions.iter().any(|action| action.key == "Ctrl+c"));
    assert!(!actions.iter().any(|action| action.key == "o"));
    assert!(!actions.iter().any(|action| action.key == "Enter"));
    assert!(!actions.iter().any(|action| action.key == "d"));
}

#[test]
fn test_view_actions_review_hides_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
}

#[test]
fn test_view_footer_actions_review_hides_stop() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Enabled,
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
    };

    // Act
    let actions = view_footer_actions(state);

    // Assert
    assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
}

#[test]
fn test_session_view_state_maps_agent_review_and_orchestrator_review_statuses() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::AgentReview)
        .build();
    let orchestrator = SessionFixtureBuilder::new()
        .role(SessionRole::Orchestrator)
        .status(Status::Review)
        .build();
    let managed = SessionFixtureBuilder::new()
        .role(SessionRole::OrchestrationWorker)
        .status(Status::Review)
        .build();
    let researcher = SessionFixtureBuilder::new()
        .role(SessionRole::OrchestrationResearcher)
        .status(Status::Review)
        .build();

    // Act
    let state = session_view_state(&session);
    let orchestrator_state = session_view_state(&orchestrator);
    let managed_state = session_view_state(&managed);
    let researcher_state = session_view_state(&researcher);

    // Assert
    assert_eq!(state, ViewSessionState::AgentReview);
    assert_eq!(orchestrator_state, ViewSessionState::Orchestrator);
    assert_eq!(managed_state, ViewSessionState::Managed);
    assert_eq!(researcher_state, ViewSessionState::ManagedResearch);
}

#[test]
fn orchestration_view_actions_expose_only_owned_controls() {
    // Arrange
    let mut state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Disabled,
        can_merge_session_branch: ViewActionAvailability::Disabled,
        can_mutate_session_branch: ViewActionAvailability::Disabled,
        can_open_worktree: ViewActionAvailability::Disabled,
        can_rebase_session_branch: ViewActionAvailability::Disabled,
        can_show_diff: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Disabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Managed,
    };

    // Act
    let managed_keys = view_actions(state)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();
    state.session_state = ViewSessionState::ManagedResearch;
    let research_keys = view_actions(state)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();
    state.session_state = ViewSessionState::Orchestrator;
    state.reply_to_session = ViewActionAvailability::Enabled;
    let controller_keys = view_actions(state)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();

    // Assert
    assert!(managed_keys.contains(&"d"));
    assert!(managed_keys.contains(&"D"));
    assert!(!managed_keys.contains(&"o"));
    assert!(!managed_keys.contains(&"Enter"));
    assert!(research_keys.contains(&"d"));
    assert!(!research_keys.contains(&"D"));
    assert!(!research_keys.contains(&"o"));
    assert!(controller_keys.contains(&"a"));
    assert!(controller_keys.contains(&"Enter"));
    assert!(!controller_keys.contains(&"m"));
}

#[test]
fn managed_review_view_actions_expose_worktree_open_when_available() {
    // Arrange
    let state = ViewHelpState {
        can_fork_session: ViewActionAvailability::Disabled,
        can_merge_session_branch: ViewActionAvailability::Disabled,
        can_mutate_session_branch: ViewActionAvailability::Disabled,
        can_open_worktree: ViewActionAvailability::Enabled,
        can_rebase_session_branch: ViewActionAvailability::Disabled,
        can_show_diff: ViewActionAvailability::Enabled,
        reply_to_session: ViewActionAvailability::Disabled,
        can_start_staged_session: ViewActionAvailability::Disabled,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Managed,
    };

    // Act
    let managed_keys = view_actions(state)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();

    // Assert
    assert!(managed_keys.contains(&"o"));
    assert!(!managed_keys.contains(&"Enter"));
    assert!(!managed_keys.contains(&"m"));
}

#[test]
fn test_read_only_detail_and_diff_action_groups_expose_expected_keys() {
    // Arrange, Act
    let diff_keys = diff_actions(true)
        .iter()
        .map(|action| action.key)
        .collect::<Vec<_>>();
    let file_footer_keys = diff_footer_actions(DiffFooterContext {
        file_comment: DiffFileCommentAvailability::Available,
        can_mark_selected: true,
        can_submit: true,
        focus: DiffFocus::Files,
        has_review_comments: true,
        line_comment_state: DiffLineCommentFooterState::Ready { comment_count: 0 },
        sidebar_focus: DiffSidebarFocus::Files,
    })
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
            "f/Esc/Left",
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
    assert_eq!(
        file_footer_keys,
        ["q/Esc", "j/k", "Enter/l", "p", "Shift+C", "c", "?"]
    );
}

#[test]
fn test_diff_content_footer_exposes_line_navigation_and_file_return() {
    // Arrange, Act
    let content_footer_keys = diff_footer_actions(DiffFooterContext {
        file_comment: DiffFileCommentAvailability::Unavailable,
        can_mark_selected: false,
        can_submit: false,
        focus: DiffFocus::Content,
        has_review_comments: true,
        line_comment_state: DiffLineCommentFooterState::Ready { comment_count: 2 },
        sidebar_focus: DiffSidebarFocus::Files,
    })
    .iter()
    .map(|action| action.key)
    .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        content_footer_keys,
        ["q", "Esc/Left", "j/k", "Enter", "s", "?"]
    );
}

#[test]
fn test_diff_file_footer_exposes_completed_comment_submission() {
    // Arrange, Act
    let file_footer_keys = diff_footer_actions(DiffFooterContext {
        file_comment: DiffFileCommentAvailability::Available,
        can_mark_selected: false,
        can_submit: false,
        focus: DiffFocus::Files,
        has_review_comments: true,
        line_comment_state: DiffLineCommentFooterState::Ready { comment_count: 2 },
        sidebar_focus: DiffSidebarFocus::Files,
    })
    .iter()
    .map(|action| action.key)
    .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        file_footer_keys,
        ["q/Esc", "s", "j/k", "Enter/l", "p", "Shift+C", "c", "?"]
    );
}

#[test]
fn test_read_only_diff_hides_inline_comment_actions() {
    // Arrange, Act
    let help_labels = diff_actions(false)
        .iter()
        .map(|action| action.footer_label)
        .collect::<Vec<_>>();
    let footer_keys = diff_footer_actions(DiffFooterContext {
        file_comment: DiffFileCommentAvailability::Unavailable,
        can_mark_selected: false,
        can_submit: false,
        focus: DiffFocus::Content,
        has_review_comments: false,
        line_comment_state: DiffLineCommentFooterState::ReadOnly,
        sidebar_focus: DiffSidebarFocus::Files,
    })
    .iter()
    .map(|action| action.key)
    .collect::<Vec<_>>();

    // Assert
    assert!(!help_labels.contains(&"open/comment"));
    assert!(!help_labels.contains(&"comment file"));
    assert!(!help_labels.contains(&"save comment"));
    assert!(!help_labels.contains(&"submit comments"));
    assert_eq!(footer_keys, ["q", "Esc/Left", "j/k", "?"]);
}

#[test]
fn test_diff_content_footer_limits_actions_while_editing_comment() {
    // Arrange, Act
    let editing_keys = diff_footer_actions(DiffFooterContext {
        file_comment: DiffFileCommentAvailability::Unavailable,
        can_mark_selected: false,
        can_submit: false,
        focus: DiffFocus::Content,
        has_review_comments: false,
        line_comment_state: DiffLineCommentFooterState::Editing,
        sidebar_focus: DiffSidebarFocus::Files,
    })
    .iter()
    .map(|action| action.key)
    .collect::<Vec<_>>();

    // Assert
    assert_eq!(editing_keys, ["Alt/Shift+Enter", "Enter/Esc"]);
}

#[test]
fn test_diff_content_footer_exposes_visual_row_selection_actions() {
    // Arrange, Act
    let selecting_keys = diff_footer_actions(DiffFooterContext {
        file_comment: DiffFileCommentAvailability::Available,
        can_mark_selected: false,
        can_submit: false,
        focus: DiffFocus::Content,
        has_review_comments: false,
        line_comment_state: DiffLineCommentFooterState::Selecting,
        sidebar_focus: DiffSidebarFocus::Files,
    })
    .iter()
    .map(|action| action.key)
    .collect::<Vec<_>>();

    // Assert
    assert_eq!(selecting_keys, ["q", "Esc", "j/k", "Enter", "Shift+C"]);
}

#[test]
fn test_review_comment_actions_include_selection_and_submit_keys() {
    // Arrange, Act
    let actions = diff_footer_actions(DiffFooterContext {
        file_comment: DiffFileCommentAvailability::Unavailable,
        can_mark_selected: true,
        can_submit: true,
        focus: DiffFocus::Files,
        has_review_comments: true,
        line_comment_state: DiffLineCommentFooterState::Ready { comment_count: 2 },
        sidebar_focus: DiffSidebarFocus::Comments,
    });
    let comment_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        comment_keys,
        ["q", "s", "j/k", "Space", "Enter", "f/Esc", "Up/Down", "?"]
    );
}
