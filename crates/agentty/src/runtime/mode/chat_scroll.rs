//! Shared session-transcript scrolling used by the chat page modes.
//!
//! Session view, the prompt composer, and question mode all scroll the same
//! transcript with the same keys, so the metrics and offset math live here
//! instead of in each mode handler.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::domain::session::SessionId;
use crate::runtime::mode::session_output_metric;
use crate::ui::page::session_chat::{self, SessionChatLayoutInput};
use crate::ui::{RenderCacheStore, input_layout, layout, session_format};

/// Bottom-panel height of a chat page that only reserves a footer row.
const FOOTER_ONLY_BOTTOM_HEIGHT: u16 = 1;

/// Scroll metrics for the session transcript rendered above the bottom panel.
#[derive(Clone, Copy)]
pub(crate) struct ChatScrollMetrics {
    /// Rendered transcript height in terminal lines.
    pub(crate) total_lines: u16,
    /// Visible transcript height in terminal lines.
    pub(crate) view_height: u16,
}

impl ChatScrollMetrics {
    /// Computes transcript scroll metrics for one session at `terminal_size`.
    ///
    /// The visible height comes from the session chat page layout for the
    /// current mode, so a tall prompt composer or an open suggestion dropdown
    /// shrinks the scroll viewport by the same rows it hides on screen.
    pub(crate) fn new(
        app: &App,
        render_cache_store: &RenderCacheStore,
        session_id: &SessionId,
        session_index: usize,
        terminal_size: Rect,
    ) -> Self {
        let page_area = layout::app_frame_areas(terminal_size).content_area;
        let output_width = page_area.width.saturating_sub(2);
        let (_, review_text) = app.review_view_state(session_id);

        let total_lines = session_output_metric::rendered_output_line_count_with_cache(
            app,
            render_cache_store,
            session_id,
            session_index,
            output_width,
        );
        let view_height = app.sessions.session_at(session_index).map_or_else(
            || Self::footer_only_view_height(page_area),
            |session| {
                session_chat::transcript_view_height(SessionChatLayoutInput {
                    area: page_area,
                    default_reasoning_level: app.settings.reasoning_level,
                    mode: &app.mode,
                    review_text,
                    session,
                    wall_clock_unix_seconds: app.wall_clock_unix_seconds(),
                })
            },
        );

        Self {
            total_lines,
            view_height,
        }
    }

    /// Returns metrics for a transcript that renders no lines, used when the
    /// scrolled session is not loaded into the session list.
    pub(crate) fn empty(terminal_size: Rect) -> Self {
        Self {
            total_lines: 0,
            view_height: Self::footer_only_view_height(
                layout::app_frame_areas(terminal_size).content_area,
            ),
        }
    }

    /// Returns the transcript height for a chat page with a one-row footer.
    ///
    /// Used when no session snapshot is available to derive the real header and
    /// bottom-panel geometry from.
    fn footer_only_view_height(page_area: Rect) -> u16 {
        let session_areas = layout::session_chat_areas(page_area, FOOTER_ONLY_BOTTOM_HEIGHT, 0);

        input_layout::panel_inner_height(
            session_areas.output_area,
            session_format::session_output_panel_borders(),
        )
    }
}

/// Returns whether `key` scrolls the session transcript.
///
/// Handlers that swallow every other key check this before building
/// [`ChatScrollMetrics`], because those metrics lay out and fingerprint the
/// whole transcript and would otherwise run on each ignored keystroke.
pub(crate) fn is_scroll_key(key: KeyEvent) -> bool {
    ScrollStep::from_key(key).is_some()
}

/// Applies one transcript scroll key to `scroll_offset`.
///
/// Handles `j`/`k`/`Down`/`Up` line steps, `g`/`G` jumps, and `Ctrl+d`/`Ctrl+u`
/// half-page steps. A `None` offset keeps the transcript pinned to the newest
/// output. Returns `true` when the key was consumed as a scroll action.
pub(crate) fn apply_scroll_key(
    scroll_offset: &mut Option<u16>,
    metrics: ChatScrollMetrics,
    key: KeyEvent,
) -> bool {
    let Some(scroll_step) = ScrollStep::from_key(key) else {
        return false;
    };

    match scroll_step {
        ScrollStep::LineDown => {
            *scroll_offset = scroll_offset_down(*scroll_offset, metrics, 1);
        }
        ScrollStep::LineUp => {
            *scroll_offset = Some(scroll_offset_up(*scroll_offset, metrics, 1));
        }
        ScrollStep::Top => *scroll_offset = Some(0),
        ScrollStep::Bottom => *scroll_offset = None,
        ScrollStep::HalfPageDown => {
            *scroll_offset = scroll_offset_down(*scroll_offset, metrics, half_page_step(metrics));
        }
        ScrollStep::HalfPageUp => {
            *scroll_offset = Some(scroll_offset_up(
                *scroll_offset,
                metrics,
                half_page_step(metrics),
            ));
        }
    }

    true
}

