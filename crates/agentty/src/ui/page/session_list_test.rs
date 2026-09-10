use std::collections::HashMap;

use ratatui::layout::Constraint;
use ratatui::widgets::TableState;

use super::{
    EMPTY_SESSIONS_HINT, ROW_HIGHLIGHT_SYMBOL, SessionListPage, TABLE_COLUMN_SPACING, column_width,
    model_column_width, prepare_grouped_table_state, prepared_session_rows, reasoning_level_color,
    selected_render_row, selected_session_id, session_list_help_line, size_color,
    status_column_width, timer_column_width, tree_position_label,
};
use crate::app::session_state::SessionGitStatus;
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionSize, Status,
};
use crate::domain::session_order::SessionTreePosition;
use crate::domain::theme::ColorTheme;
use crate::ui::render::Page;
use crate::ui::style;

/// Flattens a rendered test buffer into a plain string for assertions.
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

/// Splits a rendered test buffer into terminal rows.
fn buffer_lines(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    let width = usize::from(buffer.area.width.max(1));

    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
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
    let first_message =
        crate::ui::page::fyi::rotating_message(crate::ui::page::fyi::session_list_messages(), 0);
    let second_message =
        crate::ui::page::fyi::rotating_message(crate::ui::page::fyi::session_list_messages(), 1);
    let third_message =
        crate::ui::page::fyi::rotating_message(crate::ui::page::fyi::session_list_messages(), 2);
    let fourth_message =
        crate::ui::page::fyi::rotating_message(crate::ui::page::fyi::session_list_messages(), 3);
    let fifth_message =
        crate::ui::page::fyi::rotating_message(crate::ui::page::fyi::session_list_messages(), 4);
    let wrapped_message =
        crate::ui::page::fyi::rotating_message(crate::ui::page::fyi::session_list_messages(), 5);

    // Assert
    assert_eq!(
        first_message,
        Some("Use list sync before starting work when you need the newest base branch.")
    );
    assert_eq!(
        second_message,
        Some("Sessions are grouped as merge queue, active work, then archive."),
    );
    assert_eq!(
        third_message,
        Some("Session timers count active agent work and freeze between turns.")
    );
    assert_eq!(
        fourth_message,
        Some("Forge badges show whether review requests are open, merged, or closed.")
    );
    assert_eq!(
        fifth_message,
        Some(
            "Session launch configurations run through tmux and use the configured Settings \
             entries."
        )
    );
    assert_eq!(
        wrapped_message,
        Some("Use list sync before starting work when you need the newest base branch.")
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
    let sessions = vec![crate::test_support::titled_session_fixture(
        "new-1",
        Status::Draft,
    )];

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
fn test_render_empty_session_list_shows_creation_hint_without_group_labels() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    let sessions = Vec::new();

    // Act
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                .render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains(EMPTY_SESSIONS_HINT));
    assert!(!text.contains("MERGE QUEUE"));
    assert!(!text.contains("ACTIVE"));
    assert!(!text.contains("ARCHIVE"));
}

#[test]
fn test_render_group_labels_show_counts_with_spacing_between_sections() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 18);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(1));
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                .render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let lines = buffer_lines(terminal.backend().buffer());
    let merge_queue_row = lines
        .iter()
        .position(|line| line.contains(" MERGE QUEUE —— 1"))
        .expect("merge queue label should be visible");
    let active_row = lines
        .iter()
        .position(|line| line.contains(" ACTIVE —— 1"))
        .expect("active label should be visible");
    let archive_row = lines
        .iter()
        .position(|line| line.contains(" ARCHIVE —— 1"))
        .expect("archive label should be visible");
    let is_blank_table_row = |line: &str| {
        line.trim_matches(|character| matches!(character, ' ' | '│'))
            .is_empty()
    };

    assert!(!is_blank_table_row(&lines[merge_queue_row - 1]));
    assert!(is_blank_table_row(&lines[active_row - 1]));
    assert!(is_blank_table_row(&lines[archive_row - 1]));
}

