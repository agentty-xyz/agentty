use std::path::PathBuf;

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Borders;

use super::{
    QuestionPanelLayout, app_frame_areas, centered_content_rect, prompt_panel_areas,
    question_at_mention_max_visible, question_options_height, question_panel_areas,
    question_panel_layout, question_panel_reserved_height, session_chat_areas, tab_page_areas,
};
use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputState;
use crate::domain::session::{COMMITTING_PROGRESS_LABEL, Session, SessionDiffState, Status};
use crate::domain::theme::ColorTheme;
use crate::presentation::app_mode::ChatFocus;
use crate::presentation::help_action;
use crate::presentation::help_action::{ViewActionAvailability, ViewHelpState};
use crate::presentation::prompt::{PromptAtMentionState, PromptSlashStage, PromptSlashState};
use crate::ui::icon::Icon;
use crate::ui::input_layout::{
    bottom_pinned_scroll_offset, calculate_input_height, calculate_input_viewport,
    compute_input_layout, first_table_column_width, input_cursor_position, move_input_cursor_down,
    move_input_cursor_up, overlay_area_above, panel_inner_width, placeholder_cursor_position,
    suggestion_dropdown_height,
};
use crate::ui::prompt_format::{
    file_lookup_suggestion_list, prompt_footer_line, prompt_suggestion_list,
};
use crate::ui::question_format::{
    QuestionLookupState, question_help_footer_line, question_option_lines, question_panel_lines,
};
use crate::ui::session_format::{
    format_review_markdown, session_header_lines, session_metadata_text, session_output_done_line,
    session_output_panel_borders, session_output_status_lines, session_view_footer_line,
};
use crate::ui::style;

fn session_fixture() -> Session {
    crate::test_support::SessionFixtureBuilder::new()
        .status(Status::Draft)
        .build()
}

fn view_footer_text(session: &Session, can_open_worktree: bool) -> String {
    session_view_footer_line(ViewHelpState {
        can_fork_session: ViewActionAvailability::from_bool(session.allows_fork_action()),
        can_merge_session_branch: ViewActionAvailability::Enabled,
        can_mutate_session_branch: ViewActionAvailability::Enabled,
        can_open_worktree: ViewActionAvailability::from_bool(can_open_worktree),
        can_rebase_session_branch: ViewActionAvailability::Enabled,
        can_show_diff: ViewActionAvailability::from_bool(session.stats.should_show_diff()),
        reply_to_session: ViewActionAvailability::Enabled,
        can_start_staged_session: ViewActionAvailability::from_bool(
            session.can_start_staged_session(),
        ),
        publish_pull_request_action: session.publish_pull_request_action(),
        session_state: help_action::session_view_state(session),
    })
    .to_string()
}

fn file_entries_fixture() -> Vec<FileEntry> {
    vec![
        FileEntry {
            is_dir: true,
            path: "src".to_string(),
        },
        FileEntry {
            is_dir: true,
            path: "tests".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "Cargo.toml".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "README.md".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/lib.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "tests/integration.rs".to_string(),
        },
    ]
}

#[test]
fn test_app_frame_areas_reserve_status_and_footer_bars() {
    // Arrange
    let area = Rect::new(0, 0, 80, 24);

    // Act
    let frame_areas = app_frame_areas(area);

    // Assert
    assert_eq!(frame_areas.status_bar_area, Rect::new(0, 0, 80, 1));
    assert_eq!(frame_areas.content_area, Rect::new(0, 1, 80, 22));
    assert_eq!(frame_areas.footer_bar_area, Rect::new(0, 23, 80, 1));
}

#[test]
fn test_session_chat_areas_reserve_margin_header_and_bottom_panel() {
    // Arrange
    let area = Rect::new(0, 0, 80, 24);

    // Act
    let chat_areas = session_chat_areas(area, 5, 2);

    // Assert
    assert_eq!(chat_areas.header_area, Rect::new(1, 1, 78, 2));
    assert_eq!(chat_areas.output_area, Rect::new(1, 3, 78, 15));
    assert_eq!(chat_areas.bottom_area, Rect::new(1, 18, 78, 5));
}

#[test]
fn test_session_chat_areas_expand_for_three_line_header() {
    // Arrange
    let area = Rect::new(0, 0, 80, 24);

    // Act
    let chat_areas = session_chat_areas(area, 5, 3);

    // Assert
    assert_eq!(chat_areas.header_area, Rect::new(1, 1, 78, 3));
    assert_eq!(chat_areas.output_area, Rect::new(1, 4, 78, 14));
    assert_eq!(chat_areas.bottom_area, Rect::new(1, 18, 78, 5));
}

#[test]
fn test_prompt_panel_areas_split_input_and_footer_rows() {
    // Arrange
    let area = Rect::new(3, 10, 50, 6);

    // Act
    let panel_areas = prompt_panel_areas(area);

    // Assert
    assert_eq!(panel_areas.input_area, Rect::new(3, 10, 50, 5));
    assert_eq!(panel_areas.footer_area, Rect::new(3, 15, 50, 1));
}

#[test]
fn test_prompt_panel_areas_collapse_footer_when_only_one_row_fits() {
    // Arrange
    let area = Rect::new(3, 10, 50, 1);

    // Act
    let panel_areas = prompt_panel_areas(area);

    // Assert
    assert_eq!(panel_areas.input_area, area);
    assert_eq!(panel_areas.footer_area, Rect::new(3, 11, 50, 0));
}

#[test]
fn test_session_header_lines_truncate_long_titles_and_keep_metadata() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    session.title = Some("This is a very long timer-aware session header title".to_string());
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    session.in_progress_started_at = Some(0);

    // Act
    let header_lines = session_header_lines(&session, 50, ReasoningLevel::default(), 3_660, false);

    // Assert
    assert_eq!(header_lines.len(), 2);
    assert!(header_lines[0].to_string().contains("..."));
    assert!(header_lines[1].to_string().contains("Timer: 1h 1m 0s"));
}

