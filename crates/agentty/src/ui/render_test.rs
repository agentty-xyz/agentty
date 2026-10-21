use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{FooterBarRenderContext, ProjectFooterContext, render_footer_bar};
use crate::app::Tab;
use crate::app::session::session_branch;
use crate::app::session_state::SessionGitStatus;
use crate::domain::session::{Session, SessionId};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode, DiffFocus, DiffLineComments};
use crate::test_support::SessionFixtureBuilder;

/// Builds one deterministic session fixture for footer render tests.
fn session_fixture(session_id: &str, folder: &str) -> Session {
    SessionFixtureBuilder::new()
        .id(session_id)
        .folder(PathBuf::from(folder))
        .prompt("prompt")
        .title(Some("title".to_string()))
        .build()
}

/// Flattens one test backend buffer into plain text for assertions.
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

/// Builds a deterministic session-id lookup map for footer render tests.
fn session_index_by_id(sessions: &[Session]) -> HashMap<SessionId, usize> {
    sessions
        .iter()
        .enumerate()
        .map(|(session_index, session)| (session.id.clone(), session_index))
        .collect()
}

#[test]
fn render_footer_bar_prefers_session_folder_and_branch_for_session_modes() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "session-view-mode";
    let session = session_fixture(session_id, "/tmp/session-view-folder");
    let modes = [
        AppMode::View {
            session_id: session_id.into(),
            scroll_offset: None,
        },
        AppMode::DiffLoading {
            fallback_view_scroll_offset: None,
            request_id: 1,
            restore: None,
            session_id: session_id.into(),
            sidebar_focus: crate::presentation::app_mode::DiffSidebarFocus::Files,
        },
        AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: crate::presentation::app_mode::DiffPreview::default(),
            review_comments: Some(crate::presentation::app_mode::DiffReviewComments::loading(
                1,
            )),
            restore: None,
            scroll_cache: None,
            session_id: session_id.into(),
            scroll_offset: 0,
        },
    ];
    let sessions = vec![session];
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names = HashMap::new();

    // Act
    let rendered_texts = modes
        .iter()
        .map(|mode| {
            terminal
                .draw(|frame| {
                    render_footer_bar(
                        frame,
                        frame.area(),
                        FooterBarRenderContext {
                            current_tab: Tab::Sessions,
                            mode,
                            project: ProjectFooterContext {
                                git_branch: Some("main"),
                                git_status: Some((2, 1)),
                                git_upstream_ref: Some("origin/main"),
                                working_dir: Path::new("/tmp/workspace-root"),
                            },
                            session_branch_names: &session_branch_names,
                            session_git_statuses: &HashMap::new(),
                            session_index_by_id: &session_index_by_id,
                            sessions: &sessions,
                        },
                    );
                })
                .expect("failed to draw");

            buffer_text(terminal.backend().buffer())
        })
        .collect::<Vec<_>>();

    // Assert
    for text in rendered_texts {
        assert!(text.contains("/tmp/session-view-folder"));
        assert!(text.contains(&session_branch(session_id)));
    }
}

