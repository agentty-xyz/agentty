use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::handle_mouse_event;
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::presentation::app_mode::{
    AppMode, ChatFocus, DiffFocus, DiffLineComments, DiffPreview, HelpContext, ViewportRect,
};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};
use crate::presentation::viewport::{
    LayoutSnapshot, ScrollRegion, ScrollRegionKind, ScrollbarDrag,
};
use crate::runtime::PresentationState;

fn chat_region() -> ScrollRegion {
    ScrollRegion {
        area: ViewportRect {
            height: 12,
            width: 78,
            x: 1,
            y: 3,
        },
        scrollbar: Some(ViewportRect {
            height: 10,
            width: 1,
            x: 78,
            y: 4,
        }),
        total_lines: 100,
        viewport_height: 10,
    }
}

fn presentation_with_chat_region() -> PresentationState {
    let presentation = PresentationState::default();
    presentation.set_layout_snapshot(LayoutSnapshot {
        chat_output: Some(chat_region()),
        ..LayoutSnapshot::default()
    });

    presentation
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn view_mode(scroll_offset: Option<u16>) -> AppMode {
    AppMode::View {
        session_id: "session-1".into(),
        scroll_offset,
    }
}

fn prompt_mode(scroll_offset: Option<u16>) -> AppMode {
    AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::default(),
        input: InputState::default(),
        scroll_offset,
        session_id: "session-1".into(),
        slash_state: PromptSlashState::default(),
    }
}

#[tokio::test]
async fn test_wheel_up_over_chat_pins_view_above_tail() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(None);
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 10, 8),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(87),
            ..
        }
    ));
}

#[tokio::test]
async fn test_wheel_down_over_chat_returns_to_follow_tail_at_bottom() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(88));
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollDown, 10, 8),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: None,
            ..
        }
    ));
}

#[tokio::test]
async fn test_wheel_outside_chat_region_is_ignored() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(None);
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 10, 20),
    );

    // Assert
    assert!(!changed);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: None,
            ..
        }
    ));
}

#[tokio::test]
async fn test_wheel_in_prompt_mode_scrolls_transcript_and_keeps_composer() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = prompt_mode(None);
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.insert_text("draft text");
    }
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 10, 8),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            scroll_offset: Some(87),
            input,
            ..
        } if input.text() == "draft text"
    ));
}

#[tokio::test]
async fn test_wheel_in_question_mode_keeps_answer_focus() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        questions: vec![QuestionItem {
            options: vec!["yes".to_string()],
            text: "Continue?".to_string(),
        }],
        responses: Vec::new(),
        selected_option_index: Some(0),
        session_id: "session-1".into(),
    };
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 10, 8),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Input,
            scroll_offset: Some(87),
            ..
        }
    ));
}

#[tokio::test]
async fn test_press_on_chat_thumb_starts_drag_and_drag_follows_pointer() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(0));
    let presentation = presentation_with_chat_region();

    // Act
    let press_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 78, 4),
    );
    let drag_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 8),
    );
    let drag_state = presentation.mouse_drag();
    handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Up(MouseButton::Left), 78, 8),
    );

    // Assert
    assert!(!press_changed);
    assert!(drag_changed);
    assert_eq!(
        drag_state,
        Some(ScrollbarDrag {
            grab_row_within_thumb: 0,
            region: ScrollRegionKind::ChatOutput,
        })
    );
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(40),
            ..
        }
    ));
    assert_eq!(presentation.mouse_drag(), None);
}

#[tokio::test]
async fn test_drag_to_track_bottom_returns_chat_to_follow_tail() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(0));
    let presentation = presentation_with_chat_region();
    handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 78, 4),
    );

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 13),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: None,
            ..
        }
    ));
}

#[tokio::test]
async fn test_press_on_track_outside_thumb_jumps_scroll_position() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(0));
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 78, 8),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(40),
            ..
        }
    ));
}

#[tokio::test]
async fn test_drag_without_press_is_ignored() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(5));
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 8),
    );

    // Assert
    assert!(!changed);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(5),
            ..
        }
    ));
}

#[tokio::test]
async fn test_wheel_over_diff_panel_scrolls_and_clamps() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 18,
        selected_diff_line_index: 0,
        session_id: "session-1".into(),
    };
    let presentation = PresentationState::default();
    presentation.set_layout_snapshot(LayoutSnapshot {
        diff_panel: Some(ScrollRegion {
            area: ViewportRect {
                height: 12,
                width: 60,
                x: 20,
                y: 1,
            },
            scrollbar: None,
            total_lines: 30,
            viewport_height: 10,
        }),
        ..LayoutSnapshot::default()
    });

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollDown, 30, 5),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 20,
            ..
        }
    ));
}

#[tokio::test]
async fn test_wheel_over_help_overlay_clamps_to_content() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: Vec::new(),
        },
        scroll_offset: 4,
    };
    let presentation = PresentationState::default();
    presentation.set_layout_snapshot(LayoutSnapshot {
        help_overlay: Some(ScrollRegion {
            area: ViewportRect {
                height: 10,
                width: 40,
                x: 10,
                y: 5,
            },
            scrollbar: None,
            total_lines: 13,
            viewport_height: 8,
        }),
        ..LayoutSnapshot::default()
    });

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollDown, 15, 7),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::Help {
            scroll_offset: 5,
            ..
        }
    ));
}

