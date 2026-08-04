use std::sync::Arc;

pub use ag_tui_text::markdown::{markdown_block_preservation_mask, parse_inline_spans};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::ui::prompt_block::{self, USER_PROMPT_PREFIX, USER_PROMPT_RIGHT_GUTTER_WIDTH};
use crate::ui::style;

const CLARIFICATION_HEADER: &str = "Clarifications:";

#[derive(Clone, Copy)]
enum PromptBlockKind {
    Clarification,
    UserPrompt,
}

struct PromptBlock {
    content_lines: Vec<String>,
    raw_lines: Vec<String>,
    consumed_closing_line: bool,
    next_line_index: usize,
}

/// Theme-aware cache for rendered Markdown and forge HTML lines.
///
/// Markdown and HTML use separate bounded entry sets because identical source
/// text can require different renderers. Both sets key entries by source text,
/// width, and theme version, and [`Self::bump_version()`] invalidates them
/// together.
#[derive(Default)]
pub struct MarkdownRenderCache {
    html_inner: ag_tui_text::markdown::MarkdownRenderCache,
    inner: ag_tui_text::markdown::MarkdownRenderCache,
}

impl MarkdownRenderCache {
    /// Returns cached rendered lines when the text hash, width, and active
    /// theme version match; otherwise renders fresh and updates the cache.
    pub fn render(&self, text: &str, width: usize) -> Arc<[Line<'static>]> {
        let settings = style::text_render_settings();
        if has_prompt_blocks(text) {
            return self.inner.render_with_settings_and_renderer(
                text,
                width,
                settings,
                render_markdown_with_prompt_blocks,
            );
        }

        self.inner.render_with_settings(text, width, settings)
    }

    /// Returns cached rendered lines after normalizing HTML embedded in a
    /// forge-authored Markdown body.
    pub fn render_html(&self, text: &str, width: usize) -> Arc<[Line<'static>]> {
        let settings = style::text_render_settings();

        self.html_inner.render_with_settings_and_renderer(
            text,
            width,
            settings,
            move |text, width| ag_tui_text::html::render_html_with_settings(text, width, settings),
        )
    }

    /// Bumps the cache version and drops all rendered entries.
    pub fn bump_version(&self) {
        self.html_inner.bump_version();
        self.inner.bump_version();
    }

    /// Returns the current style-cache version used by higher-level layout
    /// caches to avoid reusing styled lines after markdown entries are
    /// invalidated.
    pub(crate) fn version(&self) -> u64 {
        self.inner.version()
    }
}

/// Converts markdown text into styled, word-wrapped lines using Agentty's
/// active UI theme.
pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    if !has_prompt_blocks(text) {
        return render_shared_markdown(text, width);
    }

    render_markdown_with_prompt_blocks(text, width)
}

fn has_prompt_blocks(text: &str) -> bool {
    text.split('\n')
        .any(|line| line.starts_with(USER_PROMPT_PREFIX))
}

fn render_shared_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    ag_tui_text::markdown::render_markdown_with_settings(text, width, style::text_render_settings())
}

fn render_markdown_with_prompt_blocks(text: &str, width: usize) -> Vec<Line<'static>> {
    let raw_lines = text.split('\n').collect::<Vec<_>>();
    let mut rendered_lines = Vec::new();
    let mut normal_lines = Vec::new();
    let mut line_index = 0;

    while line_index < raw_lines.len() {
        let raw_line = raw_lines[line_index];
        if !raw_line.starts_with(USER_PROMPT_PREFIX) {
            normal_lines.push(raw_line);
            line_index += 1;

            continue;
        }

        append_shared_markdown_lines(&mut rendered_lines, &mut normal_lines, width);
        let prompt_block = collect_prompt_block_lines(&raw_lines, line_index);
        append_prompt_block_lines(&mut rendered_lines, &prompt_block, width);
        line_index = prompt_block.next_line_index;
    }

    append_shared_markdown_lines(&mut rendered_lines, &mut normal_lines, width);
    if rendered_lines.is_empty() {
        rendered_lines.push(Line::from(""));
    }

    rendered_lines
}

fn append_shared_markdown_lines(
    rendered_lines: &mut Vec<Line<'static>>,
    normal_lines: &mut Vec<&str>,
    width: usize,
) {
    if normal_lines.is_empty() {
        return;
    }

    let markdown = normal_lines.join("\n");
    rendered_lines.extend(render_shared_markdown(&markdown, width));
    normal_lines.clear();
}

