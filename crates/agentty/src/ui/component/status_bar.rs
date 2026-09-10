use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{ProjectSyncPhase, ProjectSyncStatus, UpdateStatus};
use crate::ui::page::fyi;
use crate::ui::{Component, style};

/// Top status bar showing current version, update progress, and availability.
pub struct StatusBar<'a> {
    current_version: String,
    fyi_rotation_index: u64,
    latest_available_version: Option<String>,
    page_fyis: Option<&'a [&'static str]>,
    project_sync_status: Option<ProjectSyncStatus>,
    update_status: Option<UpdateStatus>,
}

impl<'a> StatusBar<'a> {
    /// Creates a status bar with the current version.
    pub fn new(current_version: String) -> Self {
        Self {
            current_version,
            latest_available_version: None,
            page_fyis: None,
            project_sync_status: None,
            fyi_rotation_index: 0,
            update_status: None,
        }
    }

    /// Sets the latest available version for update notification.
    #[must_use]
    pub fn latest_available_version(mut self, version: Option<String>) -> Self {
        self.latest_available_version = version;
        self
    }

    /// Sets the page-scoped FYI messages available for rotation in the top
    /// status bar.
    #[must_use]
    pub fn page_fyis(mut self, page_fyis: Option<&'a [&'static str]>) -> Self {
        self.page_fyis = page_fyis;
        self
    }

    /// Sets the absolute one-minute FYI rotation slot used to select the
    /// visible page-scoped message.
    #[must_use]
    pub fn fyi_rotation_index(mut self, fyi_rotation_index: u64) -> Self {
        self.fyi_rotation_index = fyi_rotation_index;
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

    /// Returns the currently visible page-scoped FYI message, when available.
    fn page_fyi_text(&self) -> Option<&str> {
        let page_fyis = self.page_fyis?;

        fyi::rotating_message(page_fyis, self.fyi_rotation_index)
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
                deferred_session_count,
                pulled_commits,
                pushed_commits,
                resolved_conflict_count,
            } => (
                sync_complete_text(
                    &target,
                    *pulled_commits,
                    *pushed_commits,
                    *resolved_conflict_count,
                    *deferred_session_count,
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
}

impl Component for StatusBar<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
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
                    "{latest_available_version} version available update with npm i -g \
                     agentty@latest"
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
        } else if let Some(page_fyi_text) = self.page_fyi_text() {
            version_spans.push(Span::raw(" | "));
            version_spans.push(Span::styled(
                format!("FYI: {page_fyi_text}"),
                Style::default().fg(style::palette::text_muted()),
            ));
        }

        let status_bar = Paragraph::new(Line::from(version_spans)).style(
            Style::default()
                .bg(style::palette::surface())
                .fg(style::palette::text()),
        );
        f.render_widget(status_bar, area);
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
    deferred_session_count: usize,
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
    if deferred_session_count > 0 {
        summaries.push(format!(
            "{} need attention",
            count_label(deferred_session_count, "session")
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
