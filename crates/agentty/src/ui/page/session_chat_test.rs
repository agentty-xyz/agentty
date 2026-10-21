use ag_session::{BuiltinAgentDefinition, test_support as model_fixture};
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::{
    CHAT_INPUT_MAX_PANEL_HEIGHT, QuestionPanelState, SessionChatLayoutInput, SessionChatLayoutPlan,
    SessionChatPage, SessionChatPageInput, non_prompt_bottom_height, render_question_panel,
    transcript_view_height,
};
use crate::domain::agent::ReasoningLevel;
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::domain::session::{Session, Status};
use crate::domain::session_message::SessionTranscript;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::help_action;
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashState,
};
use crate::presentation::task_chain_graph::{
    TaskChainGraph, TaskChainGraphNode, TaskChainNodeStatus,
};
use crate::ui::component::session_output::{SessionOutputLayoutCache, SessionOutputLineContext};
use crate::ui::{Page, layout, markdown, prompt_format, style};

fn session_fixture() -> Session {
    crate::test_support::SessionFixtureBuilder::new()
        .folder(std::env::temp_dir())
        .status(Status::Draft)
        .build()
}

/// Builds a default test page for one session and mode.
fn test_session_chat_page<'a>(session: &'a Session, mode: &'a AppMode) -> SessionChatPage<'a> {
    SessionChatPage::new(test_page_input(session, mode))
}

/// Builds default page inputs for one session and mode.
fn test_page_input<'a>(session: &'a Session, mode: &'a AppMode) -> SessionChatPageInput<'a> {
    SessionChatPageInput {
        active_prompt_output: None,
        active_progress: None,
        resources: None,
        host_cpu_temperature_celsius: None,
        frame_time: FrameTime::new(0, 0, 0),
        has_merge_conflict: false,
        markdown_render_cache: test_markdown_render_cache(),
        mode,
        output_layout_cache: test_output_layout_cache(),
        review_text: None,
        scroll_offset: None,
        session_index: 0,
        session_update_version: 0,
        sessions: std::slice::from_ref(session),
        task_chain_graph: None,
    }
}

/// Builds prompt mode with `input_text` staged in the composer.
fn prompt_mode(input_text: &str) -> AppMode {
    AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Chat,
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::with_text(input_text.to_string()),
        scroll_offset: None,
        session_id: "session-id".into(),
        slash_state: PromptSlashState::default(),
    }
}

/// Builds page geometry inputs for one session and mode.
fn layout_input<'a>(
    area: Rect,
    session: &'a Session,
    mode: &'a AppMode,
) -> SessionChatLayoutInput<'a> {
    SessionChatLayoutInput {
        area,
        has_merge_conflict: false,
        mode,
        review_text: None,
        session,
    }
}

#[test]
fn test_composer_margin_stays_outside_transcript_viewport() {
    // Arrange
    let mut session = session_fixture();
    let view_mode = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: Some(0),
    };
    let prompt_mode = prompt_mode("draft");

    for status in [Status::Draft, Status::InProgress, Status::Review] {
        session.status = status;
        for mode in [&view_mode, &prompt_mode] {
            for height in 0..=30 {
                // Act
                let plan = SessionChatLayoutPlan::new(layout_input(
                    Rect::new(0, 0, 80, height),
                    &session,
                    mode,
                ));

                // Assert
                let gap = plan.areas.bottom_area.y - plan.areas.output_area.bottom();
                assert!(gap <= 1);
                if height >= 20 {
                    assert_eq!(gap, 1);
                    assert_eq!(
                        plan.transcript_view_height(),
                        plan.areas.output_area.height - 1
                    );
                }
            }
        }
    }
}

#[test]
fn test_transcript_view_height_reclaims_inactive_composer_row() {
    // Arrange
    let session = session_fixture();
    let view_mode = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: None,
    };
    let empty_prompt_mode = prompt_mode("");

    for width in [0, 1, 40, 80] {
        for height in 0..=30 {
            let area = Rect::new(0, 0, width, height);

            // Act
            let view_height = transcript_view_height(layout_input(area, &session, &view_mode));
            let prompt_height =
                transcript_view_height(layout_input(area, &session, &empty_prompt_mode));

            // Assert
            assert!(
                view_height == prompt_height || view_height == prompt_height + 1,
                "width {width}, height {height}"
            );
            if width >= 40 && height >= 20 {
                assert_eq!(view_height, prompt_height + 1);
            }
        }
    }
}