#[test]
fn test_render_conflicted_session_appends_red_title_alert() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
    let backend = ratatui::backend::TestBackend::new(120, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut session = crate::test_support::titled_session_fixture("conflict-1", Status::Review);
    session.title = Some("Update shared config".to_string());
    let session_id = session.id.clone();
    let sessions = vec![session];
    let session_git_statuses = HashMap::from([(
        session_id,
        SessionGitStatus {
            base_status: Some((1, 1)),
            has_merge_conflict: Some(true),
            remote_status: None,
        },
    )]);

    // Act
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                .session_git_statuses(&session_git_statuses)
                .render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let conflict_cell = find_text_start_cell(terminal.backend().buffer(), "[merge conflict]")
        .expect("merge conflict alert should be visible");
    assert_eq!(conflict_cell.fg, style::palette::danger());
}

#[test]
fn test_render_archive_rows_use_muted_text_across_columns() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::DarkHorizon);
    let backend = ratatui::backend::TestBackend::new(120, 16);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut active_session =
        crate::test_support::titled_session_fixture("active-1", Status::Review);
    active_session.title = Some("Active session title".to_string());
    active_session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    active_session.reasoning_level_override = Some(ReasoningLevel::Low);
    let mut archived_session =
        crate::test_support::titled_session_fixture("archive-1", Status::Done);
    archived_session.title = Some("Archived session title".to_string());
    archived_session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Claude,
        AgentModel::ClaudeSonnet5,
    );
    archived_session.reasoning_level_override = Some(ReasoningLevel::High);
    archived_session.size = SessionSize::Xxl;
    let sessions = vec![active_session, archived_session];

    // Act
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                .render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let active_title_cell = find_text_start_cell(buffer, "Active session title")
        .expect("active title should be visible");
    let archived_title_cell = find_text_start_cell(buffer, "Archived session title")
        .expect("archived title should be visible");
    let archived_size_cell =
        find_text_start_cell(buffer, "[XXL]").expect("archived size should be visible");
    let archived_model_cell =
        find_text_start_cell(buffer, "claude-sonnet-5").expect("archived model should be visible");
    let archived_reasoning_cell =
        find_text_start_cell(buffer, "high").expect("archived reasoning should be visible");
    let archived_status_cell =
        find_text_start_cell(buffer, "Done").expect("archived status should be visible");

    assert_eq!(active_title_cell.fg, style::palette::text());
    for archived_cell in [
        archived_title_cell,
        archived_size_cell,
        archived_model_cell,
        archived_reasoning_cell,
        archived_status_cell,
    ] {
        assert_eq!(archived_cell.fg, style::palette::text_muted());
    }
}

#[test]
fn test_selected_render_row_maps_original_selection_to_grouped_index() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("merge-1", Status::Merging),
        crate::test_support::titled_session_fixture("active-2", Status::Draft),
    ];
    let rows = prepared_session_rows(&sessions, ReasoningLevel::default(), None, 0);
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
    let width = column_width(
        "Project",
        project_names
            .into_iter()
            .map(|project_name| project_name.chars().count()),
    );

    // Assert
    assert_eq!(width, Constraint::Length(expected_width));
}

#[test]
fn test_model_column_width_uses_longest_model_value() {
    // Arrange
    let expected_width = u16::try_from("claude-sonnet-5 [low]".chars().count()).unwrap_or(u16::MAX);
    let mut default_session =
        crate::test_support::titled_session_fixture("active-1", Status::Review);
    default_session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Claude,
        AgentModel::ClaudeSonnet5,
    );
    default_session.reasoning_level_override = Some(ReasoningLevel::Low);
    let mut medium_session =
        crate::test_support::titled_session_fixture("active-2", Status::Review);
    medium_session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    medium_session.reasoning_level_override = Some(ReasoningLevel::Medium);
    let sessions = vec![default_session, medium_session];
    let rows = prepared_session_rows(&sessions, ReasoningLevel::Low, None, 0);

    // Act
    let width = model_column_width(&rows);

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
        (ReasoningLevel::Max, style::palette::danger()),
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
    let mut session = crate::test_support::titled_session_fixture("session-1", Status::Review);
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
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
    let model_cell = find_text_start_cell(buffer, "gpt-5.6-sol").unwrap_or(fallback_cell);
    let reasoning_cell = find_text_start_cell(buffer, "high").unwrap_or(fallback_cell);

    assert_eq!(model_cell.fg, style::palette::text());
    assert_eq!(reasoning_cell.fg, expected_reasoning_color);
}

