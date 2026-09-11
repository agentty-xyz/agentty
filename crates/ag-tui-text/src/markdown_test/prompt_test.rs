use ratatui::style::Style;
use ratatui::text::Span;

use crate::markdown::{
    clarification_answer_label_style, clarification_header_style,
    clarification_prompt_prefix_style, clarification_question_index_style,
    clarification_question_label_style, render_markdown, user_prompt_content_style,
    user_prompt_lookup_style, user_prompt_prefix_style, wrap_verbatim_spans,
    wrap_verbatim_spans_with_word_boundaries,
};
use crate::style;

#[test]
fn test_render_markdown_styles_user_prompt() {
    // Arrange
    let input = " › /model antigravity";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].to_string().trim_end(), "");
    assert_eq!(lines[0].width(), 80);
    assert_eq!(lines[1].to_string().trim_end(), input);
    assert_eq!(lines[1].width(), 80);
    assert_eq!(lines[1].spans[0].style, user_prompt_prefix_style());
    assert_eq!(lines[1].spans[1].style, user_prompt_content_style());
    assert_eq!(lines[1].spans[1].style.fg, Some(style::palette::text()));
    assert_eq!(
        lines[1].spans.last().expect("padding span").style,
        user_prompt_content_style()
    );
    assert_eq!(lines[2].to_string().trim_end(), "");
    assert_eq!(lines[2].width(), 80);
    assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
}

#[test]
fn test_render_markdown_styles_multiline_user_prompt() {
    // Arrange
    let input = " › first line\nsecond line\n\nassistant line";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[0].to_string().trim_end(), "");
    assert_eq!(lines[1].to_string().trim_end(), " › first line");
    assert_eq!(lines[2].to_string().trim_end(), "   second line");
    assert_eq!(lines[1].width(), 80);
    assert_eq!(lines[2].width(), 80);
    assert_eq!(lines[4].to_string(), "");
    assert_eq!(lines[5].to_string(), "assistant line");
    assert_eq!(lines[1].spans[0].style, user_prompt_prefix_style());
    assert_eq!(lines[2].spans[0].content, "   ");
    assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
    assert_eq!(lines[2].spans[1].style, user_prompt_content_style());
    assert_eq!(lines[5].spans[0].style, Style::default());
}

#[test]
fn test_render_markdown_styles_clarification_block_differently_from_user_prompt() {
    // Arrange
    let input = " › Clarifications:\n   1. Q: Need tests?\n      A: Yes";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[1].to_string().trim_end(), " › Clarifications:");
    assert_eq!(lines[1].spans[0].style, clarification_prompt_prefix_style());
    assert_eq!(lines[1].spans[1].style, clarification_header_style());
    assert_ne!(lines[1].spans[1].style.bg, user_prompt_content_style().bg);
    assert!(lines[2].spans.iter().any(|span| {
        span.content.as_ref() == "1. " && span.style == clarification_question_index_style()
    }));
    assert!(lines[2].spans.iter().any(|span| {
        span.content.as_ref() == "Q: " && span.style == clarification_question_label_style()
    }));
    assert!(lines[3].spans.iter().any(|span| {
        span.content.as_ref() == "A: " && span.style == clarification_answer_label_style()
    }));
}

#[test]
fn test_render_markdown_keeps_prompt_continuation_line_verbatim() {
    // Arrange
    let input = " › first line\n**bold**\n\nassistant";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[2].to_string().trim_end(), "   **bold**");
    assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
    assert_eq!(lines[4].to_string(), "");
    assert_eq!(lines[5].to_string(), "assistant");
}

#[test]
fn test_render_markdown_wraps_user_prompt_content_with_continuation_padding() {
    // Arrange
    let input = " › one two three";

    // Act
    let lines = render_markdown(input, 8);

    // Assert
    assert!(lines.len() >= 4);
    assert_eq!(lines[0].to_string().trim_end(), "");
    assert!(lines[1].to_string().starts_with(" › "));
    assert!(lines[2].to_string().starts_with("   "));
    assert_eq!(lines[0].spans[0].style, user_prompt_content_style());
    assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
    assert_eq!(
        lines.last().expect("bottom padding").spans[0].style,
        user_prompt_content_style()
    );
}

