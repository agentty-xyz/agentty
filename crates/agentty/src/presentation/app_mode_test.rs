use super::super::help_action::{HelpAction, ViewSessionState};
use super::{
    AppMode, ConfirmationViewMode, DiffCommentTarget, DiffLineCommentAnchor, DiffLineCommentTarget,
    DiffLineComments, DiffLineSide, DiffPreview, DiffPreviewUnavailableReason, HelpContext,
};
use crate::domain::session::PublishBranchAction;

#[test]
fn test_diff_line_comments_edit_and_build_compact_prompt() {
    // Arrange
    let anchor = DiffLineCommentAnchor {
        content: "println!(\"review\");".to_string(),
        line: 12,
        path: "src/main.rs".to_string(),
        side: DiffLineSide::New,
    };
    let mut line_comments = DiffLineComments::default();

    // Act
    line_comments.start_editing_target(DiffLineCommentTarget::single(anchor.clone()));
    line_comments
        .editing_input_mut()
        .expect("new comment should be editable")
        .insert_text("Please explain this change.");
    line_comments.finish_editing();
    let prompt = line_comments.prompt_text();

    // Assert
    assert_eq!(
        prompt,
        "Line comments:\n- src/main.rs:12 [new]: Please explain this change."
    );
    assert!(!line_comments.is_editing());

    // Act — selecting the same line edits the existing comment.
    let editing_index = line_comments.start_editing_target(DiffLineCommentTarget::single(anchor));
    let selected_target = line_comments.selected_comment_target();

    // Assert
    assert_eq!(editing_index, 0);
    assert_eq!(line_comments.comments.len(), 1);
    assert_eq!(selected_target, Some(&line_comments.comments[0].target));

    // Act
    line_comments.select_comment(usize::MAX);

    // Assert
    assert_eq!(line_comments.selected_comment_index(), None);
}

#[test]
fn test_diff_comments_build_file_and_line_prompt_sections() {
    // Arrange
    let mut comments = DiffLineComments::default();
    let file_target = DiffCommentTarget::file("src/main.rs");
    let line_target = DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "review();".to_string(),
        line: 8,
        path: "src/main.rs".to_string(),
        side: DiffLineSide::New,
    });
    comments.start_editing_target(file_target.clone());
    comments
        .editing_input_mut()
        .expect("file comment should be editable")
        .insert_text("Review the module boundaries.\nLine comments:\nKeep this attached.");
    comments.finish_editing();
    comments.start_editing_target(line_target.clone());
    comments
        .editing_input_mut()
        .expect("line comment should be editable")
        .insert_text("Explain this call.\nFile comments:\nCheck the error path.");
    comments.finish_editing();

    // Act
    let prompt = comments.prompt_text();

    // Assert
    assert_eq!(
        prompt,
        concat!(
            "File comments:\n",
            "- src/main.rs: Review the module boundaries.\n",
            "  | Line comments:\n",
            "  | Keep this attached.\n\n",
            "Line comments:\n",
            "- src/main.rs:8 [new]: Explain this call.\n",
            "  | File comments:\n",
            "  | Check the error path.",
        )
    );
    assert_ne!(file_target, DiffCommentTarget::from(line_target));
}

#[test]
fn test_deleted_diff_line_prompt_includes_captured_source() {
    // Arrange
    let anchor = DiffLineCommentAnchor {
        content: "let message = \"old\";".to_string(),
        line: 7,
        path: "src/old.rs".to_string(),
        side: DiffLineSide::Old,
    };

    // Act
    let prompt_line = anchor.prompt_line("Keep this behavior.");

    // Assert
    assert_eq!(
        prompt_line,
        "- src/old.rs:7 [old, source=\"let message = \\\"old\\\";\"]: Keep this behavior."
    );
}

