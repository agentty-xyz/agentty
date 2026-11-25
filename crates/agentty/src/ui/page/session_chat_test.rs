//! Session chat page layout, transcript, composer, and question-panel
//! rendering unit tests.

use std::cell::Cell;
use std::sync::Arc;

use ag_contracts::ActivityEvent;
use ag_session::test_support as model_fixture;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::{
    CHAT_INPUT_MAX_PANEL_HEIGHT, QuestionPanelState, SessionChatLayoutInput, SessionChatLayoutPlan,
    SessionChatPage, SessionChatPageInput, TimingFooter, non_prompt_bottom_height,
    render_question_panel, session_composer_title, transcript_view_height,
};
use crate::domain::agent::ReasoningLevel;
use crate::domain::input::InputState;
use crate::domain::question::{QuestionAttachmentState, QuestionItem};
use crate::domain::session::{Session, Status};
use crate::domain::session_message::SessionTranscript;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::domain::turn_timeline::TurnTimeline;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::graph_chat::GraphChatSnapshot;
use crate::presentation::prompt::{
    PendingImagePaste, PromptAtMentionState, PromptAttachmentState, PromptGraphState,
    PromptHistoryState, PromptSlashState,
};
use crate::ui::component::session_output::{SessionOutputLayoutCache, SessionOutputLineContext};
use crate::ui::usage_format::{
    CostFigure, ReadoutContext, ReadoutFigures, ReadoutTurn, UsageFigure,
};
use crate::ui::usage_readout::UsageReadout;
use crate::ui::{Page, layout, markdown, prompt_format, session_format, style};

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
        color_support: style::ColorSupport::Enabled,
        frame_time: FrameTime::new(0, 0, 0),
        greeting_animated: test_greeting_animated(),
        has_merge_conflict: false,
        markdown_render_cache: test_markdown_render_cache(),
        mode,
        output_layout_cache: test_output_layout_cache(),
        review_text: None,
        scroll_offset: None,
        session_index: 0,
        session_update_version: 0,
        sessions: std::slice::from_ref(session),
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
fn test_transcript_view_height_reclaims_inactive_composer_and_footer_rows() {
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
                (prompt_height..=prompt_height + 2).contains(&view_height),
                "width {width}, height {height}"
            );
            if width >= 40 && height >= 20 {
                assert_eq!(view_height, prompt_height + 2);
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
        slash_state.graph = PromptGraphState::Armed;
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

/// Returns a leaked greeting-animation flag for test page builders that need
/// a stable borrow across the page lifetime.
fn test_greeting_animated() -> &'static Cell<bool> {
    Box::leak(Box::new(Cell::new(false)))
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

/// Verifies view mode renders no keybinding help line for any session state.
#[test]
fn test_render_view_mode_omits_keybinding_help_line() {
    for status in [
        Status::Done,
        Status::Canceled,
        Status::InProgress,
        Status::Review,
    ] {
        // Arrange
        let mut session = session_fixture();
        session.status = status;
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
            .expect("failed to draw session");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        for action in [
            "q: back",
            "?: help",
            "c: continue",
            "r: sync",
            "Ctrl+c: stop",
        ] {
            assert!(!text.contains(action), "unexpected {action} for {status:?}");
        }
    }
}

/// Verifies a running turn's live timeline reserves a status row above the
/// composer margin, and that a pane too short for it keeps the transcript.
#[test]
fn test_live_timeline_renders_a_status_line_above_the_composer() {
    // Arrange
    let session = running_timeline_session();
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let short_plan =
        SessionChatLayoutPlan::new(layout_input(Rect::new(0, 0, 80, 6), &session, &mode));
    // The frame comes 12 seconds after the call started, so both timers
    // read the frame clock.
    let mut page = SessionChatPage::new(SessionChatPageInput {
        frame_time: FrameTime::new(12, 12_000, 0),
        ..test_page_input(&session, &mode)
    });
    let backend = ratatui::backend::TestBackend::new(80, 16);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let plan = SessionChatLayoutPlan::new(layout_input(Rect::new(0, 0, 80, 16), &session, &mode));
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw session");

    // Assert
    let status_area = plan.turn_status_area.expect("status row");
    assert_eq!(status_area.y, plan.areas.output_area.bottom());
    assert_eq!(plan.areas.bottom_area.y - status_area.bottom(), 1);
    assert_eq!(short_plan.turn_status_area, None);
    let buffer = terminal.backend().buffer();
    let status_row = buffer_row_text(buffer, status_area.y, 80);
    assert_eq!(
        status_row.trim_end(),
        " ⠋ Running cargo test 12s  ·  step 1  ·  turn 0s"
    );
    let running_row = (plan.areas.output_area.y..plan.areas.output_area.bottom())
        .map(|row| buffer_row_text(buffer, row, 80))
        .find(|row| row.contains("Running   cargo test"))
        .expect("running row");
    assert!(running_row.trim_end().ends_with(" 12s"), "{running_row}");
}

/// Returns an in-progress session whose live timeline runs `cargo test`.
fn running_timeline_session() -> Session {
    let mut session = session_fixture();
    session.status = Status::InProgress;
    let mut timeline = TurnTimeline::default();
    timeline.observe_activity(
        ActivityEvent {
            input: Some("cargo test".into()),
            ..crate::test_support::activity_event("call")
        },
        0,
    );
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterActiveTurn,
        body: TransientMessageBody::Timeline(Arc::new(timeline.snapshot())),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Turn,
        turn_position: None,
    });

    session
}

