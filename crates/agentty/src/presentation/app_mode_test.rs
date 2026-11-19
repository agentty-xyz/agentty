//! App mode tests for diff comments, help keybinding contexts, and view
//! restoration.

use std::path::PathBuf;

use super::super::help_action::{HelpAction, ViewSessionState};
use super::{
    AppMode, ChatFocus, ConfirmationViewMode, DiffCommentTarget, DiffFocus, DiffLineCommentAnchor,
    DiffLineCommentTarget, DiffLineComments, DiffLineSide, DiffPreview,
    DiffPreviewUnavailableReason, DiffRestoreTarget, DiffReviewComments, DiffSidebarFocus,
    HelpContext, PromptModeSnapshot, QuestionModeSnapshot,
};
use crate::domain::input::InputState;
use crate::domain::question::QuestionAttachmentState;
use crate::domain::session::PublishBranchAction;
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachment, PromptAttachmentState, PromptHistoryState,
    PromptSlashState,
};

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
        scroll_offset: Some(70_000),
        session_id: "session-id".into(),
    };

    // Act
    let mode = confirmation_view_mode.into_view_mode();

    // Assert
    assert!(matches!(
        mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(70_000),
        } if session_id == "session-id"
    ));
}

#[test]
fn test_help_context_view_keybindings_for_in_progress_show_sync_and_queue_and_hide_merge() {
    // Arrange
    let context = HelpContext::View {
        can_fork_session: true,
        can_merge_session_branch: true,
        can_mutate_session_branch: true,
        can_open_worktree: true,
        can_rebase_session_branch: true,
        can_reply_to_session: true,
        can_start_staged_session: false,
        can_view_review_comments: false,
        publish_pull_request_action: None,
        review_watch_toggle: None,
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
    assert!(
        bindings
            .iter()
            .any(|binding| binding.key == "Enter" && binding.popup_label == "Queue message")
    );
    assert!(bindings.iter().any(|binding| binding.key == "d"));
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
        can_reply_to_session: true,
        can_start_staged_session: false,
        can_view_review_comments: false,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        review_watch_toggle: None,
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
fn test_help_context_restore_mode_unboxes_diff_review_comments() {
    // Arrange
    let context = HelpContext::Diff {
        can_comment: true,
        can_reply_review_comments: false,
        diff: "diff --git a/a.rs b/a.rs".to_string(),
        file_explorer_selected_index: 2,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::Off { request_id: 3 },
        review_comments: Some(Box::new(DiffReviewComments::loading(7))),
        restore: None,
        session_id: "session-id".into(),
        scroll_offset: 5,
        selected_diff_line_index: 1,
    };

    // Act
    let mode = context.restore_mode();

    // Assert
    assert!(matches!(
        mode,
        AppMode::Diff {
            file_explorer_selected_index: 2,
            focus: DiffFocus::Content,
            review_comments: Some(ref review_comments),
            scroll_cache: None,
            scroll_offset: 5,
            selected_diff_line_index: 1,
            ref session_id,
            ..
        } if *review_comments == DiffReviewComments::loading(7)
            && session_id.as_str() == "session-id"
    ));
}

#[test]
fn test_help_context_diff_keybindings_follow_linked_review_comment_reply() {
    // Arrange
    let diff_context = |review_comments: Option<Box<DiffReviewComments>>,
                        can_reply_review_comments: bool| HelpContext::Diff {
        can_comment: false,
        can_reply_review_comments,
        diff: "diff --git a/a.rs b/a.rs".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::default(),
        review_comments,
        restore: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
        selected_diff_line_index: 0,
    };

    // Act
    let unlinked_keys = diff_context(None, true)
        .keybindings()
        .into_iter()
        .map(|binding| binding.key)
        .collect::<Vec<_>>();
    let linked_keys = diff_context(Some(Box::new(DiffReviewComments::loading(1))), true)
        .keybindings()
        .into_iter()
        .map(|binding| binding.key)
        .collect::<Vec<_>>();

    // Assert
    assert!(!unlinked_keys.contains(&"c"));
    assert!(!unlinked_keys.contains(&"Space"));
    assert!(linked_keys.contains(&"c"));
    assert!(linked_keys.contains(&"Space"));
    assert!(linked_keys.contains(&"Enter"));
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
        can_reply_to_session: true,
        can_start_staged_session: false,
        can_view_review_comments: false,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        review_watch_toggle: None,
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

#[test]
fn into_input_attachments_returns_composer_and_answer_images() {
    // Arrange
    let mut prompt_attachments = PromptAttachmentState::default();
    prompt_attachments
        .attachments
        .push(PromptAttachment::new(1, PathBuf::from("/tmp/active.png")));
    prompt_attachments
        .archived_attachments
        .push(PromptAttachment::new(2, PathBuf::from("/tmp/undo.png")));
    let prompt_mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: prompt_attachments,
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::default(),
        scroll_offset: None,
        session_id: "prompt-session".into(),
        slash_state: PromptSlashState::default(),
    };
    let mut answer_attachments = QuestionAttachmentState::default();
    answer_attachments
        .answered
        .push(PromptAttachment::new(1, PathBuf::from("/tmp/answered.png")));
    let question_mode = AppMode::Question {
        at_mention_state: None,
        attachment_state: answer_attachments,
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        questions: Vec::new(),
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "question-session".into(),
    };

    // Act
    let paths = [prompt_mode, question_mode, AppMode::List].map(|mode| {
        mode.into_input_attachments()
            .into_iter()
            .map(|attachment| attachment.local_image_path)
            .collect::<Vec<_>>()
    });

    // Assert
    assert_eq!(
        paths,
        [
            vec![
                PathBuf::from("/tmp/active.png"),
                PathBuf::from("/tmp/undo.png")
            ],
            vec![PathBuf::from("/tmp/answered.png")],
            Vec::new(),
        ]
    );
}

#[test]
fn into_input_attachments_returns_images_saved_in_diff_restore_targets() {
    // Arrange
    let mut answer_attachments = QuestionAttachmentState::default();
    answer_attachments
        .answered
        .push(PromptAttachment::new(1, PathBuf::from("/tmp/answer.png")));
    let question_snapshot = QuestionModeSnapshot {
        at_mention_state: None,
        attachment_state: answer_attachments,
        current_index: 0,
        input: InputState::default(),
        questions: Vec::new(),
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "question-session".into(),
    };
    let diff_mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::default(),
        review_comments: None,
        restore: Some(Box::new(DiffRestoreTarget::Question(question_snapshot))),
        scroll_cache: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session_id: "question-session".into(),
    };
    let mut prompt_attachments = PromptAttachmentState::default();
    prompt_attachments
        .attachments
        .push(PromptAttachment::new(1, PathBuf::from("/tmp/prompt.png")));
    let prompt_snapshot = PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: prompt_attachments,
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::default(),
        scroll_offset: None,
        session_id: "prompt-session".into(),
        slash_state: PromptSlashState::default(),
    };
    let loading_mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: None,
        request_id: 1,
        restore: Some(Box::new(DiffRestoreTarget::Prompt(prompt_snapshot))),
        session_id: "prompt-session".into(),
        sidebar_focus: DiffSidebarFocus::Files,
    };
    let view_diff_mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: None,
        request_id: 2,
        restore: None,
        session_id: "view-session".into(),
        sidebar_focus: DiffSidebarFocus::Files,
    };

    // Act
    let paths = [diff_mode, loading_mode, view_diff_mode].map(|mode| {
        mode.into_input_attachments()
            .into_iter()
            .map(|attachment| attachment.local_image_path)
            .collect::<Vec<_>>()
    });

    // Assert
    assert_eq!(
        paths,
        [
            vec![PathBuf::from("/tmp/answer.png")],
            vec![PathBuf::from("/tmp/prompt.png")],
            Vec::new(),
        ]
    );
}