#[test]
fn test_session_header_lines_use_theme_status_color() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Green);
    let mut session = session_fixture();
    session.status = Status::Canceled;
    session.title = Some("Canceled session".to_string());

    // Act
    let header_lines = session_header_lines(&session, 80, ReasoningLevel::default(), 0, false);

    // Assert
    assert_eq!(
        header_lines[0].spans[0].style.fg,
        Some(style::status_color(Status::Canceled))
    );
}

#[test]
fn test_session_metadata_text_ticks_live_in_progress_timer() {
    // Arrange
    let mut session = session_fixture();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    session.stats.added_lines = 9;
    session.stats.deleted_lines = 3;
    session.status = Status::InProgress;
    session.in_progress_started_at = Some(60);

    // Act
    let early_metadata = session_metadata_text(&session, 120, ReasoningLevel::default(), 90);
    let later_metadata = session_metadata_text(&session, 120, ReasoningLevel::default(), 3_720);

    // Assert
    assert!(early_metadata.contains("Lines: +9 / -3"));
    assert!(early_metadata.contains("Timer: 30s"));
    assert!(early_metadata.contains("Model: gpt-5.6-sol"));
    assert!(
        early_metadata.find("Model: gpt-5.6-sol") < early_metadata.find("Reasoning: high"),
        "model should appear before reasoning in metadata text"
    );
    assert!(later_metadata.contains("Timer: 1h 1m 0s"));
}

#[test]
fn test_session_metadata_text_freezes_timer_after_in_progress_ends() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    session.in_progress_total_seconds = 3_660;

    // Act
    let earlier_metadata = session_metadata_text(&session, 80, ReasoningLevel::default(), 4_000);
    let later_metadata = session_metadata_text(&session, 80, ReasoningLevel::default(), 40_000);

    // Assert
    assert_eq!(earlier_metadata, later_metadata);
    assert!(earlier_metadata.contains("Timer: 1h 1m 0s"));
}

#[test]
fn test_session_view_footer_line_in_progress_shows_stop_and_hides_open_and_diff() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;

    // Act
    let help_text = view_footer_text(&session, true);

    // Assert
    assert!(help_text.contains("q: back"));
    assert!(help_text.contains("Ctrl+c: stop"));
    assert!(help_text.contains("j/k: scroll"));
    assert!(!help_text.contains("o: open"));
    assert!(!help_text.contains("d: diff"));
    assert!(!help_text.contains("Enter: reply"));
}

#[test]
fn test_session_view_footer_line_hides_known_empty_diff() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    session.stats.diff_state = SessionDiffState::Empty;

    // Act
    let help_text = view_footer_text(&session, true);

    // Assert
    assert!(!help_text.contains("d: diff"));
    assert!(help_text.contains("Enter: reply"));
}

#[test]
fn test_session_view_footer_line_noninteractive_busy_statuses_hide_worktree_open_hint() {
    // Arrange
    let busy_statuses = [Status::Rebasing, Status::Queued, Status::Merging];

    // Act
    let help_texts: Vec<String> = busy_statuses
        .iter()
        .map(|session_status| {
            let mut session = session_fixture();
            session.status = *session_status;

            view_footer_text(&session, true)
        })
        .collect();

    // Assert
    for help_text in help_texts {
        assert!(help_text.contains("q: back"));
        assert!(help_text.contains("j/k: scroll"));
        assert!(!help_text.contains("o: open"));
        assert!(!help_text.contains("Ctrl+c: stop"));
    }
}

#[test]
fn test_session_view_footer_line_new_session_shows_draft_and_start_actions() {
    // Arrange
    let mut session = session_fixture();
    session.folder = PathBuf::new();
    session.is_draft = true;
    session.prompt = "Staged draft".to_string();

    // Act
    let help_text = view_footer_text(&session, false);

    // Assert
    assert!(help_text.contains("Enter: add draft"));
    assert!(help_text.contains("s: start"));
    assert!(!help_text.contains("o: open"));
    assert!(help_text.contains("m: add to merge queue"));
    assert!(!help_text.contains("r: sync"));
    assert!(help_text.contains("/: commands menu"));
    assert!(!help_text.contains("d: diff"));
}

#[test]
fn test_session_view_footer_line_done_session_shows_continue_action() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Done;

    // Act
    let help_text = view_footer_text(&session, true);

    // Assert
    assert!(help_text.contains("c: continue"));
    assert!(!help_text.contains("comments"));
}

#[test]
fn test_prompt_footer_line_shows_highlighted_actions_and_attachment_count() {
    // Arrange
    let session = session_fixture();

    // Act
    let footer_line = prompt_footer_line(&session, 2, ChatFocus::Input);

    // Assert
    assert_eq!(
        footer_line.to_string(),
        "Tab: focus | Enter: send | Alt+Enter: newline | Ctrl+V/Alt+V: paste image | Esc: cancel \
         | Shift+Tab: switch mode | 2 images ready"
    );
    assert_eq!(
        footer_line.spans[0].style,
        Style::default()
            .fg(style::palette::accent())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        footer_line.spans[1].style,
        Style::default().fg(style::palette::text_muted())
    );
    assert_eq!(
        footer_line.spans[footer_line.spans.len() - 2].style,
        Style::default().fg(style::palette::text_subtle())
    );
    assert_eq!(
        footer_line.spans[footer_line.spans.len() - 1].style,
        Style::default().fg(style::palette::text_muted())
    );
}

#[test]
fn test_prompt_footer_line_uses_stage_label_for_new_sessions() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Draft;
    session.is_draft = true;

    // Act
    let footer_line = prompt_footer_line(&session, 0, ChatFocus::Input);

    // Assert
    assert!(footer_line.to_string().contains("Enter: stage draft"));
    assert!(!footer_line.to_string().contains("Enter: send"));
}