#[test]
fn test_render_selected_session_model_uses_selection_surface() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::DarkHorizon);
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut session = crate::test_support::titled_session_fixture("session-1", Status::Review);
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    let sessions = vec![session];

    // Act
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::Medium, 0)
                .render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let fallback_cell = &buffer.content()[0];
    let model_cell = find_text_start_cell(buffer, "gpt-5.6-sol").unwrap_or(fallback_cell);

    assert_eq!(model_cell.bg, style::palette::surface_selection());
}

#[test]
fn test_render_session_row_shows_model_with_persisted_reasoning_level() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut session = crate::test_support::titled_session_fixture("session-1", Status::Review);
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    session.reasoning_level_override = Some(ReasoningLevel::Medium);
    let sessions = vec![session];

    // Act
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::XHigh, 0)
                .render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("gpt-5.6-sol [medium]"));
}

#[test]
fn test_render_session_row_connects_stacked_child_to_parent() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(1));
    let mut parent_session =
        crate::test_support::titled_session_fixture("parent-1", Status::Review);
    parent_session.title = Some("Parent session".to_string());
    let mut child_session = crate::test_support::titled_session_fixture("child-1", Status::Draft);
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
    assert_eq!(tree_cell.bg, style::palette::surface_selection());
    assert_ne!(tree_cell.fg, tree_cell.bg);
}

#[test]
fn test_tree_position_label_uses_middle_branch_for_nonfinal_child() {
    // Arrange
    let tree_position = SessionTreePosition::Child {
        depth: 2,
        is_last: false,
    };

    // Act
    let label = tree_position_label(tree_position);

    // Assert
    assert_eq!(label, "  ├ ");
}

#[test]
fn test_render_session_rows_indent_to_stack_depth_five() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(5));
    let sessions = (0..=5)
        .map(|depth| {
            let mut session = crate::test_support::titled_session_fixture(
                &format!("level-{depth}"),
                Status::Review,
            );
            session.title = Some(if depth == 0 {
                "Stack root".to_string()
            } else {
                format!("Stack level {depth}")
            });
            if depth > 0 {
                session.parent_session_id = Some(format!("level-{}", depth - 1).into());
            }

            session
        })
        .collect::<Vec<_>>();

    // Act
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::default(), 0)
                .render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Stack root"));
    assert!(text.contains("└ [XS] Stack level 1"));
    assert!(text.contains("  └ [XS] Stack level 2"));
    assert!(text.contains("    └ [XS] Stack level 3"));
    assert!(text.contains("      └ [XS] Stack level 4"));
    assert!(text.contains("        └ [XS] Stack level 5"));
}