fn append_prompt_block_lines(
    rendered_lines: &mut Vec<Line<'static>>,
    prompt_block: &PromptBlock,
    width: usize,
) {
    match prompt_block_kind(
        prompt_block
            .raw_lines
            .first()
            .map(String::as_str)
            .unwrap_or_default(),
    ) {
        PromptBlockKind::Clarification => {
            rendered_lines.extend(render_shared_markdown(
                &prompt_block.raw_lines.join("\n"),
                width,
            ));
        }
        PromptBlockKind::UserPrompt => {
            rendered_lines.extend(render_user_prompt_markdown_block(
                &prompt_block.content_lines,
                width,
            ));
        }
    }

    if prompt_block.consumed_closing_line {
        rendered_lines.push(Line::from(""));
    }
}

fn prompt_block_kind(raw_line: &str) -> PromptBlockKind {
    let is_clarification_header = raw_line
        .strip_prefix(USER_PROMPT_PREFIX)
        .is_some_and(|content| content.trim() == CLARIFICATION_HEADER);
    if is_clarification_header {
        return PromptBlockKind::Clarification;
    }

    PromptBlockKind::UserPrompt
}

fn collect_prompt_block_lines(raw_lines: &[&str], start_index: usize) -> PromptBlock {
    let mut content_lines = Vec::new();
    let mut raw_prompt_lines = Vec::new();
    let mut line_index = start_index;
    let continuation_padding = prompt_block::user_prompt_continuation_prefix();

    while let Some(raw_line) = raw_lines.get(line_index) {
        if line_index > start_index {
            if raw_line.is_empty() {
                return PromptBlock {
                    content_lines,
                    raw_lines: raw_prompt_lines,
                    consumed_closing_line: true,
                    next_line_index: line_index + 1,
                };
            }

            if raw_line.starts_with(USER_PROMPT_PREFIX) {
                return PromptBlock {
                    content_lines,
                    raw_lines: raw_prompt_lines,
                    consumed_closing_line: false,
                    next_line_index: line_index,
                };
            }
        }

        raw_prompt_lines.push((*raw_line).to_string());
        let content = if line_index == start_index {
            raw_line
                .strip_prefix(USER_PROMPT_PREFIX)
                .unwrap_or(raw_line)
        } else {
            raw_line
                .strip_prefix(continuation_padding.as_str())
                .unwrap_or(raw_line)
        };
        content_lines.push(content.to_string());
        line_index += 1;
    }

    PromptBlock {
        content_lines,
        raw_lines: raw_prompt_lines,
        consumed_closing_line: false,
        next_line_index: line_index,
    }
}

fn render_user_prompt_markdown_block(prompt_lines: &[String], width: usize) -> Vec<Line<'static>> {
    let prompt_text = prompt_lines.join("\n");
    if prompt_text.trim().is_empty() {
        return vec![prompt_block::user_prompt_padding_line(width)];
    }

    let prefix_width = USER_PROMPT_PREFIX.chars().count();
    let content_width = width
        .saturating_sub(prefix_width)
        .saturating_sub(USER_PROMPT_RIGHT_GUTTER_WIDTH)
        .max(1);
    let rendered_prompt_lines = render_shared_markdown(&prompt_text, content_width);
    let mut lines = Vec::with_capacity(rendered_prompt_lines.len().saturating_add(2));
    let mut has_rendered_content_line = false;
    let continuation_prefix = prompt_block::user_prompt_continuation_prefix();

    lines.push(prompt_block::user_prompt_padding_line(width));
    for rendered_line in rendered_prompt_lines {
        if rendered_line.width() == 0 {
            lines.push(prompt_block::user_prompt_padding_line(width));

            continue;
        }

        let prefix = if has_rendered_content_line {
            continuation_prefix.as_str()
        } else {
            USER_PROMPT_PREFIX
        };
        let prefix_style = if has_rendered_content_line {
            prompt_block::user_prompt_content_style()
        } else {
            prompt_block::user_prompt_prefix_style()
        };
        lines.push(prompt_block::user_prompt_markdown_line(
            user_prompt_content_line_spans(rendered_line.spans),
            prefix,
            prefix_style,
            width,
        ));
        has_rendered_content_line = true;
    }
    lines.push(prompt_block::user_prompt_padding_line(width));

    lines
}

