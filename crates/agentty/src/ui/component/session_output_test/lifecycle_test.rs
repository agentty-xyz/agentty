use std::sync::Arc;

use ratatui::layout::Rect;

use super::super::SessionOutputLayoutCache;
use super::support::{line_context, session_fixture};
use crate::ui::markdown;

#[test]
fn test_output_layout_cache_keys_staged_draft_prompt() {
    // Arrange
    let mut session = session_fixture();
    session.is_draft = true;
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let context = line_context();

    // Act
    let empty_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );
    session.prompt = "First staged draft".to_string();
    let staged_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );
    let staged_text = staged_layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(!Arc::ptr_eq(&empty_layout.lines, &staged_layout.lines));
    assert!(staged_text.contains("First staged draft"));
}
