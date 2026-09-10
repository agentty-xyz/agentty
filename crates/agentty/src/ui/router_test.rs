use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::widgets::{Paragraph, TableState};

use super::{
    DiffSurfaceInput, FrameResources, RouteSharedContext, SessionSurfaceMode, Surface, SurfaceKind,
    render_confirmation_overlay, render_diff_surface, render_list_background, render_mode_overlay,
    render_session_surface, render_surface, surface_for_help_context, surface_for_mode,
};
use crate::app::Tab;
use crate::app::session_state::SessionGitStatus;
use crate::domain::agent::ReasoningLevel;
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::domain::session::{Session, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::presentation::app_mode::{
    AppMode, ChatFocus, ConfirmationIntent, ConfirmationViewMode, DiffFocus, DiffLineComments,
    DiffPreview, DiffReviewComments, DiffSidebarFocus, HelpContext,
};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};
use crate::presentation::setting::{
    LaunchConfigurationListEditorMode, LaunchConfigurationListEditorSnapshot,
    SettingsScreenSnapshot, SettingsSelectorDropdown, SettingsSelectorDropdownOption,
};
use crate::test_support::SessionFixtureBuilder;
use crate::ui::{component, markdown, page};

/// Builds one deterministic session fixture for router render tests.
fn session_fixture(session_id: &str) -> Session {
    let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        "Captured output",
    )]);
    let mut session = SessionFixtureBuilder::new()
        .id(session_id)
        .folder(PathBuf::from(format!("/tmp/{session_id}")))
        .prompt("Prompt")
        .status(Status::Review)
        .title(Some("Router Session".to_string()))
        .build();
    session.transcript = Some(transcript);

    session
}

#[test]
fn route_shared_context_finds_appendable_parent_for_selected_review_session() {
    // Arrange
    let sessions = vec![session_fixture("source"), session_fixture("parent")];
    let mut project_table_state = TableState::default();
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let shared = RouteSharedContext {
        active_project_id: 1,
        available_agent_clis: &[],
        current_tab: Tab::Sessions,
        default_reasoning_level: ReasoningLevel::High,
        mru_project_order: &[],
        project_table_state: &mut project_table_state,
        projects: &[],
        session_git_statuses: &HashMap::new(),
        sessions: &sessions,
        settings_screen: None,
        stats_activity: &[],
        table_state: &mut table_state,
    };

    // Act
    let can_append = shared.can_append_selected_session();
    let parent_sessions = shared.stack_append_parent_sessions("source");

    // Assert
    assert!(can_append);
    assert_eq!(parent_sessions.len(), 1);
    assert_eq!(parent_sessions[0].id.as_str(), "parent");
}

/// Flattens a rendered test buffer into a plain string for text assertions.
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

fn settings_screen_with_overlays() -> SettingsScreenSnapshot {
    SettingsScreenSnapshot {
        footer_hint: "Editing settings",
        global_rows: vec![("Theme", "Agentty Default".to_string())],
        launch_configuration_list_editor: Some(LaunchConfigurationListEditorSnapshot {
            commands: vec!["cargo test".to_string()],
            input: None,
            mode: LaunchConfigurationListEditorMode::Browse,
            selected_index: 0,
        }),
        project_rows: vec![("Launch Configurations", "cargo test".to_string())],
        selected_row_index: Some(0),
        selector_dropdown: Some(SettingsSelectorDropdown {
            options: vec![SettingsSelectorDropdownOption {
                label: "Agentty Default".to_string(),
            }],
            row_index: 0,
            selected_index: 0,
            title: "Select setting value",
        }),
    }
}

fn render_list_tab(
    current_tab: Tab,
    settings_screen: Option<&SettingsScreenSnapshot>,
) -> (String, ReasoningLevel) {
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let available_agent_clis = Vec::new();
    let mut project_table_state = TableState::default();
    let projects = Vec::new();
    let sessions = Vec::new();
    let stats_activity = Vec::new();
    let mut table_state = TableState::default();
    let mut shared = RouteSharedContext {
        active_project_id: 1,
        available_agent_clis: &available_agent_clis,
        current_tab,
        default_reasoning_level: ReasoningLevel::Max,
        mru_project_order: &[],
        project_table_state: &mut project_table_state,
        projects: &projects,
        session_git_statuses: &HashMap::new(),
        sessions: &sessions,
        settings_screen,
        stats_activity: &stats_activity,
        table_state: &mut table_state,
    };
    let default_reasoning_level = shared.default_reasoning_level;

    terminal
        .draw(|frame| {
            render_list_background(frame, frame.area(), &mut shared, FrameTime::new(0, 0, 0));
        })
        .expect("failed to draw list tab");

    (
        buffer_text(terminal.backend().buffer()),
        default_reasoning_level,
    )
}