#[test]
fn test_transcript_view_height_shrinks_for_multiline_prompt_draft() {
    // Arrange
    let session = session_fixture();
    let area = Rect::new(0, 0, 80, 30);
    let single_line_mode = prompt_mode("draft");
    let multiline_mode = prompt_mode("draft\nsecond line\nthird line");

    // Act
    let single_line_height =
        transcript_view_height(layout_input(area, &session, &single_line_mode));
    let multiline_height = transcript_view_height(layout_input(area, &session, &multiline_mode));

    // Assert
    assert_eq!(multiline_height, single_line_height.saturating_sub(2));
}

#[test]
fn test_transcript_view_height_shrinks_for_open_suggestion_dropdown() {
    // Arrange
    let session = session_fixture();
    let area = Rect::new(0, 0, 80, 30);
    let plain_mode = prompt_mode("draft");
    let slash_mode = prompt_mode("/");
    let mut armed_slash_mode = prompt_mode("/");
    if let AppMode::Prompt { slash_state, .. } = &mut armed_slash_mode {
        slash_state.is_plan_armed = true;
    }

    // Act
    let plain_height = transcript_view_height(layout_input(area, &session, &plain_mode));
    let slash_height = transcript_view_height(layout_input(area, &session, &slash_mode));
    let armed_height = transcript_view_height(layout_input(area, &session, &armed_slash_mode));

    // Assert
    assert!(
        slash_height < plain_height,
        "slash dropdown rows must shrink the transcript viewport: {slash_height} < {plain_height}"
    );
    assert_eq!(armed_height, plain_height);
}

/// Returns a leaked markdown cache for test page builders that need a
/// stable borrow across the page lifetime.
fn test_markdown_render_cache() -> &'static markdown::MarkdownRenderCache {
    Box::leak(Box::new(markdown::MarkdownRenderCache::default()))
}

/// Returns a leaked output-layout cache for test page builders that need a
/// stable borrow across the page lifetime.
fn test_output_layout_cache() -> &'static SessionOutputLayoutCache {
    Box::leak(Box::new(SessionOutputLayoutCache::default()))
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

fn buffer_row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
    let start = usize::from(row) * usize::from(width);
    let end = start + usize::from(width);

    buffer.content()[start..end]
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

/// Verifies the chat footer advertises continuation for completed
/// sessions.
#[test]
fn test_render_done_session_shows_continue_footer_action() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Done;
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw done session");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("c: continue"));
    assert!(!text.contains("comments"));
}

/// Verifies canceled terminal sessions advertise continuation in the chat
/// footer.
#[test]
fn test_render_canceled_session_shows_continue_footer_action() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Canceled;
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw canceled session");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("c: continue"));
    assert!(!text.contains("comments"));
}

/// Verifies running sessions advertise sync as a queued action while
/// keeping the running-turn stop action visible.
#[test]
fn test_render_in_progress_session_shows_sync_and_stop_footer_actions() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw in-progress session");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("r: sync"));
    assert!(text.contains("Ctrl+c: stop"));
}

#[test]
fn test_status_bar_fyi_rotates_between_session_chat_messages() {
    // Arrange
    let actual_messages = crate::ui::page::fyi::session_chat_messages();

    // Act
    let wrapped_message = crate::ui::page::fyi::rotating_message(
        actual_messages,
        u64::try_from(actual_messages.len()).unwrap_or_default(),
    );

    // Assert
    assert_eq!(actual_messages.len(), 9);
    assert_eq!(
        wrapped_message,
        Some("Queued replies run one by one after the active turn finishes.")
    );
}

#[test]
fn test_rendered_output_line_count_counts_wrapped_content() {
    // Arrange
    let mut session = session_fixture();
    session.transcript = Some(crate::test_support::assistant_transcript(
        "word ".repeat(40),
    ));
    let raw_line_count = u16::try_from(
        session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .unwrap_or_default()
            .lines()
            .count(),
    )
    .unwrap_or(u16::MAX);
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();

    // Act
    let rendered_line_count = SessionChatPage::rendered_output_line_count(
        &session,
        20,
        5,
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress: None,
            session_update_version: 0,
            task_chain_graph: None,
        },
        &markdown_render_cache,
        &output_layout_cache,
    );

    // Assert
    assert!(rendered_line_count > raw_line_count);
}

