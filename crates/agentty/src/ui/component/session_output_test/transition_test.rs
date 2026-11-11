use std::sync::Arc;

use ratatui::layout::Rect;
use ratatui::text::Line;

use super::super::{
    SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT, SessionOutput, SessionOutputLayoutCache,
    SessionOutputLayoutLines, TranscriptFingerprint,
};
use super::support::{line_context, session_fixture};
use crate::domain::session::SessionId;
use crate::domain::session_message::SessionTranscript;
use crate::ui::icon::{QUEUED_ACTION_WIDTH, TACHYON_LOADER_WIDTH};
use crate::ui::session_output_line::OutputLines;

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
fn test_transcript_fingerprint_treats_empty_transcript_like_missing_transcript() {
    // Arrange
    let mut missing_transcript_session = session_fixture();
    missing_transcript_session.transcript = None;
    let mut empty_transcript_session = session_fixture();
    empty_transcript_session.transcript = Some(SessionTranscript::new(Vec::new()));

    // Act
    let missing_fingerprint = TranscriptFingerprint::from_session(&missing_transcript_session);
    let empty_fingerprint = TranscriptFingerprint::from_session(&empty_transcript_session);

    // Assert
    assert_eq!(empty_fingerprint, missing_fingerprint);
    assert!(!empty_fingerprint.is_some);
}

#[test]
fn test_indicator_area_rejects_areas_without_room_for_the_indicator() {
    // Arrange
    let narrow_area = Rect::new(0, 0, TACHYON_LOADER_WIDTH - 1, 8);
    let borders_only_area = Rect::new(0, 0, 80, 1);

    // Act
    let narrow_loader_area = SessionOutput::indicator_area(narrow_area, 0, 0, TACHYON_LOADER_WIDTH);
    let borders_only_loader_area =
        SessionOutput::indicator_area(borders_only_area, 0, 0, TACHYON_LOADER_WIDTH);

    // Assert
    assert_eq!(narrow_loader_area, None);
    assert_eq!(borders_only_loader_area, None);
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
fn test_visible_lines_skip_rows_outside_viewport() {
    // Arrange
    let mut body = OutputLines::default();
    body.extend([
        Line::from("hidden before viewport"),
        Line::from("visible first row"),
        Line::from("visible second row"),
        Line::from("visible bottom row"),
        Line::from("hidden after viewport"),
    ]);
    let cached_lines = SessionOutputLayoutLines {
        body: Arc::new(body),
        body_line_count: 5,
        queued: Arc::from([]),
        tail: Vec::new(),
    };

    // Act
    let paint_lines = cached_lines.visible_lines(1, 3).collect::<Vec<_>>();
    let rendered_text = paint_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        rendered_text,
        [
            "visible first row",
            "visible second row",
            "visible bottom row"
        ]
    );
    assert!(std::ptr::eq(
        paint_lines[0],
        cached_lines
            .body
            .visible_lines(1, 1)
            .next()
            .expect("visible row")
    ));
}

#[test]
fn test_visible_rows_cross_shared_body_and_tail_without_copying_hidden_rows() {
    // Arrange
    let mut body = OutputLines::default();
    body.extend([Line::from("one"), Line::from("two"), Line::from("")]);
    let lines = SessionOutputLayoutLines {
        body: Arc::new(body),
        body_line_count: 2,
        queued: Arc::from([Line::from("queued first"), Line::from("queued second")]),
        tail: vec![Line::from("status"), Line::from("done")],
    };

    // Act
    let crossing = lines.visible_lines(1, 2).collect::<Vec<_>>();
    let tail_only = lines.visible_lines(2, 8).collect::<Vec<_>>();
    let past_end = lines.visible_lines(10, 2).collect::<Vec<_>>();
    let zero_height = lines.visible_lines(0, 0).collect::<Vec<_>>();
    let queue_crossing = lines.visible_lines(3, 2).collect::<Vec<_>>();
    let queue_only = lines.visible_lines(4, 1).collect::<Vec<_>>();

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
        ["status", "done", "queued first", "queued second"]
    );
    assert_eq!(
        queue_crossing
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["done", "queued first"]
    );
    assert_eq!(queue_only[0].to_string(), "queued first");
    assert_eq!(past_end, Vec::<&Line<'_>>::new());
    assert_eq!(zero_height, Vec::<&Line<'_>>::new());
    assert!(std::ptr::eq(crossing[1], &raw const lines.tail[0]));
    assert!(std::ptr::eq(queue_only[0], &raw const lines.queued[0]));
}