/// One transcript movement requested by a scroll key.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ScrollStep {
    /// Jump to the newest output and keep following it.
    Bottom,
    /// Move down by half a viewport.
    HalfPageDown,
    /// Move up by half a viewport.
    HalfPageUp,
    /// Move down one line.
    LineDown,
    /// Move up one line.
    LineUp,
    /// Jump to the oldest output.
    Top,
}

impl ScrollStep {
    /// Returns the transcript movement `key` requests, if it is a scroll key.
    fn from_key(key: KeyEvent) -> Option<Self> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => Some(Self::LineDown),
            KeyCode::Char('k') | KeyCode::Up => Some(Self::LineUp),
            KeyCode::Char('g') => Some(Self::Top),
            KeyCode::Char('G') => Some(Self::Bottom),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Self::HalfPageDown)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Self::HalfPageUp)
            }
            _ => None,
        }
    }
}

/// Returns the next scroll offset after scrolling down by `step` lines.
///
/// Returns `None` once the transcript bottom is reached so live output keeps
/// following the newest lines. A `None` offset already sits at that bottom, so
/// scrolling down again stays pinned there.
pub(crate) fn scroll_offset_down(
    scroll_offset: Option<u16>,
    metrics: ChatScrollMetrics,
    step: u16,
) -> Option<u16> {
    let current_offset = scroll_offset?;

    let next_offset = current_offset.saturating_add(step.max(1));
    if next_offset >= metrics.total_lines.saturating_sub(metrics.view_height) {
        return None;
    }

    Some(next_offset)
}

/// Returns the next scroll offset after scrolling up by `step` lines.
fn scroll_offset_up(scroll_offset: Option<u16>, metrics: ChatScrollMetrics, step: u16) -> u16 {
    let current_offset =
        scroll_offset.unwrap_or_else(|| metrics.total_lines.saturating_sub(metrics.view_height));

    current_offset.saturating_sub(step.max(1))
}

/// Returns the number of lines used for half-page scroll shortcuts.
fn half_page_step(metrics: ChatScrollMetrics) -> u16 {
    metrics.view_height / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a plain key press without modifiers.
    fn plain_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn test_is_scroll_key_accepts_scroll_keys_and_rejects_other_keys() {
        // Arrange
        let scroll_keys = [
            plain_key(KeyCode::Char('j')),
            plain_key(KeyCode::Up),
            plain_key(KeyCode::Char('G')),
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        ];
        let other_keys = [
            plain_key(KeyCode::Char('x')),
            plain_key(KeyCode::Char('d')),
            plain_key(KeyCode::Enter),
        ];

        // Act, Assert
        assert!(scroll_keys.into_iter().all(is_scroll_key));
        assert!(!other_keys.into_iter().any(is_scroll_key));
    }

    #[test]
    fn test_apply_scroll_key_steps_down_one_line() {
        // Arrange
        let metrics = ChatScrollMetrics {
            total_lines: 30,
            view_height: 10,
        };
        let mut scroll_offset = Some(0);

        // Act
        let is_consumed =
            apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('j')));

        // Assert
        assert!(is_consumed);
        assert_eq!(scroll_offset, Some(1));
    }

    #[test]
    fn test_apply_scroll_key_jumps_to_top_and_bottom() {
        // Arrange
        let metrics = ChatScrollMetrics {
            total_lines: 30,
            view_height: 10,
        };
        let mut scroll_offset = None;

        // Act
        apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('g')));
        let top_offset = scroll_offset;
        apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('G')));

        // Assert
        assert_eq!(top_offset, Some(0));
        assert_eq!(scroll_offset, None);
    }

    #[test]
    fn test_apply_scroll_key_scrolls_half_page_up_from_bottom() {
        // Arrange
        let metrics = ChatScrollMetrics {
            total_lines: 30,
            view_height: 10,
        };
        let mut scroll_offset = None;

        // Act
        let is_consumed = apply_scroll_key(
            &mut scroll_offset,
            metrics,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );

        // Assert
        assert!(is_consumed);
        assert_eq!(scroll_offset, Some(15));
    }

    #[test]
    fn test_apply_scroll_key_ignores_unrelated_keys() {
        // Arrange
        let metrics = ChatScrollMetrics {
            total_lines: 30,
            view_height: 10,
        };
        let mut scroll_offset = Some(3);

        // Act
        let is_consumed =
            apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('x')));

        // Assert
        assert!(!is_consumed);
        assert_eq!(scroll_offset, Some(3));
    }

    #[test]
    fn test_scroll_offset_down_returns_none_at_end_of_content() {
        // Arrange
        let metrics = ChatScrollMetrics {
            total_lines: 20,
            view_height: 10,
        };

        // Act
        let next_offset = scroll_offset_down(Some(9), metrics, 1);

        // Assert
        assert_eq!(next_offset, None);
    }

    #[test]
    fn test_scroll_offset_up_uses_bottom_when_scroll_is_unset() {
        // Arrange
        let metrics = ChatScrollMetrics {
            total_lines: 30,
            view_height: 10,
        };

        // Act
        let next_offset = scroll_offset_up(None, metrics, 5);

        // Assert
        assert_eq!(next_offset, 15);
    }
}