#[test]
fn test_review_text_reads_snapshot_for_prompt_output() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::default(),
        slash_state: PromptSlashState::default(),
        session_id: "session-id".into(),
        input: InputState::default(),
        scroll_offset: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    page.review_text = Some("Focused review");

    // Act
    let review_text = page.review_text;

    // Assert
    assert_eq!(review_text, Some("Focused review"));
}

#[test]
fn test_review_text_reads_snapshot_for_question_output() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem::new("Need tests?")],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    page.review_text = Some("Focused review");

    // Act
    let review_text = page.review_text;

    // Assert
    assert_eq!(review_text, Some("Focused review"));
}

#[test]
fn test_rendered_output_line_count_includes_question_review_output() {
    // Arrange
    let mut session = session_fixture();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let without_review = SessionChatPage::rendered_output_line_count(
        &session,
        40,
        5,
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress: None,
            session_update_version: 0,
            task_chain_graph: None,
        },
        &markdown_render_cache,
        &output_layout_cache,
    );
    session
        .transient_messages
        .upsert(crate::domain::transient_message::TransientMessage {
            anchor: crate::domain::transient_message::TransientMessageAnchor::AfterCompletedTurn,
            body: crate::domain::transient_message::TransientMessageBody::Markdown(
                "## Review\n\n- Finding".to_string(),
            ),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::ClearOnNewTurn,
            slot: crate::domain::transient_message::TransientMessageSlot::Review,
            turn_position: None,
        });

    // Act
    let with_review = SessionChatPage::rendered_output_line_count(
        &session,
        40,
        5,
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress: None,
            session_update_version: 0,
            task_chain_graph: None,
        },
        &markdown_render_cache,
        &output_layout_cache,
    );

    // Assert
    assert!(with_review > without_review);
}