/// Returns a priced readout of a running turn at 38% context.
fn running_usage_readout() -> UsageReadout {
    let cost = |text: &str| {
        UsageFigure::Cost(CostFigure {
            is_api_equivalent: false,
            is_lower_bound: false,
            is_upper_bound: false,
            text: text.to_string(),
        })
    };
    let tokens = |text: &str| UsageFigure::Known {
        is_lower_bound: false,
        text: text.to_string(),
    };

    UsageReadout::new(
        ReadoutFigures {
            context: Some(ReadoutContext {
                gauge_filled: Some(2),
                percent: Some(38),
                tokens: "380.0k".to_string(),
            }),
            session_cost: cost("$3.27"),
            turn: Some(ReadoutTurn {
                cost: cost("$0.24"),
                input: tokens("412k"),
                output: tokens("3.1k"),
            }),
        },
        false,
    )
}

/// The running turn's readout fills the status line's right slot, and the
/// composer border keeps only the timer while it does.
#[test]
fn test_running_turn_readout_fills_the_status_line_and_leaves_the_border() {
    // Arrange
    let session = running_timeline_session();
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let readout = running_usage_readout();
    let mut page =
        SessionChatPage::new(test_page_input(&session, &mode)).usage_readout(Some(&readout));
    let backend = ratatui::backend::TestBackend::new(130, 16);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let plan = SessionChatLayoutPlan::new(layout_input(Rect::new(0, 0, 130, 16), &session, &mode));

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw session");

    // Assert
    let buffer = terminal.backend().buffer();
    let status_area = plan.turn_status_area.expect("status row");
    let status_row = buffer_row_text(buffer, status_area.y, 130);
    assert!(
        status_row
            .trim_end()
            .ends_with("↑412k ↓3.1k · ctx ▰▰▱▱▱ 38% · $0.24 turn"),
        "{status_row}"
    );
    let border_row = buffer_row_text(buffer, plan.areas.bottom_area.bottom() - 1, 130);
    assert!(border_row.contains("Timer:"), "{border_row}");
    assert!(!border_row.contains("session"), "{border_row}");
}

/// A pane too narrow for the status line's readout slot keeps the session
/// figures in the composer border while the turn runs.
#[test]
fn test_running_turn_readout_stays_in_the_border_on_a_narrow_pane() {
    // Arrange
    let session = running_timeline_session();
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let readout = running_usage_readout();
    let mut page =
        SessionChatPage::new(test_page_input(&session, &mode)).usage_readout(Some(&readout));
    let backend = ratatui::backend::TestBackend::new(72, 16);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let plan = SessionChatLayoutPlan::new(layout_input(Rect::new(0, 0, 72, 16), &session, &mode));

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw session");

    // Assert
    let buffer = terminal.backend().buffer();
    let status_area = plan.turn_status_area.expect("status row");
    let status_row = buffer_row_text(buffer, status_area.y, 72);
    assert!(!status_row.contains("ctx"), "{status_row}");
    let border_row = buffer_row_text(buffer, plan.areas.bottom_area.bottom() - 1, 72);
    assert!(border_row.contains("$3.27 session"), "{border_row}");
    assert!(border_row.contains("Timer:"), "{border_row}");
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
        },
        &markdown_render_cache,
        &output_layout_cache,
    );

    // Assert
    assert!(rendered_line_count > usize::from(raw_line_count));
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
    let mut page = SessionChatPage::new(SessionChatPageInput {
        review_text: Some("Initial review"),
        ..test_page_input(&session, &mode)
    });

    // Act
    let initial_review_text = page.review_text;
    page.review_text = Some("Focused review");
    let review_text = page.review_text;

    // Assert
    assert_eq!(initial_review_text, Some("Initial review"));
    assert_eq!(review_text, Some("Focused review"));
}