#[test]
fn test_prompt_footer_line_shows_scroll_actions_while_chat_is_focused() {
    // Arrange
    let mut session = session_fixture();
    session.stats.diff_state = SessionDiffState::Empty;

    // Act
    let unchanged_footer_line = prompt_footer_line(&session, 0, ChatFocus::Chat);
    session.stats.diff_state = SessionDiffState::Present;
    let changed_footer_line = prompt_footer_line(&session, 0, ChatFocus::Chat);

    // Assert
    assert_eq!(
        unchanged_footer_line.to_string(),
        "Tab: focus | j/k: scroll | q: sessions"
    );
    assert_eq!(
        changed_footer_line.to_string(),
        "Tab: focus | j/k: scroll | d: diff | q: sessions"
    );
}

#[test]
fn test_prompt_suggestion_list_for_command_stage_has_description() {
    // Arrange
    let input = InputState::with_text("/model".to_string());
    let slash_state = PromptSlashState::default();

    // Act
    let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
        .expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 1);
    assert_eq!(menu.items[0].label, "/model");
    assert_eq!(
        menu.items[0].detail,
        Some("Choose an agent and model for this session.".to_string())
    );
}

#[test]
fn test_prompt_suggestion_list_for_agent_stage_has_agent_descriptions() {
    // Arrange
    let input = InputState::with_text("/model".to_string());
    let slash_state = PromptSlashState {
        stage: PromptSlashStage::Agent,
        ..PromptSlashState::default()
    };

    // Act
    let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
        .expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), AgentKind::ALL.len());
    assert_eq!(menu.items[0].label, "gemini");
    assert_eq!(
        menu.items[0].detail,
        Some("Google Gemini CLI agent.".to_string())
    );
}

#[test]
fn test_prompt_suggestion_list_for_agent_stage_filters_available_agents() {
    // Arrange
    let input = InputState::with_text("/model".to_string());
    let mut slash_state = PromptSlashState::with_available_agent_kinds(vec![AgentKind::Codex]);
    slash_state.stage = PromptSlashStage::Agent;

    // Act
    let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
        .expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 1);
    assert_eq!(menu.items[0].label, "codex");
}

#[test]
fn test_prompt_suggestion_list_for_model_stage_has_model_descriptions() {
    // Arrange
    let input = InputState::with_text("/model".to_string());
    let slash_state = PromptSlashState {
        stage: PromptSlashStage::Model,
        selected_agent: Some(AgentKind::Codex),
        ..PromptSlashState::default()
    };

    // Act
    let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
        .expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), AgentKind::Codex.models().len());
    assert_eq!(menu.items[0].label, "gpt-6-astra");
    assert_eq!(
        menu.items[0].detail,
        Some("Most capable Codex model for the hardest end-to-end work.".to_string())
    );
}

#[test]
fn test_prompt_suggestion_list_for_command_stage_includes_commands() {
    // Arrange
    let input = InputState::with_text("/".to_string());

    // Act
    let menu = prompt_suggestion_list(
        &input,
        &PromptSlashState::default(),
        None,
        AgentKind::Codex,
        true,
    )
    .expect("expected suggestion list");
    let labels: Vec<&str> = menu.items.iter().map(|item| item.label.as_str()).collect();

    // Assert
    assert!(labels.contains(&"/model"));
    assert!(labels.contains(&"/reasoning"));
}

#[test]
fn test_prompt_suggestion_list_for_command_stage_omits_disabled_apply() {
    // Arrange
    let input = InputState::with_text("/".to_string());

    // Act
    let menu = prompt_suggestion_list(
        &input,
        &PromptSlashState::default(),
        None,
        AgentKind::Codex,
        false,
    )
    .expect("expected suggestion list");
    let labels: Vec<&str> = menu.items.iter().map(|item| item.label.as_str()).collect();

    // Assert
    assert_eq!(
        labels,
        vec![
            "/mode",
            "/model",
            "/personality",
            "/reasoning",
            "/style",
            "/speed"
        ]
    );
    assert_eq!(menu.selected_index, 0);
}

#[test]
fn test_file_lookup_suggestion_list_with_matches() {
    // Arrange
    let state = PromptAtMentionState::new(file_entries_fixture());

    // Act
    let menu =
        file_lookup_suggestion_list("@src", 4, &state, 10).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 3);
    assert_eq!(menu.items[0].label, "src/");
    assert_eq!(menu.items[0].detail, Some("folder".to_string()));
    assert_eq!(menu.items[1].label, "src/lib.rs");
    assert_eq!(menu.items[2].label, "src/main.rs");
}

#[test]
fn test_file_lookup_suggestion_list_with_trailing_slash_includes_exact_directory() {
    // Arrange
    let state = PromptAtMentionState::new(file_entries_fixture());

    // Act
    let menu =
        file_lookup_suggestion_list("@src/", 5, &state, 10).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items[0].label, "src/");
    assert_eq!(menu.items[0].detail, Some("folder".to_string()));
    assert_eq!(menu.items[1].label, "src/lib.rs");
    assert_eq!(menu.items[2].label, "src/main.rs");
}

#[test]
fn test_file_lookup_suggestion_list_no_matches() {
    // Arrange
    let state = PromptAtMentionState::new(file_entries_fixture());

    // Act
    let menu = file_lookup_suggestion_list("@nonexistent", 12, &state, 10);

    // Assert
    assert!(menu.is_none());
}

#[test]
fn test_file_lookup_suggestion_list_empty_query_returns_all() {
    // Arrange
    let state = PromptAtMentionState::new(file_entries_fixture());

    // Act
    let menu = file_lookup_suggestion_list("@", 1, &state, 10).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 7);
}

