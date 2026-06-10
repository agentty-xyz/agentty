use ag_forge::RequestedReview;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::ui::state::help_action;
use crate::ui::{Page, layout, markdown, style};

/// Page renderer for one requested PR or MR review summary.
pub struct ReviewDetailPage<'a> {
    /// Shared cache used for styled markdown body rendering.
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Requested review opened from the review list.
    review: &'a RequestedReview,
    /// Vertical offset applied to the rendered title and description rows.
    scroll_offset: u16,
}

impl<'a> ReviewDetailPage<'a> {
    /// Creates a detail renderer for one requested review snapshot.
    pub fn new(
        review: &'a RequestedReview,
        markdown_render_cache: &'a markdown::MarkdownRenderCache,
        scroll_offset: u16,
    ) -> Self {
        Self {
            markdown_render_cache,
            review,
            scroll_offset,
        }
    }
}

impl Page for ReviewDetailPage<'_> {
    /// Renders only the selected review's title and description, leaving
    /// provider actions for later iterations.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);
        let content_width = detail_content_width(area);
        let paragraph = Paragraph::new(detail_lines(
            self.review,
            self.markdown_render_cache,
            content_width,
        ))
        .block(review_detail_block())
        .style(Style::default().fg(style::palette::text()))
        .scroll((self.scroll_offset, 0));

        f.render_widget(paragraph, areas.main_area);

        let help = Paragraph::new(review_detail_footer_line());
        f.render_widget(help, areas.footer_area);
    }
}

/// Returns the largest valid vertical scroll offset for a review detail page.
pub(crate) fn review_detail_max_scroll_offset(
    review: &RequestedReview,
    area: Rect,
    markdown_render_cache: &markdown::MarkdownRenderCache,
) -> u16 {
    let viewport_height = detail_view_height(area);
    if viewport_height == 0 {
        return 0;
    }

    let rendered_line_count =
        detail_lines(review, markdown_render_cache, detail_content_width(area)).len();

    u16::try_from(rendered_line_count.saturating_sub(usize::from(viewport_height)))
        .unwrap_or(u16::MAX)
}

/// Builds the visible title and rendered markdown description lines for a
/// review detail page.
fn detail_lines(
    review: &RequestedReview,
    markdown_render_cache: &markdown::MarkdownRenderCache,
    width: usize,
) -> Vec<Line<'static>> {
    let description = review
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .unwrap_or("No description provided.");
    let description = review_description_markdown(description);
    let mut lines = vec![
        section_label("Title"),
        Line::from(review.title.clone()),
        Line::from(""),
        section_label("Description"),
    ];

    lines.extend(
        markdown_render_cache
            .render(&description, width)
            .iter()
            .cloned(),
    );

    lines
}

/// Converts common HTML snippets embedded in forge markdown bodies to
/// markdown-like text before terminal markdown rendering.
fn review_description_markdown(description: &str) -> String {
    let mut rendered = String::new();
    let mut index = 0;

    while index < description.len() {
        if let Some(tag) = parse_html_tag(description, index) {
            append_html_tag_replacement(&mut rendered, &tag);
            index = tag.end_index;

            continue;
        }

        if let Some((decoded, consumed)) = decode_html_entity(&description[index..]) {
            rendered.push(decoded);
            index += consumed;

            continue;
        }

        let Some(character) = description[index..].chars().next() else {
            break;
        };
        rendered.push(character);
        index += character.len_utf8();
    }

    compact_blank_lines(&rendered)
}

/// Parsed representation of one HTML tag embedded in a forge description.
struct HtmlTag<'a> {
    /// Byte index immediately after the closing `>` in the source string.
    end_index: usize,
    /// Whether the tag starts with `/`.
    is_closing: bool,
    /// Lowercase-insensitive tag name without attributes.
    name: &'a str,
}

