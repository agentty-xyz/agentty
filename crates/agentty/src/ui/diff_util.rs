use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;

use crate::ui::style;

const BORDER_HORIZONTAL_WIDTH: u16 = 2;
const DIFF_GIT_FILE_HEADER_PREFIX: &str = "diff --git";
const DIFF_GIT_HEADER_PREFIX: &str = "diff --git ";
const FOOTER_HEIGHT: u16 = 1;
const GUTTER_EXTRA_WIDTH: usize = 2;
const LINE_NUMBER_COLUMN_COUNT: usize = 2;
const LAYOUT_MARGIN: u16 = 1;
const MIN_GUTTER_WIDTH: usize = 1;
const NO_NEWLINE_MARKER: &str = r"\ No newline at end of file";
const SCROLLBAR_WIDTH: usize = 1;
const SIGN_COLUMN_WIDTH: usize = 1;

/// The kind of a line in a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// Git metadata identifying a changed file.
    FileHeader,
    /// Unified-diff range header.
    HunkHeader,
    /// Unchanged context line.
    Context,
    /// Added line from the new file.
    Addition,
    /// Deleted line from the old file.
    Deletion,
}

/// A parsed line from a unified diff, with optional old/new line numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffLine<'a> {
    /// Semantic line classification.
    pub kind: DiffLineKind,
    /// Line number in the old file, when applicable.
    pub old_line: Option<u32>,
    /// Line number in the new file, when applicable.
    pub new_line: Option<u32>,
    /// Diff content without the addition, deletion, or context prefix.
    pub content: &'a str,
}

/// Identifies what a tree line in the diff file explorer represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTreeItem {
    /// A folder prefix path (e.g. `"src/ui/"`).
    Folder(String),
    /// A full file path (e.g. `"src/ui/component/file_explorer.rs"`).
    File(String),
}

/// Shared page areas used by the diff view after applying its layout splits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffPageAreas {
    /// Right-side diff content panel.
    pub diff_area: Rect,
    /// Left-side changed-file explorer panel.
    pub file_list_area: Rect,
    /// Bottom keybinding footer.
    pub footer_area: Rect,
}

/// Vertical sections inside the unified diff sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSidebarAreas {
    /// Linked review-comment selector shown below changed files.
    pub comment_list_area: Rect,
    /// Changed-file explorer shown above linked review comments.
    pub file_list_area: Rect,
}

/// Shared wrapping and viewport measurements for rendering the diff panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffRenderLayout {
    /// Available row width after borders and scrollbar reservation.
    pub content_width: usize,
    /// Width of each old or new line-number column.
    pub gutter_width: usize,
    /// Combined width reserved before diff content.
    pub prefix_width: usize,
    /// Number of visible rows inside the diff border.
    pub viewport_height: u16,
}

/// Extract `(old_start, old_count, new_start, new_count)` from a hunk header
/// like `@@ -10,5 +20,7 @@`.
pub fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let line = line.strip_prefix("@@ -")?;
    let at_idx = line.find(" @@")?;
    let range_part = &line[..at_idx];
    let mut parts = range_part.split(" +");
    let old_range = parts.next()?;
    let new_range = parts.next()?;

    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;

    Some((old_start, old_count, new_start, new_count))
}