#[test]
fn test_diff_line_comment_target_formats_new_and_mixed_row_ranges() {
    // Arrange
    let new_target = DiffLineCommentTarget::from_anchors(vec![
        DiffLineCommentAnchor {
            content: "first();".to_string(),
            line: 4,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        },
        DiffLineCommentAnchor {
            content: "second();".to_string(),
            line: 5,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        },
    ])
    .expect("new-line range should create a target");
    let mixed_target = DiffLineCommentTarget::from_anchors(vec![
        DiffLineCommentAnchor {
            content: "old();".to_string(),
            line: 7,
            path: "src/old.rs".to_string(),
            side: DiffLineSide::Old,
        },
        DiffLineCommentAnchor {
            content: "new();".to_string(),
            line: 8,
            path: "src/new.rs".to_string(),
            side: DiffLineSide::New,
        },
    ])
    .expect("mixed range should create a target");

    // Act
    let new_prompt = new_target.prompt_line("Explain this range.");
    let mixed_prompt = mixed_target.prompt_line("Preserve the behavior.");
    let last_anchor = mixed_target.last_anchor();
    let empty_target = DiffLineCommentTarget::from_anchors(Vec::new());

    // Assert
    assert_eq!(new_prompt, "- src/main.rs:4-5 [new]: Explain this range.");
    assert_eq!(
        mixed_prompt,
        "- src/old.rs:7 [old]..src/new.rs:8 [new], deleted source=[\"old();\"]: Preserve the \
         behavior."
    );
    assert_eq!(last_anchor.content, "new();");
    assert_eq!(empty_target, None);
}

#[test]
fn test_diff_line_comments_tracks_visual_row_selection() {
    // Arrange
    let mut line_comments = DiffLineComments::default();
    let target = DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "selected();".to_string(),
        line: 4,
        path: "src/lib.rs".to_string(),
        side: DiffLineSide::New,
    });

    // Act
    line_comments.start_selection(3);
    line_comments.start_selection(9);
    let upward_bounds = line_comments.selected_row_bounds(1);
    let downward_bounds = line_comments.selected_row_bounds(5);
    line_comments.start_editing_target(target);

    // Assert
    assert!(line_comments.is_selecting());
    assert!(line_comments.is_editing());
    assert_eq!(line_comments.selected_comment_index(), Some(0));
    assert_eq!(upward_bounds, (1, 3));
    assert_eq!(downward_bounds, (3, 5));

    // Act
    line_comments.finish_editing();

    // Assert
    assert!(!line_comments.is_selecting());
    assert_eq!(line_comments.selected_row_bounds(5), (5, 5));
    assert_eq!(line_comments.selected_comment_index(), None);

    // Act
    line_comments.start_selection(5);
    line_comments.cancel_selection();

    // Assert
    assert!(!line_comments.is_selecting());
    assert_eq!(line_comments.selected_comment_index(), None);
}

#[test]
fn test_diff_line_comments_remove_blank_editor() {
    // Arrange
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "removed".to_string(),
        line: 3,
        path: "src/lib.rs".to_string(),
        side: DiffLineSide::Old,
    }));

    // Act
    line_comments.finish_editing();
    line_comments.finish_editing();

    // Assert
    assert_eq!(line_comments.comments, []);
    assert!(line_comments.editing_input_mut().is_none());
    assert_eq!(line_comments.selected_comment_index(), None);
    assert_eq!(line_comments.prompt_text(), "");
}

#[test]
fn test_confirmation_view_mode_into_view_mode_restores_view_identity() {
    // Arrange
    let confirmation_view_mode = ConfirmationViewMode {
        scroll_offset: Some(7),
        session_id: "session-id".into(),
    };

    // Act
    let mode = confirmation_view_mode.into_view_mode();

    // Assert
    assert!(matches!(
        mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(7),
        } if session_id == "session-id"
    ));
}