#[test]
fn test_review_text_reads_snapshot_for_question_output() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::Question {
        at_mention_state: None,
        attachment_state: QuestionAttachmentState::default(),
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
        let sessions = [session_fixture(), session];
        let session = &sessions[1];
        let mut page = SessionChatPage::new(SessionChatPageInput {
            has_merge_conflict: true,
            session_index: 1,
            session_update_version: 7,
            sessions: &sessions,
            ..test_page_input(session, &mode)
        });
        let width = 120;
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
            "codex/{} [high] · Concise · Normal",
            model_fixture::CODEX_MODEL_ID
        );
        let composer_row = (0..height)
            .find(|row| buffer_row_text(buffer, *row, width).contains(&settings))
            .expect("session settings should remain visible");
        let border = &buffer[(1, composer_row)];
        assert_eq!(border.symbol(), "╭");
        assert_eq!(border.fg, style::palette::border());
        assert!(!buffer_text(buffer).contains("Type your message"));
        assert_eq!(composer_row, height - 2);
        assert_eq!(buffer[(1, composer_row + 1)].symbol(), "╰");
        assert!(!buffer_text(buffer).contains("›"));
        let text = buffer_text(buffer);
        assert!(text.contains("Merge conflict with main"));
        assert!(!text.contains("CPU"));
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
fn test_render_archived_session_keeps_transcript_and_timing_without_composer() {
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
        let timing = buffer_row_text(buffer, 19, 100);
        assert!(timing.contains("Timer: 2m 5s · Started"));
        assert!(!text.contains("╭"));
        assert!(!text.contains("q: back"));
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
        "codex/{} [high] · Concise · Normal",
        model_fixture::CODEX_MODEL_ID
    )));
    assert!(!text.contains(&format!(
        "codex/{} [high]  · Concise · Normal",
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
        "codex/{} [low] · Concise · Normal",
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
fn test_prompt_footer_omits_permission_mode_shortcut() {
    // Arrange
    let session = session_fixture();

    // Act
    let footer = prompt_format::prompt_footer_line(&session, 0, ChatFocus::Input, false);

    // Assert
    assert!(!footer.to_string().contains("Shift+Tab"));
}

#[test]
fn test_render_prompt_composer_shows_speed_without_permission_mode() {
    // Arrange
    let mut session = session_fixture();
    session.agent = model_fixture::codex_selection();

    // Act
    let text = rendered_prompt_mode_text(&session);

    // Assert
    assert!(text.contains("· Concise · Normal"));
    assert!(!text.contains("Auto Edit"));
}

#[test]
fn test_render_prompt_composer_omits_unsupported_speed_status() {
    // Arrange
    let mut session = session_fixture();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Antigravity,
        crate::domain::agent::AgentModel::Gemini4Argon,
    );

    // Act
    let text = rendered_prompt_mode_text(&session);

    // Assert
    assert!(text.contains("· Concise"));
    assert!(!text.contains("· Normal"));
    assert!(!text.contains("Auto Edit"));
}

#[test]
fn test_render_leaves_frame_untouched_for_missing_session_index() {
    // Arrange
    let session = session_fixture();
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let mut page = SessionChatPage::new(SessionChatPageInput {
        session_index: 3,
        ..test_page_input(&session, &mode)
    });
    let backend = ratatui::backend::TestBackend::new(40, 8);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new("sentinel"), frame.area());
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw missing session");

    // Assert
    assert_eq!(
        buffer_text(terminal.backend().buffer()).trim_end(),
        "sentinel"
    );
}

#[test]
fn test_render_in_progress_session_forwards_scroll_and_active_progress() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    let mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: Some(0),
    };
    let mut page = SessionChatPage::new(SessionChatPageInput {
        active_progress: Some("Compiling crates"),
        scroll_offset: Some(0),
        ..test_page_input(&session, &mode)
    });
    let backend = ratatui::backend::TestBackend::new(80, 16);
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
    assert!(text.contains("Compiling crates"));
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
        input: &input,
        is_pasting_image: false,
        questions: &questions,
        selected_option_index: None,
        spinner_frame: 0,
        timing_label: "",
        usage_readout: None,
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
        attachment_state: QuestionAttachmentState::default(),
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
fn question_mode_footer_advertises_a_running_image_capture() {
    // Arrange
    let session = session_fixture();
    let mut attachment_state = QuestionAttachmentState::default();
    attachment_state.composer.pending_image_paste = Some(PendingImagePaste {
        anchor: Some(0),
        request_id: 1,
    });
    let mode = AppMode::Question {
        at_mention_state: None,
        attachment_state,
        session_id: "session-id".into(),
        questions: vec![QuestionItem::new("Which screen?")],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("See ".to_string()),
        scroll_offset: None,
        selected_option_index: None,
    };
    let mut page = test_session_chat_page(&session, &mode);
    let backend = ratatui::backend::TestBackend::new(120, 20);
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
    assert!(text.contains("Pasting image..."));
    assert!(text.contains("Enter: send"));
}