/// Parse a full unified diff into structured [`DiffLine`] entries with line
/// numbers. Git's no-newline marker is retained without advancing either
/// source counter.
pub fn parse_diff_lines(diff: &str) -> Vec<DiffLine<'_>> {
    let mut result = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for line in diff.lines() {
        if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            result.push(DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if line.starts_with("@@") {
            if let Some((old_start, _, new_start, _)) = parse_hunk_header(line) {
                old_line = old_start;
                new_line = new_start;
            }
            result.push(DiffLine {
                kind: DiffLineKind::HunkHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if line == NO_NEWLINE_MARKER {
            result.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if let Some(rest) = line.strip_prefix('+') {
            result.push(DiffLine {
                kind: DiffLineKind::Addition,
                old_line: None,
                new_line: Some(new_line),
                content: rest,
            });
            new_line += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            result.push(DiffLine {
                kind: DiffLineKind::Deletion,
                old_line: Some(old_line),
                new_line: None,
                content: rest,
            });
            old_line += 1;
        } else {
            let content = line.strip_prefix(' ').unwrap_or(line);
            result.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_line),
                new_line: Some(new_line),
                content,
            });
            old_line += 1;
            new_line += 1;
        }
    }

    result
}

/// Extracts and decodes the old and new repository-relative paths from a
/// `diff --git a/<old> b/<new>` file header.
///
/// Git C-quotes paths containing non-ASCII or other special bytes. Those
/// quoted tokens are decoded before their `a/` and `b/` prefixes are removed.
pub fn diff_header_paths(header_line: &str) -> Option<(String, String)> {
    let encoded_paths = header_line.strip_prefix(DIFF_GIT_HEADER_PREFIX)?;
    let (old_path, remaining) = parse_git_path_token(encoded_paths)?;
    let remaining = remaining.strip_prefix(' ')?;
    let (new_path, trailing) = parse_git_path_token(remaining.trim_start())?;
    if !trailing.is_empty() {
        return None;
    }
    let old_path = old_path.strip_prefix("a/")?.to_string();
    let new_path = new_path.strip_prefix("b/")?.to_string();

    Some((old_path, new_path))
}

/// Extracts the new/right-side repository-relative path from a diff header.
pub fn diff_header_new_path(header_line: &str) -> Option<String> {
    let (_, new_path) = diff_header_paths(header_line)?;

    Some(new_path)
}

/// Find the maximum line number across all parsed diff lines for gutter width
/// calculation.
pub fn max_diff_line_number(lines: &[DiffLine<'_>]) -> u32 {
    lines
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .unwrap_or(0)
}

/// Returns the shared display width for each old/new line-number column.
pub fn diff_line_gutter_width(lines: &[DiffLine<'_>]) -> usize {
    let max_line_number = max_diff_line_number(lines);
    if max_line_number == 0 {
        return MIN_GUTTER_WIDTH;
    }

    max_line_number.ilog10() as usize + MIN_GUTTER_WIDTH
}

/// Counts total added and removed lines across parsed diff content.
pub fn diff_line_change_totals(lines: &[DiffLine<'_>]) -> (usize, usize) {
    lines.iter().fold(
        (0_usize, 0_usize),
        |(added_count, removed_count), line| match line.kind {
            DiffLineKind::Addition => (added_count.saturating_add(1), removed_count),
            DiffLineKind::Deletion => (added_count, removed_count.saturating_add(1)),
            _ => (added_count, removed_count),
        },
    )
}

/// Returns the sign and semantic content style shared by diff body rows.
pub fn body_diff_line_style(kind: DiffLineKind) -> (&'static str, Style) {
    match kind {
        DiffLineKind::Addition => (
            "+",
            Style::default()
                .fg(style::palette::success())
                .bg(style::palette::surface_success()),
        ),
        DiffLineKind::Deletion => (
            "-",
            Style::default()
                .fg(style::palette::danger())
                .bg(style::palette::surface_danger()),
        ),
        DiffLineKind::Context | DiffLineKind::FileHeader | DiffLineKind::HunkHeader => {
            (" ", Style::default().fg(style::palette::text_muted()))
        }
    }
}

/// Returns the subtle style shared by old/new line-number gutters.
pub fn body_diff_line_gutter_style() -> Style {
    Style::default().fg(style::palette::text_subtle())
}

/// Builds the old/new line-number gutter shared by diff body rows.
pub fn body_diff_line_gutter(diff_line: &DiffLine<'_>, gutter_width: usize) -> String {
    let old_line = diff_line.old_line.map_or_else(
        || " ".repeat(gutter_width),
        |line_number| format!("{line_number:>gutter_width$}"),
    );
    let new_line = diff_line.new_line.map_or_else(
        || " ".repeat(gutter_width),
        |line_number| format!("{line_number:>gutter_width$}"),
    );

    format!("{old_line}│{new_line} ")
}

/// Split a diff content string into chunks that fit within `max_width`
/// characters. Returns at least one chunk (empty string if content is empty).
pub fn wrap_diff_content(content: &str, max_width: usize) -> Vec<&str> {
    if max_width == 0 {
        return vec![content];
    }

    let mut chunks = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if remaining.len() <= max_width {
            chunks.push(remaining);

            break;
        }

        let split_at = remaining
            .char_indices()
            .nth(max_width)
            .map_or(remaining.len(), |(idx, _)| idx);
        chunks.push(&remaining[..split_at]);
        remaining = &remaining[split_at..];
    }

    if chunks.is_empty() {
        chunks.push("");
    }

    chunks
}

/// Returns the shared diff-page areas after applying the page margin, footer,
/// and file-list split used by the diff view.
pub fn diff_page_areas(terminal_area: Rect) -> DiffPageAreas {
    let page_chunks = Layout::default()
        .constraints([Constraint::Min(0), Constraint::Length(FOOTER_HEIGHT)])
        .margin(LAYOUT_MARGIN)
        .split(terminal_area);
    let content_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(page_chunks[0]);

    DiffPageAreas {
        diff_area: content_layout[1],
        file_list_area: content_layout[0],
        footer_area: page_chunks[1],
    }
}

/// Splits the diff sidebar into Files and Comments sections when linked
/// review-comment state is available.
pub fn diff_sidebar_areas(sidebar_area: Rect, show_comments: bool) -> DiffSidebarAreas {
    if !show_comments {
        return DiffSidebarAreas {
            comment_list_area: Rect::default(),
            file_list_area: sidebar_area,
        };
    }

    let sidebar_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(sidebar_area);

    DiffSidebarAreas {
        comment_list_area: sidebar_layout[1],
        file_list_area: sidebar_layout[0],
    }
}

/// Returns the wrapping metrics used to render one diff panel.
pub fn diff_render_layout(
    parsed_lines: &[DiffLine<'_>],
    diff_area: Rect,
    reserve_scrollbar_width: bool,
) -> DiffRenderLayout {
    let gutter_width = diff_line_gutter_width(parsed_lines);
    let prefix_width =
        gutter_width * LINE_NUMBER_COLUMN_COUNT + GUTTER_EXTRA_WIDTH + SIGN_COLUMN_WIDTH;
    let scrollbar_width = usize::from(reserve_scrollbar_width) * SCROLLBAR_WIDTH;
    let scrollbar_width = u16::try_from(scrollbar_width).unwrap_or(u16::MAX);
    let content_width = diff_area
        .width
        .saturating_sub(BORDER_HORIZONTAL_WIDTH)
        .saturating_sub(scrollbar_width);
    let content_width = usize::from(content_width);
    let viewport_height = diff_area.height.saturating_sub(BORDER_HORIZONTAL_WIDTH);

    DiffRenderLayout {
        content_width,
        gutter_width,
        prefix_width,
        viewport_height,
    }
}

/// Returns whether a diff panel needs scrolling for the given viewport.
pub fn diff_has_scrollable_overflow(line_count: usize, viewport_height: u16) -> bool {
    line_count > usize::from(viewport_height) && viewport_height > 0
}

/// Clamps a diff scroll offset to the last visible line in the viewport.
pub fn clamp_diff_scroll_offset(
    scroll_offset: u16,
    line_count: usize,
    viewport_height: u16,
) -> u16 {
    let max_scroll = line_count.saturating_sub(usize::from(viewport_height));
    let max_scroll = u16::try_from(max_scroll).unwrap_or(u16::MAX);

    scroll_offset.min(max_scroll)
}

/// Returns the inner scrollbar track rectangle for the diff panel.
pub fn diff_scrollbar_area(diff_area: Rect, viewport_height: u16) -> Rect {
    Rect::new(
        diff_area.x + diff_area.width.saturating_sub(BORDER_HORIZONTAL_WIDTH),
        diff_area.y + 1,
        1,
        viewport_height,
    )
}

/// Counts rendered diff rows after gutter formatting and content wrapping.
pub fn rendered_diff_line_count(parsed_lines: &[DiffLine<'_>], layout: DiffRenderLayout) -> usize {
    let content_available = layout.content_width.saturating_sub(layout.prefix_width);
    let mut rendered_line_count = 0;

    for diff_line in parsed_lines {
        match diff_line.kind {
            DiffLineKind::FileHeader => {
                if diff_line.content.starts_with("diff ") && rendered_line_count > 0 {
                    rendered_line_count += 1;
                }

                rendered_line_count += 1;
            }
            DiffLineKind::HunkHeader => rendered_line_count += 1,
            DiffLineKind::Context | DiffLineKind::Addition | DiffLineKind::Deletion => {
                rendered_line_count +=
                    wrap_diff_content(diff_line.content, content_available).len();
            }
        }
    }

    if rendered_line_count == 0 {
        return 1;
    }

    rendered_line_count
}

/// Returns the largest valid vertical scroll offset for the diff panel.
///
/// The calculation mirrors diff-page layout, wrapping, and scrollbar width so
/// runtime key handling can clamp scroll state to what the user can actually
/// see.
pub fn diff_view_max_scroll_offset(parsed_lines: &[DiffLine<'_>], terminal_area: Rect) -> u16 {
    let diff_area = diff_page_areas(terminal_area).diff_area;
    let mut layout = diff_render_layout(parsed_lines, diff_area, false);
    if layout.viewport_height == 0 {
        return 0;
    }

    let mut rendered_line_count = rendered_diff_line_count(parsed_lines, layout);
    if diff_has_scrollable_overflow(rendered_line_count, layout.viewport_height) {
        layout = diff_render_layout(parsed_lines, diff_area, true);
        rendered_line_count = rendered_diff_line_count(parsed_lines, layout);
    }

    clamp_diff_scroll_offset(u16::MAX, rendered_line_count, layout.viewport_height)
}

/// Filters `parsed_lines` to only the lines belonging to the given
/// [`FileTreeItem`].
///
/// For a [`FileTreeItem::File`] the result contains the diff section whose
/// `diff --git` header references that file path. For a
/// [`FileTreeItem::Folder`] the result contains all sections whose file paths
/// start with the folder prefix.
pub fn filter_diff_lines<'a>(
    parsed_lines: &[DiffLine<'a>],
    item: &FileTreeItem,
) -> Vec<DiffLine<'a>> {
    let mut result = Vec::new();
    let mut include_section = false;

    for diff_line in parsed_lines {
        if diff_line.kind == DiffLineKind::FileHeader
            && diff_line.content.starts_with(DIFF_GIT_FILE_HEADER_PREFIX)
        {
            include_section = diff_header_matches_item(diff_line.content, item);
        }

        if include_section {
            result.push(DiffLine {
                kind: diff_line.kind,
                old_line: diff_line.old_line,
                new_line: diff_line.new_line,
                content: diff_line.content,
            });
        }
    }

    result
}

/// Returns diff lines for the selected file-tree item, or the full diff when
/// the selection is out of bounds.
pub fn selected_diff_lines<'a>(
    parsed_lines: &[DiffLine<'a>],
    tree_items: &[FileTreeItem],
    selected_index: usize,
) -> Vec<DiffLine<'a>> {
    let Some(selected_item) = tree_items.get(selected_index) else {
        return parsed_lines.to_vec();
    };

    filter_diff_lines(parsed_lines, selected_item)
}

/// Checks whether a `diff --git` header line matches the given tree item.
fn diff_header_matches_item(header_line: &str, item: &FileTreeItem) -> bool {
    let Some(file_path) = diff_header_new_path(header_line) else {
        return false;
    };

    match item {
        FileTreeItem::File(path) => file_path == path.as_str(),
        FileTreeItem::Folder(prefix) => file_path.starts_with(prefix.as_str()),
    }
}

const DEFAULT_REVIEW_COMMENT: &str = "Review the highlighted changes.";
const MAX_REVIEW_HIGHLIGHT_COUNT: usize = 8;
const MAX_REVIEW_FALLBACK_COUNT: usize = 5;
const MAX_REVIEW_SNIPPET_WIDTH: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewHighlight {
    comment: &'static str,
    file_path: String,
    line_number: Option<u32>,
    order: usize,
    score: u16,
    sign: char,
    snippet: String,
}

/// Builds review markdown using critical diff highlights.
pub fn build_review_text(diff: &str) -> String {
    let highlights = review_highlights(diff);

    let mut lines = vec![
        "## Review".to_string(),
        String::new(),
        "### Agent Comments".to_string(),
    ];

    lines.push(format!("- {DEFAULT_REVIEW_COMMENT}"));

    lines.push(String::new());
    lines.push("### Critical Diff Highlights".to_string());

    if highlights.is_empty() {
        lines.push("- No changes found in the current diff.".to_string());
    } else {
        lines.extend(highlights.iter().map(review_highlight_markdown));
    }

    lines.push(String::new());
    lines.push("Press `d` for the full diff.".to_string());

    lines.join("\n")
}

/// Returns scored review highlights from unified diff text.
fn review_highlights(diff: &str) -> Vec<ReviewHighlight> {
    let mut cursor = DiffReviewCursor::default();
    let mut highlight_state = ReviewHighlightCollection::default();
    let mut order = 0_usize;

    for raw_line in diff.lines() {
        if cursor.advance_metadata(raw_line) {
            continue;
        }

        if let Some(changed_line) = cursor.changed_line(raw_line) {
            highlight_state.push_changed_line(&changed_line, order);
            order = order.saturating_add(1);

            continue;
        }

        cursor.advance_context(raw_line);
    }

    highlight_state.into_ranked_highlights()
}

/// Tracks the current file and line numbers while scanning a unified diff.
struct DiffReviewCursor {
    current_file: String,
    new_line: u32,
    old_line: u32,
}

impl Default for DiffReviewCursor {
    fn default() -> Self {
        Self {
            current_file: "unknown".to_string(),
            new_line: 0,
            old_line: 0,
        }
    }
}

impl DiffReviewCursor {
    /// Advances through file headers, hunk headers, and diff metadata.
    fn advance_metadata(&mut self, raw_line: &str) -> bool {
        if let Some(file_path) = parse_diff_file_path(raw_line) {
            self.current_file = file_path;

            return true;
        }

        if let Some((old_start, _, new_start, _)) = parse_hunk_header(raw_line) {
            self.old_line = old_start;
            self.new_line = new_start;

            return true;
        }

        raw_line.starts_with("index ")
            || raw_line.starts_with("--- ")
            || raw_line.starts_with("+++ ")
    }

    /// Returns changed-line data and advances the matching line counter.
    fn changed_line<'line>(&mut self, raw_line: &'line str) -> Option<ChangedReviewLine<'line>> {
        if let Some(content) = raw_line.strip_prefix('+') {
            let line_number = self.new_line;
            self.new_line = self.new_line.saturating_add(1);

            return Some(self.review_line(line_number, '+', content));
        }

        if let Some(content) = raw_line.strip_prefix('-') {
            let line_number = self.old_line;
            self.old_line = self.old_line.saturating_add(1);

            return Some(self.review_line(line_number, '-', content));
        }

        None
    }

    /// Advances both counters for an unchanged context line.
    fn advance_context(&mut self, raw_line: &str) {
        if raw_line.starts_with('\\') {
            return;
        }

        self.old_line = self.old_line.saturating_add(1);
        self.new_line = self.new_line.saturating_add(1);
    }

    /// Builds changed-line data for scoring.
    fn review_line<'line>(
        &self,
        line_number: u32,
        sign: char,
        content: &'line str,
    ) -> ChangedReviewLine<'line> {
        ChangedReviewLine {
            content,
            file_path: self.current_file.clone(),
            line_number,
            sign,
        }
    }
}

