use ratatui::style::Modifier;

use crate::markdown::{code_block_style, inline_code_style, render_markdown};

#[test]
fn test_render_markdown_parses_inline_styles() {
    // Arrange
    let input = "before **bold** *italic* `code`";

    // Act
    let lines = render_markdown(input, 80);
    let line = &lines[0];

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(line.to_string(), "before bold italic code");
    assert!(line.spans.iter().any(|span| {
        span.content.as_ref() == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
    }));
    assert!(line.spans.iter().any(|span| {
        span.content.as_ref() == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
    }));
    assert!(
        line.spans
            .iter()
            .any(|span| span.content.as_ref() == "code" && span.style == inline_code_style())
    );
}

#[test]
fn test_render_markdown_renders_inline_right_arrow_math_symbol() {
    // Arrange
    let input = r"Move $\rightarrow$ forward.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "Move → forward.");
}

#[test]
fn test_render_markdown_renders_inline_right_arrow_math_inside_bold() {
    // Arrange
    let input = r"Move **$\rightarrow$** forward.";

    // Act
    let lines = render_markdown(input, 80);
    let arrow_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "→")
        .expect("right arrow should render");

    // Assert
    assert!(arrow_span.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(lines[0].to_string(), "Move → forward.");
}

#[test]
fn test_render_markdown_renders_inline_right_arrow_math_inside_italic() {
    // Arrange
    let input = r"Move *$\rightarrow$* forward.";

    // Act
    let lines = render_markdown(input, 80);
    let arrow_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "→")
        .expect("right arrow should render");

    // Assert
    assert!(arrow_span.style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(lines[0].to_string(), "Move → forward.");
}

#[test]
fn test_render_markdown_preserves_unsupported_inline_math() {
    // Arrange
    let input = r"Keep $x + y$, unmatched $\rightarrow literal, and $$text $\rightarrow$ literal.";

    // Act
    let lines = render_markdown(input, 120);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_display_math() {
    // Arrange
    let input = r"Keep $$text $\rightarrow$ text$$ literal.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_bold_syntax_inside_display_math() {
    // Arrange
    let input = r"Keep $$text **$\rightarrow$** text$$ literal.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_italic_syntax_inside_display_math() {
    // Arrange
    let input = r"Keep $$text *$\rightarrow$* text$$ literal.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_display_math_inside_bold() {
    // Arrange
    let input = r"Keep **$$text $\rightarrow$ text$$** literal.";

    // Act
    let lines = render_markdown(input, 80);
    let math_text = lines[0]
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
        .map(|span| span.content.as_ref())
        .collect::<String>();

    // Assert
    assert_eq!(math_text, r"$$text $\rightarrow$ text$$");
    assert_eq!(
        lines[0].to_string(),
        r"Keep $$text $\rightarrow$ text$$ literal."
    );
}

#[test]
fn test_render_markdown_preserves_display_math_inside_italic() {
    // Arrange
    let input = r"Keep *$$text $\rightarrow$ text$$* literal.";

    // Act
    let lines = render_markdown(input, 80);
    let math_text = lines[0]
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::ITALIC))
        .map(|span| span.content.as_ref())
        .collect::<String>();

    // Assert
    assert_eq!(math_text, r"$$text $\rightarrow$ text$$");
    assert_eq!(
        lines[0].to_string(),
        r"Keep $$text $\rightarrow$ text$$ literal."
    );
}

#[test]
fn test_render_markdown_preserves_inline_code_inside_bold() {
    // Arrange
    let input = r"Keep **`$\rightarrow$`** literal.";

    // Act
    let lines = render_markdown(input, 80);
    let code_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == r"`$\rightarrow$`")
        .expect("inline code should remain literal");

    // Assert
    assert!(code_span.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(lines[0].to_string(), r"Keep `$\rightarrow$` literal.");
}

#[test]
fn test_render_markdown_preserves_inline_code_inside_italic() {
    // Arrange
    let input = r"Keep *`$\rightarrow$`* literal.";

    // Act
    let lines = render_markdown(input, 80);
    let code_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == r"`$\rightarrow$`")
        .expect("inline code should remain literal");

    // Assert
    assert!(code_span.style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(lines[0].to_string(), r"Keep `$\rightarrow$` literal.");
}

#[test]
fn test_render_markdown_keeps_inline_style_punctuation_adjacent() {
    // Arrange
    let input = "Use (`session_messages_from_rows`), then [`Image #1`].";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].to_string(),
        "Use (session_messages_from_rows), then [Image #1]."
    );
}

#[test]
fn test_render_markdown_leaves_unmatched_inline_delimiters_literal() {
    // Arrange
    let input = "text **bold";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
    assert!(
        !lines[0]
            .spans
            .iter()
            .any(|span| span.style.add_modifier.contains(Modifier::BOLD))
    );
}

#[test]
fn test_render_markdown_renders_fenced_code_without_inline_parsing() {
    // Arrange
    let input = "```rust\nlet value = **raw**;\n```";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "let value = **raw**;");
    assert_eq!(lines[0].spans[0].style, code_block_style());
}