/// Renders prompt mode for one session and returns the painted text.
fn rendered_prompt_mode_text(session: &Session) -> String {
    let mode = prompt_mode("draft");
    let mut page = test_session_chat_page(session, &mode);
    let backend = ratatui::backend::TestBackend::new(80, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw prompt mode");

    buffer_text(terminal.backend().buffer())
}

#[test]
fn test_render_session_view_keeps_settings_in_an_inactive_composer() {
    for status in [Status::Draft, Status::InProgress, Status::Review] {
        // Arrange
        let mut session = session_fixture();
        session.agent = model_fixture::codex_selection();
        session.status = status;
        let mode = AppMode::View {
            session_id: session.id.clone(),
            scroll_offset: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let width = 80;
        let height = 14;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw session view");

        // Assert
        let buffer = terminal.backend().buffer();
        let settings = format!(
            "codex/{} [high] · Balanced · Normal · Auto Edit",
            model_fixture::CODEX_MODEL_ID
        );
        let composer_row = (0..height)
            .find(|row| buffer_row_text(buffer, *row, width).contains(&settings))
            .expect("session settings should remain visible");
        let border = &buffer[(1, composer_row)];
        assert_eq!(border.symbol(), "╭");
        assert_eq!(border.fg, style::palette::border());
        assert!(!buffer_text(buffer).contains("Type your message"));
        assert_eq!(composer_row, height - 3);
        assert_eq!(buffer[(1, composer_row + 1)].symbol(), "╰");
        assert!(!buffer_text(buffer).contains("›"));
        assert!(buffer_row_text(buffer, height - 1, width).contains("q: back"));
    }
}

#[test]
fn test_archived_session_reclaims_composer_space_for_transcript() {
    // Arrange
    let mut session = session_fixture();
    let mode = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: None,
    };
    let area = Rect::new(0, 0, 80, 30);
    let active_height = transcript_view_height(layout_input(area, &session, &mode));

    for status in [Status::Done, Status::Canceled] {
        session.status = status;

        // Act
        let archived_height = transcript_view_height(layout_input(area, &session, &mode));

        // Assert
        assert_eq!(archived_height, active_height + 2);
    }
}

#[test]
fn test_render_archived_session_keeps_transcript_and_navigation_without_composer() {
    // Arrange
    for status in [Status::Done, Status::Canceled] {
        let mut session = session_fixture();
        session.status = status;
        session.transcript = Some(crate::test_support::assistant_transcript(
            "Archived conversation remains readable.",
        ));
        session.in_progress_total_seconds = 125;
        let mode = AppMode::View {
            session_id: session.id.clone(),
            scroll_offset: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("render archived session");

        // Assert
        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer);
        assert!(text.contains("Archived conversation remains readable."));
        assert!(!text.contains("Auto Edit"));
        let timing = buffer_row_text(buffer, 18, 100);
        assert!(timing.contains("Timer: 2m 5s · Started"));
        assert!(!text.contains("╭"));
        let footer = buffer_row_text(buffer, 19, 100);
        for action in ["q: back", "c: continue", "j/k: scroll", "?: help"] {
            assert!(footer.contains(action), "missing archive action: {action}");
        }
    }
}

#[test]
fn test_render_prompt_composer_shows_speed_and_auto_edit_for_supported_provider() {
    // Arrange
    let mut session = session_fixture();
    session.agent = model_fixture::codex_selection();

    // Act
    let text = rendered_prompt_mode_text(&session);

    // Assert
    assert!(text.contains(&format!(
        "codex/{} [high] · Balanced · Normal · Auto Edit",
        model_fixture::CODEX_MODEL_ID
    )));
    assert!(!text.contains(&format!(
        "codex/{} [high]  · Balanced · Normal · Auto Edit",
        model_fixture::CODEX_MODEL_ID
    )));
}

#[test]
fn test_render_prompt_composer_shows_saved_reasoning_level() {
    // Arrange
    let mut session = session_fixture();
    session.agent = model_fixture::codex_selection();
    session.reasoning_level_override = Some(ReasoningLevel::Low);

    // Act
    let text = rendered_prompt_mode_text(&session);

    // Assert
    assert!(text.contains(&format!(
        "codex/{} [low] · Balanced · Normal · Auto Edit",
        model_fixture::CODEX_MODEL_ID
    )));
    assert!(!text.contains("Reasoning:"));
    assert!(!text.contains("[high]"));
}

#[test]
fn test_render_prompt_footer_shows_loader_while_pasting_image() {
    // Arrange
    let session = session_fixture();
    let mut mode = prompt_mode("draft");
    if let AppMode::Prompt {
        attachment_state, ..
    } = &mut mode
    {
        attachment_state.pending_image_paste = Some(crate::domain::composer::PendingImagePaste {
            anchor: Some(0),
            request_id: 1,
        });
    }
    let mut page = test_session_chat_page(&session, &mode);
    let backend = ratatui::backend::TestBackend::new(80, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw prompt mode");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("▌▌▌ Pasting image..."));
}

#[test]
fn test_prompt_footer_shows_permission_mode_shortcut() {
    // Arrange
    let session = session_fixture();

    // Act
    let footer = prompt_format::prompt_footer_line(&session, 0, ChatFocus::Input, false);

    // Assert
    assert!(footer.to_string().contains("Shift+Tab: switch mode"));
}

#[test]
fn test_render_prompt_composer_shows_read_only_after_speed_status() {
    // Arrange
    let mut session = session_fixture();
    session.agent = model_fixture::codex_selection();
    session.permission_mode = crate::domain::permission::PermissionMode::ReadOnly;

    // Act
    let text = rendered_prompt_mode_text(&session);

    // Assert
    assert!(text.contains("· Normal · Read Only"));
}

#[test]
fn test_render_prompt_composer_shows_auto_edit_without_unsupported_speed_status() {
    // Arrange
    let mut session = session_fixture();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Gemini,
        crate::domain::agent::AgentModel::Gemini31Pro,
    );

    // Act
    let text = rendered_prompt_mode_text(&session);

    // Assert
    assert!(text.contains("· Auto Edit"));
    assert!(!text.contains("· Normal"));
}

#[test]
fn test_render_prompt_composer_shows_read_only_without_speed_status() {
    // Arrange
    let mut session = session_fixture();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Gemini,
        crate::domain::agent::AgentModel::Gemini31Pro,
    );
    session.permission_mode = crate::domain::permission::PermissionMode::ReadOnly;

    // Act
    let text = rendered_prompt_mode_text(&session);

    // Assert
    assert!(text.contains("· Read Only"));
    assert!(!text.contains("· Normal"));
}

