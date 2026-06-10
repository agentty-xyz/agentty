use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::domain::agent::ReasoningLevel;
use crate::domain::session::{Session, SessionSize, Status};
use crate::ui::input_layout::first_table_column_width;
use crate::ui::state::help_action;
use crate::ui::text_util::{format_duration_compact, inline_text, truncate_spans_with_ellipsis};
use crate::ui::{Page, layout, markdown, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
/// Horizontal spacing between table columns in the session list.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Placeholder text rendered under group headers with no sessions.
const GROUP_EMPTY_PLACEHOLDER: &str = "No sessions...";
/// Tree branch prefix for child rows that have siblings after them.
const TREE_BRANCH_MIDDLE: &str = "├ ";
/// Tree branch prefix for the final child row in a stack.
const TREE_BRANCH_LAST: &str = "└ ";
/// Session list page renderer.
pub struct SessionListPage<'a> {
    /// Active project-scoped default reasoning level for sessions without an
    /// override.
    pub default_reasoning_level: ReasoningLevel,
    /// Session rows available for rendering.
    pub sessions: &'a [Session],
    /// Table selection state tied to the raw session ordering.
    pub table_state: &'a mut TableState,
    /// Current wall-clock time expressed as Unix seconds for live timer labels.
    wall_clock_unix_seconds: i64,
}

impl<'a> SessionListPage<'a> {
    /// Creates a session list page renderer.
    pub fn new(
        sessions: &'a [Session],
        table_state: &'a mut TableState,
        default_reasoning_level: ReasoningLevel,
        wall_clock_unix_seconds: i64,
    ) -> Self {
        Self {
            default_reasoning_level,
            sessions,
            table_state,
            wall_clock_unix_seconds,
        }
    }
}

impl Page for SessionListPage<'_> {
    /// Renders the grouped session table directly below the tab header plus
    /// the list footer.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);

        let selected_style = Style::default().bg(style::palette::surface());
        let header_style = Style::default()
            .bg(style::palette::surface())
            .fg(style::palette::text_muted())
            .add_modifier(Modifier::BOLD);
        let header_cells = ["Session", "Model", "Status", "Timer"]
            .iter()
            .map(|h| Cell::from(*h));
        let header = Row::new(header_cells).style(header_style).height(1);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Sessions")
            .border_style(style::border_style());
        let column_constraints = [
            Constraint::Fill(1),
            model_column_width(self.sessions, self.default_reasoning_level),
            status_column_width(self.sessions),
            timer_column_width(self.sessions, self.wall_clock_unix_seconds),
        ];
        let title_column_width = first_table_column_width(
            block.inner(areas.main_area).width,
            &column_constraints,
            TABLE_COLUMN_SPACING,
            0,
        );
        let table_rows = grouped_session_rows(self.sessions);
        let selected_session_id = selected_session_id(self.sessions, self.table_state.selected());
        let selected_row = selected_render_row(&table_rows, selected_session_id);
        let rows = table_rows.iter().map(|table_row| {
            render_table_row(
                table_row,
                title_column_width,
                self.default_reasoning_level,
                self.wall_clock_unix_seconds,
            )
        });
        let table = Table::new(rows, column_constraints)
            .column_spacing(TABLE_COLUMN_SPACING)
            .header(header)
            .block(block)
            .row_highlight_style(selected_style)
            .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        let previous_selection = self.table_state.selected();
        prepare_grouped_table_state(self.table_state, selected_row);
        f.render_stateful_widget(table, areas.main_area, self.table_state);
        self.table_state.select(previous_selection);

        let selected_session = self
            .table_state
            .selected()
            .and_then(|selected_index| self.sessions.get(selected_index));
        let help_message = Paragraph::new(session_list_help_line(selected_session));
        f.render_widget(help_message, areas.footer_area);
    }
}

/// Builds footer help content for session list mode.
fn session_list_help_line(selected_session: Option<&Session>) -> Line<'static> {
    let can_cancel_selected_session = selected_session.is_some_and(Session::allows_cancel_action);
    let can_open_selected_session = selected_session.is_some();
    let actions = help_action::session_list_footer_actions(
        can_cancel_selected_session,
        can_open_selected_session,
    );

    help_action::footer_line(&actions)
}

/// Prepares list table state for grouped row rendering.
///
/// The app stores selection as an index in the raw session slice, while the
/// table is rendered with extra group label and placeholder rows. Resetting
/// the offset before selecting a grouped row avoids stale deep offsets hiding
/// top group sections after scrolling back up.
fn prepare_grouped_table_state(table_state: &mut TableState, selected_row: Option<usize>) {
    *table_state.offset_mut() = 0;
    table_state.select(selected_row);
}