#[test]
fn test_render_session_row_shows_model_with_override_reasoning_level() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut session = crate::test_support::titled_session_fixture("session-1", Status::Review);
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
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
    assert!(text.contains("gpt-5.6-sol [high]"));
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
fn test_status_column_uses_visible_orchestration_phase_with_forge_indicator() {
    // Arrange
    let mut session = crate::test_support::titled_session_fixture("session-1", Status::Review);
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
    let expected_title = session.display_title().to_string();
    session.orchestration_progress = Some(
        "Phase: Running\nParallel workers: 3 (global setting)\n- protocol: running".to_string(),
    );
    let sessions = vec![session];
    let expected_label = "Phase: Running ⊙ #42";
    let expected_width = u16::try_from(expected_label.chars().count()).unwrap_or(u16::MAX);
    let backend = ratatui::backend::TestBackend::new(120, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let rows = prepared_session_rows(&sessions, ReasoningLevel::High, None, 0);

    // Act
    let width = status_column_width(&rows);
    terminal
        .draw(|frame| {
            SessionListPage::new(&sessions, &mut table_state, ReasoningLevel::High, 0)
                .render(frame, frame.area());
        })
        .expect("failed to render session list");
    let rendered = buffer_text(terminal.backend().buffer());

    // Assert
    assert_eq!(width, Constraint::Length(expected_width));
    assert!(rendered.contains(expected_label));
    assert!(rendered.contains(&expected_title));
}

#[test]
fn test_timer_column_width_uses_longest_rendered_timer_label() {
    // Arrange
    let mut active_session =
        crate::test_support::titled_session_fixture("active-1", Status::InProgress);
    active_session.in_progress_started_at = Some(100);
    active_session.in_progress_total_seconds = 60;
    let mut archived_session = crate::test_support::titled_session_fixture("done-1", Status::Done);
    archived_session.in_progress_total_seconds = 3_661;
    let sessions = vec![active_session, archived_session];
    let expected_width = u16::try_from("1h 1m 1s".chars().count()).unwrap_or(u16::MAX);
    let rows = prepared_session_rows(&sessions, ReasoningLevel::default(), None, 160);

    // Act
    let width = timer_column_width(&rows);

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
    let mut session = crate::test_support::titled_session_fixture("new-1", Status::Draft);
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
    let session = crate::test_support::titled_session_fixture("session-1", Status::Review);

    // Act
    let help_text = session_list_help_line(Some(&session)).to_string();

    // Assert
    assert!(help_text.contains("s: sync"));
}

#[test]
fn test_session_list_help_line_includes_project_switcher() {
    // Arrange
    let session = crate::test_support::titled_session_fixture("session-1", Status::Review);

    // Act
    let help_text = session_list_help_line(Some(&session)).to_string();

    // Assert
    assert!(help_text.contains("p: projects"));
}

#[test]
fn test_session_list_help_line_hides_cancel_for_regular_new_session() {
    // Arrange
    let session = crate::test_support::titled_session_fixture("session-1", Status::Draft);

    // Act
    let help_text = session_list_help_line(Some(&session)).to_string();

    // Assert
    assert!(!help_text.contains("c: cancel"));
}

#[test]
fn test_session_list_help_line_includes_cancel_for_draft_session() {
    // Arrange
    let mut session = crate::test_support::titled_session_fixture("session-1", Status::Draft);
    session.is_draft = true;

    // Act
    let help_text = session_list_help_line(Some(&session)).to_string();

    // Assert
    assert!(help_text.contains("c: cancel"));
}

#[test]
fn test_session_list_help_line_includes_open_for_canceled_session() {
    // Arrange
    let session = crate::test_support::titled_session_fixture("session-1", Status::Canceled);

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
    let mut session = crate::test_support::titled_session_fixture("active-1", Status::InProgress);
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
    assert!(text.contains("2m 0s"));
}

#[test]
fn test_render_shows_frozen_completed_timer_in_grouped_session_row() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut session = crate::test_support::titled_session_fixture("done-1", Status::Done);
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
    assert!(text.contains("2m 5s"));
}

#[test]
fn test_render_shows_full_agent_review_status_label() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let sessions = vec![crate::test_support::titled_session_fixture(
        "review-1",
        Status::AgentReview,
    )];

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
    let mut session = crate::test_support::titled_session_fixture("draft-1", Status::Draft);
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
    let sessions = vec![crate::test_support::titled_session_fixture(
        "new-1",
        Status::Draft,
    )];

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
    assert_eq!(new_cell.bg, style::palette::surface_selection());
    assert_ne!(new_cell.fg, new_cell.bg);
}