/// Renders one list-backed mode through the production router.
fn render_list_backed_mode(mode: &AppMode) -> (bool, String) {
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut project_table_state = TableState::default();
    let sessions = vec![session_fixture("session-overlay")];
    let mut table_state = TableState::default();
    let mut shared = RouteSharedContext {
        active_project_id: 1,
        available_agent_clis: &[],
        current_tab: Tab::Sessions,
        default_reasoning_level: ReasoningLevel::High,
        mru_project_order: &[],
        project_table_state: &mut project_table_state,
        projects: &[],
        session_git_statuses: &HashMap::new(),
        sessions: &sessions,
        settings_screen: None,
        stats_activity: &[],
        table_state: &mut table_state,
    };
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
    let active_prompt_outputs = HashMap::new();
    let session_progress_messages = HashMap::new();
    let session_update_versions = HashMap::new();
    let session_worktree_availability = HashMap::new();
    let mut handled = false;

    terminal
        .draw(|frame| {
            render_surface(
                frame,
                frame.area(),
                surface_for_mode(mode),
                &mut shared,
                FrameResources {
                    active_prompt_outputs: &active_prompt_outputs,
                    default_reasoning_level: ReasoningLevel::High,
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &markdown_render_cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &session_progress_messages,
                    session_update_versions: &session_update_versions,
                    session_worktree_availability: &session_worktree_availability,
                    frame_time: FrameTime::new(90, 90_000, -28_800),
                },
            );
            render_mode_overlay(
                frame,
                frame.area(),
                mode,
                &shared,
                FrameResources {
                    active_prompt_outputs: &active_prompt_outputs,
                    default_reasoning_level: ReasoningLevel::High,
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &markdown_render_cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &session_progress_messages,
                    session_update_versions: &session_update_versions,
                    session_worktree_availability: &session_worktree_availability,
                    frame_time: FrameTime::new(90, 90_000, -28_800),
                },
            );
            handled = true;
        })
        .expect("failed to draw list-backed mode");

    (handled, buffer_text(terminal.backend().buffer()))
}

#[test]
fn surface_for_help_context_classifies_each_restored_page() {
    // Arrange
    let list_context = HelpContext::List {
        keybindings: vec![],
    };
    let view_context = HelpContext::View {
        can_fork_session: true,
        can_merge_session_branch: true,
        can_mutate_session_branch: true,
        can_open_worktree: true,
        can_rebase_session_branch: true,
        can_show_diff: true,
        can_reply_to_session: true,
        can_start_staged_session: false,
        can_view_review_comments: false,
        publish_pull_request_action: None,
        scroll_offset: Some(3),
        session_id: "session-help".into(),
        session_state: crate::presentation::help_action::ViewSessionState::Review,
    };
    let diff_context = HelpContext::Diff {
        can_comment: true,
        diff: "diff --git a/file b/file".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(Box::new(DiffReviewComments {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..DiffReviewComments::loading(1)
        })),
        restore: None,
        scroll_offset: 2,
        session_id: "session-help".into(),
    };

    // Act
    let list_surface = surface_for_help_context(&list_context);
    let view_surface = surface_for_help_context(&view_context);
    let diff_surface = surface_for_help_context(&diff_context);

    // Assert
    assert!(matches!(list_surface, Surface::List));
    assert!(matches!(view_surface, Surface::Session { .. }));
    assert!(matches!(
        diff_surface,
        Surface::Diff {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..
        }
    ));
}

#[test]
fn surface_kind_classifies_each_base_page() {
    // Arrange
    let diff = String::new();
    let line_comments = DiffLineComments::default();
    let preview = DiffPreview::default();
    let surfaces = [
        Surface::Diff {
            diff: &diff,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: &line_comments,
            selected_diff_line_index: 0,
            preview: &preview,
            review_comments: None,
            restore: None,
            scroll_offset: 0,
            session_id: "session-surface-kind",
            sidebar_focus: DiffSidebarFocus::Files,
        },
        Surface::DiffLoading {
            session_id: "session-surface-kind",
            sidebar_focus: DiffSidebarFocus::Comments,
        },
        Surface::List,
        Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: None,
            session_id: "session-surface-kind",
        },
    ];

    // Act
    let surface_kinds = surfaces.map(Surface::kind);

    // Assert
    assert_eq!(
        surface_kinds,
        [
            SurfaceKind::Diff,
            SurfaceKind::Diff,
            SurfaceKind::List,
            SurfaceKind::Session,
        ]
    );
}

