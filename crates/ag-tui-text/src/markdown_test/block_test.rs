use crate::markdown::{
    STATS_LABEL_WIDTH, code_block_style, heading_style, horizontal_rule_style,
    markdown_block_preservation_mask, render_markdown, stats_metric_style, stats_section_style,
    stats_value_style, table_header_style,
};

#[test]
fn test_render_markdown_styles_heading() {
    // Arrange
    let input = "# Heading";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "Heading");
    assert_eq!(lines[0].spans[0].style, heading_style(1));
}

#[test]
fn test_render_markdown_wraps_fenced_code_on_word_boundaries() {
    // Arrange
    let input = "```text\nformatted blocks in user messages without words breaking\n```";

    // Act
    let lines = render_markdown(input, 32);
    let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

    // Assert
    assert_eq!(rendered_lines[0], "formatted blocks in user ");
    assert_eq!(rendered_lines[1], "messages without words breaking");
    assert!(!rendered_lines.iter().any(|line| line.ends_with("message")));
    assert!(!rendered_lines.iter().any(|line| line.starts_with("s ")));
    assert!(lines.iter().all(|line| {
        line.spans
            .first()
            .is_some_and(|span| span.style == code_block_style())
    }));
}

#[test]
fn test_render_markdown_treats_unclosed_fence_as_code() {
    // Arrange
    let input = "```\n**raw**";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "**raw**");
    assert_eq!(lines[0].spans[0].style, code_block_style());
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_diagram_wider_than_width() {
    // Arrange
    let input = "```mermaid\ngraph TD\n    A[Start] --> B[Long finish label]\n```";

    // Act
    let lines = render_markdown(input, 10);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("graph TD"));
    assert!(!text.contains("┌"));
}

#[test]
fn test_render_markdown_renders_stats_metric_with_fixed_alignment() {
    // Arrange
    let input = "```stats\nSession ID\tsession-id\n```";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].to_string().find("session-id"),
        Some(STATS_LABEL_WIDTH)
    );
    assert!(lines[0].spans.iter().any(|span| {
        span.content.as_ref().contains("Session ID") && span.style == stats_metric_style()
    }));
    assert!(lines[0].spans.iter().any(|span| {
        span.content.as_ref().contains("session-id") && span.style == stats_value_style()
    }));
}

#[test]
fn test_render_markdown_renders_stats_section_title_style() {
    // Arrange
    let input = "```stats\nTokens Usage\n```";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "Tokens Usage");
    assert_eq!(lines[0].spans[0].style, stats_section_style());
}

#[test]
fn test_render_markdown_wraps_bullets_with_continuation_indent() {
    // Arrange
    let input = "- one two three four";

    // Act
    let lines = render_markdown(input, 8);

    // Assert
    assert!(lines.len() >= 2);
    assert!(lines[0].to_string().starts_with("- "));
    assert!(lines[1].to_string().starts_with("  "));
}

#[test]
fn test_render_markdown_wraps_numbered_list_with_continuation_indent() {
    // Arrange
    let input = "12. one two three";

    // Act
    let lines = render_markdown(input, 9);

    // Assert
    assert!(lines.len() >= 2);
    assert!(lines[0].to_string().starts_with("12. "));
    assert!(lines[1].to_string().starts_with("    "));
}

#[test]
fn test_render_markdown_wraps_blockquote_with_prefix() {
    // Arrange
    let input = "> one two three";

    // Act
    let lines = render_markdown(input, 7);

    // Assert
    assert!(lines.len() >= 2);
    assert!(lines[0].to_string().starts_with("│ "));
    assert!(lines[1].to_string().starts_with("│ "));
}

#[test]
fn test_render_markdown_renders_horizontal_rule() {
    // Arrange
    let input = "---";

    // Act
    let lines = render_markdown(input, 5);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "-----");
    assert_eq!(lines[0].spans[0].style, horizontal_rule_style());
}

#[test]
fn test_render_markdown_renders_pipe_table() {
    // Arrange
    let input = "| Name | Status |\n| --- | ---: |\n| Build | passing |\n| Docs | queued |";

    // Act
    let lines = render_markdown(input, 80);
    let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    let rendered_text = rendered_lines.join("\n");

    // Assert
    assert!(rendered_text.contains("Name"));
    assert!(rendered_text.contains("Status"));
    assert!(rendered_text.contains("Build"));
    assert!(rendered_text.contains("passing"));
    assert!(!rendered_text.contains("| --- | ---: |"));
    assert!(rendered_lines.iter().any(|line| line.starts_with("┌")));
    assert!(rendered_lines.iter().any(|line| line.starts_with("├")));
    assert!(lines[1].spans.iter().any(|span| {
        span.content.as_ref().contains("Name") && span.style == table_header_style()
    }));
}

#[test]
fn test_markdown_block_preservation_mask_uses_shared_block_classifiers() {
    // Arrange
    let input = concat!(
        "  plain\n",
        "  | Name | Status |\n",
        "  | --- | ---: |\n",
        "  | Build | passing |\n",
        "\n",
        "  ---\n",
        "  ```text\n",
        "    fenced\n",
        "  ```",
    );

    // Act
    let preservation_mask = markdown_block_preservation_mask(input);

    // Assert
    assert_eq!(
        preservation_mask,
        vec![false, true, true, true, false, true, true, true, true]
    );
}

#[test]
fn test_render_markdown_wraps_table_cells_to_available_width() {
    // Arrange
    let input = "| Column | Notes |\n| --- | --- |\n| One | alpha beta gamma delta |";

    // Act
    let lines = render_markdown(input, 20);
    let rendered_text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(rendered_text.contains("alpha"));
    assert!(rendered_text.contains("beta"));
    assert!(rendered_text.contains("gamma"));
    assert!(lines.iter().all(|line| line.width() <= 20));
}
