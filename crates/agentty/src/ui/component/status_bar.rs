//! Top status bar showing the version, update and sync notices, the active
//! project name, and a help hint.

use std::borrow::Cow;

use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{ProjectSyncPhase, ProjectSyncStatus, UpdateStatus};
use crate::ui::{Component, help_format, style};

/// Minimum columns a shortened project name keeps before it is omitted.
const MIN_TRUNCATED_PROJECT_NAME_WIDTH: usize = 4;

/// Top status bar showing current version, update progress, availability, the
/// centered active project name, and a right-aligned `?: help` hint.
///
/// Space is assigned by priority on narrow terminals: the version and status
/// notices first, then the help hint, then the project name.
pub struct StatusBar {
    current_version: String,
    latest_available_version: Option<String>,
    project_name: Option<String>,
    project_sync_status: Option<ProjectSyncStatus>,
    /// Connected device count while remote control listens.
    remote_control_connections: Option<usize>,
    update_status: Option<UpdateStatus>,
}

impl StatusBar {
    /// Creates a status bar with the current version.
    pub fn new(current_version: String) -> Self {
        Self {
            current_version,
            latest_available_version: None,
            project_name: None,
            project_sync_status: None,
            remote_control_connections: None,
            update_status: None,
        }
    }

    /// Shows that remote control is listening, with `connections` devices
    /// connected; `None` hides the notice.
    #[must_use]
    pub fn remote_control_connections(mut self, connections: Option<usize>) -> Self {
        self.remote_control_connections = connections;

        self
    }

    /// Sets the latest available version for update notification.
    #[must_use]
    pub fn latest_available_version(mut self, version: Option<String>) -> Self {
        self.latest_available_version = version;
        self
    }

    /// Sets the active project name centered in the status bar; `None` leaves
    /// the center empty.
    #[must_use]
    pub fn project_name(mut self, project_name: Option<String>) -> Self {
        self.project_name = project_name;

        self
    }

    /// Sets the latest non-modal explicit project-sync state.
    #[must_use]
    pub(crate) fn project_sync_status(
        mut self,
        project_sync_status: Option<ProjectSyncStatus>,
    ) -> Self {
        self.project_sync_status = project_sync_status;

        self
    }

    /// Sets the background auto-update progress state.
    #[must_use]
    pub fn update_status(mut self, update_status: Option<UpdateStatus>) -> Self {
        self.update_status = update_status;
        self
    }

    /// Builds the update progress text and color when an update is active.
    ///
    /// Returns `None` for [`UpdateStatus::Failed`] so the caller falls back
    /// to the manual update hint.
    fn update_progress_text(&self) -> Option<(String, ratatui::style::Color)> {
        match &self.update_status {
            Some(UpdateStatus::InProgress { version }) => Some((
                format!("Updating to {version}..."),
                style::palette::accent(),
            )),
            Some(UpdateStatus::Complete { version }) => Some((
                format!("Updated to {version} — restart to use new version"),
                style::palette::success(),
            )),
            Some(UpdateStatus::Failed { .. }) | None => None,
        }
    }

    /// Builds the highest-priority project-sync status text and color.
    fn project_sync_text(&self) -> Option<(String, ratatui::style::Color)> {
        let status = self.project_sync_status.as_ref()?;
        let target = format!(
            "{}/{}",
            status.context.project_name, status.context.default_branch
        );
        let (text, color) = match &status.phase {
            ProjectSyncPhase::Running => (format!("Syncing {target}..."), style::palette::accent()),
            ProjectSyncPhase::ResolvingConflicts {
                conflicted_file_count,
            } => (
                format!(
                    "Resolving {} for {target}...",
                    count_label(*conflicted_file_count, "conflict")
                ),
                style::palette::warning(),
            ),
            ProjectSyncPhase::Complete {
                pulled_commits,
                pushed_commits,
                resolved_conflict_count,
            } => (
                sync_complete_text(
                    &target,
                    *pulled_commits,
                    *pushed_commits,
                    *resolved_conflict_count,
                ),
                style::palette::success(),
            ),
            ProjectSyncPhase::Blocked { message } => (
                format!("Sync blocked for {target}: {}", single_line(message)),
                style::palette::warning(),
            ),
            ProjectSyncPhase::Failed { message } => (
                format!("Sync failed for {target}: {}", single_line(message)),
                style::palette::danger(),
            ),
        };

        Some((text, color))
    }

    /// Renders the active project name centered across `area`, between left
    /// text of `left_width` and right text of `right_width` columns.
    ///
    /// The name is shortened to the free gap, or omitted when the gap is too
    /// narrow.
    fn render_project_name(
        &self,
        frame: &mut Frame,
        area: Rect,
        left_width: usize,
        right_width: usize,
    ) {
        let Some(project_name) = self.project_name.as_deref() else {
            return;
        };
        let project_name_width = Line::raw(project_name).width();
        let Some((label_offset, label_width)) = centered_label_area(
            usize::from(area.width),
            left_width,
            right_width,
            project_name_width,
        ) else {
            return;
        };

        // Shorten only when the rendered width overflows, so joined grapheme
        // clusters such as ZWJ emoji that fit stay intact.
        let label_text: Cow<'_, str> = if label_width < project_name_width {
            Cow::Owned(text_util::truncate_with_ellipsis(project_name, label_width))
        } else {
            Cow::Borrowed(project_name)
        };
        let label = Span::styled(
            label_text,
            Style::default()
                .fg(style::palette::accent_soft())
                .add_modifier(Modifier::BOLD),
        );

