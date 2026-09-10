use std::borrow::Cow;
use std::collections::HashMap;

use ag_tui_text::text_util::{format_duration_compact, inline_text, truncate_spans_with_ellipsis};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::app::session_state::SessionGitStatus;
use crate::domain::agent::ReasoningLevel;
use crate::domain::session::{Session, SessionId, SessionSize, Status};
use crate::domain::session_order::{self, GroupedSessionRow, SessionGroup, SessionTreePosition};
use crate::presentation::help_action;
use crate::ui::input_layout::first_table_column_width;
use crate::ui::{Page, layout, markdown, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";

/// Horizontal spacing between table columns in the session list.
const TABLE_COLUMN_SPACING: u16 = 2;

/// Guidance rendered when the project has no sessions.
const EMPTY_SESSIONS_HINT: &str = "No sessions. Press 'a' to start one.";

/// Warning suffix appended to titles whose branches conflict with their base.
const MERGE_CONFLICT_LABEL: &str = " [merge conflict]";

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
    /// Latest session branch comparisons keyed by stable session id.
    session_git_statuses: Option<&'a HashMap<SessionId, SessionGitStatus>>,
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
            session_git_statuses: None,
            table_state,
            wall_clock_unix_seconds,
        }
    }

    /// Sets the latest session branch comparisons used for conflict labels.
    #[must_use]
    pub fn session_git_statuses(
        mut self,
        session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    ) -> Self {
        self.session_git_statuses = Some(session_git_statuses);

        self
    }
}

/// Render-ready cells and exact display widths derived once per session row.
struct PreparedSessionCells {
    model_cell: Line<'static>,
    model_width: usize,
    status_cell: Cell<'static>,
    status_width: usize,
    timer_label: String,
    timer_width: usize,
}

impl PreparedSessionCells {
    /// Builds the model, status, and timer values shared by layout and paint.
    fn new(
        session: &Session,
        _default_reasoning_level: ReasoningLevel,
        wall_clock_unix_seconds: i64,
    ) -> Self {
        let reasoning_level = session.effective_reasoning_level();
        let model_name = session.agent.model().as_str();
        let model_width = model_name
            .chars()
            .count()
            .saturating_add(reasoning_level.as_str().chars().count())
            .saturating_add(3);
        let model_cell = Line::from(vec![
            Span::raw(model_name),
            Span::raw(" ["),
            Span::styled(
                reasoning_level.as_str(),
                Style::default().fg(session_detail_color(
                    session,
                    reasoning_level_color(reasoning_level),
                )),
            ),
            Span::raw("]"),
        ]);

        let status = session.status;
        let status_label = session_list_status_label(session).into_owned();
        let forge_indicator = session.forge_indicator();
        let forge_indicator_width = if forge_indicator.is_empty() {
            0
        } else {
            forge_indicator.chars().count().saturating_add(1)
        };
        let status_width = status_label
            .chars()
            .count()
            .saturating_add(forge_indicator_width);
        let status_cell = if forge_indicator.is_empty() {
            Cell::from(status_label).style(
                Style::default().fg(session_detail_color(session, style::status_color(status))),
            )
        } else {
            let review_state = session.review_request.as_ref().map(|rr| rr.summary.state);
            let indicator_color =
                session_detail_color(session, style::forge_indicator_color(review_state));

            Cell::from(Line::from(vec![
                Span::styled(
                    format!("{status_label} "),
                    Style::default().fg(session_detail_color(session, style::status_color(status))),
                ),
                Span::styled(forge_indicator, Style::default().fg(indicator_color)),
            ]))
        };

        let timer_label = if session.has_in_progress_timer() {
            format_duration_compact(session.in_progress_duration_seconds(wall_clock_unix_seconds))
        } else {
            String::new()
        };
        let timer_width = timer_label.chars().count();

        Self {
            model_cell,
            model_width,
            status_cell,
            status_width,
            timer_label,
            timer_width,
        }
    }
}

/// Grouped table row carrying the session cells prepared for this frame.
enum PreparedSessionRow<'a> {
    GroupLabel {
        group: SessionGroup,
        session_count: usize,
    },
    Session {
        adds_group_spacing: bool,
        cells: PreparedSessionCells,
        has_merge_conflict: bool,
        session: &'a Session,
        tree_position: SessionTreePosition,
    },
}