/// Parses one ASCII HTML tag at `index`, returning `None` for literal `<`
/// characters or malformed tags.
fn parse_html_tag(description: &str, index: usize) -> Option<HtmlTag<'_>> {
    let suffix = description.get(index..)?;
    if !suffix.starts_with('<') {
        return None;
    }

    let close_offset = suffix.find('>')?;
    let raw_tag = suffix[1..close_offset].trim();
    let (is_closing, tag_content) = raw_tag
        .strip_prefix('/')
        .map_or((false, raw_tag), |content| (true, content.trim_start()));
    let name_end = tag_content
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_alphanumeric())
        .map(|(offset, character)| offset + character.len_utf8())
        .last()?;
    let name = &tag_content[..name_end];
    if !name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
    {
        return None;
    }

    Some(HtmlTag {
        end_index: index + close_offset + 1,
        is_closing,
        name,
    })
}

/// Appends markdown punctuation or spacing for one recognized HTML tag.
fn append_html_tag_replacement(output: &mut String, tag: &HtmlTag<'_>) {
    match (tag.name.to_ascii_lowercase().as_str(), tag.is_closing) {
        ("h1", false) => append_line_prefix(output, "# "),
        ("h2", false) => append_line_prefix(output, "## "),
        ("h3" | "summary", false) => append_line_prefix(output, "### "),
        ("h4", false) => append_line_prefix(output, "#### "),
        ("li", false) => append_line_prefix(output, "- "),
        ("blockquote", false) => append_line_prefix(output, "> "),
        ("code", _) => output.push('`'),
        ("strong" | "b", _) => output.push_str("**"),
        ("em" | "i", _) => output.push('*'),
        (
            "br" | "p" | "details" | "summary" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "li",
            true,
        )
        | ("br" | "p" | "ul" | "ol" | "details", false) => append_line_break(output),
        _ => {}
    }
}

/// Appends `prefix` at the beginning of a logical markdown line.
fn append_line_prefix(output: &mut String, prefix: &str) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }

    output.push_str(prefix);
}

/// Appends a single line break while avoiding duplicate blank lines from
/// adjacent HTML block tags.
fn append_line_break(output: &mut String) {
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

/// Decodes a small set of named and numeric HTML entities common in forge
/// review descriptions.
fn decode_html_entity(input: &str) -> Option<(char, usize)> {
    if !input.starts_with('&') {
        return None;
    }

    let semicolon_index = input.find(';')?;
    let entity = &input[1..semicolon_index];
    let decoded = match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" => ' ',
        _ => decode_numeric_html_entity(entity)?,
    };

    Some((decoded, semicolon_index + 1))
}

/// Decodes decimal and hexadecimal numeric HTML entities.
fn decode_numeric_html_entity(entity: &str) -> Option<char> {
    let codepoint = if let Some(hexadecimal) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hexadecimal, 16).ok()?
    } else {
        let decimal = entity.strip_prefix('#')?;
        decimal.parse::<u32>().ok()?
    };

    char::from_u32(codepoint)
}

/// Collapses runs of blank lines produced by neighboring HTML block tags while
/// preserving meaningful leading indentation inside content lines.
fn compact_blank_lines(markdown: &str) -> String {
    let mut compacted = Vec::new();
    let mut previous_blank = false;

    for line in markdown.lines().map(str::trim_end) {
        let is_blank = line.trim().is_empty();
        if is_blank && previous_blank {
            continue;
        }

        compacted.push(line);
        previous_blank = is_blank;
    }

    compacted.join("\n").trim().to_string()
}

/// Returns the width used to wrap rendered review description markdown.
fn detail_content_width(area: Rect) -> usize {
    usize::from(detail_content_area(area).width)
}

/// Returns the visible row count inside the review detail content block.
fn detail_view_height(area: Rect) -> u16 {
    detail_content_area(area).height
}

/// Returns the inner content area of the review-detail frame.
fn detail_content_area(area: Rect) -> Rect {
    let areas = layout::tab_page_areas(area);

    review_detail_block().inner(areas.main_area)
}

/// Builds one emphasized field label for the detail page.
fn section_label(label: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        label,
        Style::default()
            .fg(style::palette::text_muted())
            .add_modifier(Modifier::BOLD),
    ))
}

/// Builds the review-detail footer with the currently supported action.
fn review_detail_footer_line() -> Line<'static> {
    help_action::footer_line(&[
        help_action::HelpAction::new("back", "q", "Back"),
        help_action::HelpAction::new("scroll", "j/k", "Scroll"),
        help_action::HelpAction::new("page", "Ctrl+d/u", "Page"),
        help_action::HelpAction::new("top/bottom", "g/G", "Top/bottom"),
    ])
}