        frame.render_widget(
            Paragraph::new(Line::from(label)).style(Style::default().bg(style::palette::surface())),
            column_slice(area, label_offset, label_width),
        );
    }
}

impl Component for StatusBar {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let mut version_spans = vec![Span::styled(
            format!(" Agentty {}", self.current_version),
            Style::default()
                .fg(style::palette::accent())
                .add_modifier(Modifier::BOLD),
        )];

        if let Some((text, color)) = self.update_progress_text() {
            version_spans.push(Span::raw(" | "));
            version_spans.push(Span::styled(
                text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        } else if let Some(latest_available_version) = &self.latest_available_version {
            version_spans.push(Span::raw(" | "));
            version_spans.push(Span::styled(
                format!(
                    "{latest_available_version} available — rerun your original install command"
                ),
                Style::default()
                    .fg(style::palette::warning())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        if let Some((text, color)) = self.project_sync_text() {
            version_spans.push(Span::raw(" | "));
            version_spans.push(Span::styled(
                text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }

        if let Some(connections) = self.remote_control_connections {
            version_spans.push(Span::raw(" | "));
            version_spans.push(Span::styled(
                format!(
                    "Remote control on, {} connected",
                    count_label(connections, "device")
                ),
                Style::default()
                    .fg(style::palette::success())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let bar_style = Style::default()
            .bg(style::palette::surface())
            .fg(style::palette::text());
        let status_line = Line::from(version_spans);
        let help_hint = help_hint_line();
        let status_width = status_line.width();
        let required_width = status_width.saturating_add(help_hint.width());
        if required_width > usize::from(area.width) {
            frame.render_widget(Paragraph::new(status_line).style(bar_style), area);
            self.render_project_name(frame, area, status_width, 0);

            return;
        }

        let help_hint_width = help_hint.width();
        let [status_area, help_hint_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(u16::try_from(help_hint_width).unwrap_or(u16::MAX)),
        ])
        .areas(area);

        frame.render_widget(Paragraph::new(status_line).style(bar_style), status_area);
        frame.render_widget(Paragraph::new(help_hint).style(bar_style), help_hint_area);
        self.render_project_name(frame, area, status_width, help_hint_width);
    }
}

/// Builds the right-aligned `?: help` hint, shown whenever the status text
/// leaves room for it so update and sync notices are never truncated.
fn help_hint_line() -> Line<'static> {
    Line::from(vec![
        help_format::footer_key_span("?"),
        help_format::footer_muted_span(": help "),
    ])
}

/// Returns the column offset and width for a label centered across
/// `area_width` between a left text of `left_width` and a right text of
/// `right_width` columns.
///
/// One blank column separates the label from each side. When the centered
/// position would touch either side, the label is centered in the free gap
/// instead. A label wider than the gap is shortened to the gap width, and
/// `None` is returned when the label is empty or the gap is narrower than
/// [`MIN_TRUNCATED_PROJECT_NAME_WIDTH`] for a label that does not fit whole.
fn centered_label_area(
    area_width: usize,
    left_width: usize,
    right_width: usize,
    label_width: usize,
) -> Option<(usize, usize)> {
    let gap_start = left_width.saturating_add(1);
    let gap_end = area_width.saturating_sub(right_width.saturating_add(1));
    let gap_width = gap_end.saturating_sub(gap_start);
    let fitted_width = label_width.min(gap_width);
    if fitted_width == 0
        || (fitted_width < label_width && fitted_width < MIN_TRUNCATED_PROJECT_NAME_WIDTH)
    {
        return None;
    }

    let centered_start = (area_width - fitted_width) / 2;
    if centered_start >= gap_start && centered_start + fitted_width <= gap_end {
        return Some((centered_start, fitted_width));
    }

    Some((gap_start + (gap_width - fitted_width) / 2, fitted_width))
}

/// Returns the one-row slice of `area` starting `offset` columns from its
/// left edge and spanning `width` columns.
fn column_slice(area: Rect, offset: usize, width: usize) -> Rect {
    Rect {
        width: u16::try_from(width).unwrap_or(area.width),
        x: area.x + u16::try_from(offset).unwrap_or_default(),
        ..area
    }
}

/// Formats one singular or plural count label.
fn count_label(count: usize, singular: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };

    format!("{count} {singular}{suffix}")
}

/// Formats a compact successful-sync summary for the one-line status bar.
fn sync_complete_text(
    target: &str,
    pulled_commits: Option<u32>,
    pushed_commits: Option<u32>,
    resolved_conflict_count: usize,
) -> String {
    let mut summaries = Vec::new();
    if let Some(pulled_commits) = pulled_commits {
        summaries.push(format!("{pulled_commits} pulled"));
    }
    if let Some(pushed_commits) = pushed_commits {
        summaries.push(format!("{pushed_commits} pushed"));
    }
    if resolved_conflict_count > 0 {
        summaries.push(format!(
            "{} resolved",
            count_label(resolved_conflict_count, "conflict")
        ));
    }
    if summaries.is_empty() {
        return format!("Synced {target}");
    }

    format!("Synced {target}: {}", summaries.join(", "))
}

/// Flattens multi-paragraph workflow errors for the single-line status bar.
fn single_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[path = "status_bar_test.rs"]
mod tests;
