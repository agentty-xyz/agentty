//! Usage panel dismissal, scrolling, and scroll-bound key tests.

use ag_contracts::test_support::token_usage;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::{handle, max_scroll_offset};
use crate::domain::session_usage::{LiveUsageKey, LiveUsageRun, SessionUsage};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode, UsagePanel, UsageRestore};
use crate::runtime::EventResult;
use crate::test_support::{new_test_app_without_retained_base_dir, prompt_mode_snapshot};

/// Largest scroll offset the test panels can reach.
const MAX_SCROLL_OFFSET: u16 = 20;

/// Returns the usage panel restoring `restore`, scrolled by
/// `scroll_offset`.
fn usage_mode(restore: UsageRestore, scroll_offset: u16) -> AppMode {
    AppMode::Usage(Box::new(UsagePanel {
        restore,
        scroll_offset,
        usage: SessionUsage::default(),
    }))
}

/// Returns the restore target of the session view of `sess`.
fn view_restore() -> UsageRestore {
    UsageRestore::View(ConfirmationViewMode {
        scroll_offset: Some(4),
        session_id: "sess".into(),
    })
}

/// Returns the panel's scroll offset after pressing `code` at
/// `scroll_offset`.
fn scroll_after(scroll_offset: u16, code: KeyCode) -> Option<u16> {
    let mut mode = usage_mode(view_restore(), scroll_offset);
    handle(
        &mut mode,
        MAX_SCROLL_OFFSET,
        KeyEvent::new(code, KeyModifiers::NONE),
    );

    match mode {
        AppMode::Usage(panel) => Some(panel.scroll_offset),
        _ => None,
    }
}

#[test]
fn test_esc_restores_the_session_view() {
    // Arrange
    let mut mode = usage_mode(view_restore(), 3);

    // Act
    let result = handle(
        &mut mode,
        MAX_SCROLL_OFFSET,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(
        mode,
        AppMode::View {
            scroll_offset: Some(4),
            ref session_id,
        } if session_id == "sess"
    ));
}

#[test]
fn test_esc_restores_the_prompt_composer() {
    // Arrange
    let mut mode = usage_mode(
        UsageRestore::Prompt(Box::new(prompt_mode_snapshot("sess", "draft"))),
        0,
    );

    // Act
    handle(
        &mut mode,
        MAX_SCROLL_OFFSET,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        mode,
        AppMode::Prompt { ref input, ref session_id, .. }
            if input.text() == "draft" && session_id == "sess"
    ));
}

#[test]
fn test_scroll_keys_move_by_a_line_or_a_page_and_stop_at_the_top() {
    // Arrange, Act, Assert
    assert_eq!(scroll_after(0, KeyCode::Char('j')), Some(1));
    assert_eq!(scroll_after(0, KeyCode::Down), Some(1));
    assert_eq!(scroll_after(2, KeyCode::Char('k')), Some(1));
    assert_eq!(scroll_after(2, KeyCode::Up), Some(1));
    assert_eq!(scroll_after(0, KeyCode::Up), Some(0));
    assert_eq!(scroll_after(0, KeyCode::PageDown), Some(10));
    assert_eq!(scroll_after(15, KeyCode::PageUp), Some(5));
    assert_eq!(scroll_after(5, KeyCode::PageUp), Some(0));
    assert_eq!(scroll_after(5, KeyCode::Char('u')), Some(5));
}

#[test]
fn test_scrolling_past_the_end_stops_at_the_bound() {
    // Arrange, Act
    let page_past_end = scroll_after(15, KeyCode::PageDown);
    let line_past_end = scroll_after(MAX_SCROLL_OFFSET, KeyCode::Char('j'));
    let back_up = scroll_after(MAX_SCROLL_OFFSET, KeyCode::Char('k'));

    // Assert
    assert_eq!(page_past_end, Some(MAX_SCROLL_OFFSET));
    assert_eq!(line_past_end, Some(MAX_SCROLL_OFFSET));
    assert_eq!(back_up, Some(MAX_SCROLL_OFFSET - 1));
}

#[test]
fn test_other_modes_are_left_unchanged() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    handle(
        &mut mode,
        MAX_SCROLL_OFFSET,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(mode, AppMode::List));
}

#[tokio::test]
async fn test_max_scroll_offset_follows_the_panel_and_its_live_rows() {
    // Arrange
    let mut app = new_test_app_without_retained_base_dir().await;
    let list_bound = max_scroll_offset(&app, Rect::new(0, 0, 100, 12));
    app.mode = usage_mode(view_restore(), 0);
    let empty_bound = max_scroll_offset(&app, Rect::new(0, 0, 100, 40));
    for index in 0..6 {
        app.session_live_usage.insert(
            LiveUsageKey {
                run: LiveUsageRun::Attempt(format!("attempt-{index}")),
                session_id: "sess".into(),
            },
            token_usage(&format!("model-{index}"), 10, 1),
        );
    }

    // Act
    let short_bound = max_scroll_offset(&app, Rect::new(0, 0, 100, 12));
    let tall_bound = max_scroll_offset(&app, Rect::new(0, 0, 100, 60));

    // Assert
    assert_eq!(list_bound, 0);
    assert_eq!(empty_bound, 0);
    assert!(short_bound > 0);
    assert_eq!(tall_bound, 0);
}