#[test]
fn test_render_markdown_wraps_user_prompt_on_word_boundaries() {
    // Arrange
    let input = " › one two three";

    // Act
    let lines = render_markdown(input, 8);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(rendered_lines.contains(&" › one".to_string()));
    assert!(rendered_lines.contains(&"   two".to_string()));
    assert!(rendered_lines.contains(&"   three".to_string()));
}

#[test]
fn test_render_markdown_wraps_clarification_answer_on_word_boundaries() {
    // Arrange
    let input =
        " › Clarifications:\n   1. Q: Need tests?\n      A: very long answer text for review";

    // Act
    let lines = render_markdown(input, 18);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(rendered_lines.contains(&"      A: very".to_string()));
    assert!(
        rendered_lines
            .iter()
            .any(|line| line.trim_start().starts_with("long answer"))
    );
    assert!(
        rendered_lines
            .iter()
            .any(|line| line.trim_start().starts_with("text for"))
    );
    assert!(
        rendered_lines
            .iter()
            .any(|line| line.trim_start().starts_with("review"))
    );
}

#[test]
fn test_render_markdown_wraps_long_prompt_word_with_hard_fallback() {
    // Arrange
    let input = " › supercalifragilisticexpialidocious";

    // Act
    let lines = render_markdown(input, 8);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(rendered_lines.contains(&" › super".to_string()));
    assert!(rendered_lines.contains(&"   calif".to_string()));
    assert!(rendered_lines.contains(&"   ragil".to_string()));
}

#[test]
fn test_wrap_verbatim_spans_with_word_boundaries_handles_wide_characters() {
    // Arrange
    let spans = vec![Span::raw("你好 你好".to_string())];

    // Act
    let lines = wrap_verbatim_spans_with_word_boundaries(spans, 5);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "你好 ");
    assert_eq!(lines[0].width(), 5);
    assert_eq!(lines[1].to_string(), "你好");
    assert_eq!(lines[1].width(), 4);
}

#[test]
fn test_wrap_verbatim_spans_with_word_boundaries_wraps_when_word_reaches_edge() {
    // Arrange
    let spans = vec![Span::raw("foo bar".to_string())];

    // Act
    let lines = wrap_verbatim_spans_with_word_boundaries(spans, 7);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "foo ");
    assert_eq!(lines[1].to_string(), "bar");
}

#[test]
fn test_wrap_verbatim_spans_handles_wide_characters() {
    // Arrange
    let spans = vec![Span::raw("你好你好".to_string())];

    // Act
    let lines = wrap_verbatim_spans(spans, 5);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "你好");
    assert_eq!(lines[0].width(), 4);
    assert_eq!(lines[1].to_string(), "你好");
    assert_eq!(lines[1].width(), 4);
}

#[test]
fn test_render_markdown_highlights_file_lookups_in_user_prompt_block() {
    // Arrange
    let input = " › check @crates/agentty/src/ui/markdown.rs";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert!(lines[1].spans.iter().any(|span| {
        span.content.as_ref() == "@crates/agentty/src/ui/markdown.rs"
            && span.style == user_prompt_lookup_style()
    }));
}

#[test]
fn test_render_markdown_does_not_highlight_non_lookup_at_symbol_in_user_prompt_block() {
    // Arrange
    let input = " › reach me at email@example.com";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert!(
        !lines[1]
            .spans
            .iter()
            .any(|span| span.style == user_prompt_lookup_style())
    );
}

#[test]
fn test_render_markdown_keeps_text_after_multiple_blank_lines_in_user_prompt_block() {
    // Arrange
    let input = " › first line\n   \n   \n   after gap\n\nassistant";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert!(lines.iter().any(|line| {
        line.to_string().trim_end() == "   after gap"
            && line
                .spans
                .iter()
                .all(|span| span.style == user_prompt_content_style())
    }));
    assert_eq!(
        lines.last().expect("assistant line").to_string(),
        "assistant"
    );
}