#[test]
fn test_file_lookup_suggestion_list_caps_at_10() {
    // Arrange
    let entries: Vec<FileEntry> = (0..20)
        .map(|index| FileEntry {
            is_dir: false,
            path: format!("file_{index:02}.rs"),
        })
        .collect();
    let state = PromptAtMentionState::new(entries);

    // Act
    let menu = file_lookup_suggestion_list("@", 1, &state, 10).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 10);
}

#[test]
fn test_file_lookup_suggestion_list_respects_capacity() {
    // Arrange
    let entries: Vec<FileEntry> = (0..20)
        .map(|index| FileEntry {
            is_dir: false,
            path: format!("file_{index:02}.rs"),
        })
        .collect();
    let state = PromptAtMentionState::new(entries);

    // Act
    let menu = file_lookup_suggestion_list("@", 1, &state, 5).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 5);
}

#[test]
fn test_file_lookup_suggestion_list_capacity_scroll_keeps_selection_visible() {
    // Arrange
    let entries: Vec<FileEntry> = (0..20)
        .map(|index| FileEntry {
            is_dir: false,
            path: format!("file_{index:02}.rs"),
        })
        .collect();
    let mut state = PromptAtMentionState::new(entries);
    state.selected_index = 18;

    // Act
    let menu = file_lookup_suggestion_list("@", 1, &state, 5).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 5);
    assert_eq!(menu.items[0].label, "file_15.rs");
    assert_eq!(menu.items[4].label, "file_19.rs");
    assert_eq!(menu.selected_index, 3);
}

#[test]
fn test_file_lookup_suggestion_list_clamps_selected_index() {
    // Arrange
    let mut state = PromptAtMentionState::new(file_entries_fixture());
    state.selected_index = 100;

    // Act
    let menu =
        file_lookup_suggestion_list("@src", 4, &state, 10).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.selected_index, 2);
}

#[test]
fn test_file_lookup_suggestion_list_scroll_window() {
    // Arrange
    let entries: Vec<FileEntry> = (0..20)
        .map(|index| FileEntry {
            is_dir: false,
            path: format!("file_{index:02}.rs"),
        })
        .collect();
    let mut state = PromptAtMentionState::new(entries);
    state.selected_index = 15;

    // Act
    let menu = file_lookup_suggestion_list("@", 1, &state, 10).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items.len(), 10);
    assert_eq!(menu.items[0].label, "file_10.rs");
    assert_eq!(menu.items[9].label, "file_19.rs");
    assert_eq!(menu.selected_index, 5);
}

#[test]
fn test_file_lookup_suggestion_list_directory_has_trailing_slash() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: true,
            path: "src".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        },
    ];
    let state = PromptAtMentionState::new(entries);

    // Act
    let menu =
        file_lookup_suggestion_list("@src", 4, &state, 10).expect("expected suggestion list");

    // Assert
    assert_eq!(menu.items[0].label, "src/");
    assert_eq!(menu.items[0].detail, Some("folder".to_string()));
    assert_eq!(menu.items[1].label, "src/main.rs");
    assert_eq!(menu.items[1].detail, None);
}

#[test]
fn test_question_panel_lines_wrap_and_style_title() {
    // Arrange
    let title = "Question 1/2";

    // Act
    let lines = question_panel_lines(title, "Need tests for the new flow?", false, 12);

    // Assert
    assert_eq!(lines[0].to_string(), title);
    assert!(lines.len() > 2);
    assert_eq!(lines[0].spans[0].style.fg, Some(style::palette::question()));
}

#[test]
fn test_question_option_lines_highlight_selected_row() {
    // Arrange
    let options = vec!["Yes".to_string(), "No".to_string()];

    // Act
    let lines = question_option_lines(&options, Some(1), false);

    // Assert
    assert_eq!(lines[0].to_string(), "Options:");
    assert_eq!(lines[2].to_string(), "▸ 2. No");
    assert_eq!(lines[2].spans[0].style.bg, Some(style::palette::warning()));
}

#[test]
fn test_question_at_mention_max_visible_clamps_to_available_rows() {
    // Arrange
    let bottom_area = Rect::new(1, 10, 78, 20);

    for (available_rows, limit, expected) in [
        (0, 10, 0),
        (1, 10, 0),
        (2, 10, 0),
        (3, 10, 1),
        (5, 10, 3),
        (20, 10, 10),
        (5, 0, 0),
    ] {
        let input_area = Rect::new(1, 10 + available_rows, 78, 2);

        // Act
        let max_visible = question_at_mention_max_visible(bottom_area, input_area, limit);

        // Assert
        assert_eq!(max_visible, expected);
    }
}

#[test]
fn test_format_review_markdown_compacts_sections_and_appends_apply_hint() {
    // Arrange
    let review_markdown =
        "## Review\n\n### Project Impact\n\n- None\n\n### Suggestions\n\n- Fix typo.";

    // Act
    let formatted = format_review_markdown(review_markdown);

    // Assert
    assert!(formatted.starts_with("## Review\n\n### Project Impact\n- None"));
    assert!(
        formatted.contains("### Suggestions (type \"/apply\" to verify and apply)\n- Fix typo.")
    );
    assert!(!formatted.contains("### Suggestions\n"));
}

#[test]
fn test_format_review_markdown_leaves_non_matching_lines_untouched() {
    // Arrange
    let review_markdown = "### Suggestions extra\n- keep as-is";

    // Act
    let formatted = format_review_markdown(review_markdown);

    // Assert
    assert_eq!(formatted, review_markdown);
}

#[test]
fn test_format_review_markdown_compacts_empty_suggestions_without_hint() {
    // Arrange
    let review_markdown = "## Review\n\n### Suggestions\n\n- None\n\n### Project Impact\n\n- None";

    // Act
    let formatted = format_review_markdown(review_markdown);

    // Assert
    assert_eq!(
        formatted,
        "## Review\n\n### Suggestions\n- None\n\n### Project Impact\n- None"
    );
    assert!(!formatted.contains("(type \"/apply\" to verify and apply)"));
}