/// Borrowed data for one added or removed diff line.
struct ChangedReviewLine<'line> {
    content: &'line str,
    file_path: String,
    line_number: u32,
    sign: char,
}

/// Collects high-signal and fallback review highlights during diff scanning.
#[derive(Default)]
struct ReviewHighlightCollection {
    fallback_highlights: Vec<ReviewHighlight>,
    highlights: Vec<ReviewHighlight>,
}

impl ReviewHighlightCollection {
    /// Scores one changed line and stores it in the best matching collection.
    fn push_changed_line(&mut self, changed_line: &ChangedReviewLine<'_>, order: usize) {
        let line_number = Some(changed_line.line_number);
        if let Some(highlight) = review_highlight(
            &changed_line.file_path,
            line_number,
            changed_line.sign,
            changed_line.content,
            order,
        ) {
            self.highlights.push(highlight);

            return;
        }

        if let Some(fallback_highlight) = review_fallback_highlight(
            &changed_line.file_path,
            line_number,
            changed_line.sign,
            changed_line.content,
            order,
        ) {
            self.fallback_highlights.push(fallback_highlight);
        }
    }

    /// Returns ranked high-signal highlights, falling back to representative
    /// lines.
    fn into_ranked_highlights(mut self) -> Vec<ReviewHighlight> {
        if self.highlights.is_empty() {
            self.fallback_highlights.truncate(MAX_REVIEW_FALLBACK_COUNT);

            return self.fallback_highlights;
        }

        self.highlights.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.order.cmp(&right.order))
        });
        self.highlights.truncate(MAX_REVIEW_HIGHLIGHT_COUNT);
        self.highlights.sort_by_key(|highlight| highlight.order);

        self.highlights
    }
}