impl<'a> PreparedSessionRow<'a> {
    /// Converts one grouped domain row into its render-ready representation.
    fn new(
        row: &GroupedSessionRow<'a>,
        following_rows: &[GroupedSessionRow<'a>],
        default_reasoning_level: ReasoningLevel,
        has_merge_conflict: bool,
        wall_clock_unix_seconds: i64,
    ) -> Self {
        match row {
            GroupedSessionRow::GroupLabel(group) => {
                let session_count = following_rows
                    .iter()
                    .take_while(|following_row| {
                        matches!(following_row, GroupedSessionRow::Session { .. })
                    })
                    .count();

                Self::GroupLabel {
                    group: *group,
                    session_count,
                }
            }
            GroupedSessionRow::Session {
                session,
                tree_position,
                ..
            } => Self::Session {
                adds_group_spacing: matches!(
                    following_rows.first(),
                    Some(GroupedSessionRow::GroupLabel(_))
                ),
                cells: PreparedSessionCells::new(
                    session,
                    default_reasoning_level,
                    wall_clock_unix_seconds,
                ),
                has_merge_conflict,
                session,
                tree_position: *tree_position,
            },
        }
    }
}

impl Page for SessionListPage<'_> {
    /// Renders the grouped session table directly below the tab header plus
    /// the list footer.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);

        let selected_style = Style::default().bg(style::palette::surface_selection());
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
        let table_rows = prepared_session_rows(
            self.sessions,
            self.default_reasoning_level,
            self.session_git_statuses,
            self.wall_clock_unix_seconds,
        );
        let column_constraints = [
            Constraint::Fill(1),
            model_column_width(&table_rows),
            status_column_width(&table_rows),
            timer_column_width(&table_rows),
        ];
        let title_column_width = first_table_column_width(
            block.inner(areas.main_area).width,
            &column_constraints,
            TABLE_COLUMN_SPACING,
            0,
        );
        let selected_session_id = selected_session_id(self.sessions, self.table_state.selected());
        let selected_row = selected_render_row(&table_rows, selected_session_id);
        let is_empty = table_rows.is_empty();
        let rows = table_rows
            .into_iter()
            .map(|table_row| render_table_row(table_row, title_column_width))
            .chain(is_empty.then(render_empty_sessions_hint_row));
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

    crate::ui::help_format::footer_line(&actions)
}

/// Prepares list table state for grouped row rendering.
///
/// The app stores selection as an index in the raw session slice, while the
/// table is rendered with extra group label rows. Resetting the offset before
/// selecting a grouped row avoids stale deep offsets hiding top group sections
/// after scrolling back up.
fn prepare_grouped_table_state(table_state: &mut TableState, selected_row: Option<usize>) {
    *table_state.offset_mut() = 0;
    table_state.select(selected_row);
}

/// Returns the display label for a session group.
fn session_group_label(group: SessionGroup) -> &'static str {
    match group {
        SessionGroup::MergeQueue => "MERGE QUEUE",
        SessionGroup::Active => "ACTIVE",
        SessionGroup::Archive => "ARCHIVE",
    }
}

/// Returns the indented tree marker for one grouped session row.
fn tree_position_label(tree_position: SessionTreePosition) -> String {
    match tree_position {
        SessionTreePosition::Root => String::new(),
        SessionTreePosition::Child { depth, is_last } => {
            let branch = if is_last {
                TREE_BRANCH_LAST
            } else {
                TREE_BRANCH_MIDDLE
            };

            format!("{}{branch}", "  ".repeat(depth.saturating_sub(1)))
        }
    }
}

/// Returns the display width consumed by a grouped session tree marker.
fn tree_position_width(tree_position: SessionTreePosition) -> usize {
    tree_position_label(tree_position).chars().count()
}

/// Resolves the selected session id from the original session ordering.
fn selected_session_id(sessions: &[Session], selected_index: Option<usize>) -> Option<&str> {
    selected_index
        .and_then(|index| sessions.get(index))
        .map(|session| session.id.as_str())
}

