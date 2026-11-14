use super::StatusBar;
use crate::app::{ProjectSyncPhase, ProjectSyncStatus, UpdateStatus};
use crate::ui::Component;

fn project_sync_status(phase: ProjectSyncPhase) -> ProjectSyncStatus {
    ProjectSyncStatus {
        context: crate::app::test_support::ProjectSyncContext {
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
fn test_status_bar_render_shows_running_project_sync() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let status_bar = StatusBar::new("v0.1.12".to_string())
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
    assert!(text.contains("Agentty v0.1.12 | Syncing agentty/main..."));
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
    let backend = ratatui::backend::TestBackend::new(80, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let status_bar =
        StatusBar::new("v0.1.12".to_string()).latest_available_version(Some("v0.1.13".to_string()));

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
    assert!(text.contains("v0.1.13 available — rerun your original install command"));
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
    assert!(!text.contains("rerun your original install command"));
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
fn test_status_bar_render_shows_manual_hint_on_update_failure() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 1);
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
    assert!(text.contains("rerun your original install command"));
    assert!(!text.contains("Updating to"));
}