/// Render rows for grouped session list display.
enum SessionTableRow<'a> {
    GroupLabel(SessionGroup),
    /// Marker row shown when a group has zero sessions.
    EmptyGroupPlaceholder,
    /// Selectable session row with raw index and tree placement metadata.
    Session {
        index: usize,
        session: &'a Session,
        tree_prefix: SessionTreePrefix,
    },
}

/// Visual tree marker for a selectable session row.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SessionTreePrefix {
    /// Root-level session row with no tree marker.
    Root,
    /// One-level child row connected to its parent.
    Child { is_last: bool },
}

impl SessionTreePrefix {
    /// Returns the rendered tree marker for this row.
    fn label(self) -> &'static str {
        match self {
            Self::Root => "",
            Self::Child { is_last: true } => TREE_BRANCH_LAST,
            Self::Child { is_last: false } => TREE_BRANCH_MIDDLE,
        }
    }

    /// Returns the display width consumed by the tree marker.
    fn width(self) -> usize {
        self.label().chars().count()
    }
}

/// Session list groups shown in the table.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SessionGroup {
    ActiveSessions,
    Archive,
    MergeQueue,
}

impl SessionGroup {
    /// Returns the display label for a session group.
    fn label(self) -> &'static str {
        match self {
            Self::MergeQueue => "Merge queue",
            Self::ActiveSessions => "Active sessions",
            Self::Archive => "Archive",
        }
    }
}

/// Resolves the initial raw-session selection index for list-mode focus.
///
/// Active sessions are preferred so opening the session list lands on ongoing
/// work when both active and archived items are present. If no active sessions
/// exist, this falls back to the first selectable grouped row.
pub(crate) fn preferred_initial_session_index(sessions: &[Session]) -> Option<usize> {
    sessions_for_group(sessions, SessionGroup::ActiveSessions)
        .map(|(index, _)| index)
        .next()
        .or_else(|| grouped_session_indexes(sessions).first().copied())
}

/// Returns session indexes in the same order as selectable rows in the grouped
/// session table.
pub(crate) fn grouped_session_indexes(sessions: &[Session]) -> Vec<usize> {
    grouped_session_rows(sessions)
        .into_iter()
        .filter_map(|row| match row {
            SessionTableRow::Session { index, .. } => Some(index),
            SessionTableRow::GroupLabel(_) | SessionTableRow::EmptyGroupPlaceholder => None,
        })
        .collect()
}

/// Returns grouped display rows with merge queue, active, then archive
/// sessions.
fn grouped_session_rows(sessions: &[Session]) -> Vec<SessionTableRow<'_>> {
    let mut rows = Vec::with_capacity(sessions.len() + 6);
    append_group_rows(&mut rows, sessions, SessionGroup::MergeQueue);
    append_group_rows(&mut rows, sessions, SessionGroup::ActiveSessions);
    append_group_rows(&mut rows, sessions, SessionGroup::Archive);

    rows
}

/// Adds one group label row and either its sessions or an empty placeholder.
fn append_group_rows<'a>(
    rows: &mut Vec<SessionTableRow<'a>>,
    sessions: &'a [Session],
    group: SessionGroup,
) {
    rows.push(SessionTableRow::GroupLabel(group));

    let mut group_has_sessions = false;
    for (index, session) in sessions_for_group(sessions, group) {
        if has_loaded_parent_session(sessions, session) {
            continue;
        }

        rows.push(SessionTableRow::Session {
            index,
            session,
            tree_prefix: SessionTreePrefix::Root,
        });
        append_stacked_child_rows(rows, sessions, session.id.as_str());
        group_has_sessions = true;
    }

    if !group_has_sessions {
        rows.push(SessionTableRow::EmptyGroupPlaceholder);
    }
}

/// Adds the one-level stacked children for a parent session row.
fn append_stacked_child_rows<'a>(
    rows: &mut Vec<SessionTableRow<'a>>,
    sessions: &'a [Session],
    parent_session_id: &str,
) {
    let children = sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| {
            session
                .parent_session_id
                .as_ref()
                .is_some_and(|parent_id| parent_id.as_str() == parent_session_id)
        })
        .collect::<Vec<_>>();

    let child_count = children.len();
    for (child_position, (index, session)) in children.into_iter().enumerate() {
        rows.push(SessionTableRow::Session {
            index,
            session,
            tree_prefix: SessionTreePrefix::Child {
                is_last: child_position + 1 == child_count,
            },
        });
    }
}

