use crossterm::event::KeyEvent;
use ratatui::layout::Rect;

use super::super::handle_with_cache;
use crate::app::App;
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::runtime::EventResult;
use crate::ui::RenderCacheStore;

/// Fake terminal size used by tests that don't exercise scrolling.
pub(super) const TEST_TERMINAL_SIZE: Rect = Rect::new(0, 0, 80, 24);

/// Creates a question mode with predefined options for navigation tests.
///
/// Defaults `selected_option_index` to `Some(0)` matching production
/// behavior where the first option is pre-selected.
pub(super) fn question_mode_with_options() -> AppMode {
    AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem {
            options: vec![
                "Option A".to_string(),
                "Option B".to_string(),
                "Option C".to_string(),
            ],
            text: "Pick one?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: Some(0),
    }
}

/// Creates a free-text question mode with the given text and cursor
/// position for modifier key tests.
pub(super) fn free_text_question_mode(text: &str, cursor: usize) -> AppMode {
    let mut input = InputState::with_text(text.to_string());
    input.cursor = cursor;

    AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Question?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input,
        scroll_offset: None,
        selected_option_index: None,
    }
}

pub(super) async fn handle(app: &mut App, terminal_size: Rect, key: KeyEvent) -> EventResult {
    handle_with_cache(app, &RenderCacheStore::default(), terminal_size, key).await
}