#[test]
fn test_question_help_footer_line_switches_actions_by_focus() {
    // Arrange

    // Act
    let unchanged_chat_focus_line =
        question_help_footer_line(ChatFocus::Chat, false, false, QuestionLookupState::Closed);
    let changed_chat_focus_line =
        question_help_footer_line(ChatFocus::Chat, true, false, QuestionLookupState::Closed);
    let answer_focus_line =
        question_help_footer_line(ChatFocus::Input, false, false, QuestionLookupState::Closed);

    // Assert
    assert_eq!(
        unchanged_chat_focus_line.to_string(),
        "Tab: focus | j/k: scroll | q: sessions"
    );
    assert_eq!(
        changed_chat_focus_line.to_string(),
        "Tab: focus | j/k: scroll | d: diff | q: sessions"
    );
    assert_eq!(
        answer_focus_line.to_string(),
        "Tab: focus | Enter: send | Ctrl+C: end turn"
    );
}

#[test]
fn test_question_help_footer_line_shows_sessions_in_answer_focus_when_navigating_options() {
    // Arrange — answer focus while navigating predefined options. The
    // runtime accepts plain `q` as an exit-to-list shortcut here, so the
    // footer must surface it. The at-mention overlay variant is also
    // covered here so the footer surfaces `Esc` as a dedicated cancel
    // hint without advertising it as an end-turn shortcut.

    // Act
    let answer_focus_with_options =
        question_help_footer_line(ChatFocus::Input, false, true, QuestionLookupState::Closed)
            .to_string();
    let answer_focus_with_overlay =
        question_help_footer_line(ChatFocus::Input, false, false, QuestionLookupState::Matches)
            .to_string();

    // Assert
    assert_eq!(
        answer_focus_with_options,
        "Tab: focus | Enter: send | q: sessions | Ctrl+C: end turn"
    );
    assert_eq!(
        answer_focus_with_overlay,
        "Tab/Enter: select | Up/Down: navigate | Esc: cancel @ | Ctrl+C: end turn"
    );
}

#[test]
fn test_session_output_panel_borders_leave_full_inner_width_without_vertical_edges() {
    // Arrange & Act
    let inner_width = panel_inner_width(Rect::new(0, 0, 80, 5), session_output_panel_borders());
    let output_panel_borders = session_output_panel_borders();

    // Assert
    assert_eq!(inner_width, 80);
    assert!(!output_panel_borders.intersects(Borders::LEFT));
    assert!(!output_panel_borders.intersects(Borders::RIGHT));
}

#[test]
fn test_session_output_done_line_shows_continue_hint() {
    // Arrange

    // Act
    let done_line = session_output_done_line();

    // Assert
    assert_eq!(
        done_line.to_string(),
        "Press 'c' to continue in a new session."
    );
}

#[test]
fn test_session_output_status_lines_for_in_progress_include_progress_text() {
    // Arrange

    // Act
    let status_lines = session_output_status_lines(
        Status::InProgress,
        Some("Inspecting changed files"),
        None,
        None,
    );

    // Assert
    assert_eq!(status_lines.len(), 1);
    assert!(
        status_lines[0]
            .to_string()
            .contains("Working... Inspecting changed files")
    );
}

#[test]
fn test_session_output_status_lines_for_in_progress_use_committing_label() {
    // Arrange

    // Act
    let status_lines = session_output_status_lines(
        Status::InProgress,
        Some(COMMITTING_PROGRESS_LABEL),
        None,
        None,
    );

    // Assert
    assert_eq!(status_lines.len(), 1);
    assert!(
        status_lines[0]
            .to_string()
            .contains(COMMITTING_PROGRESS_LABEL)
    );
    assert!(
        !status_lines[0]
            .to_string()
            .contains(&format!("Working... {COMMITTING_PROGRESS_LABEL}"))
    );
}

#[test]
fn test_session_output_status_lines_for_agent_review_use_two_line_hierarchy() {
    // Arrange

    // Act
    let status_lines = session_output_status_lines(
        Status::AgentReview,
        None,
        Some("Reviewing changes\nCodex · gpt-5.6-sol · Extra-high reasoning · Fast"),
        None,
    );

    // Assert
    assert_eq!(status_lines.len(), 2);
    assert_eq!(
        status_lines[0].to_string(),
        format!("{} Reviewing changes", Icon::TachyonLoader)
    );
    assert_eq!(
        status_lines[1].to_string(),
        "    Codex · gpt-5.6-sol · Extra-high reasoning · Fast"
    );
    assert_eq!(
        status_lines[0].spans[0].style.fg,
        Some(style::status_color(Status::AgentReview))
    );
    assert_eq!(
        status_lines[1].spans[0].style.fg,
        Some(style::palette::text_muted())
    );
}

#[test]
fn test_session_output_status_lines_for_agent_review_use_generic_fallback() {
    // Arrange

    // Act
    let status_lines = session_output_status_lines(Status::AgentReview, None, None, None);

    // Assert
    assert_eq!(status_lines.len(), 1);
    assert!(status_lines[0].to_string().contains("Reviewing changes..."));
}

#[test]
fn test_session_output_status_lines_for_merging_use_status_label() {
    // Arrange

    // Act
    let status_lines = session_output_status_lines(Status::Merging, None, None, None);

    // Assert
    assert_eq!(status_lines.len(), 1);
    assert!(status_lines[0].to_string().contains("Merging..."));
}

#[test]
fn test_centered_content_rect_centers_requested_size() {
    // Arrange
    let area = Rect::new(10, 5, 40, 12);

    // Act
    let centered_area = centered_content_rect(area, 20, 5);

    // Assert
    assert_eq!(centered_area, Rect::new(20, 8, 20, 5));
}