/// Maps selected session id to the grouped table row index.
fn selected_render_row(
    rows: &[PreparedSessionRow<'_>],
    selected_session_id: Option<&str>,
) -> Option<usize> {
    let selected_session_id = selected_session_id?;

    rows.iter().position(|row| match row {
        PreparedSessionRow::GroupLabel { .. } => false,
        PreparedSessionRow::Session { session, .. } => session.id == selected_session_id,
    })
}

/// Prepares every grouped row once so layout sizing and painting share values.
fn prepared_session_rows<'a>(
    sessions: &'a [Session],
    default_reasoning_level: ReasoningLevel,
    session_git_statuses: Option<&HashMap<SessionId, SessionGitStatus>>,
    wall_clock_unix_seconds: i64,
) -> Vec<PreparedSessionRow<'a>> {
    let grouped_rows = session_order::grouped_session_rows(sessions);

    grouped_rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            let following_rows = &grouped_rows[row_index + 1..];
            let has_merge_conflict = match row {
                GroupedSessionRow::GroupLabel(_) => false,
                GroupedSessionRow::Session { session, .. } => session_git_statuses
                    .and_then(|statuses| statuses.get(&session.id))
                    .and_then(|status| status.has_merge_conflict)
                    .unwrap_or(false),
            };

            PreparedSessionRow::new(
                row,
                following_rows,
                default_reasoning_level,
                has_merge_conflict,
                wall_clock_unix_seconds,
            )
        })
        .collect()
}