#[test]
fn test_render_question_panel_preserves_frame_without_prepared_areas() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(40, 10);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let input = InputState::default();
    let questions = vec![QuestionItem {
        options: vec![],
        text: "Which path?".to_string(),
    }];
    let state = QuestionPanelState {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Input,
        has_session_diff: false,
        input: &input,
        questions: &questions,
        selected_option_index: None,
        timing_label: "",
    };

    // Act
    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new("sentinel"), frame.area());
            render_question_panel(frame, frame.area(), None, &state);
        })
        .expect("failed to draw question panel");

    // Assert
    assert!(buffer_text(terminal.backend().buffer()).contains("sentinel"));
}

#[test]
fn test_render_question_mode_keeps_typed_answer_visible_in_tight_layout() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Need a detailed migration plan with rollback guidance?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("typed answer".to_string()),
        scroll_offset: None,
        selected_option_index: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    let backend = ratatui::backend::TestBackend::new(32, 8);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw question mode");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("typed answer"));
}

#[test]
fn test_render_question_mode_includes_blank_row_between_question_and_input() {
    // Arrange
    let session = session_fixture();
    let question = "Need a detailed migration plan with rollback guidance?".to_string();
    let mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: question.clone(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("typed answer".to_string()),
        scroll_offset: None,
        selected_option_index: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    let width = 40;
    let height = 12;
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw question mode");

    // Assert
    let area = Rect::new(0, 0, width, height);
    let bottom_height = non_prompt_bottom_height(area, &mode);
    let bottom_top = height.saturating_sub(bottom_height);
    let panel_areas = layout::question_panel_areas(
        Rect::new(1, bottom_top, width.saturating_sub(2), bottom_height),
        &question,
        "typed answer",
        0,
        CHAT_INPUT_MAX_PANEL_HEIGHT,
    );
    let spacer_row = panel_areas.spacer_area.y;
    let spacer_text = buffer_row_text(terminal.backend().buffer(), spacer_row, width);
    assert_eq!(spacer_text.trim(), "");
}

#[test]
fn test_render_question_mode_with_options_shows_option_text() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem {
            options: vec!["Yes".to_string(), "No".to_string()],
            text: "Continue?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: Some(0),
    };
    let mut page = test_session_chat_page(&session, &mode);
    let width = 50;
    let height = 14;
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw question mode with options");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Options:"), "should render options header");
    assert!(text.contains("Yes"), "should render first option");
    assert!(text.contains("No"), "should render second option");
}

#[test]
fn test_render_question_lookup_shows_file_and_selection_controls() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::Question {
        at_mention_state: Some(PromptAtMentionState::new(vec![
            crate::domain::file_entry::FileEntry {
                is_dir: false,
                path: "src/session_chat.rs".to_string(),
            },
        ])),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("@session".to_string()),
        questions: vec![QuestionItem::new("Which file?")],
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "session-id".into(),
    };
    let mut page = test_session_chat_page(&session, &mode);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24))
        .expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| Page::render(&mut page, frame, frame.area()))
        .expect("failed to draw question lookup");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("src/session_chat.rs"));
    assert!(text.contains("Tab/Enter: select"));
    assert!(text.contains("Up/Down: navigate"));
    assert!(text.contains("Esc: cancel @"));
    assert!(!text.contains("Enter: send"));
}

