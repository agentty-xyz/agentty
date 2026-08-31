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
    latest_available_version: Option<String>,
    page_fyis: Option<&'a [&'static str]>,
    project_sync_status: Option<ProjectSyncStatus>,
    fyi_rotation_index: u64,
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
mod tests {
    use super::*;
    use crate::ui::page;

    fn project_sync_status(phase: ProjectSyncPhase) -> ProjectSyncStatus {
        ProjectSyncStatus {
            context: crate::app::ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id: 1,
                project_name: "agentty".to_string(),
            },
            phase,
        }
    }

    /// Flattens a test backend buffer into plain text for assertions.
    fn buffer_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_status_bar_new_stores_versions() {
        // Arrange
        let current_version = "v0.1.12".to_string();
        let latest_available_version = Some("v0.1.13".to_string());

        // Act
        let status_bar = StatusBar::new(current_version.clone())
            .latest_available_version(latest_available_version.clone());

        // Assert
        assert_eq!(status_bar.current_version, current_version);
        assert_eq!(
            status_bar.latest_available_version,
            latest_available_version
        );
    }

    #[test]
    fn test_status_bar_render_shows_current_version_without_update() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string());

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(text.contains("Agentty v0.1.12"));
        assert!(!text.contains("version available update"));
    }

    #[test]
    fn test_status_bar_render_shows_rotating_page_fyi_prefix() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string())
            .page_fyis(Some(page::fyi::session_list_messages()))
            .fyi_rotation_index(1);

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(
            text.contains("FYI: Sessions are grouped as merge queue, active work, then archive.")
        );
    }

    #[test]
    fn test_status_bar_render_prioritizes_running_project_sync_over_page_fyi() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string())
            .page_fyis(Some(page::fyi::session_list_messages()))
            .project_sync_status(Some(project_sync_status(ProjectSyncPhase::Running)));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(text.contains("Syncing agentty/main..."));
        assert!(!text.contains("FYI:"));
    }

    #[test]
    fn test_project_sync_text_formats_each_terminal_and_progress_phase() {
        // Arrange
        let cases = [
            (
                ProjectSyncPhase::ResolvingConflicts {
                    conflicted_file_count: 1,
                },
                "Resolving 1 conflict for agentty/main...",
            ),
            (
                ProjectSyncPhase::Complete {
                    deferred_session_count: 2,
                    pulled_commits: Some(3),
                    pushed_commits: Some(4),
                    resolved_conflict_count: 1,
                },
                "Synced agentty/main: 3 pulled, 4 pushed, 1 conflict resolved, 2 sessions need \
                 attention",
            ),
            (
                ProjectSyncPhase::Complete {
                    deferred_session_count: 0,
                    pulled_commits: None,
                    pushed_commits: None,
                    resolved_conflict_count: 0,
                },
                "Synced agentty/main",
            ),
            (
                ProjectSyncPhase::Blocked {
                    message: "dirty checkout\n\ncommit or stash".to_string(),
                },
                "Sync blocked for agentty/main: dirty checkout commit or stash",
            ),
            (
                ProjectSyncPhase::Failed {
                    message: "network unavailable".to_string(),
                },
                "Sync failed for agentty/main: network unavailable",
            ),
        ];

        // Act
        let actual = cases
            .iter()
            .map(|(phase, _)| {
                StatusBar::new("v0.1.12".to_string())
                    .project_sync_status(Some(project_sync_status(phase.clone())))
                    .project_sync_text()
                    .map(|(text, _)| text)
                    .expect("sync text should exist")
            })
            .collect::<Vec<_>>();

        // Assert
        let expected = cases
            .iter()
            .map(|(_, expected)| (*expected).to_string())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_status_bar_render_shows_update_notice_when_available() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string())
            .latest_available_version(Some("v0.1.13".to_string()));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(text.contains("Agentty v0.1.12"));
        assert!(text.contains("v0.1.13 version available update with npm i -g agentty@latest"));
    }

    #[test]
    fn test_status_bar_render_shows_updating_in_progress() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string())
            .latest_available_version(Some("v0.1.13".to_string()))
            .update_status(Some(UpdateStatus::InProgress {
                version: "v0.1.13".to_string(),
            }));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(text.contains("Updating to v0.1.13..."));
        assert!(!text.contains("npm i -g agentty@latest"));
    }

    #[test]
    fn test_status_bar_render_shows_update_complete() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar =
            StatusBar::new("v0.1.12".to_string()).update_status(Some(UpdateStatus::Complete {
                version: "v0.1.13".to_string(),
            }));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(text.contains("Updated to v0.1.13"));
        assert!(text.contains("restart to use new version"));
    }

    #[test]
    fn test_status_bar_render_keeps_update_state_visible_with_page_fyi() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(180, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string())
            .page_fyis(Some(page::fyi::session_chat_messages()))
            .fyi_rotation_index(0)
            .update_status(Some(UpdateStatus::Complete {
                version: "v0.1.13".to_string(),
            }));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(text.contains("Updated to v0.1.13"));
        assert!(
            text.contains("FYI: Queued replies run one by one after the active turn finishes.")
        );
    }

    #[test]
    fn test_status_bar_render_shows_manual_hint_on_update_failure() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string())
            .latest_available_version(Some("v0.1.13".to_string()))
            .update_status(Some(UpdateStatus::Failed {
                version: "v0.1.13".to_string(),
            }));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(&terminal);
        assert!(text.contains("npm i -g agentty@latest"));
        assert!(!text.contains("Updating to"));
    }
}
