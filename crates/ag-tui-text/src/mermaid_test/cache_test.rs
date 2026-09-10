use std::sync::Arc;

use crate::mermaid::{
    MAX_SOURCE_BYTE_COUNT, PARSED_MERMAID_CACHE, PARSED_MERMAID_CACHE_ENTRY_LIMIT, parsed_mermaid,
    render_mermaid_for_width, render_mermaid_with_settings,
};
use crate::style::TextRenderSettings;

#[test]
fn test_parsed_cache_reuses_source_across_widths_and_palettes() {
    // Arrange
    let source = "graph LR\nParse[Parse once] --> Paint[Paint twice]";
    let first = parsed_mermaid(source).expect("parsed graph");
    let mut settings = TextRenderSettings::default();
    settings.palette.text = ratatui::style::Color::Red;

    // Act
    let wide = render_mermaid_for_width(source, 80).expect("wide diagram");
    let narrow = render_mermaid_for_width(source, 20).expect("stacked diagram");
    let themed = render_mermaid_with_settings(source, settings).expect("themed diagram");
    let repeated = parsed_mermaid(source).expect("cached graph");

    // Assert
    assert!(Arc::ptr_eq(&first, &repeated));
    assert!(wide.width > narrow.width);
    assert!(narrow.lines.len() > wide.lines.len());
    assert!(
        themed
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.fg == Some(ratatui::style::Color::Red))
    );
}

#[test]
fn test_parsed_cache_is_bounded_and_promotes_hits() {
    // Arrange
    PARSED_MERMAID_CACHE.with(|cache| cache.borrow_mut().clear());
    let source = "graph TD\nKeep --> Cached";
    let first = parsed_mermaid(source).expect("first diagram");
    for index in 0..PARSED_MERMAID_CACHE_ENTRY_LIMIT - 1 {
        parsed_mermaid(&format!("graph TD\nNode{index} --> End"));
    }

    // Act
    let promoted = parsed_mermaid(source).expect("promoted diagram");
    parsed_mermaid("graph TD\nExtra --> End");
    let retained = parsed_mermaid(source).expect("retained diagram");

    // Assert
    assert!(Arc::ptr_eq(&first, &promoted));
    assert!(Arc::ptr_eq(&first, &retained));
    PARSED_MERMAID_CACHE.with(|cache| {
        let entries = cache.borrow();
        assert_eq!(entries.len(), PARSED_MERMAID_CACHE_ENTRY_LIMIT);
        assert!(entries.iter().all(|entry| !entry.source.contains("Node0 ")));
    });
}

#[test]
fn test_parsed_cache_retains_unsupported_source_but_rejects_oversized_input() {
    // Arrange
    PARSED_MERMAID_CACHE.with(|cache| cache.borrow_mut().clear());
    let unsupported = "classDiagram\nA <|-- B";

    // Act
    let first = parsed_mermaid(unsupported);
    let repeated = parsed_mermaid(unsupported);
    let oversized = parsed_mermaid(&"x".repeat(MAX_SOURCE_BYTE_COUNT + 1));

    // Assert
    assert!(first.is_none());
    assert!(repeated.is_none());
    assert!(oversized.is_none());
    PARSED_MERMAID_CACHE.with(|cache| assert_eq!(cache.borrow().len(), 1));
}