#[test]
fn test_centered_content_rect_clamps_requested_size_to_available_area() {
    // Arrange
    let area = Rect::new(3, 2, 12, 4);

    // Act
    let centered_area = centered_content_rect(area, 20, 6);

    // Assert
    assert_eq!(centered_area, area);
}

#[test]
fn test_tab_page_areas_start_content_at_top_and_keep_footer_inset() {
    // Arrange
    let area = Rect::new(5, 3, 40, 10);

    // Act
    let areas = tab_page_areas(area);

    // Assert
    assert_eq!(areas.main_area, Rect::new(6, 3, 38, 8));
    assert_eq!(areas.footer_area, Rect::new(6, 11, 38, 1));
}

#[test]
fn test_calculate_input_height() {
    // Arrange & Act & Assert
    assert_eq!(calculate_input_height(20, ""), 3);
    assert_eq!(calculate_input_height(12, "1234567"), 4);
    assert_eq!(calculate_input_height(12, "12345678"), 4);
    assert_eq!(calculate_input_height(12, "12345671234567890"), 5);
    assert_eq!(calculate_input_height(12, &"a".repeat(120)), 12);
}

#[test]
fn test_question_panel_layout_preserves_input_height_before_trimming_question() {
    // Arrange
    let question = "Need a detailed migration plan with rollback guidance?";
    let input = "Use two phases.";

    // Act
    let panel_layout = question_panel_layout(28, 4, question, input, 10);

    // Assert
    assert_eq!(
        panel_layout,
        QuestionPanelLayout {
            help_height: 1,
            input_height: 3,
            question_height: 0,
            spacer_height: 0,
        }
    );
}

#[test]
fn test_question_panel_layout_uses_wrapped_question_height_when_space_allows() {
    // Arrange
    let question = "Need a detailed migration plan with rollback guidance?";
    let input = "Use two phases.";

    // Act
    let panel_layout = question_panel_layout(28, 9, question, input, 10);

    // Assert — question_height includes +1 for the "Question N/M" title
    // line.
    assert_eq!(
        panel_layout,
        QuestionPanelLayout {
            help_height: 1,
            input_height: 3,
            question_height: 3,
            spacer_height: 1,
        }
    );
}

#[test]
fn test_question_panel_layout_drops_spacer_before_last_question_line() {
    // Arrange
    let question = "Need a detailed migration plan with rollback guidance?";
    let input = "Use two phases.";

    // Act
    let panel_layout = question_panel_layout(28, 6, question, input, 10);

    // Assert — question_height includes +1 for the "Question N/M" title
    // line but the spacer is dropped when space is tight.
    assert_eq!(
        panel_layout,
        QuestionPanelLayout {
            help_height: 1,
            input_height: 3,
            question_height: 2,
            spacer_height: 0,
        }
    );
}

#[test]
fn test_question_panel_reserved_height_includes_options_and_input_rows() {
    // Arrange
    let question = "Continue?";
    let input = "";

    // Act
    let reserved_height = question_panel_reserved_height(40, 12, question, input, 2, 10);

    // Assert
    assert_eq!(reserved_height, 10);
}

#[test]
fn test_question_panel_areas_assign_expected_rows() {
    // Arrange
    let area = Rect::new(4, 10, 40, 10);
    let question = "Continue?";
    let input = "";

    // Act
    let panel_areas = question_panel_areas(area, question, input, 2, 10);

    // Assert
    assert_eq!(panel_areas.question_area, Rect::new(4, 10, 40, 2));
    assert_eq!(panel_areas.options_area, Rect::new(4, 12, 40, 3));
    assert_eq!(panel_areas.spacer_area, Rect::new(4, 15, 40, 1));
    assert_eq!(panel_areas.input_area, Rect::new(4, 16, 40, 3));
    assert_eq!(panel_areas.help_area, Rect::new(4, 19, 40, 1));
}

#[test]
fn test_question_options_height_adds_header_and_clamps() {
    // Arrange & Act & Assert
    assert_eq!(question_options_height(0, 10), 0);
    assert_eq!(question_options_height(2, 10), 3);
    assert_eq!(question_options_height(5, 3), 3);
}

#[test]
fn test_calculate_input_viewport_without_scroll() {
    // Arrange
    let total_line_count = 4;
    let cursor_y = 2;
    let viewport_height = 10;

    // Act
    let (scroll_offset, cursor_row) =
        calculate_input_viewport(total_line_count, cursor_y, viewport_height);

    // Assert
    assert_eq!(scroll_offset, 0);
    assert_eq!(cursor_row, 2);
}

#[test]
fn test_calculate_input_viewport_with_scroll() {
    // Arrange
    let total_line_count = 20;
    let cursor_y = 15;
    let viewport_height = 10;

    // Act
    let (scroll_offset, cursor_row) =
        calculate_input_viewport(total_line_count, cursor_y, viewport_height);

    // Assert
    assert_eq!(scroll_offset, 6);
    assert_eq!(cursor_row, 9);
}

#[test]
fn test_calculate_input_viewport_clamps_cursor_to_last_line() {
    // Arrange
    let total_line_count = 3;
    let cursor_y = 10;
    let viewport_height = 2;

    // Act
    let (scroll_offset, cursor_row) =
        calculate_input_viewport(total_line_count, cursor_y, viewport_height);

    // Assert
    assert_eq!(scroll_offset, 1);
    assert_eq!(cursor_row, 1);
}

#[test]
fn test_overlay_area_above_clamps_to_available_height() {
    // Arrange
    let container_area = Rect::new(0, 5, 30, 12);
    let anchor_area = Rect::new(2, 9, 20, 3);

    // Act
    let overlay_area = overlay_area_above(container_area, anchor_area, 6);

    // Assert
    assert_eq!(overlay_area, Some(Rect::new(2, 5, 20, 4)));
}

#[test]
fn test_overlay_area_above_returns_none_without_space() {
    // Arrange
    let container_area = Rect::new(0, 5, 30, 12);
    let anchor_area = Rect::new(2, 5, 20, 3);

    // Act
    let overlay_area = overlay_area_above(container_area, anchor_area, 2);

    // Assert
    assert_eq!(overlay_area, None);
}