#[tokio::test]
async fn test_mouse_move_and_other_buttons_are_ignored() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(None);
    let presentation = presentation_with_chat_region();

    // Act
    let moved = handle_mouse_event(&mut app, &presentation, mouse(MouseEventKind::Moved, 10, 8));
    let right_pressed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Right), 78, 4),
    );

    // Assert
    assert!(!moved);
    assert!(!right_pressed);
    assert_eq!(presentation.mouse_drag(), None);
}

fn diff_region() -> ScrollRegion {
    ScrollRegion {
        area: ViewportRect {
            height: 12,
            width: 60,
            x: 20,
            y: 1,
        },
        scrollbar: Some(ViewportRect {
            height: 10,
            width: 1,
            x: 78,
            y: 2,
        }),
        total_lines: 100,
        viewport_height: 10,
    }
}

fn presentation_with_diff_region() -> PresentationState {
    let presentation = PresentationState::default();
    presentation.set_layout_snapshot(LayoutSnapshot {
        diff_panel: Some(diff_region()),
        ..LayoutSnapshot::default()
    });

    presentation
}

fn diff_mode(scroll_offset: u16) -> AppMode {
    AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset,
        selected_diff_line_index: 0,
        session_id: "session-1".into(),
    }
}

#[tokio::test]
async fn test_wheel_in_chat_mode_without_recorded_region_is_ignored() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(5));
    let presentation = PresentationState::default();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 10, 8),
    );

    // Assert
    assert!(!changed);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(5),
            ..
        }
    ));
}

#[tokio::test]
async fn test_wheel_in_list_mode_is_ignored() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::List;
    let presentation = presentation_with_chat_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollDown, 10, 8),
    );

    // Assert
    assert!(!changed);
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_wheel_over_help_overlay_scrolls_up_and_ignores_pointer_outside() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: Vec::new(),
        },
        scroll_offset: 5,
    };
    let presentation = PresentationState::default();
    presentation.set_layout_snapshot(LayoutSnapshot {
        help_overlay: Some(ScrollRegion {
            area: ViewportRect {
                height: 14,
                width: 48,
                x: 16,
                y: 5,
            },
            scrollbar: None,
            total_lines: 40,
            viewport_height: 12,
        }),
        ..LayoutSnapshot::default()
    });

    // Act
    let outside_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 2, 2),
    );
    let inside_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 20, 8),
    );

    // Assert
    assert!(!outside_changed);
    assert!(inside_changed);
    assert!(matches!(
        app.mode,
        AppMode::Help {
            scroll_offset: 2,
            ..
        }
    ));
}

#[tokio::test]
async fn test_wheel_over_help_mode_without_recorded_region_is_ignored() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: Vec::new(),
        },
        scroll_offset: 5,
    };
    let presentation = PresentationState::default();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::ScrollUp, 20, 8),
    );

    // Assert
    assert!(!changed);
}

#[tokio::test]
async fn test_press_on_diff_thumb_starts_drag_and_drag_follows_pointer() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = diff_mode(0);
    let presentation = presentation_with_diff_region();

    // Act
    let press_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 78, 2),
    );
    let drag_state = presentation.mouse_drag();
    let drag_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 6),
    );

    // Assert
    assert!(!press_changed);
    assert_eq!(
        drag_state,
        Some(ScrollbarDrag {
            grab_row_within_thumb: 0,
            region: ScrollRegionKind::DiffPanel,
        })
    );
    assert!(drag_changed);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 40,
            ..
        }
    ));
}

#[tokio::test]
async fn test_press_on_diff_track_outside_thumb_jumps_scroll_position() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = diff_mode(0);
    let presentation = presentation_with_diff_region();

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 78, 11),
    );

    // Assert
    assert!(changed);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 90,
            ..
        }
    ));
}

#[tokio::test]
async fn test_diff_drag_is_ignored_without_region_or_diff_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = diff_mode(0);
    let presentation = presentation_with_diff_region();
    handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 78, 2),
    );

    // Act
    presentation.set_layout_snapshot(LayoutSnapshot::default());
    let without_region_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 6),
    );
    presentation.set_layout_snapshot(LayoutSnapshot {
        diff_panel: Some(diff_region()),
        ..LayoutSnapshot::default()
    });
    app.mode = AppMode::List;
    let without_diff_mode_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 6),
    );

    // Assert
    assert!(!without_region_changed);
    assert!(!without_diff_mode_changed);
}

#[tokio::test]
async fn test_chat_drag_is_ignored_without_region_or_chat_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(0));
    let presentation = presentation_with_chat_region();
    handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 78, 4),
    );

    // Act
    presentation.set_layout_snapshot(LayoutSnapshot::default());
    let without_region_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 8),
    );
    presentation.set_layout_snapshot(LayoutSnapshot {
        chat_output: Some(chat_region()),
        ..LayoutSnapshot::default()
    });
    app.mode = AppMode::List;
    let without_chat_mode_changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Drag(MouseButton::Left), 78, 8),
    );

    // Assert
    assert!(!without_region_changed);
    assert!(!without_chat_mode_changed);
}

#[tokio::test]
async fn test_press_outside_any_scrollbar_clears_drag_and_changes_nothing() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = view_mode(Some(3));
    let presentation = presentation_with_chat_region();
    presentation.set_mouse_drag(Some(ScrollbarDrag {
        grab_row_within_thumb: 0,
        region: ScrollRegionKind::ChatOutput,
    }));

    // Act
    let changed = handle_mouse_event(
        &mut app,
        &presentation,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 8),
    );

    // Assert
    assert!(!changed);
    assert_eq!(presentation.mouse_drag(), None);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(3),
            ..
        }
    ));
}