#[test]
fn test_render_question_lookup_requires_space_for_a_complete_result_row() {
    for height in 4..=7 {
        // Arrange
        let input = InputState::with_text("@src".to_string());
        let questions = vec![QuestionItem::new("Which file?")];
        let lookup = PromptAtMentionState::new(vec![crate::domain::file_entry::FileEntry {
            is_dir: false,
            path: "src/lib.rs".to_string(),
        }]);
        let state = QuestionPanelState {
            at_mention_state: Some(&lookup),
            current_index: 0,
            focus: ChatFocus::Input,
            has_session_diff: false,
            input: &input,
            questions: &questions,
            selected_option_index: None,
            timing_label: "",
        };
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, height))
            .expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                let panel_areas = layout::question_panel_areas(
                    area,
                    "Which file?",
                    input.text(),
                    0,
                    CHAT_INPUT_MAX_PANEL_HEIGHT,
                );
                render_question_panel(frame, area, Some(panel_areas), &state);
            })
            .expect("failed to draw constrained lookup");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("@src"));
        assert!(text.contains("Esc: cancel @"));
        assert!(!text.contains("Tab/Enter: close @"));
        assert_eq!(text.contains("src/lib.rs"), height == 7);
        assert_eq!(text.contains("Tab/Enter: select"), height == 7);
        assert_eq!(text.contains("Up/Down: navigate"), height == 7);
    }
}

#[test]
fn test_render_question_empty_lookup_shows_only_dismissal_controls() {
    for entries in [
        Vec::new(),
        vec![crate::domain::file_entry::FileEntry {
            is_dir: false,
            path: "src/session_chat.rs".to_string(),
        }],
    ] {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: Some(PromptAtMentionState::new(entries)),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("@missing".to_string()),
            questions: vec![QuestionItem::new("Which file?")],
            responses: Vec::new(),
            scroll_offset: None,
            selected_option_index: None,
            session_id: "session-id".into(),
        };
        let mut page = test_session_chat_page(&session, &mode);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24))
            .expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| Page::render(&mut page, frame, frame.area()))
            .expect("failed to draw empty question lookup");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("@missing"));
        assert!(text.contains("Tab/Enter: close @"));
        assert!(text.contains("Esc: cancel @"));
        assert!(!text.contains("Tab/Enter: select"));
        assert!(!text.contains("Up/Down: navigate"));
        assert!(!text.contains("Enter: send"));
    }
}

#[test]
fn test_render_question_mode_with_options_in_small_terminal_does_not_panic() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem {
            options: vec![
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
                "E".to_string(),
            ],
            text: "Pick one of the many options?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    let backend = ratatui::backend::TestBackend::new(30, 6);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act + Assert (should not panic)
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw question mode with many options in small terminal");
}

/// Renders `mode` for a session started two hours before the frame clock with
/// an hour and a minute of work, and returns every buffer row.
fn rendered_rows_with_started_session(mode: &AppMode, width: u16, height: u16) -> Vec<String> {
    let mut session = session_fixture();
    session.created_at = 1_782_864_000;
    session.in_progress_total_seconds = 3_660;
    let mut page = test_session_chat_page(&session, mode);
    page.frame_time = FrameTime::new(1_782_871_200, 0, 0);
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw started session");

    (0..height)
        .map(|row| buffer_row_text(terminal.backend().buffer(), row, width))
        .collect()
}

#[test]
fn test_render_session_preserves_timer_across_composer_modes_and_widths() {
    // Arrange
    let view_mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let mut question_mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem::new("Continue?")],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    let mut prompt = prompt_mode("");

    // Act
    let mut rendered_rows = Vec::new();
    for width in [40, 41, 80, 240] {
        rendered_rows.push((
            width,
            rendered_rows_with_started_session(&view_mode, width, 16),
        ));
        for focus in [ChatFocus::Input, ChatFocus::Chat] {
            if let AppMode::Prompt { focus: current, .. } = &mut prompt {
                *current = focus;
            }
            if let AppMode::Question { focus: current, .. } = &mut question_mode {
                *current = focus;
            }
            rendered_rows.push((
                width,
                rendered_rows_with_started_session(&prompt, width, 16),
            ));
            rendered_rows.push((
                width,
                rendered_rows_with_started_session(&question_mode, width, 16),
            ));
        }
    }

    // Assert
    for (width, rows) in &rendered_rows {
        let label = if *width == 40 {
            assert!(rows.iter().all(|row| !row.contains("Started")));
            "Timer: 1h 1m 0s"
        } else {
            "Timer: 1h 1m 0s · Started 2h 0m ago"
        };
        let label_rows: Vec<&String> = rows.iter().filter(|row| row.contains(label)).collect();
        let resource_row = rows.iter().find(|row| row.contains("Processes: --"));
        assert_eq!(label_rows.len(), 1, "rows: {rows:#?}");
        assert!(label_rows[0].contains("╰"));
        assert!(label_rows[0].trim_end().ends_with("╯"));
        assert!(resource_row.is_some_and(|row| !row.contains("Started")));
    }
}