#[test]
fn test_panel_inner_width_excludes_horizontal_borders() {
    // Arrange
    let area = Rect::new(0, 0, 10, 5);

    // Act
    let inner_width = panel_inner_width(area, Borders::LEFT | Borders::RIGHT);

    // Assert
    assert_eq!(inner_width, 8);
}

#[test]
fn test_bottom_pinned_scroll_offset_uses_inner_height() {
    // Arrange
    let area = Rect::new(0, 0, 20, 5);

    // Act
    let scroll_offset = bottom_pinned_scroll_offset(area, Borders::TOP | Borders::BOTTOM, 6, None);

    // Assert
    assert_eq!(scroll_offset, 3);
}

#[test]
fn test_bottom_pinned_scroll_offset_preserves_explicit_value() {
    // Arrange
    let area = Rect::new(0, 0, 20, 5);

    // Act
    let scroll_offset =
        bottom_pinned_scroll_offset(area, Borders::TOP | Borders::BOTTOM, 6, Some(2));

    // Assert
    assert_eq!(scroll_offset, 2);
}

#[test]
fn test_placeholder_cursor_position_accounts_for_border_and_prompt_prefix() {
    // Arrange
    let area = Rect::new(10, 5, 40, 4);

    // Act
    let cursor_position = placeholder_cursor_position(area);

    // Assert
    assert_eq!(cursor_position, (14, 6));
}

#[test]
fn test_input_cursor_position_accounts_for_border_and_offsets() {
    // Arrange
    let area = Rect::new(10, 5, 40, 6);
    let cursor_x = 7;
    let cursor_row = 2;

    // Act
    let cursor_position = input_cursor_position(area, cursor_x, cursor_row);

    // Assert
    assert_eq!(cursor_position, (18, 8));
}

#[test]
fn test_suggestion_dropdown_height_includes_border_lines() {
    // Arrange
    let option_count = 4;

    // Act
    let dropdown_height = suggestion_dropdown_height(option_count);

    // Assert
    assert_eq!(dropdown_height, 6);
}

#[test]
fn test_suggestion_dropdown_height_saturates_at_u16_max() {
    // Arrange
    let option_count = usize::MAX;

    // Act
    let dropdown_height = suggestion_dropdown_height(option_count);

    // Assert
    assert_eq!(dropdown_height, u16::MAX);
}

#[test]
fn test_compute_input_layout_empty() {
    // Arrange
    let input = "";
    let width = 20;

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(cursor_x, 3); // prefix " › "
    assert_eq!(cursor_y, 0);
}

#[test]
fn test_compute_input_layout_single_line() {
    // Arrange
    let input = "test";
    let width = 20;
    let cursor = input.chars().count();

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(cursor_x, 7); // 3 (prefix) + 4 (text)
    assert_eq!(cursor_y, 0);
}

#[test]
fn test_compute_input_layout_exact_fit() {
    // Arrange
    let input = "1234567";
    let width = 12; // Inner width 10, Prefix 3, Available 7
    let cursor = input.chars().count();

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].width(), 10);
    assert_eq!(cursor_x, 3);
    assert_eq!(cursor_y, 1);
}

#[test]
fn test_compute_input_layout_wrap() {
    // Arrange
    let input = "12345678";
    let width = 12;
    let cursor = input.chars().count();

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].width(), 10);
    assert_eq!(lines[1].width(), 4);
    assert_eq!(lines[1].to_string(), "   8");
    assert_eq!(cursor_x, 4);
    assert_eq!(cursor_y, 1);
}

#[test]
fn test_compute_input_layout_wraps_whole_words_when_they_fit_on_next_line() {
    // Arrange
    let input = "abc def";
    let width = 11;
    let cursor = input.chars().count();

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), " › abc ");
    assert_eq!(lines[1].to_string(), "   def");
    assert_eq!(cursor_x, 6);
    assert_eq!(cursor_y, 1);
}

#[test]
fn test_compute_input_layout_multiline_exact_fit() {
    // Arrange
    let input = "1234567".to_owned() + "1234567890";
    let width = 12;
    let cursor = input.chars().count();

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(&input, width, cursor);

    // Assert
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].width(), 10);
    assert_eq!(lines[1].width(), 10);
    assert_eq!(lines[2].width(), 6);
    assert_eq!(cursor_x, 6);
    assert_eq!(cursor_y, 2);
}

#[test]
fn test_compute_input_layout_cursor_at_start() {
    // Arrange
    let input = "hello";
    let width = 20;

    // Act
    let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

    // Assert — cursor sits right after prefix
    assert_eq!(cursor_x, 3);
    assert_eq!(cursor_y, 0);
}

#[test]
fn test_compute_input_layout_cursor_in_middle() {
    // Arrange
    let input = "hello";
    let width = 20;

    // Act
    let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 2);

    // Assert — prefix(3) + 2 chars
    assert_eq!(cursor_x, 5);
    assert_eq!(cursor_y, 0);
}

#[test]
fn test_compute_input_layout_cursor_before_wrapped_char() {
    // Arrange
    let input = "12345678";
    let width = 12;

    // Act
    let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 7);

    // Assert
    assert_eq!(cursor_x, 3);
    assert_eq!(cursor_y, 1);
}

#[test]
fn test_compute_input_layout_cursor_moves_to_next_line_before_wrapped_word() {
    // Arrange
    let input = "abc def";
    let width = 11;

    // Act
    let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 4);

    // Assert
    assert_eq!(cursor_x, 3);
    assert_eq!(cursor_y, 1);
}

#[test]
fn test_move_input_cursor_up_on_wrapped_layout() {
    // Arrange
    let input = "12345678";
    let width = 12;
    let cursor = input.chars().count();

    // Act
    let cursor = move_input_cursor_up(input, width, cursor);

    // Assert
    assert_eq!(cursor, 1);
}