/// Builds one markdown list item for a review highlight.
fn review_highlight_markdown(highlight: &ReviewHighlight) -> String {
    let location = highlight
        .line_number
        .map_or_else(|| "?".to_string(), |line_number| line_number.to_string());

    format!(
        "- `{}`:{} {} `{}` — {}",
        highlight.file_path, location, highlight.sign, highlight.snippet, highlight.comment
    )
}

/// Creates a scored highlight when a change matches high-signal criticality
/// heuristics.
fn review_highlight(
    file_path: &str,
    line_number: Option<u32>,
    sign: char,
    content: &str,
    order: usize,
) -> Option<ReviewHighlight> {
    const DEFAULT_COMMENT: &str = "Behavior changed.";
    const RUNTIME_COMMENT: &str = "Runtime safety or error handling changed.";
    const SECURITY_COMMENT: &str = "Authorization or security-sensitive logic changed.";
    const DATABASE_COMMENT: &str = "Database behavior or schema logic changed.";
    const PROCESS_COMMENT: &str = "External command execution path changed.";
    const CONFIG_COMMENT: &str = "Build or runtime configuration changed.";

    let normalized_content = content.to_lowercase();
    let normalized_path = file_path.to_lowercase();
    let mut classification = ReviewHighlightClassification::new(DEFAULT_COMMENT);

    classification.add_runtime_score(
        contains_any(
            &normalized_content,
            &["unsafe", "unwrap(", "expect(", "panic!("],
        ),
        RUNTIME_COMMENT,
    );
    classification.add_category_score(
        contains_any(
            &normalized_content,
            &[
                "auth",
                "permission",
                "token",
                "secret",
                "password",
                "admin",
                "role",
                "acl",
            ],
        ) || contains_any(&normalized_path, &["auth", "permission", "security"]),
        4,
        SECURITY_COMMENT,
    );
    classification.add_category_score(
        contains_any(
            &normalized_content,
            &[
                "select ", "insert ", "update ", "delete ", "drop ", "alter ",
            ],
        ) || contains_any(&normalized_path, &["migration", ".sql"]),
        4,
        DATABASE_COMMENT,
    );
    classification.add_category_score(
        contains_any(
            &normalized_content,
            &["command", "shell", "process", "exec(", "spawn(", "system("],
        ),
        3,
        PROCESS_COMMENT,
    );
    classification.add_category_score(
        contains_any(
            &normalized_path,
            &[
                "cargo.toml",
                "containerfile",
                ".github/workflows",
                "dockerfile",
                ".yaml",
                ".yml",
                ".toml",
            ],
        ),
        2,
        CONFIG_COMMENT,
    );

    let (score, comment) = classification.finish()?;

    Some(ReviewHighlight {
        comment,
        file_path: file_path.to_string(),
        line_number,
        order,
        score,
        sign,
        snippet: review_snippet(content),
    })
}

