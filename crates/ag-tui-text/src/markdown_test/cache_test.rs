use std::cell::Cell;
use std::sync::Arc;

use ratatui::style::Color;
use ratatui::text::Line;

use crate::markdown::{MARKDOWN_RENDER_CACHE_ENTRY_LIMIT, MarkdownRenderCache, render_markdown};
use crate::style::TextRenderSettings;

#[test]
fn test_markdown_render_cache_retains_multiple_entries() {
    // Arrange
    let cache = MarkdownRenderCache::default();

    // Act
    let first_lines = cache.render("# First", 24);
    let second_lines = cache.render("# Second", 24);
    let cached_first_lines = cache.render("# First", 24);

    // Assert
    assert!(Arc::ptr_eq(&first_lines, &cached_first_lines));
    assert_eq!(
        second_lines.as_ref(),
        render_markdown("# Second", 24).as_slice()
    );
    assert_eq!(cache.entries.borrow().len(), 2);
}

#[test]
fn test_markdown_render_cache_reuses_custom_renderer_result() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let settings = TextRenderSettings::default();
    let render_count = Cell::new(0);

    // Act
    let first_lines =
        cache.render_with_settings_and_renderer("custom", 24, settings, |_text, _width| {
            render_count.set(render_count.get() + 1);

            vec![Line::from("custom render")]
        });
    let cached_lines =
        cache.render_with_settings_and_renderer("custom", 24, settings, |_text, _width| {
            render_count.set(render_count.get() + 1);

            vec![Line::from("unexpected render")]
        });

    // Assert
    assert!(Arc::ptr_eq(&first_lines, &cached_lines));
    assert_eq!(first_lines[0].to_string(), "custom render");
    assert_eq!(render_count.get(), 1);
}

#[test]
fn test_markdown_render_cache_evicts_least_recently_used_entry() {
    // Arrange
    let cache = MarkdownRenderCache::default();

    // Act
    for index in 0..MARKDOWN_RENDER_CACHE_ENTRY_LIMIT {
        let markdown = format!("# Entry {index}");
        cache.render(&markdown, 24);
    }
    cache.render("# Entry 0", 24);
    cache.render("# Overflow", 24);

    // Assert
    let cached_hashes = cache
        .entries
        .borrow()
        .iter()
        .map(|entry| entry.key.content_hash)
        .collect::<Vec<_>>();
    assert_eq!(cached_hashes.len(), MARKDOWN_RENDER_CACHE_ENTRY_LIMIT);
    assert!(cached_hashes.contains(&MarkdownRenderCache::hash_text("# Entry 0")));
    assert!(!cached_hashes.contains(&MarkdownRenderCache::hash_text("# Entry 1")));
    assert!(cached_hashes.contains(&MarkdownRenderCache::hash_text("# Overflow")));
}

#[test]
fn test_markdown_render_cache_uses_injected_palette() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let settings = TextRenderSettings {
        cache_version: 7,
        palette: crate::TextPalette {
            accent: Color::Red,
            ..crate::TextPalette::default()
        },
    };

    // Act
    let lines = cache.render_with_settings("# Entry", 24, settings);

    // Assert
    assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
}

#[test]
fn test_markdown_render_cache_bump_version_clears_styled_entries() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let initial_lines = cache.render("# Entry", 24);

    // Act
    cache.bump_version();
    let refreshed_lines = cache.render("# Entry", 24);

    // Assert
    assert!(!Arc::ptr_eq(&initial_lines, &refreshed_lines));
    assert_eq!(cache.entries.borrow().len(), 1);
    assert_eq!(cache.version.get(), 1);
}