#[test]
fn test_render_question_mode_includes_blank_row_between_question_and_input() {
    // Arrange
    let session = session_fixture();
    let question = "Need a detailed migration plan with rollback guidance?".to_string();
    let mode = AppMode::Question {
        at_mention_state: None,
        attachment_state: QuestionAttachmentState::default(),
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
        attachment_state: QuestionAttachmentState::default(),
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
        attachment_state: QuestionAttachmentState::default(),
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
            input: &input,
            is_pasting_image: false,
            questions: &questions,
            selected_option_index: None,
            spinner_frame: 0,
            timing_label: "",
            usage_readout: None,
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
            attachment_state: QuestionAttachmentState::default(),
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
        attachment_state: QuestionAttachmentState::default(),
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
    let mut page = SessionChatPage::new(SessionChatPageInput {
        frame_time: FrameTime::new(1_782_871_200, 0, 0),
        ..test_page_input(&session, mode)
    });
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
        attachment_state: QuestionAttachmentState::default(),
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
        assert_eq!(label_rows.len(), 1, "rows: {rows:#?}");
        assert!(label_rows[0].contains("╰"));
        assert!(label_rows[0].trim_end().ends_with("╯"));
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
fn composer_shows_the_session_settings_title_while_replying_and_viewing() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    let mut replying = prompt_mode("");
    if let AppMode::Prompt { focus, .. } = &mut replying {
        *focus = ChatFocus::Input;
    }
    let viewing = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: None,
    };
    let title = format!(
        "{}/{}",
        session.agent.kind(),
        session.agent.model().as_str()
    );

    // Act
    let replying_text = rendered_text(&session, &replying);
    let viewing_text = rendered_text(&session, &viewing);
    let slash_text = rendered_text(&session, &prompt_mode("/"));

    // Assert
    assert!(replying_text.contains(&title));
    assert!(replying_text.contains("Type your message"));
    assert!(viewing_text.contains(&title));
    assert!(slash_text.contains("/model"));
}

#[test]
fn graph_chat_renders_beside_the_armed_and_regular_composer() {
    // Arrange
    let session = session_fixture();
    let snapshot = GraphChatSnapshot {
        error: None,
        progress: None,
        proposal: model_fixture::graph_proposal_state("proposal", &session.id),
    };
    for armed in [true, false] {
        let mut mode = prompt_mode("");
        if let AppMode::Prompt {
            slash_state, focus, ..
        } = &mut mode
        {
            slash_state.graph = if armed {
                PromptGraphState::Armed
            } else {
                PromptGraphState::Available
            };
            *focus = ChatFocus::Input;
        }
        let mut page =
            test_session_chat_page(&session, &mode).graph_chat((!armed).then_some(&snapshot));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))
            .expect("test terminal");

        // Act
        terminal
            .draw(|frame| page.render(frame, frame.area()))
            .expect("render graph chat");
        let text = buffer_text(terminal.backend().buffer());

        // Assert
        let model_title = session_composer_title(&session);
        let graph_title = format!("Graph · {model_title}");
        assert!(text.contains("Execution graph"));
        assert!(text.contains(&model_title));
        assert!(text.contains(&session_format::prompt_session_status(&session)));
        assert_eq!(text.contains(&graph_title), armed);
        assert!(text.contains(if armed {
            "Describe the task for the execution graph"
        } else {
            "Type your message"
        }));
        if !armed {
            assert!(text.contains("Generating preview"));
        }
    }
}

#[test]
fn slash_leading_drafts_keep_active_file_mentions_visible() {
    for (status, armed, input, active_mention) in [
        (Status::Draft, true, "/src/main.rs @session", true),
        (Status::InProgress, false, "/src/main.rs @session", true),
        (Status::Draft, false, "/src/main.rs @session", true),
        (Status::Draft, false, "/", false),
    ] {
        // Arrange
        let mut session = session_fixture();
        session.status = status;
        let mut mode = prompt_mode(input);
        if let AppMode::Prompt {
            at_mention_state,
            slash_state,
            ..
        } = &mut mode
        {
            if armed {
                slash_state.graph = PromptGraphState::Armed;
            }
            *at_mention_state = Some(PromptAtMentionState::new(vec![
                crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/session_chat.rs".to_string(),
                },
            ]));
        }

        // Act
        let text = rendered_text(&session, &mode);

        // Assert
        assert_eq!(text.contains("src/session_chat.rs"), active_mention);
        assert_eq!(text.contains("/model"), !active_mention);
    }
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
            page.render_prompt_composer(
                frame,
                frame.area(),
                &panel,
                &input,
                &TimingFooter {
                    label: "Timer: 0s".to_string(),
                    readout: None,
                },
            );
        })
        .expect("render short composer");
    let text = buffer_text(terminal.backend().buffer());

    // Assert
    assert!(text.chars().any(|glyph| matches!(glyph, '╭' | '╰' | '─')));
}