#[test]
fn test_render_session_omits_started_label_when_rows_are_too_narrow() {
    // Arrange
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };

    // Act
    let rows = rendered_rows_with_started_session(&mode, 12, 12);

    // Assert
    assert!(rows.iter().all(|row| !row.contains("Started")));
}

#[test]
fn terminal_diff_help_requires_retained_archive() {
    // Arrange
    for status in [Status::Done, Status::Canceled] {
        for has_archived_diff in [false, true] {
            let mut session = session_fixture();
            session.status = status;
            session.has_archived_diff = has_archived_diff;
            let mode = AppMode::View {
                session_id: session.id.clone(),
                scroll_offset: None,
            };
            let page = test_session_chat_page(&session, &mode);

            // Act
            let actions = help_action::view_actions(page.view_help_state(&session));

            // Assert
            assert_eq!(
                actions.iter().any(|action| action.key == "d"),
                has_archived_diff
            );
        }
    }
}

#[test]
fn composer_shows_the_armed_plan_request_and_the_orchestrator_state() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    let graph = TaskChainGraph {
        chain_id: "chain".into(),
        edges: Vec::new(),
        nodes: vec![TaskChainGraphNode {
            agent: BuiltinAgentDefinition::Planner,
            status: TaskChainNodeStatus::AwaitingApproval,
        }],
    };
    let mut armed = prompt_mode("");
    if let AppMode::Prompt {
        focus, slash_state, ..
    } = &mut armed
    {
        *focus = ChatFocus::Input;
        slash_state.is_plan_armed = true;
    }
    let mut replying = prompt_mode("");
    if let AppMode::Prompt { focus, .. } = &mut replying {
        *focus = ChatFocus::Input;
    }
    let viewing = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: None,
    };

    // Act
    let armed_text = rendered_text(&session, &armed, None);
    let replying_text = rendered_text(&session, &replying, Some(&graph));
    let viewing_text = rendered_text(&session, &viewing, Some(&graph));
    let ordinary_text = rendered_text(&session, &replying, None);
    let slash_text = rendered_text(&session, &prompt_mode("/"), None);

    // Assert
    assert!(armed_text.contains("Plan · describe the task for the Planner"));
    assert!(armed_text.contains("Describe the task for the Planner"));
    assert!(replying_text.contains("Orchestrator · Planner · Awaiting your approval"));
    assert!(replying_text.contains("tell the orchestrator to start implementing"));
    assert!(viewing_text.contains("Orchestrator · Planner · Awaiting your approval"));
    assert!(ordinary_text.contains("Type your message"));
    assert!(slash_text.contains("/mode"));
}

#[test]
fn prompt_composer_keeps_its_border_when_only_one_row_is_available() {
    // Arrange
    let session = session_fixture();
    let mode = prompt_mode("draft");
    let page = test_session_chat_page(&session, &mode);
    let panel = SessionChatLayoutPlan::new(layout_input(Rect::new(0, 0, 100, 14), &session, &mode))
        .prompt_panel
        .expect("prompt panel");
    let input = InputState::with_text("draft".to_string());
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 1)).expect("terminal");

    // Act
    terminal
        .draw(|frame| {
            page.render_prompt_composer(frame, frame.area(), &panel, (&input, false), "Timer: 0s");
        })
        .expect("render short composer");
    let text = buffer_text(terminal.backend().buffer());

    // Assert
    assert!(text.chars().any(|glyph| matches!(glyph, '╭' | '╰' | '─')));
}

/// Renders one session page with an optional task-chain graph.
fn rendered_text(session: &Session, mode: &AppMode, graph: Option<&TaskChainGraph>) -> String {
    let mut page = SessionChatPage::new(SessionChatPageInput {
        task_chain_graph: graph,
        ..test_page_input(session, mode)
    });
    let backend = ratatui::backend::TestBackend::new(100, 14);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw session page");

    buffer_text(terminal.backend().buffer())
}