#[test]
fn surface_for_mode_uses_confirmation_restore_only_for_session_intents() {
    // Arrange
    let session_confirmation = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeSession,
        confirmation_message: "Merge?".to_string(),
        confirmation_title: "Confirm".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: "session-confirm".into(),
        }),
        selected_confirmation_index: 0,
        session_id: Some("session-confirm".into()),
    };
    let list_confirmation = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::Quit,
        confirmation_message: "Quit?".to_string(),
        confirmation_title: "Confirm".to_string(),
        restore_view: None,
        selected_confirmation_index: 1,
        session_id: None,
    };

    // Act
    let session_surface = surface_for_mode(&session_confirmation);
    let list_surface = surface_for_mode(&list_confirmation);

    // Assert
    assert!(matches!(session_surface, Surface::Session { .. }));
    assert!(matches!(list_surface, Surface::List));
}

#[test]
fn surface_for_mode_classifies_primary_page_modes() {
    // Arrange
    let help_mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: vec![],
        },
        scroll_offset: 0,
    };
    let view_mode = AppMode::View {
        scroll_offset: Some(1),
        session_id: "session-overlay".into(),
    };
    let prompt_mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::default(),
        scroll_offset: Some(2),
        session_id: "session-overlay".into(),
        slash_state: PromptSlashState::default(),
    };
    let question_mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        questions: vec![QuestionItem {
            options: vec![],
            text: "Which path?".to_string(),
        }],
        responses: vec![],
        scroll_offset: Some(3),
        selected_option_index: None,
        session_id: "session-overlay".into(),
    };
    let diff_mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(DiffReviewComments {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..DiffReviewComments::loading(1)
        }),
        restore: None,
        scroll_cache: None,
        scroll_offset: 4,
        session_id: "session-overlay".into(),
    };
    let diff_loading_mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: Some(4),
        request_id: 1,
        restore: None,
        session_id: "session-overlay".into(),
        sidebar_focus: DiffSidebarFocus::Comments,
    };

    // Act
    let help_is_list = matches!(surface_for_mode(&help_mode), Surface::List);
    let view_is_session = matches!(surface_for_mode(&view_mode), Surface::Session { .. });
    let prompt_is_session = matches!(surface_for_mode(&prompt_mode), Surface::Session { .. });
    let question_is_session = matches!(surface_for_mode(&question_mode), Surface::Session { .. });
    let diff_is_diff = matches!(surface_for_mode(&diff_mode), Surface::Diff { .. });
    let (_, comments_text) = render_list_backed_mode(&diff_mode);
    let diff_loading_is_diff = matches!(
        surface_for_mode(&diff_loading_mode),
        Surface::DiffLoading {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..
        }
    );
    let (_, loading_text) = render_list_backed_mode(&diff_loading_mode);

    // Assert
    assert!(help_is_list);
    assert!(view_is_session);
    assert!(prompt_is_session);
    assert!(question_is_session);
    assert!(diff_is_diff);
    assert!(diff_loading_is_diff);
    assert!(comments_text.contains("Comment — Router Session"));
    assert!(loading_text.contains("Loading diff..."));
    assert!(!loading_text.contains("No files"));
}

#[test]
fn render_list_background_renders_settings_snapshot_and_overlays() {
    // Arrange
    let settings_screen = settings_screen_with_overlays();

    // Act
    let (text, _) = render_list_tab(Tab::Settings, Some(&settings_screen));

    // Assert
    assert!(text.contains("Launch Configurations"));
    assert!(text.contains("cargo test"));
}

#[test]
fn render_list_background_projects_reasoning_into_sessions_page() {
    // Arrange

    // Act
    let (_, default_reasoning_level) = render_list_tab(Tab::Sessions, None);

    // Assert
    assert_eq!(default_reasoning_level, ReasoningLevel::Max);
}