/// Renders one session page.
fn rendered_text(session: &Session, mode: &AppMode) -> String {
    let mut page = test_session_chat_page(session, mode);
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

/// Renders prompt mode for one session in a terminal tall enough for the
/// nerpa greeting and returns the painted text.
fn rendered_tall_prompt_mode_text(session: &Session) -> String {
    rendered_tall_prompt_mode_text_with_colors(session, style::ColorSupport::Enabled)
}

/// Renders the tall prompt-mode page for a terminal that does or does not
/// render colors and returns the painted text.
fn rendered_tall_prompt_mode_text_with_colors(
    session: &Session,
    color_support: style::ColorSupport,
) -> String {
    let mode = prompt_mode("");
    let mut page = SessionChatPage::new(SessionChatPageInput {
        color_support,
        ..test_page_input(session, &mode)
    });
    let backend = ratatui::backend::TestBackend::new(80, 24);
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
fn test_render_shows_nerpa_greeting_until_first_message() {
    // Arrange
    let fresh_session = session_fixture();
    let mut started_session = session_fixture();
    started_session.prompt = "Ship it".to_string();

    // Act
    let fresh_text = rendered_tall_prompt_mode_text(&fresh_session);
    let started_text = rendered_tall_prompt_mode_text(&started_session);

    // Assert
    assert!(fresh_text.contains("▄████████████▄"));
    assert!(!started_text.contains("▄████████████▄"));
}

#[test]
fn test_render_hides_right_nerpa_whiskers_without_colors() {
    // Arrange
    let session = session_fixture();
    let colored_muzzle_row = format!("▀▀▀{}▀▀▀{}▀██▄", "█".repeat(12), "█".repeat(9));
    let colorless_muzzle_row = format!("▀▀▀{}▀██▄", "█".repeat(24));

    // Act
    let colored_text =
        rendered_tall_prompt_mode_text_with_colors(&session, style::ColorSupport::Enabled);
    let colorless_text =
        rendered_tall_prompt_mode_text_with_colors(&session, style::ColorSupport::Disabled);

    // Assert
    assert!(colored_text.contains(&colored_muzzle_row));
    assert!(!colored_text.contains(&colorless_muzzle_row));
    assert!(colorless_text.contains(&colorless_muzzle_row));
}

#[test]
fn test_render_reports_greeting_animation_only_for_visible_art() {
    // Arrange
    let fresh_session = session_fixture();
    let mut started_session = session_fixture();
    started_session.prompt = "Ship it".to_string();

    // Act
    let colored_animated =
        rendered_prompt_mode_greeting_animated(&fresh_session, style::ColorSupport::Enabled, 24);
    let colorless_animated =
        rendered_prompt_mode_greeting_animated(&fresh_session, style::ColorSupport::Disabled, 24);
    let cramped_animated =
        rendered_prompt_mode_greeting_animated(&fresh_session, style::ColorSupport::Enabled, 12);
    let started_animated =
        rendered_prompt_mode_greeting_animated(&started_session, style::ColorSupport::Enabled, 24);

    // Assert
    assert!(colored_animated);
    assert!(colorless_animated);
    assert!(!cramped_animated);
    assert!(!started_animated);
}

/// Renders prompt mode in an 80-column terminal `height` rows tall and
/// returns whether the page reported an animating nerpa greeting.
fn rendered_prompt_mode_greeting_animated(
    session: &Session,
    color_support: style::ColorSupport,
    height: u16,
) -> bool {
    let mode = prompt_mode("");
    let greeting_animated = Cell::new(false);
    let mut page = SessionChatPage::new(SessionChatPageInput {
        color_support,
        greeting_animated: &greeting_animated,
        ..test_page_input(session, &mode)
    });
    let backend = ratatui::backend::TestBackend::new(80, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| {
            let area = frame.area();
            Page::render(&mut page, frame, area);
        })
        .expect("failed to draw prompt mode");

    greeting_animated.get()
}