/// Accumulates review-highlight category scoring for one changed line.
struct ReviewHighlightClassification {
    comment: &'static str,
    matched_runtime: bool,
    score: u16,
}

impl ReviewHighlightClassification {
    /// Creates a score accumulator with the default comment.
    fn new(comment: &'static str) -> Self {
        Self {
            comment,
            matched_runtime: false,
            score: 0,
        }
    }

    /// Adds the runtime category, which keeps comment precedence.
    fn add_runtime_score(&mut self, matched: bool, comment: &'static str) {
        if !matched {
            return;
        }

        self.score = self.score.saturating_add(5);
        self.comment = comment;
        self.matched_runtime = true;
    }

    /// Adds a non-runtime category score and comment when matched.
    fn add_category_score(&mut self, matched: bool, score: u16, comment: &'static str) {
        if !matched {
            return;
        }

        self.score = self.score.saturating_add(score);
        if !self.matched_runtime {
            self.comment = comment;
        }
    }

    /// Returns the accumulated score and comment when a category matched.
    fn finish(self) -> Option<(u16, &'static str)> {
        if self.score == 0 {
            return None;
        }

        Some((self.score, self.comment))
    }
}

/// Creates an unscored fallback highlight when no criticality heuristic
/// matches.
fn review_fallback_highlight(
    file_path: &str,
    line_number: Option<u32>,
    sign: char,
    content: &str,
    order: usize,
) -> Option<ReviewHighlight> {
    let snippet = review_snippet(content);
    if snippet.is_empty() {
        return None;
    }

    Some(ReviewHighlight {
        comment: "General code change; inspect full diff for context.",
        file_path: file_path.to_string(),
        line_number,
        order,
        score: 0,
        sign,
        snippet,
    })
}