#[test]
fn render_footer_bar_prefers_session_upstream_reference_for_view_mode() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "upstream";
    let mut session = session_fixture(session_id, "/tmp/session-view-folder");
    session.published_upstream_ref = Some("origin/wt/upstream".to_string());
    let mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };
    let sessions = vec![session];
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names = HashMap::new();

    // Act
    terminal
        .draw(|frame| {
            render_footer_bar(
                frame,
                frame.area(),
                FooterBarRenderContext {
                    current_tab: Tab::Sessions,
                    mode: &mode,
                    project: ProjectFooterContext {
                        git_branch: Some("main"),
                        git_status: Some((2, 1)),
                        git_upstream_ref: Some("origin/main"),
                        working_dir: Path::new("/tmp/workspace-root"),
                    },
                    session_branch_names: &session_branch_names,
                    session_git_statuses: &HashMap::new(),
                    session_index_by_id: &session_index_by_id,
                    sessions: &sessions,
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("wt/upstream -> origin/wt/upstream"));
}

#[test]
fn render_footer_bar_prefers_session_branch_for_view_info_popup() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "popup";
    let mut session = session_fixture(session_id, "/tmp/session-popup-folder");
    session.published_upstream_ref = Some("origin/wt/popup".to_string());
    let mode = AppMode::ViewInfoPopup {
        is_loading: false,
        loading_label: "Publishing branch".to_string(),
        message: "Published".to_string(),
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: session_id.into(),
        },
        title: "Branch pushed".to_string(),
    };
    let sessions = vec![session];
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names = HashMap::new();

    // Act
    terminal
        .draw(|frame| {
            render_footer_bar(
                frame,
                frame.area(),
                FooterBarRenderContext {
                    current_tab: Tab::Sessions,
                    mode: &mode,
                    project: ProjectFooterContext {
                        git_branch: Some("main"),
                        git_status: Some((2, 1)),
                        git_upstream_ref: Some("origin/main"),
                        working_dir: Path::new("/tmp/workspace-root"),
                    },
                    session_branch_names: &session_branch_names,
                    session_git_statuses: &HashMap::new(),
                    session_index_by_id: &session_index_by_id,
                    sessions: &sessions,
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("wt/popup -> origin/wt/popup"));
    assert!(!text.contains("main -> origin/main"));
}

#[test]
fn render_footer_bar_uses_working_directory_when_mode_has_no_session() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mode = AppMode::List;
    let sessions = Vec::new();
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names = HashMap::new();
    let working_dir = Path::new("/tmp/current-workspace");
    let git_branch = Some("feature/test-render");
    let git_status = Some((0, 0));

    // Act
    terminal
        .draw(|frame| {
            render_footer_bar(
                frame,
                frame.area(),
                FooterBarRenderContext {
                    current_tab: Tab::Sessions,
                    mode: &mode,
                    project: ProjectFooterContext {
                        git_branch,
                        git_status,
                        git_upstream_ref: Some("origin/feature/test-render"),
                        working_dir,
                    },
                    session_branch_names: &session_branch_names,
                    session_git_statuses: &HashMap::new(),
                    session_index_by_id: &session_index_by_id,
                    sessions: &sessions,
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("/tmp/current-workspace"));
    assert!(text.contains("feature/test-render -> origin/feature/test-render"));
}

#[test]
fn render_footer_bar_hides_project_context_on_projects_tab() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mode = AppMode::List;
    let sessions = Vec::new();
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names = HashMap::new();
    let working_dir = Path::new("/tmp/current-workspace");

    // Act
    terminal
        .draw(|frame| {
            render_footer_bar(
                frame,
                frame.area(),
                FooterBarRenderContext {
                    current_tab: Tab::Projects,
                    mode: &mode,
                    project: ProjectFooterContext {
                        git_branch: Some("feature/test-render"),
                        git_status: Some((0, 0)),
                        git_upstream_ref: Some("origin/feature/test-render"),
                        working_dir,
                    },
                    session_branch_names: &session_branch_names,
                    session_git_statuses: &HashMap::new(),
                    session_index_by_id: &session_index_by_id,
                    sessions: &sessions,
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert_eq!(text.trim(), "");
    assert!(!text.contains("/tmp/current-workspace"));
    assert!(!text.contains("feature/test-render"));
}

#[test]
fn render_footer_bar_uses_session_git_status_when_available() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "session-status";
    let mut session = session_fixture(session_id, "/tmp/session-status-folder");
    session.published_upstream_ref = Some("origin/wt/session-status".to_string());
    let mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };
    let sessions = vec![session];
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names: HashMap<SessionId, String> = HashMap::new();
    let session_git_statuses: HashMap<SessionId, SessionGitStatus> = HashMap::from([(
        session_id.to_string().into(),
        SessionGitStatus {
            base_status: Some((3, 2)),
            has_merge_conflict: Some(true),
            remote_status: Some((1, 4)),
        },
    )]);

    // Act
    terminal
        .draw(|frame| {
            render_footer_bar(
                frame,
                frame.area(),
                FooterBarRenderContext {
                    current_tab: Tab::Sessions,
                    mode: &mode,
                    project: ProjectFooterContext {
                        git_branch: Some("main"),
                        git_status: Some((0, 0)),
                        git_upstream_ref: Some("origin/main"),
                        working_dir: Path::new("/tmp/workspace-root"),
                    },
                    session_branch_names: &session_branch_names,
                    session_git_statuses: &session_git_statuses,
                    session_index_by_id: &session_index_by_id,
                    sessions: &sessions,
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("↓2 ↑3 main"));
    assert!(text.contains("↓4 ↑1 wt/session- -> origin/wt/session-status"));
    assert!(!text.contains("↓0"));
}

#[test]
fn render_footer_bar_uses_session_git_status_without_published_upstream() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "session-unpublished-status";
    let session = session_fixture(session_id, "/tmp/session-unpublished-status-folder");
    let mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };
    let sessions = vec![session];
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names: HashMap<SessionId, String> = HashMap::new();
    let session_git_statuses: HashMap<SessionId, SessionGitStatus> = HashMap::from([(
        session_id.to_string().into(),
        SessionGitStatus {
            base_status: Some((5, 1)),
            has_merge_conflict: Some(false),
            remote_status: None,
        },
    )]);

    // Act
    terminal
        .draw(|frame| {
            render_footer_bar(
                frame,
                frame.area(),
                FooterBarRenderContext {
                    current_tab: Tab::Sessions,
                    mode: &mode,
                    project: ProjectFooterContext {
                        git_branch: Some("main"),
                        git_status: Some((0, 0)),
                        git_upstream_ref: Some("origin/main"),
                        working_dir: Path::new("/tmp/workspace-root"),
                    },
                    session_branch_names: &session_branch_names,
                    session_git_statuses: &session_git_statuses,
                    session_index_by_id: &session_index_by_id,
                    sessions: &sessions,
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("↓1 ↑5 main"));
    assert!(text.contains("| ✓ wt/session-"));
    assert!(!text.contains("origin/wt/session-unpublished-status"));
}

#[test]
fn render_footer_bar_uses_detected_session_branch_name_for_legacy_worktrees() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "legacy";
    let session = session_fixture(session_id, "/tmp/session-legacy-folder");
    let mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };
    let sessions = vec![session];
    let session_index_by_id = session_index_by_id(&sessions);
    let session_branch_names: HashMap<SessionId, String> =
        HashMap::from([(session_id.to_string().into(), "agentty/legacy".to_string())]);

    // Act
    terminal
        .draw(|frame| {
            render_footer_bar(
                frame,
                frame.area(),
                FooterBarRenderContext {
                    current_tab: Tab::Sessions,
                    mode: &mode,
                    project: ProjectFooterContext {
                        git_branch: Some("main"),
                        git_status: Some((2, 1)),
                        git_upstream_ref: Some("origin/main"),
                        working_dir: Path::new("/tmp/workspace-root"),
                    },
                    session_branch_names: &session_branch_names,
                    session_git_statuses: &HashMap::new(),
                    session_index_by_id: &session_index_by_id,
                    sessions: &sessions,
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("agentty/legacy"));
    assert!(!text.contains("wt/legacy"));
}
