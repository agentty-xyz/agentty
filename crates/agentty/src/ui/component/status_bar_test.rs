//! Status bar version, update, project sync, project name, and help-hint
//! rendering unit tests.

use ratatui::style::Modifier;

use super::{StatusBar, centered_label_area};
use crate::app::{ProjectSyncPhase, ProjectSyncStatus, UpdateStatus};
use crate::domain::theme::ColorTheme;
use crate::presentation::remote_control::RemoteControlNotice;
use crate::ui::{Component, style};

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

/// Renders a status bar with `notice` and returns its text.
fn remote_control_status_text(notice: RemoteControlNotice) -> String {
    let backend = ratatui::backend::TestBackend::new(120, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let status_bar = StatusBar::new("v0.1.12".to_string()).remote_control(Some(notice));
    terminal
        .draw(|frame| {
            let area = frame.area();
            Component::render(&status_bar, frame, area);
        })
        .expect("failed to draw");

    buffer_text(&terminal)
}

#[test]
fn test_status_bar_render_shows_remote_control_connections() {
    // Arrange
    let notice = RemoteControlNotice {
        connected_devices: 1,
        pairing_requests: 0,
    };

    // Act
    let text = remote_control_status_text(notice);

    // Assert
    assert!(text.contains("Agentty v0.1.12 | Remote control on, 1 device connected"));
}

#[test]
fn test_status_bar_render_asks_to_approve_pending_pairings() {
    // Arrange
    let notice = RemoteControlNotice {
        connected_devices: 1,
        pairing_requests: 2,
    };

    // Act
    let text = remote_control_status_text(notice);

    // Assert
    assert!(text.contains("Remote control: approve 2 pairing requests in /remote-control"));
}

#[test]
fn test_status_bar_render_right_aligns_help_hint_with_accent_key() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
    let backend = ratatui::backend::TestBackend::new(40, 1);
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
    let buffer = terminal.backend().buffer();
    assert!(text.starts_with(" Agentty v0.1.12"));
    assert!(text.ends_with("?: help "));
    assert_eq!(buffer[(32, 0)].symbol(), "?");
    assert_eq!(buffer[(32, 0)].fg, style::palette::accent());
    assert_eq!(buffer[(39, 0)].bg, style::palette::surface());
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
                pulled_commits: Some(3),
                pushed_commits: Some(4),
                resolved_conflict_count: 1,
            },
            "Synced agentty/main: 3 pulled, 4 pushed, 1 conflict resolved",
        ),
        (
            ProjectSyncPhase::Complete {
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
    assert!(!text.contains("?: help"));
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

/// Renders `status_bar` into a one-row terminal of `width` columns and
/// returns the terminal for buffer inspection.
fn rendered_status_bar(
    status_bar: &StatusBar,
    width: u16,
) -> ratatui::Terminal<ratatui::backend::TestBackend> {
    let backend = ratatui::backend::TestBackend::new(width, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    terminal
        .draw(|frame| Component::render(status_bar, frame, frame.area()))
        .expect("failed to draw");

    terminal
}

#[test]
fn test_status_bar_render_centers_project_name_between_version_and_help_hint() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
    let status_bar =
        StatusBar::new("v0.1.12".to_string()).project_name(Some("agentty".to_string()));

    // Act
    let terminal = rendered_status_bar(&status_bar, 60);

    // Assert
    let text = buffer_text(&terminal);
    let buffer = terminal.backend().buffer();
    assert!(text.starts_with(" Agentty v0.1.12 "));
    assert!(text.ends_with("?: help "));
    assert_eq!(text.find("agentty"), Some(26));
    assert_eq!(buffer[(26, 0)].fg, style::palette::accent_soft());
    assert_eq!(buffer[(26, 0)].bg, style::palette::surface());
    assert!(buffer[(26, 0)].modifier.contains(Modifier::BOLD));
}

#[test]
fn test_status_bar_render_shortens_then_omits_project_name_before_help_hint() {
    // Arrange
    let status_bar = StatusBar::new("v0.1.12".to_string())
        .project_name(Some("my-long-project-name".to_string()));

    // Act
    let shortened = buffer_text(&rendered_status_bar(&status_bar, 40));
    let omitted = buffer_text(&rendered_status_bar(&status_bar, 28));

    // Assert
    assert_eq!(shortened, " Agentty v0.1.12 my-long-pro... ?: help ");
    assert_eq!(omitted, " Agentty v0.1.12    ?: help ");
}

#[test]
fn test_status_bar_render_keeps_project_name_when_help_hint_does_not_fit() {
    // Arrange
    let status_bar = StatusBar::new("v0.1.12".to_string()).project_name(Some("ab".to_string()));

    // Act
    let text = buffer_text(&rendered_status_bar(&status_bar, 20));

    // Assert
    assert_eq!(text, " Agentty v0.1.12 ab ");
}

#[test]
fn test_status_bar_render_keeps_joined_emoji_project_name_that_fits() {
    // Arrange
    let status_bar = StatusBar::new("v0.1.12".to_string()).project_name(Some("👩‍💻".to_string()));

    // Act
    let text = buffer_text(&rendered_status_bar(&status_bar, 40));

    // Assert
    assert!(text.contains("👩‍💻"), "status bar was {text:?}");
    assert!(!text.contains(".."), "status bar was {text:?}");
}

#[test]
fn test_centered_label_area_centers_label_across_full_width() {
    // Arrange
    let (area_width, left_width, right_width, label_width) = (60, 16, 8, 7);

    // Act
    let label_area = centered_label_area(area_width, left_width, right_width, label_width);

    // Assert
    assert_eq!(label_area, Some((26, 7)));
}

#[test]
fn test_centered_label_area_moves_label_into_free_gap_near_long_left_text() {
    // Arrange
    let (area_width, left_width, right_width, label_width) = (60, 30, 8, 10);

    // Act
    let label_area = centered_label_area(area_width, left_width, right_width, label_width);

    // Assert
    assert_eq!(label_area, Some((36, 10)));
}

#[test]
fn test_centered_label_area_shortens_label_to_free_gap() {
    // Arrange, Act
    let shortened = centered_label_area(40, 16, 8, 20);
    let minimum = centered_label_area(30, 16, 8, 10);
    let whole_short_label = centered_label_area(29, 16, 8, 2);

    // Assert
    assert_eq!(shortened, Some((17, 14)));
    assert_eq!(minimum, Some((17, 4)));
    assert_eq!(whole_short_label, Some((17, 2)));
}

#[test]
fn test_centered_label_area_omits_label_without_room() {
    // Arrange, Act
    let below_minimum = centered_label_area(29, 16, 8, 10);
    let no_gap = centered_label_area(10, 16, 0, 5);
    let empty_label = centered_label_area(60, 16, 8, 0);

    // Assert
    assert_eq!(below_minimum, None);
    assert_eq!(no_gap, None);
    assert_eq!(empty_label, None);
}