#[test]
fn test_help_context_view_keybindings_for_in_progress_show_sync_and_hide_edit_actions() {
    // Arrange
    let context = HelpContext::View {
        can_fork_session: true,
        can_merge_session_branch: true,
        can_mutate_session_branch: true,
        can_open_worktree: true,
        can_rebase_session_branch: true,
        can_show_diff: true,
        can_reply_to_session: true,
        can_start_staged_session: false,
        can_view_review_comments: false,
        publish_pull_request_action: None,
        session_id: "session-id".into(),
        session_state: ViewSessionState::InProgress,
        scroll_offset: Some(2),
    };

    // Act
    let bindings = context.keybindings();

    // Assert
    assert!(bindings.iter().any(|binding| binding.key == "q"));
    assert!(bindings.iter().any(|binding| binding.key == "j/k"));
    assert!(bindings.iter().any(|binding| binding.key == "?"));
    assert!(bindings.iter().any(|binding| binding.key == "Ctrl+c"));
    assert!(bindings.iter().any(|binding| binding.key == "r"));
    assert!(!bindings.iter().any(|binding| binding.key == "Enter"));
    assert!(!bindings.iter().any(|binding| binding.key == "d"));
    assert!(!bindings.iter().any(|binding| binding.key == "m"));
    assert!(!bindings.iter().any(|binding| binding.key == "S-Tab"));
}

#[test]
fn test_help_context_restore_mode_ignores_help_only_view_fields() {
    // Arrange
    let context = HelpContext::View {
        can_fork_session: true,
        can_merge_session_branch: true,
        can_mutate_session_branch: true,
        can_open_worktree: true,
        can_rebase_session_branch: true,
        can_show_diff: true,
        can_reply_to_session: true,
        can_start_staged_session: false,
        can_view_review_comments: false,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_id: "session-id".into(),
        session_state: ViewSessionState::InProgress,
        scroll_offset: Some(4),
    };

    // Act
    let mode = context.restore_mode();

    // Assert
    assert!(matches!(
        mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(4),
            ..
        } if session_id == "session-id"
    ));
}

#[test]
fn test_help_context_view_keybindings_include_publish_pull_request_action() {
    // Arrange
    let context = HelpContext::View {
        can_fork_session: true,
        can_merge_session_branch: true,
        can_mutate_session_branch: true,
        can_open_worktree: true,
        can_rebase_session_branch: true,
        can_show_diff: true,
        can_reply_to_session: true,
        can_start_staged_session: false,
        can_view_review_comments: false,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_id: "session-id".into(),
        session_state: ViewSessionState::Interactive,
        scroll_offset: None,
    };

    // Act
    let bindings = context.keybindings();

    // Assert
    assert!(bindings.iter().any(|binding| binding.key == "p"));
}

#[test]
fn test_help_context_list_keybindings_return_stored_actions() {
    // Arrange
    let keybindings = vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("help", "?", "Help"),
    ];
    let context = HelpContext::List { keybindings };

    // Act
    let bindings = context.keybindings();

    // Assert
    assert_eq!(bindings.len(), 2);
    assert!(bindings.iter().any(|binding| binding.key == "q"));
    assert!(bindings.iter().any(|binding| binding.key == "?"));
}

#[test]
fn test_diff_preview_tracks_enabled_state_and_request_generation() {
    // Arrange
    let states = [
        DiffPreview::Off { request_id: 0 },
        DiffPreview::Unsupported { request_id: 1 },
        DiffPreview::Loading {
            path: "README.md".to_string(),
            request_id: 2,
        },
        DiffPreview::Ready {
            content: "# Ready".to_string(),
            path: "README.md".to_string(),
            request_id: 3,
        },
        DiffPreview::Unavailable {
            path: "README.md".to_string(),
            reason: DiffPreviewUnavailableReason::Deleted,
            request_id: 4,
        },
    ];

    // Act
    let enabled = states
        .iter()
        .map(DiffPreview::is_enabled)
        .collect::<Vec<_>>();
    let request_ids = states
        .iter()
        .map(DiffPreview::request_id)
        .collect::<Vec<_>>();
    let paths = states.iter().map(DiffPreview::path).collect::<Vec<_>>();
    let next_request_id = states[4].next_request_id();
    let disabled = states[3].disabled();

    // Assert
    assert_eq!(enabled, [false, true, true, true, true]);
    assert_eq!(request_ids, [0, 1, 2, 3, 4]);
    assert_eq!(
        paths,
        [
            None,
            None,
            Some("README.md"),
            Some("README.md"),
            Some("README.md")
        ]
    );
    assert_eq!(next_request_id, 5);
    assert_eq!(disabled, DiffPreview::Off { request_id: 4 });
}
