use std::sync::Arc;

use ratatui::layout::Rect;
use ratatui::text::Line;

use super::super::{
    SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT, SessionOutput, SessionOutputLayoutCache,
    SessionOutputLayoutLines,
};
use super::support::{line_context, session_fixture};
use crate::domain::session::SessionId;
use crate::ui::icon::{QUEUED_ACTION_WIDTH, TACHYON_LOADER_WIDTH};

#[test]
fn test_resolved_cache_evicts_old_sessions() {
    // Arrange
    let mut session = session_fixture();
    let cache = SessionOutputLayoutCache::default();

    // Act
    for index in 0..=SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT {
        session.id = SessionId::from(format!("resolved-{index}"));
        cache.resolved_layout(&session, Rect::new(0, 0, 80, 24), 22, line_context(), None);
    }

    // Assert
    let entries = cache.resolved_entries.borrow();
    assert_eq!(entries.len(), SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT);
    assert!(
        entries
            .iter()
            .all(|entry| entry.key.session_id.as_str() != "resolved-0")
    );
}

#[test]
fn test_indicator_area_locates_row_before_following_hint() {
    // Arrange
    let output_area = Rect::new(0, 0, 80, 8);

    // Act
    let loader_area = SessionOutput::indicator_area(output_area, 1, 0, TACHYON_LOADER_WIDTH);

    // Assert
    assert_eq!(loader_area, Some(Rect::new(0, 2, TACHYON_LOADER_WIDTH, 1)));
}

#[test]
fn test_indicator_area_skips_line_outside_viewport() {
    // Arrange
    let output_area = Rect::new(0, 0, 80, 10);

    // Act
    let loader_area = SessionOutput::indicator_area(output_area, 20, 0, QUEUED_ACTION_WIDTH);

    // Assert
    assert_eq!(loader_area, None);
}

#[test]
fn test_visible_paint_lines_skip_rows_outside_viewport() {
    // Arrange
    let cached_lines = SessionOutputLayoutLines {
        body: Arc::from([
            Line::from("hidden before viewport"),
            Line::from("visible first row"),
            Line::from("visible second row"),
            Line::from("hidden after viewport"),
        ]),
        body_line_count: 4,
        tail: Vec::new(),
    };

    // Act
    let paint_lines = SessionOutput::visible_paint_lines(Rect::new(0, 0, 30, 4), &cached_lines, 1);
    let rendered_text = paint_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(rendered_text, ["visible first row", "visible second row"]);
}

#[test]
fn test_visible_rows_cross_shared_body_and_tail_without_copying_hidden_rows() {
    // Arrange
    let lines = SessionOutputLayoutLines {
        body: Arc::from([Line::from("one"), Line::from("two"), Line::from("")]),
        body_line_count: 2,
        tail: vec![Line::from("status"), Line::from("done")],
    };

    // Act
    let crossing = lines.paint_lines(1, 2);
    let tail_only = lines.paint_lines(2, 8);
    let past_end = lines.paint_lines(10, 2);
    let zero_height = lines.paint_lines(0, 0);

    // Assert
    assert_eq!(
        crossing.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["two", "status"]
    );
    assert_eq!(
        tail_only
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["status", "done"]
    );
    assert_eq!(past_end, Vec::<Line<'_>>::new());
    assert_eq!(zero_height, Vec::<Line<'_>>::new());
}