/// Parses the destination file path from a `diff --git` header line.
fn parse_diff_file_path(line: &str) -> Option<String> {
    let suffix = line.strip_prefix("diff --git a/")?;
    let (_, rhs) = suffix.split_once(" b/")?;

    Some(rhs.to_string())
}

/// Returns a clean one-line snippet for review output.
fn review_snippet(content: &str) -> String {
    let collapsed = content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if collapsed.is_empty() {
        return String::new();
    }

    let char_count = collapsed.chars().count();
    if char_count <= MAX_REVIEW_SNIPPET_WIDTH {
        return collapsed;
    }

    let truncated = collapsed
        .chars()
        .take(MAX_REVIEW_SNIPPET_WIDTH.saturating_sub(3))
        .collect::<String>();

    format!("{truncated}...")
}

/// Returns whether `text` contains any token from `needles`.
fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    if let Some((start, count)) = range.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((range.parse().ok()?, 1))
    }
}

/// Decodes one quoted or unquoted path token and returns the unconsumed input.
fn parse_git_path_token(input: &str) -> Option<(String, &str)> {
    if !input.starts_with('"') {
        let token_end = input.find(' ').unwrap_or(input.len());
        let token = input.get(..token_end)?;
        if token.is_empty() {
            return None;
        }

        return Some((token.to_string(), input.get(token_end..)?));
    }

    let bytes = input.as_bytes();
    let mut decoded = Vec::new();
    let mut byte_index = 1;
    while byte_index < bytes.len() {
        match bytes[byte_index] {
            b'"' => {
                let path = String::from_utf8(decoded).ok()?;
                let remaining = input.get(byte_index.saturating_add(1)..)?;

                return Some((path, remaining));
            }
            b'\\' => {
                byte_index = byte_index.saturating_add(1);
                let escaped = *bytes.get(byte_index)?;
                let decoded_byte = match escaped {
                    b'a' => b'\x07',
                    b'b' => b'\x08',
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => b'\x0b',
                    b'f' => b'\x0c',
                    b'r' => b'\r',
                    b'\\' => b'\\',
                    b'"' => b'"',
                    b'0'..=b'7' => {
                        let mut octal_value = escaped - b'0';
                        for _ in 1..3 {
                            let Some(next_digit @ b'0'..=b'7') = bytes.get(byte_index + 1).copied()
                            else {
                                break;
                            };
                            byte_index += 1;
                            octal_value = octal_value
                                .saturating_mul(8)
                                .saturating_add(next_digit - b'0');
                        }
                        octal_value
                    }
                    _ => return None,
                };
                decoded.push(decoded_byte);
            }
            byte => decoded.push(byte),
        }
        byte_index = byte_index.saturating_add(1);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF_MAIN_HEADER: &str = "diff --git a/src/main.rs b/src/main.rs";
    const DIFF_NESTED_HEADER: &str =
        "diff --git a/src/ui/component/file_explorer.rs b/src/ui/component/file_explorer.rs";
    const DIFF_README_HEADER: &str = "diff --git a/README.md b/README.md";

    #[test]
    fn test_diff_header_paths_decodes_git_quoted_non_ascii_paths() {
        // Arrange
        let header = concat!(
            "diff --git \"a/docs/\\346\\227\\245\\346\\234\\254.md\" ",
            "\"b/docs/\\346\\227\\245\\346\\234\\254.md\"",
        );

        // Act
        let paths = diff_header_paths(header);
        let new_path = diff_header_new_path(header);

        // Assert
        assert_eq!(
            paths,
            Some(("docs/日本.md".to_string(), "docs/日本.md".to_string()))
        );
        assert_eq!(new_path, Some("docs/日本.md".to_string()));
    }

    #[test]
    fn test_diff_header_paths_decodes_spaces_and_c_escapes() {
        // Arrange
        let spaced_header = "diff --git \"a/docs/old file.md\" \"b/docs/new file.md\"";
        let escape_cases = [
            (r#""a/\a" tail"#, "a/\x07"),
            (r#""a/\b" tail"#, "a/\x08"),
            (r#""a/\t" tail"#, "a/\t"),
            (r#""a/\n" tail"#, "a/\n"),
            (r#""a/\v" tail"#, "a/\x0b"),
            (r#""a/\f" tail"#, "a/\x0c"),
            (r#""a/\r" tail"#, "a/\r"),
            (r#""a/\\" tail"#, "a/\\"),
            (r#""a/\"" tail"#, "a/\""),
            (r#""a/\7x" tail"#, "a/\x07x"),
        ];

        // Act
        let spaced_paths = diff_header_paths(spaced_header);
        let decoded_escapes = escape_cases
            .iter()
            .map(|(encoded, _)| parse_git_path_token(encoded))
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            spaced_paths,
            Some((
                "docs/old file.md".to_string(),
                "docs/new file.md".to_string(),
            ))
        );
        for ((_, expected), decoded) in escape_cases.iter().zip(decoded_escapes) {
            assert_eq!(decoded, Some(((*expected).to_string(), " tail")));
        }
    }

    #[test]
    fn test_git_path_token_and_header_reject_malformed_input() {
        // Arrange
        let malformed_tokens = [
            "",
            r#""unterminated"#,
            r#""a/\q""#,
            r#""a/\"#,
            r#""a/\377""#,
        ];
        let malformed_headers = [
            "not a diff header",
            "diff --git a/only-one-path",
            "diff --git a/old.md b/new.md trailing",
            "diff --git old.md b/new.md",
            "diff --git a/old.md new.md",
            "diff --git \"a/old.md\"\"b/new.md\"",
        ];

        // Act
        let unquoted = parse_git_path_token("a/old.md b/new.md");
        let rejected_tokens = malformed_tokens
            .iter()
            .map(|token| parse_git_path_token(token))
            .collect::<Vec<_>>();
        let rejected_headers = malformed_headers
            .iter()
            .map(|header| diff_header_paths(header))
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(unquoted, Some(("a/old.md".to_string(), " b/new.md")));
        assert!(rejected_tokens.iter().all(Option::is_none));
        assert!(rejected_headers.iter().all(Option::is_none));
    }

    #[test]
    fn test_parse_hunk_header_basic() {
        // Arrange
        let line = "@@ -10,5 +20,7 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((10, 5, 20, 7)));
    }

    #[test]
    fn test_parse_hunk_header_no_count() {
        // Arrange
        let line = "@@ -1 +1 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((1, 1, 1, 1)));
    }

    #[test]
    fn test_parse_hunk_header_with_context() {
        // Arrange
        let line = "@@ -100,3 +200,4 @@ fn main() {";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((100, 3, 200, 4)));
    }

    #[test]
    fn test_parse_hunk_header_invalid() {
        // Arrange & Act & Assert
        assert_eq!(parse_hunk_header("not a hunk"), None);
        assert_eq!(parse_hunk_header("@@@ invalid @@@"), None);
    }

    #[test]
    fn test_parse_diff_lines_full() {
        // Arrange
        let diff = "\
diff --git a/file.rs b/file.rs
index abc..def 100644
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2
-removed";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 9);

        assert_eq!(lines[0].kind, DiffLineKind::FileHeader);
        assert_eq!(lines[0].content, "diff --git a/file.rs b/file.rs");
        assert_eq!(lines[0].old_line, None);

        assert_eq!(lines[4].kind, DiffLineKind::HunkHeader);
        assert_eq!(lines[4].old_line, None);

        assert_eq!(lines[5].kind, DiffLineKind::Context);
        assert_eq!(lines[5].content, "line1");
        assert_eq!(lines[5].old_line, Some(1));
        assert_eq!(lines[5].new_line, Some(1));

        assert_eq!(lines[6].kind, DiffLineKind::Addition);
        assert_eq!(lines[6].content, "added");
        assert_eq!(lines[6].old_line, None);
        assert_eq!(lines[6].new_line, Some(2));

        assert_eq!(lines[7].kind, DiffLineKind::Context);
        assert_eq!(lines[7].content, "line2");
        assert_eq!(lines[7].old_line, Some(2));
        assert_eq!(lines[7].new_line, Some(3));

        assert_eq!(lines[8].kind, DiffLineKind::Deletion);
        assert_eq!(lines[8].content, "removed");
        assert_eq!(lines[8].old_line, Some(3));
        assert_eq!(lines[8].new_line, None);
    }

    #[test]
    fn test_parse_diff_lines_does_not_count_no_newline_marker() {
        // Arrange
        let diff = concat!(
            "diff --git a/file.rs b/file.rs\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "\\ No newline at end of file\n",
            "+new\n",
        );

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines[3].content, r"\ No newline at end of file");
        assert_eq!(lines[3].old_line, None);
        assert_eq!(lines[3].new_line, None);
        assert_eq!(lines[4].new_line, Some(1));
    }

    #[test]
    fn test_parse_diff_lines_empty() {
        // Arrange
        let diff = "";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn test_max_diff_line_number() {
        // Arrange
        let diff = "\
@@ -95,3 +100,4 @@
 context
+added
 context2
-removed";
        let lines = parse_diff_lines(diff);

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 102);
    }

    #[test]
    fn test_max_diff_line_number_empty() {
        // Arrange
        let lines: Vec<DiffLine<'_>> = Vec::new();

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 0);
    }

    #[test]
    fn test_diff_line_gutter_width_matches_largest_line_number() {
        // Arrange
        let lines = parse_diff_lines("@@ -95,1 +100,1 @@\n context");

        // Act
        let gutter_width = diff_line_gutter_width(&lines);

        // Assert
        assert_eq!(gutter_width, 3);
    }

    #[test]
    fn test_diff_line_change_totals() {
        // Arrange
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2
-removed";
        let lines = parse_diff_lines(diff);

        // Act
        let totals = diff_line_change_totals(&lines);

        // Assert
        assert_eq!(totals, (1, 1));
    }

    #[test]
    fn test_diff_line_change_totals_ignores_headers() {
        // Arrange
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs";
        let lines = parse_diff_lines(diff);

        // Act
        let totals = diff_line_change_totals(&lines);

        // Assert
        assert_eq!(totals, (0, 0));
    }

    #[test]
    fn test_filter_diff_lines_by_file() {
        // Arrange
        let diff =
            format!("{DIFF_MAIN_HEADER}\n+added in main\n{DIFF_README_HEADER}\n+added in readme");
        let parsed_lines = parse_diff_lines(&diff);
        let item = FileTreeItem::File("src/main.rs".to_string());

        // Act
        let filtered = filter_diff_lines(&parsed_lines, &item);

        // Assert
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].content, DIFF_MAIN_HEADER);
        assert_eq!(filtered[1].content, "added in main");
    }

    #[test]
    fn test_filter_diff_lines_by_folder() {
        // Arrange
        let diff = format!(
            "{DIFF_MAIN_HEADER}\n+added in main\n{DIFF_NESTED_HEADER}\n-deleted in \
             explorer\n{DIFF_README_HEADER}\n+added in readme"
        );
        let parsed_lines = parse_diff_lines(&diff);
        let item = FileTreeItem::Folder("src/".to_string());

        // Act
        let filtered = filter_diff_lines(&parsed_lines, &item);

        // Assert
        assert_eq!(filtered.len(), 4);
        assert_eq!(filtered[0].content, DIFF_MAIN_HEADER);
        assert_eq!(filtered[1].content, "added in main");
        assert_eq!(filtered[2].content, DIFF_NESTED_HEADER);
        assert_eq!(filtered[3].content, "deleted in explorer");
    }

    #[test]
    fn test_wrap_diff_content_fits() {
        // Arrange
        let content = "short line";

        // Act
        let chunks = wrap_diff_content(content, 80);

        // Assert
        assert_eq!(chunks, vec!["short line"]);
    }

    #[test]
    fn test_wrap_diff_content_wraps() {
        // Arrange
        let content = "abcdefghij";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn test_wrap_diff_content_empty() {
        // Arrange & Act
        let chunks = wrap_diff_content("", 10);

        // Assert
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn test_wrap_diff_content_exact() {
        // Arrange
        let content = "abcd";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd"]);
    }

    #[test]
    fn test_diff_view_max_scroll_offset_returns_zero_for_short_diff() {
        // Arrange
        let parsed_lines = parse_diff_lines("+short");
        let terminal_area = Rect::new(0, 0, 120, 30);

        // Act
        let max_scroll_offset = diff_view_max_scroll_offset(&parsed_lines, terminal_area);

        // Assert
        assert_eq!(max_scroll_offset, 0);
    }

    #[test]
    fn test_diff_view_max_scroll_offset_counts_wrapped_overflow() {
        // Arrange
        let diff = format!("+{}", "0123456789".repeat(20));
        let parsed_lines = parse_diff_lines(&diff);
        let terminal_area = Rect::new(0, 0, 30, 8);

        // Act
        let max_scroll_offset = diff_view_max_scroll_offset(&parsed_lines, terminal_area);

        // Assert
        assert!(max_scroll_offset > 0);
    }

    #[test]
    fn test_build_review_text_includes_critical_highlights() {
        // Arrange
        let diff = "\
diff --git a/src/auth.rs b/src/auth.rs
@@ -8,1 +8,1 @@
-let can_merge = false;
+let can_merge = user.role == \"admin\";
@@ -20,1 +20,1 @@
-let value = maybe_value.unwrap();
+let value = maybe_value.expect(\"missing value\");";
        // Act
        let review = build_review_text(diff);

        // Assert
        assert!(review.contains("## Review"));
        assert!(review.contains(DEFAULT_REVIEW_COMMENT));
        assert!(review.contains("Authorization or security-sensitive logic changed."));
        assert!(review.contains("Runtime safety or error handling changed."));
        assert!(review.contains("src/auth.rs"));
    }

    #[test]
    fn test_build_review_text_highlights_containerfile_configuration() {
        // Arrange
        let diff = "\
diff --git a/container/e2e.Containerfile b/container/e2e.Containerfile
@@ -1,1 +1,1 @@
-FROM scratch
+FROM debian";

        // Act
        let review = build_review_text(diff);

        // Assert
        assert!(review.contains("container/e2e.Containerfile"));
        assert!(review.contains("Build or runtime configuration changed."));
    }

    #[test]
    fn test_build_review_text_uses_fallback_when_critical_hits_missing() {
        // Arrange
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
@@ -1,1 +1,1 @@
-let old_value = 1;
+let new_value = 2;";

        // Act
        let review = build_review_text(diff);

        // Assert
        assert!(review.contains(DEFAULT_REVIEW_COMMENT));
        assert!(review.contains("General code change; inspect full diff for context."));
        assert!(review.contains("src/main.rs"));
    }

    #[test]
    fn test_build_review_text_handles_empty_diff() {
        // Arrange
        let diff = "";

        // Act
        let review = build_review_text(diff);

        // Assert
        assert!(review.contains(DEFAULT_REVIEW_COMMENT));
        assert!(review.contains("No changes found in the current diff."));
    }
}