/// Returns whether a session should be nested under a loaded parent row.
fn has_loaded_parent_session(sessions: &[Session], session: &Session) -> bool {
    let Some(parent_session_id) = session.parent_session_id.as_ref() else {
        return false;
    };

    sessions
        .iter()
        .any(|candidate| candidate.id.as_str() == parent_session_id.as_str())
}

/// Returns session indexes and snapshots for one grouped section.
fn sessions_for_group(
    sessions: &[Session],
    group: SessionGroup,
) -> impl Iterator<Item = (usize, &Session)> {
    sessions
        .iter()
        .enumerate()
        .filter(move |(_, session)| session_group(session) == group)
}

/// Returns the grouped section where a session should be displayed.
fn session_group(session: &Session) -> SessionGroup {
    match session.status {
        Status::Queued | Status::Merging => SessionGroup::MergeQueue,
        Status::Done | Status::Canceled => SessionGroup::Archive,
        _ => SessionGroup::ActiveSessions,
    }
}

/// Resolves the selected session id from the original session ordering.
fn selected_session_id(sessions: &[Session], selected_index: Option<usize>) -> Option<&str> {
    selected_index
        .and_then(|index| sessions.get(index))
        .map(|session| session.id.as_str())
}

/// Maps selected session id to the grouped table row index.
fn selected_render_row(
    rows: &[SessionTableRow<'_>],
    selected_session_id: Option<&str>,
) -> Option<usize> {
    let selected_session_id = selected_session_id?;

    rows.iter().position(|row| match row {
        SessionTableRow::GroupLabel(_) | SessionTableRow::EmptyGroupPlaceholder => false,
        SessionTableRow::Session { session, .. } => session.id == selected_session_id,
    })
}

/// Converts one grouped row descriptor into a `ratatui` table row.
fn render_table_row(
    row: &SessionTableRow<'_>,
    title_column_width: usize,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> Row<'static> {
    match row {
        SessionTableRow::GroupLabel(group) => render_group_label_row(*group),
        SessionTableRow::EmptyGroupPlaceholder => render_empty_group_placeholder_row(),
        SessionTableRow::Session {
            session,
            tree_prefix,
            ..
        } => render_session_row(
            session,
            *tree_prefix,
            title_column_width,
            default_reasoning_level,
            wall_clock_unix_seconds,
        ),
    }
}

/// Renders a non-selectable group label row.
fn render_group_label_row(group: SessionGroup) -> Row<'static> {
    let cells = vec![
        Cell::from(group.label()).style(Style::default().fg(style::palette::accent())),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Renders a non-selectable placeholder row for empty groups.
fn render_empty_group_placeholder_row() -> Row<'static> {
    let cells = vec![
        Cell::from(GROUP_EMPTY_PLACEHOLDER)
            .style(Style::default().fg(style::palette::text_subtle())),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Renders one session row.
fn render_session_row(
    session: &Session,
    tree_prefix: SessionTreePrefix,
    title_column_width: usize,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> Row<'static> {
    let status = session.status;
    let title_spans = render_session_title(
        session,
        title_column_width.saturating_sub(tree_prefix.width()),
    );
    let mut title_line_spans = Vec::new();
    if !tree_prefix.label().is_empty() {
        title_line_spans.push(Span::styled(tree_prefix.label(), tree_prefix_style()));
    }
    title_line_spans.extend(title_spans);
    let timer_label = if session.has_in_progress_timer() {
        format_duration_compact(session.in_progress_duration_seconds(wall_clock_unix_seconds))
    } else {
        String::new()
    };
    let forge_indicator = session.forge_indicator();
    let status_cell = if forge_indicator.is_empty() {
        Cell::from(format!("{status}")).style(Style::default().fg(style::status_color(status)))
    } else {
        let review_state = session.review_request.as_ref().map(|rr| rr.summary.state);
        let indicator_color = style::forge_indicator_color(review_state);

        Cell::from(Line::from(vec![
            Span::styled(
                format!("{status} "),
                Style::default().fg(style::status_color(status)),
            ),
            Span::styled(forge_indicator, Style::default().fg(indicator_color)),
        ]))
    };
    let cells = vec![
        Cell::from(Line::from(title_line_spans)),
        Cell::from(session_model_and_reasoning_level_line(
            session,
            default_reasoning_level,
        )),
        status_cell,
        Cell::from(timer_label),
    ];

    Row::new(cells)
        .style(Style::default().fg(style::palette::text()))
        .height(1)
}

/// Returns the tree connector style with contrast on highlighted rows.
fn tree_prefix_style() -> Style {
    Style::default().fg(style::palette::text_muted())
}

/// Calculates the width of the model column from known session values.
pub(crate) fn model_column_width(
    sessions: &[Session],
    default_reasoning_level: ReasoningLevel,
) -> Constraint {
    text_column_width(
        "Model",
        sessions
            .iter()
            .map(|session| session_model_and_reasoning_level(session, default_reasoning_level)),
    )
}

/// Formats model name together with the effective reasoning level label.
fn session_model_and_reasoning_level(
    session: &Session,
    default_reasoning_level: ReasoningLevel,
) -> String {
    let reasoning_level = session.effective_reasoning_level(default_reasoning_level);

    format!("{} [{}]", session.model.as_str(), reasoning_level.as_str())
}

/// Builds a styled model column cell where the reasoning label is colorized.
fn session_model_and_reasoning_level_line(
    session: &Session,
    default_reasoning_level: ReasoningLevel,
) -> Line<'static> {
    let reasoning_level = session.effective_reasoning_level(default_reasoning_level);
    let color = reasoning_level_color(reasoning_level);

    Line::from(vec![
        Span::raw(session.model.as_str()),
        Span::raw(" ["),
        Span::styled(reasoning_level.as_str(), Style::default().fg(color)),
        Span::raw("]"),
    ])
}

/// Returns the palette color for one reasoning effort level.
fn reasoning_level_color(reasoning_level: ReasoningLevel) -> Color {
    match reasoning_level {
        ReasoningLevel::Low => style::palette::success(),
        ReasoningLevel::Medium => style::palette::warning(),
        ReasoningLevel::High => style::palette::warning_soft(),
        ReasoningLevel::XHigh => style::palette::danger(),
    }
}

/// Calculates the width of the status column from every supported session
/// status label plus the widest possible forge indicator suffix.
fn status_column_width(sessions: &[Session]) -> Constraint {
    let static_width = text_column_width(
        "Status",
        Status::ALL.iter().map(std::string::ToString::to_string),
    );

    let session_width = text_column_width(
        "Status",
        sessions.iter().map(|session| {
            let forge_indicator = session.forge_indicator();
            if forge_indicator.is_empty() {
                session.status.to_string()
            } else {
                format!("{} {forge_indicator}", session.status)
            }
        }),
    );

    match (static_width, session_width) {
        (Constraint::Length(static_len), Constraint::Length(session_len)) => {
            Constraint::Length(static_len.max(session_len))
        }
        _ => static_width,
    }
}

/// Calculates the width of the timer column from known session durations.
fn timer_column_width(sessions: &[Session], wall_clock_unix_seconds: i64) -> Constraint {
    text_column_width(
        "Timer",
        sessions
            .iter()
            .filter(|session| session.has_in_progress_timer())
            .map(|session| {
                format_duration_compact(
                    session.in_progress_duration_seconds(wall_clock_unix_seconds),
                )
            }),
    )
}

/// Builds display-only title spans with a colored size marker prefix.
///
/// The prefix is derived from the row's current session size at render time
/// and is not written into the persisted session title.
fn render_session_title(session: &Session, title_column_width: usize) -> Vec<Span<'static>> {
    let mut title_spans = markdown::parse_inline_spans(
        &inline_text(session.display_title()),
        Style::default().fg(style::palette::text()),
    );
    title_spans.insert(
        0,
        Span::styled(
            format!("[{}] ", session.size),
            Style::default().fg(size_color(session.size)),
        ),
    );

    truncate_spans_with_ellipsis(title_spans, title_column_width)
}

/// Returns the palette color representing each session size bucket.
fn size_color(size: SessionSize) -> Color {
    match size {
        SessionSize::Xs => style::palette::success(),
        SessionSize::S => style::palette::success_soft(),
        SessionSize::M => style::palette::warning(),
        SessionSize::L => style::palette::warning_soft(),
        SessionSize::Xl => style::palette::danger_soft(),
        SessionSize::Xxl => style::palette::danger(),
    }
}

fn text_column_width<T>(header: &str, values: impl Iterator<Item = T>) -> Constraint
where
    T: AsRef<str>,
{
    let column_width = values
        .map(|value| value.as_ref().chars().count())
        .fold(header.chars().count(), usize::max);
    let column_width = u16::try_from(column_width).unwrap_or(u16::MAX);

    Constraint::Length(column_width)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::widgets::TableState;

    use super::*;
    use crate::agent::{AgentModel, ReasoningLevel};
    use crate::domain::session::SessionStats;
    use crate::domain::theme::ColorTheme;

    fn test_session(id: &str, status: Status) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: id.into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            parent_session_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: Some(id.to_string()),
            updated_at: 0,
            workflow_notice: None,
        }
    }

    /// Flattens a rendered test buffer into a plain string for assertions.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Returns the first rendered cell that starts the requested text.
    fn find_text_start_cell<'a>(
        buffer: &'a ratatui::buffer::Buffer,
        needle: &str,
    ) -> Option<&'a ratatui::buffer::Cell> {
        let width = usize::from(buffer.area.width.max(1));
        let needle_symbols = needle.chars().map(|character| character.to_string());
        let needle_symbols = needle_symbols.collect::<Vec<_>>();
        let content = buffer.content();

        for row_start in (0..content.len()).step_by(width) {
            let row_end = row_start + width.min(content.len().saturating_sub(row_start));
            let row = &content[row_start..row_end];

            for (index, window) in row.windows(needle_symbols.len()).enumerate() {
                let window_matches = window
                    .iter()
                    .zip(&needle_symbols)
                    .all(|(cell, symbol)| cell.symbol() == symbol);

                if window_matches {
                    return Some(&row[index]);
                }
            }
        }

        None
    }

    /// Counts cells matching a rendered symbol and the active palette border
    /// color.
    fn foreground_symbol_cell_count(buffer: &ratatui::buffer::Buffer, symbol: &str) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == symbol && cell.fg == style::palette::border())
            .count()
    }

    #[test]
    fn test_status_bar_fyi_rotates_between_session_list_messages() {
        // Arrange

        // Act
        let first_message = crate::ui::page::fyi::rotating_message(
            crate::ui::page::fyi::session_list_messages(),
            0,
        );
        let second_message = crate::ui::page::fyi::rotating_message(
            crate::ui::page::fyi::session_list_messages(),
            1,
        );
        let third_message = crate::ui::page::fyi::rotating_message(
            crate::ui::page::fyi::session_list_messages(),
            2,
        );
        let fourth_message = crate::ui::page::fyi::rotating_message(
            crate::ui::page::fyi::session_list_messages(),
            3,
        );
        let wrapped_message = crate::ui::page::fyi::rotating_message(
            crate::ui::page::fyi::session_list_messages(),
            4,
        );

        // Assert
        assert_eq!(
            first_message,
            Some("Press Enter to open the selected session.")
        );
        assert_eq!(
            second_message,
            Some("Go to Settings to specify commands that can open within a session."),
        );
        assert_eq!(
            third_message,
            Some("Run Agentty in tmux to use session open commands.")
        );
        assert_eq!(
            fourth_message,
            Some("Agentty refreshes PR statuses every minute.")
        );
        assert_eq!(
            wrapped_message,
            Some("Press Enter to open the selected session.")
        );
    }

    #[test]
    fn test_row_highlight_symbol_uses_background_only_selection() {
        // Arrange
        let highlight_symbol = ROW_HIGHLIGHT_SYMBOL;

        // Act
        let is_empty_symbol = highlight_symbol.is_empty();

        // Assert
        assert!(is_empty_symbol);
    }

    #[test]
    fn test_table_column_spacing_is_wider_for_readability() {
        // Arrange
        let expected_spacing = 2;

        // Act
        let spacing = TABLE_COLUMN_SPACING;

        // Assert
        assert_eq!(spacing, expected_spacing);
    }

    #[test]
    fn test_render_uses_palette_border_for_sessions_table() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let sessions = vec![test_session("new-1", Status::Draft)];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let border_cell_count = foreground_symbol_cell_count(terminal.backend().buffer(), "┌");
        assert_eq!(border_cell_count, 1);
    }

    #[test]
    fn test_preferred_initial_session_index_prefers_active_group_when_available() {
        // Arrange
        let sessions = vec![
            test_session("archive-1", Status::Done),
            test_session("active-1", Status::Review),
            test_session("merge-1", Status::Queued),
        ];

        // Act
        let selected_index = preferred_initial_session_index(&sessions);

        // Assert
        assert_eq!(selected_index, Some(1));
    }

    #[test]
    fn test_preferred_initial_session_index_falls_back_to_first_grouped_session() {
        // Arrange
        let sessions = vec![
            test_session("archive-1", Status::Done),
            test_session("merge-1", Status::Queued),
        ];

        // Act
        let selected_index = preferred_initial_session_index(&sessions);

        // Assert
        assert_eq!(selected_index, Some(1));
    }

    #[test]
    fn test_grouped_session_indexes_orders_selectable_sessions_without_headers() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("done-1", Status::Done),
            test_session("canceled-1", Status::Canceled),
            test_session("active-2", Status::Draft),
        ];

        // Act
        let indexes = grouped_session_indexes(&sessions);
        let ordered_ids = indexes
            .into_iter()
            .map(|index| sessions[index].id.clone())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            ordered_ids,
            vec![
                "queued-1".to_string(),
                "merge-1".to_string(),
                "active-1".to_string(),
                "active-2".to_string(),
                "done-1".to_string(),
                "canceled-1".to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_indexes_places_stacked_child_after_parent() {
        // Arrange
        let mut child_session = test_session("child-1", Status::Draft);
        child_session.parent_session_id = Some("parent-1".into());
        let sessions = vec![
            child_session,
            test_session("parent-1", Status::Review),
            test_session("sibling-1", Status::Review),
        ];

        // Act
        let indexes = grouped_session_indexes(&sessions);
        let ordered_ids = indexes
            .into_iter()
            .map(|index| sessions[index].id.clone())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            ordered_ids,
            vec![
                "parent-1".to_string(),
                "child-1".to_string(),
                "sibling-1".to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_orders_merge_queue_before_active_and_archive_sessions() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("done-1", Status::Done),
            test_session("canceled-1", Status::Canceled),
            test_session("active-2", Status::Draft),
        ];

        // Act
        let rows = grouped_session_rows(&sessions);
        let labels_and_ids = rows
            .iter()
            .map(|row| match row {
                SessionTableRow::GroupLabel(group) => group.label().to_string(),
                SessionTableRow::EmptyGroupPlaceholder => GROUP_EMPTY_PLACEHOLDER.to_string(),
                SessionTableRow::Session { session, .. } => session.id.to_string(),
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            labels_and_ids,
            vec![
                SessionGroup::MergeQueue.label().to_string(),
                "queued-1".to_string(),
                "merge-1".to_string(),
                SessionGroup::ActiveSessions.label().to_string(),
                "active-1".to_string(),
                "active-2".to_string(),
                SessionGroup::Archive.label().to_string(),
                "done-1".to_string(),
                "canceled-1".to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_includes_placeholder_for_groups_without_sessions() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("active-2", Status::InProgress),
        ];

        // Act
        let rows = grouped_session_rows(&sessions);
        let labels_and_ids = rows
            .iter()
            .map(|row| match row {
                SessionTableRow::GroupLabel(group) => group.label().to_string(),
                SessionTableRow::EmptyGroupPlaceholder => GROUP_EMPTY_PLACEHOLDER.to_string(),
                SessionTableRow::Session { session, .. } => session.id.to_string(),
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            labels_and_ids,
            vec![
                SessionGroup::MergeQueue.label().to_string(),
                GROUP_EMPTY_PLACEHOLDER.to_string(),
                SessionGroup::ActiveSessions.label().to_string(),
                "active-1".to_string(),
                "active-2".to_string(),
                SessionGroup::Archive.label().to_string(),
                GROUP_EMPTY_PLACEHOLDER.to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_marks_stacked_child_with_tree_prefix() {
        // Arrange
        let mut first_child_session = test_session("child-1", Status::Draft);
        first_child_session.parent_session_id = Some("parent-1".into());
        let mut second_child_session = test_session("child-2", Status::Draft);
        second_child_session.parent_session_id = Some("parent-1".into());
        let sessions = vec![
            first_child_session,
            test_session("parent-1", Status::Review),
            second_child_session,
        ];

        // Act
        let session_rows = grouped_session_rows(&sessions)
            .into_iter()
            .filter_map(|row| match row {
                SessionTableRow::Session {
                    session,
                    tree_prefix,
                    ..
                } => Some((session.id.to_string(), tree_prefix.label().to_string())),
                SessionTableRow::GroupLabel(_) | SessionTableRow::EmptyGroupPlaceholder => None,
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            session_rows,
            vec![
                ("parent-1".to_string(), String::new()),
                ("child-1".to_string(), TREE_BRANCH_MIDDLE.to_string()),
                ("child-2".to_string(), TREE_BRANCH_LAST.to_string()),
            ]
        );
    }

    #[test]
    fn test_selected_render_row_maps_original_selection_to_grouped_index() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("active-2", Status::Draft),
        ];
        let rows = grouped_session_rows(&sessions);
        let selected_session_id = selected_session_id(&sessions, Some(3));

        // Act
        let row_index = selected_render_row(&rows, selected_session_id);

        // Assert
        assert_eq!(row_index, Some(5));
    }

    #[test]
    fn test_prepare_grouped_table_state_resets_offset_and_sets_selected_group_row() {
        // Arrange
        let mut table_state = TableState::default();
        *table_state.offset_mut() = 24;
        table_state.select(Some(7));

        // Act
        prepare_grouped_table_state(&mut table_state, Some(3));

        // Assert
        assert_eq!(table_state.offset(), 0);
        assert_eq!(table_state.selected(), Some(3));
    }

    #[test]
    fn test_text_column_width_uses_longest_project_value() {
        // Arrange
        let expected_width =
            u16::try_from("very-long-project-name".chars().count()).unwrap_or(u16::MAX);
        let project_names = ["api", "very-long-project-name"];

        // Act
        let width = text_column_width("Project", project_names.into_iter());

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_model_column_width_uses_longest_model_value() {
        // Arrange
        let expected_width =
            u16::try_from("claude-sonnet-4-6 [low]".chars().count()).unwrap_or(u16::MAX);
        let mut default_session = test_session("active-1", Status::Review);
        default_session.model = AgentModel::ClaudeSonnet46;
        let mut medium_session = test_session("active-2", Status::Review);
        medium_session.model = AgentModel::Gpt55;
        medium_session.reasoning_level_override = Some(ReasoningLevel::Medium);
        let sessions = vec![default_session, medium_session];

        // Act
        let width = model_column_width(&sessions, ReasoningLevel::Low);

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_reasoning_level_color_matches_schema() {
        // Arrange
        let expected_colors = [
            (ReasoningLevel::Low, style::palette::success()),
            (ReasoningLevel::Medium, style::palette::warning()),
            (ReasoningLevel::High, style::palette::warning_soft()),
            (ReasoningLevel::XHigh, style::palette::danger()),
        ];

        // Act & Assert
        for (reasoning_level, expected_color) in expected_colors {
            assert_eq!(reasoning_level_color(reasoning_level), expected_color);
        }
    }

    #[test]
    fn test_render_session_row_colors_reasoning_level_within_model_column() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("session-1", Status::Review);
        session.model = AgentModel::Gpt55;
        session.reasoning_level_override = Some(ReasoningLevel::High);
        let sessions = vec![session];
        let expected_reasoning_color = reasoning_level_color(ReasoningLevel::High);

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::Low, 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let fallback_cell = &buffer.content()[0];
        let model_cell = find_text_start_cell(buffer, "gpt-5.5").unwrap_or(fallback_cell);
        let reasoning_cell = find_text_start_cell(buffer, "high").unwrap_or(fallback_cell);

        assert_eq!(model_cell.fg, style::palette::text());
        assert_eq!(reasoning_cell.fg, expected_reasoning_color);
    }

    #[test]
    fn test_render_session_row_shows_model_with_default_reasoning_level() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("session-1", Status::Review);
        session.model = AgentModel::Gpt55;
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::Medium, 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("gpt-5.5 [medium]"));
    }

    #[test]
    fn test_render_session_row_connects_stacked_child_to_parent() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(1));
        let mut parent_session = test_session("parent-1", Status::Review);
        parent_session.title = Some("Parent session".to_string());
        let mut child_session = test_session("child-1", Status::Draft);
        child_session.parent_session_id = Some("parent-1".into());
        child_session.title = Some("Child session".to_string());
        let sessions = vec![parent_session, child_session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Parent session"));
        assert!(text.contains("└ [XS] Child session"));
        let buffer = terminal.backend().buffer();
        let fallback_cell = &buffer.content()[0];
        let tree_cell = find_text_start_cell(buffer, "└").unwrap_or(fallback_cell);
        assert_eq!(tree_cell.fg, style::palette::text_muted());
        assert_eq!(tree_cell.bg, style::palette::surface());
        assert_ne!(tree_cell.fg, tree_cell.bg);
    }

    #[test]
    fn test_render_session_row_shows_model_with_override_reasoning_level() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("session-1", Status::Review);
        session.model = AgentModel::Gpt55;
        session.reasoning_level_override = Some(ReasoningLevel::High);
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::Low, 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("gpt-5.5 [high]"));
    }

    #[test]
    fn test_status_column_width_uses_longest_possible_status_label() {
        // Arrange
        let expected_width = u16::try_from("AgentReview".chars().count()).unwrap_or(u16::MAX);

        // Act
        let width = status_column_width(&[]);

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_status_column_width_expands_for_forge_indicator() {
        // Arrange
        use crate::domain::session::{
            ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
        };
        let mut session = test_session("session-1", Status::Review);
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "feat".to_string(),
                web_url: String::new(),
            },
        });
        let sessions = vec![session];
        // "Review ⊙ #42" is wider than "AgentReview"
        let expected_width = u16::try_from("Review ⊙ #42".chars().count()).unwrap_or(u16::MAX);

        // Act
        let width = status_column_width(&sessions);

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_timer_column_width_uses_longest_rendered_timer_label() {
        // Arrange
        let mut active_session = test_session("active-1", Status::InProgress);
        active_session.in_progress_started_at = Some(100);
        active_session.in_progress_total_seconds = 60;
        let mut archived_session = test_session("done-1", Status::Done);
        archived_session.in_progress_total_seconds = 3_661;
        let sessions = vec![active_session, archived_session];
        let expected_width = u16::try_from("1h1m1s".chars().count()).unwrap_or(u16::MAX);

        // Act
        let width = timer_column_width(&sessions, 160);

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_size_color_uses_expected_palette() {
        // Arrange
        let test_cases = [
            (SessionSize::Xs, style::palette::success()),
            (SessionSize::S, style::palette::success_soft()),
            (SessionSize::M, style::palette::warning()),
            (SessionSize::L, style::palette::warning_soft()),
            (SessionSize::Xl, style::palette::danger_soft()),
            (SessionSize::Xxl, style::palette::danger()),
        ];

        // Act & Assert
        for (size, expected_color) in test_cases {
            assert_eq!(size_color(size), expected_color);
        }
    }

    #[test]
    fn test_render_session_row_includes_size_prefix_in_title() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("new-1", Status::Draft);
        session.size = SessionSize::Xxl;
        session.title = Some("Update dependency graph".to_string());
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("[XXL] Update dependency graph"));
        assert_eq!(
            sessions[0].title.as_deref(),
            Some("Update dependency graph")
        );
        assert!(!text.contains("Size"));
    }

    #[test]
    fn test_session_list_help_line_includes_sync_for_non_empty_sessions() {
        // Arrange
        let session = test_session("session-1", Status::Review);

        // Act
        let help_text = session_list_help_line(Some(&session)).to_string();

        // Assert
        assert!(help_text.contains("s: sync"));
    }

    #[test]
    fn test_session_list_help_line_hides_cancel_for_regular_new_session() {
        // Arrange
        let session = test_session("session-1", Status::Draft);

        // Act
        let help_text = session_list_help_line(Some(&session)).to_string();

        // Assert
        assert!(!help_text.contains("c: cancel"));
    }

    #[test]
    fn test_session_list_help_line_includes_cancel_for_draft_session() {
        // Arrange
        let mut session = test_session("session-1", Status::Draft);
        session.is_draft = true;

        // Act
        let help_text = session_list_help_line(Some(&session)).to_string();

        // Assert
        assert!(help_text.contains("c: cancel"));
    }

    #[test]
    fn test_session_list_help_line_includes_open_for_canceled_session() {
        // Arrange
        let session = test_session("session-1", Status::Canceled);

        // Act
        let help_text = session_list_help_line(Some(&session)).to_string();

        // Assert
        assert!(help_text.contains("Enter: open session"));
    }

    #[test]
    fn test_render_shows_live_active_work_timer_in_grouped_session_row() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("active-1", Status::InProgress);
        session.in_progress_started_at = Some(100);
        session.in_progress_total_seconds = 60;
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 160)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("2m0s"));
    }

    #[test]
    fn test_render_shows_frozen_completed_timer_in_grouped_session_row() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("done-1", Status::Done);
        session.in_progress_total_seconds = 125;
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(
                    &sessions,
                    &mut table_state,
                    ReasoningLevel::default(),
                    9_999,
                )
                .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("2m5s"));
    }

    #[test]
    fn test_render_shows_full_agent_review_status_label() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let sessions = vec![test_session("review-1", Status::AgentReview)];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("AgentReview"));
    }

    #[test]
    fn test_render_flattens_multiline_session_titles_for_one_line_rows() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("draft-1", Status::Draft);
        session.title = Some("First draft\n\nSecond draft".to_string());
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("First draft Second draft"));
    }

    #[test]
    fn test_render_keeps_selected_new_status_text_visible() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let sessions = vec![test_session("new-1", Status::Draft)];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let fallback_cell = &buffer.content()[0];
        let title_cell = find_text_start_cell(buffer, "new-1").unwrap_or(fallback_cell);
        let new_cell = find_text_start_cell(buffer, "Draft").unwrap_or(fallback_cell);
        assert_ne!(title_cell.fg, title_cell.bg);
        assert_eq!(new_cell.fg, style::palette::text_muted());
        assert_eq!(new_cell.bg, style::palette::surface());
        assert_ne!(new_cell.fg, new_cell.bg);
    }
}