/// Converts one grouped row descriptor into a `ratatui` table row.
fn render_table_row(row: PreparedSessionRow<'_>, title_column_width: usize) -> Row<'static> {
    match row {
        PreparedSessionRow::GroupLabel {
            group,
            session_count,
        } => render_group_label_row(group, session_count),
        PreparedSessionRow::Session {
            adds_group_spacing,
            cells,
            has_merge_conflict,
            session,
            tree_position,
        } => render_session_row(
            session,
            tree_position,
            cells,
            title_column_width,
            adds_group_spacing,
            has_merge_conflict,
        ),
    }
}

/// Renders a non-selectable group label row.
fn render_group_label_row(group: SessionGroup, session_count: usize) -> Row<'static> {
    let cells = vec![
        Cell::from(format!(
            " {} —— {session_count}",
            session_group_label(group)
        ))
        .style(Style::default().fg(style::palette::text_muted())),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Renders guidance when no grouped sessions exist.
fn render_empty_sessions_hint_row() -> Row<'static> {
    let cells = vec![
        Cell::from(EMPTY_SESSIONS_HINT).style(Style::default().fg(style::palette::text_subtle())),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Renders one session row.
fn render_session_row(
    session: &Session,
    tree_position: SessionTreePosition,
    cells: PreparedSessionCells,
    title_column_width: usize,
    adds_group_spacing: bool,
    has_merge_conflict: bool,
) -> Row<'static> {
    let title_spans = render_session_title(
        session,
        title_column_width.saturating_sub(tree_position_width(tree_position)),
        has_merge_conflict,
    );
    let mut title_line_spans = Vec::new();
    let tree_label = tree_position_label(tree_position);
    if !tree_label.is_empty() {
        title_line_spans.push(Span::styled(tree_label, tree_prefix_style()));
    }
    title_line_spans.extend(title_spans);
    let cells = vec![
        Cell::from(Line::from(title_line_spans)),
        Cell::from(cells.model_cell),
        cells.status_cell,
        Cell::from(cells.timer_label),
    ];

    Row::new(cells)
        .style(session_row_style(session))
        .height(1)
        .bottom_margin(u16::from(adds_group_spacing))
}

/// Returns the base text style for active or archived session rows.
fn session_row_style(session: &Session) -> Style {
    let text_color = if is_archived_session(session) {
        style::palette::text_muted()
    } else {
        style::palette::text()
    };

    Style::default().fg(text_color)
}

/// Returns whether a session belongs to the visually subdued archive group.
fn is_archived_session(session: &Session) -> bool {
    matches!(session.status, Status::Done | Status::Canceled)
}

/// Subdues semantic detail colors when their session is archived.
fn session_detail_color(session: &Session, active_color: Color) -> Color {
    if is_archived_session(session) {
        style::palette::text_muted()
    } else {
        active_color
    }
}

/// Returns the one-line status shown in the Sessions table.
///
/// Orchestrator progress also carries the multiline campaign-board snapshot.
/// Only its phase line is visible in a one-row table cell, so using the same
/// line for width calculation prevents hidden board details from consuming the
/// session-title column.
fn session_list_status_label(session: &Session) -> Cow<'_, str> {
    session
        .orchestration_progress
        .as_deref()
        .and_then(|progress| progress.lines().next())
        .filter(|label| !label.is_empty())
        .map_or_else(|| Cow::Owned(session.status.to_string()), Cow::Borrowed)
}

/// Returns the tree connector style with contrast on highlighted rows.
fn tree_prefix_style() -> Style {
    Style::default().fg(style::palette::text_muted())
}

/// Calculates the model width from the same prepared cells used for paint.
fn model_column_width(rows: &[PreparedSessionRow<'_>]) -> Constraint {
    column_width(
        "Model",
        rows.iter().filter_map(|row| match row {
            PreparedSessionRow::GroupLabel { .. } => None,
            PreparedSessionRow::Session { cells, .. } => Some(cells.model_width),
        }),
    )
}

/// Returns the palette color for one reasoning effort level.
fn reasoning_level_color(reasoning_level: ReasoningLevel) -> Color {
    match reasoning_level {
        ReasoningLevel::Low => style::palette::success(),
        ReasoningLevel::Medium => style::palette::warning(),
        ReasoningLevel::High => style::palette::warning_soft(),
        ReasoningLevel::XHigh | ReasoningLevel::Max => style::palette::danger(),
    }
}

/// Calculates the status width from static labels and prepared painted cells.
fn status_column_width(rows: &[PreparedSessionRow<'_>]) -> Constraint {
    let static_widths = Status::ALL
        .iter()
        .map(|status| status.to_string().chars().count());
    let session_widths = rows.iter().filter_map(|row| match row {
        PreparedSessionRow::GroupLabel { .. } => None,
        PreparedSessionRow::Session { cells, .. } => Some(cells.status_width),
    });

    column_width("Status", static_widths.chain(session_widths))
}

/// Calculates the timer width from the same prepared labels used for paint.
fn timer_column_width(rows: &[PreparedSessionRow<'_>]) -> Constraint {
    column_width(
        "Timer",
        rows.iter().filter_map(|row| match row {
            PreparedSessionRow::GroupLabel { .. } => None,
            PreparedSessionRow::Session { cells, .. } => Some(cells.timer_width),
        }),
    )
}

/// Builds display-only title spans with a colored size marker prefix.
///
/// The prefix is derived from the row's current session size at render time
/// and is not written into the persisted session title.
fn render_session_title(
    session: &Session,
    title_column_width: usize,
    has_merge_conflict: bool,
) -> Vec<Span<'static>> {
    let mut title_spans = markdown::parse_inline_spans(
        &inline_text(session.display_title()),
        session_row_style(session),
    );
    title_spans.insert(
        0,
        Span::styled(
            format!("[{}] ", session.size),
            Style::default().fg(session_detail_color(session, size_color(session.size))),
        ),
    );

    if !has_merge_conflict {
        return truncate_spans_with_ellipsis(title_spans, title_column_width);
    }

    let label_width = MERGE_CONFLICT_LABEL.chars().count();
    let mut title_spans =
        truncate_spans_with_ellipsis(title_spans, title_column_width.saturating_sub(label_width));
    title_spans.push(Span::styled(
        MERGE_CONFLICT_LABEL,
        Style::default().fg(style::palette::danger()),
    ));

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

/// Converts the maximum prepared display width into a table constraint.
fn column_width(header: &str, widths: impl Iterator<Item = usize>) -> Constraint {
    let column_width = widths.fold(header.chars().count(), usize::max);
    let column_width = u16::try_from(column_width).unwrap_or(u16::MAX);

    Constraint::Length(column_width)
}

#[cfg(test)]
#[path = "session_list_test.rs"]
mod tests;