/// Preserves markdown styling while highlighting plain `@` lookup tokens in
/// rendered user prompt content.
pub(crate) fn user_prompt_content_line_spans(
    rendered_spans: impl IntoIterator<Item = Span<'static>>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut is_lookup = false;
    let mut previous_character = None;
    let lookup_foreground = user_prompt_lookup_style().fg;

    for span in rendered_spans {
        let base_style = prompt_block::user_prompt_content_span(Span::styled("", span.style)).style;
        let content = span.content.into_owned();

        for character in content.chars() {
            if character == '@' && previous_character.is_none_or(char::is_whitespace) {
                is_lookup = true;
            } else if character.is_whitespace() {
                is_lookup = false;
            }

            let mut style = base_style;
            if is_lookup && span.style.fg.is_none() {
                style.fg = lookup_foreground;
            }

            push_span_character(&mut spans, style, character);
            previous_character = Some(character);
        }
    }

    spans
}

fn user_prompt_lookup_style() -> Style {
    let mut lookup_style = prompt_block::user_prompt_content_style();
    lookup_style.fg = Some(style::palette::info());

    lookup_style
}

fn push_span_character(spans: &mut Vec<Span<'static>>, style: Style, character: char) {
    if let Some(last_span) = spans.last_mut()
        && last_span.style == style
    {
        last_span.content.to_mut().push(character);

        return;
    }

    spans.push(Span::styled(character.to_string(), style));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_markdown_parses_prompt_continuation_line_markdown() {
        // Arrange
        let input = " › first line\n**bold**\n\nassistant";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines[2].to_string().trim_end(), "   bold");
        assert_eq!(
            lines[2].spans[0].style,
            prompt_block::user_prompt_content_style()
        );
        assert!(lines[2].spans.iter().any(|span| {
            span.content.as_ref() == "bold"
                && span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
        }));
        assert_eq!(lines[4].to_string(), "");
        assert_eq!(lines[5].to_string(), "assistant");
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
        let code_block_style = Style::default()
            .fg(style::palette::text_muted())
            .bg(style::palette::surface_overlay());
        assert!(lines.iter().all(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.style == code_block_style)
        }));
    }

    #[test]
    fn test_markdown_render_cache_reuses_prompt_block_lines() {
        // Arrange
        let cache = MarkdownRenderCache::default();
        let input = " › **bold** prompt";

        // Act
        let first_lines = cache.render(input, 80);
        let cached_lines = cache.render(input, 80);

        // Assert
        assert!(Arc::ptr_eq(&first_lines, &cached_lines));
        assert!(first_lines.iter().any(|line| {
            line.spans.iter().any(|span| {
                span.content.as_ref() == "bold"
                    && span
                        .style
                        .add_modifier
                        .contains(ratatui::style::Modifier::BOLD)
            })
        }));
    }

    #[test]
    fn test_markdown_render_cache_keeps_html_entries_separate_and_invalidates_them() {
        // Arrange
        let cache = MarkdownRenderCache::default();
        let input = "Use <strong>shared</strong> rendering.";
        let initial_version = cache.version();

        // Act
        let markdown_lines = cache.render(input, 80);
        let html_lines = cache.render_html(input, 80);
        let cached_html_lines = cache.render_html(input, 80);
        cache.bump_version();
        let refreshed_markdown_lines = cache.render(input, 80);
        let refreshed_html_lines = cache.render_html(input, 80);

        // Assert
        assert_eq!(markdown_lines[0].to_string(), input);
        assert_eq!(html_lines[0].to_string(), "Use shared rendering.");
        assert!(!Arc::ptr_eq(&markdown_lines, &html_lines));
        assert!(Arc::ptr_eq(&html_lines, &cached_html_lines));
        assert!(!Arc::ptr_eq(&markdown_lines, &refreshed_markdown_lines));
        assert!(!Arc::ptr_eq(&html_lines, &refreshed_html_lines));
        assert_eq!(cache.version(), initial_version + 1);
    }

    #[test]
    fn test_render_markdown_keeps_consecutive_prompt_blocks_separate() {
        // Arrange
        let input = " › first prompt\n › second prompt";

        // Act
        let lines = render_markdown(input, 80);
        let rendered_lines = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            rendered_lines
                .iter()
                .filter(|line| line.as_str() == " › first prompt")
                .count(),
            1
        );
        assert_eq!(
            rendered_lines
                .iter()
                .filter(|line| line.as_str() == " › second prompt")
                .count(),
            1
        );
        assert!(
            !rendered_lines
                .iter()
                .any(|line| line == "   › second prompt")
        );
    }
}