#[test]
fn render_help_mode_restores_markdown_diff_preview_background() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "session-help-diff";
    let sessions = vec![session_fixture(session_id)];
    let mode = AppMode::Help {
        context: crate::presentation::app_mode::HelpContext::Diff {
            can_comment: true,
            diff: "diff --git a/README.md b/README.md\n+# Preview".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: crate::presentation::app_mode::DiffPreview::Ready {
                content: "# Preview".to_string(),
                path: "README.md".to_string(),
                request_id: 1,
            },
            review_comments: None,
            restore: None,
            scroll_offset: 0,
            session_id: session_id.into(),
        },
        scroll_offset: 0,
    };
    let mut project_table_state = TableState::default();
    let mut table_state = TableState::default();
    let mut shared = RouteSharedContext {
        active_project_id: 1,
        available_agent_clis: &[],
        current_tab: Tab::Sessions,
        default_reasoning_level: ReasoningLevel::default(),
        mru_project_order: &[],
        project_table_state: &mut project_table_state,
        projects: &[],
        session_git_statuses: &HashMap::new(),
        sessions: &sessions,
        settings_screen: None,
        stats_activity: &[],
        table_state: &mut table_state,
    };
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();

    // Act
    let mut handled = false;
    terminal
        .draw(|frame| {
            render_surface(
                frame,
                frame.area(),
                surface_for_mode(&mode),
                &mut shared,
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &markdown_render_cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &HashMap::new(),
                    session_update_versions: &HashMap::new(),
                    session_worktree_availability: &HashMap::new(),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
            render_mode_overlay(
                frame,
                frame.area(),
                &mode,
                &shared,
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &markdown_render_cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &HashMap::new(),
                    session_update_versions: &HashMap::new(),
                    session_worktree_availability: &HashMap::new(),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
            handled = true;
        })
        .expect("failed to draw diff help mode");

    // Assert
    assert!(handled);
    assert!(buffer_text(terminal.backend().buffer()).contains("Keybindings"));
}

#[test]
fn render_list_backed_modes_forward_frame_time_to_every_overlay() {
    // Arrange
    let restore_view = ConfirmationViewMode {
        scroll_offset: None,
        session_id: "session-overlay".into(),
    };
    let modes = [
        (
            AppMode::SessionCreation {
                selected_option_index: 0,
            },
            "New Session",
        ),
        (
            AppMode::StackAppendParentSelection {
                selected_parent_index: 0,
                session_id: "session-overlay".into(),
            },
            "Append to stack",
        ),
        (
            AppMode::PreCommitHookWarning {
                message: "Install the hook".to_string(),
            },
            "Pre-commit hook warning",
        ),
        (
            AppMode::ProjectSwitcher {
                selected_option_index: 0,
            },
            "Switch project",
        ),
        (
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                confirmation_message: "Quit now?".to_string(),
                confirmation_title: "Confirm Quit".to_string(),
                restore_view: None,
                session_id: None,
                selected_confirmation_index: 0,
            },
            "Confirm Quit",
        ),
        (
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::MergeSession,
                confirmation_message: "Merge now?".to_string(),
                confirmation_title: "Confirm Merge".to_string(),
                restore_view: Some(restore_view.clone()),
                session_id: Some("session-overlay".into()),
                selected_confirmation_index: 0,
            },
            "Confirm Merge",
        ),
        (
            AppMode::SyncBlockedPopup {
                default_branch: Some("main".to_string()),
                is_loading: true,
                message: "Waiting for sync".to_string(),
                project_name: Some("agentty".to_string()),
                title: "Syncing".to_string(),
            },
            "Syncing",
        ),
        (
            AppMode::ViewInfoPopup {
                is_loading: true,
                loading_label: "Publishing branch".to_string(),
                message: "Waiting for forge".to_string(),
                restore_view,
                title: "Publishing".to_string(),
            },
            "Publishing",
        ),
        (
            AppMode::LaunchConfigurationSelector {
                commands: vec!["cargo test".to_string()],
                restore_view: ConfirmationViewMode {
                    scroll_offset: None,
                    session_id: "session-overlay".into(),
                },
                selected_command_index: 0,
            },
            "Launch Configuration",
        ),
    ];

    // Act
    let rendered_modes = modes
        .iter()
        .map(|(mode, expected_text)| {
            let (handled, text) = render_list_backed_mode(mode);

            (handled, text, *expected_text)
        })
        .collect::<Vec<_>>();

    // Assert
    for (handled, text, expected_text) in rendered_modes {
        assert!(handled);
        assert!(
            text.contains(expected_text),
            "rendered output should contain `{expected_text}`"
        );
    }
}