#[test]
fn test_move_input_cursor_down_on_wrapped_layout() {
    // Arrange
    let input = "12345678";
    let width = 12;
    let cursor = 1;

    // Act
    let cursor = move_input_cursor_down(input, width, cursor);

    // Assert
    assert_eq!(cursor, input.chars().count());
}

#[test]
fn test_compute_input_layout_explicit_newline() {
    // Arrange
    let input = "ab\ncd";
    let width = 20;
    let cursor = input.chars().count();

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].to_string(), "   cd");
    assert_eq!(cursor_x, 5); // continuation padding + "cd"
    assert_eq!(cursor_y, 1);
}

#[test]
fn test_compute_input_layout_multiple_newlines() {
    // Arrange
    let input = "a\n\nb";
    let width = 20;
    let cursor = input.chars().count();

    // Act
    let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

    // Assert — 3 lines: "a", padded continuation line, "b"
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1].to_string(), "   ");
    assert_eq!(cursor_x, 4);
    assert_eq!(cursor_y, 2);
}

#[test]
fn test_compute_input_layout_cursor_on_second_line() {
    // Arrange — cursor right after the newline
    let input = "ab\ncd";
    let width = 20;

    // Act — cursor at char index 3 = 'a','b','\n' -> position 0 of second
    // line
    let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 3);

    // Assert
    assert_eq!(cursor_x, 3);
    assert_eq!(cursor_y, 1);
}

#[test]
fn test_first_table_column_width_uses_remaining_layout_space() {
    // Arrange
    let constraints = [
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(6),
    ];

    // Act
    let width = first_table_column_width(50, &constraints, 1, 3);

    // Assert
    assert_eq!(width, 21);
}

#[test]
fn test_compute_input_layout_at_mention_highlighting() {
    // Arrange
    let input = "hello @file world";
    let width = 40;

    // Act
    let (lines, _, _) = compute_input_layout(input, width, 0);

    // Assert
    let line = &lines[0];
    let spans = &line.spans;

    // Prefix is at index 0: " › "
    assert_eq!(spans[0].content, " › ");

    // "hello " (indices 1..7) should be normal style
    for span in spans.iter().take(7).skip(1) {
        assert_eq!(span.style.fg, None);
    }

    // "@file" (indices 7..12) should be highlighted
    for span in spans.iter().take(12).skip(7) {
        assert_eq!(span.style.fg, Some(style::palette::info()));
    }

    // " world" (indices 12..18) should be normal style
    for span in spans.iter().take(18).skip(12) {
        assert_eq!(span.style.fg, None);
    }
}

#[test]
fn test_compute_input_layout_at_mention_highlighting_stops_before_trailing_comma() {
    // Arrange
    let input = "hello @file, world";
    let width = 40;

    // Act
    let (lines, _, _) = compute_input_layout(input, width, 0);

    // Assert
    let line = &lines[0];
    let spans = &line.spans;

    for span in spans.iter().take(12).skip(7) {
        assert_eq!(span.style.fg, Some(style::palette::info()));
    }
    assert_eq!(spans[12].content, ",");
    assert_eq!(spans[12].style.fg, None);
}

#[test]
fn test_compute_input_layout_at_mention_highlighting_stops_before_trailing_parenthesis() {
    // Arrange
    let input = "hello @file) world";
    let width = 40;

    // Act
    let (lines, _, _) = compute_input_layout(input, width, 0);

    // Assert
    let line = &lines[0];
    let spans = &line.spans;

    for span in spans.iter().take(12).skip(7) {
        assert_eq!(span.style.fg, Some(style::palette::info()));
    }
    assert_eq!(spans[12].content, ")");
    assert_eq!(spans[12].style.fg, None);
}

#[test]
fn test_compute_input_layout_highlights_parenthesized_at_mention() {
    // Arrange
    let input = "review (@src/main.rs)";
    let width = 40;

    // Act
    let (lines, _, _) = compute_input_layout(input, width, 0);

    // Assert
    let line = &lines[0];
    let spans = &line.spans;

    for span in spans.iter().take(21).skip(9) {
        assert_eq!(span.style.fg, Some(style::palette::info()));
    }
    assert_eq!(spans[21].content, ")");
    assert_eq!(spans[21].style.fg, None);
}

#[test]
fn test_compute_input_layout_no_highlight_for_email() {
    // Arrange
    let input = "email@example.com";
    let width = 40;

    // Act
    let (lines, _, _) = compute_input_layout(input, width, 0);

    // Assert
    let line = &lines[0];
    let spans = &line.spans;

    for span in spans.iter().skip(1) {
        assert_eq!(span.style.fg, None);
    }
}

#[test]
fn test_compute_input_layout_highlights_image_placeholder_tokens() {
    // Arrange
    let input = "Review [Image #12] before merge";
    let width = 60;

    // Act
    let (lines, _, _) = compute_input_layout(input, width, 0);

    // Assert
    let line = &lines[0];
    let spans = &line.spans;
    let image_start = 1 + "Review ".chars().count();
    let image_end = image_start + "[Image #12]".chars().count();

    for span in spans.iter().take(image_end).skip(image_start) {
        assert_eq!(span.style.fg, Some(style::palette::warning()));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn test_compute_input_layout_does_not_highlight_invalid_image_like_tokens() {
    // Arrange
    let input = "Review [Image #] before merge";
    let width = 60;

    // Act
    let (lines, _, _) = compute_input_layout(input, width, 0);

    // Assert
    let line = &lines[0];
    let spans = &line.spans;
    let token_start = 1 + "Review ".chars().count();
    let token_end = token_start + "[Image #]".chars().count();

    for span in spans.iter().take(token_end).skip(token_start) {
        assert_eq!(span.style.fg, None);
    }
}