#[test]
fn test_active_at_mention_mut_returns_question_and_editing_diff_lookups() {
    // Arrange
    let mut question_mode = AppMode::Question {
        at_mention_state: Some(PromptAtMentionState::new(Vec::new())),
        attachment_state: QuestionAttachmentState::default(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("@answer".to_string()),
        questions: Vec::new(),
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "question-session".into(),
    };
    let mut editing_comments = DiffLineComments::default();
    editing_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "fn main() {}".to_string(),
        line: 1,
        path: "src/main.rs".to_string(),
        side: DiffLineSide::New,
    }));
    editing_comments
        .editing_input_mut()
        .expect("new comment should be editable")
        .insert_text("@comment");
    editing_comments.at_mention_state = Some(Box::new(PromptAtMentionState::new(Vec::new())));
    let mut finished_comments = editing_comments.clone();
    finished_comments.editing_index = None;
    let mut editing_diff_mode = diff_mode_with_line_comments(editing_comments);
    let mut finished_diff_mode = diff_mode_with_line_comments(finished_comments);
    let mut view_mode = AppMode::View {
        scroll_offset: None,
        session_id: "view-session".into(),
    };

    // Act
    let question_lookup = question_mode
        .active_at_mention_mut()
        .map(|active| (active.input.text().to_string(), active.session_id.clone()));
    let diff_lookup = editing_diff_mode
        .active_at_mention_mut()
        .map(|active| (active.input.text().to_string(), active.session_id.clone()));
    let finished_diff_has_lookup = finished_diff_mode.active_at_mention_mut().is_some();
    let view_has_lookup = view_mode.active_at_mention_mut().is_some();

    // Assert
    assert_eq!(
        question_lookup,
        Some(("@answer".to_string(), "question-session".into()))
    );
    assert_eq!(
        diff_lookup,
        Some(("@comment".to_string(), "diff-session".into()))
    );
    assert!(!finished_diff_has_lookup);
    assert!(!view_has_lookup);
}

/// Restores diff mode for `diff-session` with the supplied comment state.
fn diff_mode_with_line_comments(line_comments: DiffLineComments) -> AppMode {
    HelpContext::Diff {
        can_comment: true,
        can_reply_review_comments: false,
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments,
        preview: DiffPreview::Off { request_id: 0 },
        review_comments: None,
        restore: None,
        session_id: "diff-session".into(),
        scroll_offset: 0,
        selected_diff_line_index: 0,
    }
    .restore_mode()
}