/// Builds the review-detail content frame.
fn review_detail_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title("Review Request")
        .border_style(style::border_style())
}

#[cfg(test)]
mod tests {
    use ag_forge::{ForgeKind, RequestedReviewAudience};
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::domain::theme::ColorTheme;

    #[test]
    fn test_render_detail_shows_title_and_description() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let review = requested_review(Some("Implements the detail page."));

        // Act
        terminal
            .draw(|frame| {
                ReviewDetailPage::new(&review, &markdown::MarkdownRenderCache::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Review Request"));
        assert!(text.contains("Title"));
        assert!(text.contains("Add review detail page"));
        assert!(text.contains("Description"));
        assert!(text.contains("Implements the detail page."));
        assert!(text.contains("q: back"));
    }

    #[test]
    fn test_render_detail_shows_missing_description_fallback() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let review = requested_review(None);

        // Act
        terminal
            .draw(|frame| {
                ReviewDetailPage::new(&review, &markdown::MarkdownRenderCache::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("No description provided."));
    }

    #[test]
    fn test_render_detail_renders_markdown_description() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let review = requested_review(Some("## Details\n- **Parser** uses `fast` mode."));

        // Act
        terminal
            .draw(|frame| {
                ReviewDetailPage::new(&review, &markdown::MarkdownRenderCache::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Details"));
        assert!(text.contains("- Parser uses fast mode."));
        assert!(!text.contains("## Details"));
        assert!(!text.contains("**Parser**"));
        assert!(!text.contains("`fast`"));
    }

    #[test]
    fn test_render_detail_normalizes_common_html_description() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(100, 14);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let review = requested_review(Some(
            "<details>\n<summary>Release notes</summary>\n<h2>v1.0.0</h2>\n<ul>\n<li>Fix <code>parser</code> by <a href=\"https://example.com\">alice</a></li>\n</ul>\n</details>",
        ));

        // Act
        terminal
            .draw(|frame| {
                ReviewDetailPage::new(&review, &markdown::MarkdownRenderCache::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Release notes"));
        assert!(text.contains("v1.0.0"));
        assert!(text.contains("- Fix parser by alice"));
        assert!(!text.contains("<summary>"));
        assert!(!text.contains("<li>"));
        assert!(!text.contains("<code>"));
    }

    #[test]
    fn test_render_detail_applies_scroll_offset() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let review = requested_review(Some(
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7",
        ));

        // Act
        terminal
            .draw(|frame| {
                ReviewDetailPage::new(&review, &markdown::MarkdownRenderCache::default(), 5)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains("Add review detail page"));
        assert!(text.contains("line 2"));
        assert!(text.contains("line 3"));
    }

    #[test]
    fn test_review_detail_max_scroll_offset_accounts_for_rendered_markdown() {
        // Arrange
        let review = requested_review(Some("line 1\nline 2\nline 3\nline 4\nline 5\nline 6"));
        let markdown_render_cache = markdown::MarkdownRenderCache::default();

        // Act
        let max_scroll_offset = review_detail_max_scroll_offset(
            &review,
            Rect::new(0, 0, 80, 8),
            &markdown_render_cache,
        );

        // Assert
        assert_eq!(max_scroll_offset, 6);
    }

    #[test]
    fn test_review_description_markdown_preserves_literal_angle_brackets() {
        // Arrange
        let description = "Keep 2 < 3 and 5 > 4 visible.";

        // Act
        let rendered = review_description_markdown(description);

        // Assert
        assert_eq!(rendered, "Keep 2 < 3 and 5 > 4 visible.");
    }

    /// Builds one requested-review fixture for detail render tests.
    fn requested_review(body: Option<&str>) -> RequestedReview {
        RequestedReview {
            audience: RequestedReviewAudience::Personal,
            body: body.map(str::to_string),
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            repository: "agentty-xyz/agentty".to_string(),
            status_summary: None,
            title: "Add review detail page".to_string(),
            updated_at: Some("2026-04-27T21:30:00Z".to_string()),
            web_url: "https://example.com/42".to_string(),
        }
    }

    /// Extracts all rendered cell symbols from a test backend buffer.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<Vec<_>>()
            .join("")
    }
}