#[test]
fn render_help_modes_forward_frame_time_to_list_and_view_backgrounds() {
    // Arrange
    let modes = [
        (
            AppMode::Help {
                context: crate::presentation::app_mode::HelpContext::List {
                    keybindings: vec![],
                },
                scroll_offset: 0,
            },
            "Keybindings",
        ),
        (
            AppMode::Help {
                context: crate::presentation::app_mode::HelpContext::View {
                    can_fork_session: true,
                    can_merge_session_branch: true,
                    can_mutate_session_branch: true,
                    can_open_worktree: true,
                    can_rebase_session_branch: true,
                    can_show_diff: true,
                    can_reply_to_session: true,
                    can_start_staged_session: false,
                    can_view_review_comments: false,
                    publish_pull_request_action: None,
                    session_id: "session-overlay".into(),
                    session_state: crate::presentation::help_action::ViewSessionState::Review,
                    scroll_offset: None,
                },
                scroll_offset: 0,
            },
            "Keybindings",
        ),
    ];

    // Act
    let rendered_modes = modes
        .iter()
        .map(|(mode, expected_text)| {
            let (handled, text) = render_list_backed_mode(mode);

            (handled, text, *expected_text)
        })
        .collect::<Vec<_>>();

    // Assert
    for (handled, text, expected_text) in rendered_modes {
        assert!(handled);
        assert!(
            text.contains(expected_text),
            "rendered output should contain `{expected_text}`"
        );
    }
}

#[test]
fn render_session_surface_renders_view_session_content() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "session-1234";
    let sessions = vec![session_fixture(session_id)];
    let mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };
    let progress_messages = HashMap::new();
    let cache = markdown::MarkdownRenderCache::default();
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
    let session_git_statuses = HashMap::from([(
        session_id.into(),
        SessionGitStatus {
            base_status: Some((1, 1)),
            has_merge_conflict: Some(true),
            remote_status: None,
        },
    )]);
    let session_update_versions = HashMap::new();

    // Act
    terminal
        .draw(|frame| {
            render_session_surface(
                frame,
                frame.area(),
                SessionSurfaceMode::Interactive(&mode),
                None,
                session_id,
                &sessions,
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: false,
                    markdown_render_cache: &cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &session_git_statuses,
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &progress_messages,
                    session_update_versions: &session_update_versions,
                    session_worktree_availability: &HashMap::from([(
                        session_id.to_string().into(),
                        true,
                    )]),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Router Session"));
    assert!(text.contains("Captured output"));
    assert!(text.contains("Merge conflict with main"));
    assert!(!text.contains("o: open"));
}

#[test]
fn render_session_surface_uses_campaign_page_for_orchestrators() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "campaign-1234";
    let mut session = session_fixture(session_id);
    session.role = crate::domain::session::SessionRole::Orchestrator;
    session.orchestration_progress = Some("Phase: AwaitingApproval".to_string());
    let sessions = [session];
    let mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };
    let cache = markdown::MarkdownRenderCache::default();
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();

    // Act
    terminal
        .draw(|frame| {
            render_session_surface(
                frame,
                frame.area(),
                SessionSurfaceMode::Interactive(&mode),
                None,
                session_id,
                &sessions,
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &HashMap::new(),
                    session_update_versions: &HashMap::new(),
                    session_worktree_availability: &HashMap::from([(
                        session_id.to_string().into(),
                        true,
                    )]),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Campaign: Router Session"));
    assert!(text.contains("Phase: AwaitingApproval"));
}

#[test]
fn render_session_surface_keeps_background_when_session_is_missing() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mode = AppMode::View {
        session_id: "missing-session".into(),
        scroll_offset: None,
    };
    let progress_messages = HashMap::new();
    let sessions = Vec::new();
    let cache = markdown::MarkdownRenderCache::default();
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
    let session_update_versions = HashMap::new();

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(Paragraph::new("sentinel"), area);
            render_session_surface(
                frame,
                area,
                SessionSurfaceMode::Interactive(&mode),
                None,
                "missing-session",
                &sessions,
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &progress_messages,
                    session_update_versions: &session_update_versions,
                    session_worktree_availability: &HashMap::new(),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("sentinel"));
}

#[test]
fn render_diff_surface_renders_page_for_matching_session() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "session-diff";
    let mut session = session_fixture(session_id);
    session.title = Some("Diff Session".to_string());
    let sessions = vec![session];
    let progress_messages = HashMap::new();
    let cache = markdown::MarkdownRenderCache::default();
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
    let session_update_versions = HashMap::new();

    // Act
    terminal
        .draw(|frame| {
            render_diff_surface(
                frame,
                frame.area(),
                DiffSurfaceInput {
                    diff: "",
                    file_explorer_selected_index: 0,
                    focus: DiffFocus::Files,
                    is_loading: false,
                    line_comments: &DiffLineComments::default(),
                    selected_diff_line_index: 0,
                    preview: &DiffPreview::default(),
                    review_comments: None,
                    restore: None,
                    scroll_offset: 0,
                    session_id,
                    sidebar_focus: DiffSidebarFocus::Files,
                },
                &sessions,
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &progress_messages,
                    session_update_versions: &session_update_versions,
                    session_worktree_availability: &HashMap::new(),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Diff Session"));
    assert!(text.contains("No changes found."));
}

#[test]
fn render_diff_surface_preserves_frame_for_missing_session() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();

    // Act
    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new("sentinel"), frame.area());
            render_diff_surface(
                frame,
                frame.area(),
                DiffSurfaceInput {
                    diff: "",
                    file_explorer_selected_index: 0,
                    focus: DiffFocus::Files,
                    is_loading: false,
                    line_comments: &DiffLineComments::default(),
                    selected_diff_line_index: 0,
                    preview: &DiffPreview::default(),
                    review_comments: None,
                    restore: None,
                    scroll_offset: 0,
                    session_id: "missing-session",
                    sidebar_focus: DiffSidebarFocus::Files,
                },
                &[],
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &markdown_render_cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &HashMap::new(),
                    session_update_versions: &HashMap::new(),
                    session_worktree_availability: &HashMap::new(),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
        })
        .expect("failed to draw");

    // Assert
    assert!(buffer_text(terminal.backend().buffer()).contains("sentinel"));
}

#[test]
fn render_diff_surface_renders_linked_comments_for_matching_session() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let session_id = "session-comments";
    let sessions = vec![session_fixture(session_id)];
    let review_comments = DiffReviewComments {
        sidebar_focus: DiffSidebarFocus::Comments,
        ..DiffReviewComments::loading(1)
    };
    let progress_messages = HashMap::new();
    let cache = markdown::MarkdownRenderCache::default();
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
    let session_update_versions = HashMap::new();

    // Act
    terminal
        .draw(|frame| {
            render_diff_surface(
                frame,
                frame.area(),
                DiffSurfaceInput {
                    diff: "",
                    file_explorer_selected_index: 0,
                    focus: DiffFocus::Files,
                    is_loading: false,
                    line_comments: &DiffLineComments::default(),
                    selected_diff_line_index: 0,
                    preview: &DiffPreview::default(),
                    review_comments: Some(&review_comments),
                    restore: None,
                    scroll_offset: 0,
                    session_id,
                    sidebar_focus: DiffSidebarFocus::Comments,
                },
                &sessions,
                FrameResources {
                    active_prompt_outputs: &HashMap::new(),
                    default_reasoning_level: ReasoningLevel::default(),
                    diff_layout_cache: &diff_layout_cache,
                    is_tmux_session: true,
                    markdown_render_cache: &cache,
                    output_layout_cache: &output_layout_cache,
                    review_snapshot: None,
                    session_git_statuses: &HashMap::new(),
                    session_cpu_temperatures: &HashMap::new(),
                    session_resources: &HashMap::new(),
                    session_progress_messages: &progress_messages,
                    session_update_versions: &session_update_versions,
                    session_worktree_availability: &HashMap::new(),
                    frame_time: FrameTime::new(0, 0, 0),
                },
            );
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Comment — Router Session"));
    assert!(text.contains("Loading review comments..."));
}

#[test]
fn render_confirmation_overlay_renders_integration_choices() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
        confirmation_message: "How should the campaign integrate?".to_string(),
        confirmation_title: "Integration Approach".to_string(),
        restore_view: None,
        selected_confirmation_index: 0,
        session_id: Some("session-controller".into()),
    };

    // Act
    terminal
        .draw(|frame| {
            render_confirmation_overlay(frame, frame.area(), &mode);
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Integration Approach"));
    assert!(text.contains("How should the campaign integrate?"));
    assert!(text.contains("Local merges"));
    assert!(text.contains("Review requests"));
}

#[test]
fn render_mode_overlay_renders_publish_input() {
    // Arrange
    let session_id = "session-publish";
    let mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session-publish".to_string(),
        input: InputState::default(),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::PublishPullRequest,
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: session_id.into(),
        },
    };

    // Act
    let (_, text) = render_list_backed_mode(&mode);

    // Assert
    assert!(text.contains("Publish Review Request"));
    assert!(text.contains("wt/session-publish"));
}
