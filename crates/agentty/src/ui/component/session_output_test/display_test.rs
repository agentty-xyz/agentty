use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::super::{
    SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT, SessionOutput, SessionOutputLayoutCache,
    SessionOutputLineContext,
};
use super::support::{
    line_context, output_lines, queued_message, session_fixture, set_assistant_transcript,
    set_conversation_transcript, table_header_background,
};
use crate::domain::session::{SessionId, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::theme::ColorTheme;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::ui::component::vertical_scrollbar::{SCROLLBAR_THUMB_SYMBOL, SCROLLBAR_TRACK_SYMBOL};
use crate::ui::icon::{Icon, TACHYON_LOADER_WIDTH};
use crate::ui::render::Component;
use crate::ui::{markdown, session_output_assembly, style};

#[test]
fn test_resolved_layout_keeps_full_width_when_output_fits() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "word word");
    let output_area = Rect::new(0, 0, 9, 3);
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let context = line_context();
    let full_width_layout = SessionOutput::rendered_layout(
        &session,
        output_area,
        context,
        Some(&markdown_render_cache),
        Some(&output_layout_cache),
    );
    let gutter_layout = SessionOutput::rendered_layout(
        &session,
        SessionOutput::scrollbar_layout_area(output_area),
        context,
        Some(&markdown_render_cache),
        Some(&output_layout_cache),
    );
    // Act
    let resolved_layout = SessionOutput::resolved_layout(
        &session,
        output_area,
        full_width_layout.line_count,
        context,
        Some(&markdown_render_cache),
        Some(&output_layout_cache),
    );

    // Assert
    assert!(gutter_layout.line_count > full_width_layout.line_count);
    assert!(!resolved_layout.show_scrollbar);
    assert!(Arc::ptr_eq(
        &resolved_layout.layout.lines,
        &full_width_layout.lines
    ));
}

#[test]
fn test_render_shows_scrollbar_for_overflowing_output() {
    // Arrange
    let mut session = session_fixture();
    let output = (0..40)
        .map(|line_index| format!("output line {line_index}"))
        .collect::<Vec<_>>()
        .join("\n");
    set_assistant_transcript(&mut session, &output);
    let backend = ratatui::backend::TestBackend::new(40, 10);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            let output = SessionOutput::new(&session).scroll_offset(12);
            output.render(frame, frame.area());
        })
        .expect("failed to draw session output");

    // Assert
    let rendered_text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(rendered_text.contains(SCROLLBAR_TRACK_SYMBOL));
    assert!(rendered_text.contains(SCROLLBAR_THUMB_SYMBOL));
}

#[test]
fn test_progress_updates_share_body_and_discard_superseded_layouts() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    set_assistant_transcript(
        &mut session,
        &"```mermaid\ngraph TD\nA --> B\n```\n".repeat(100),
    );
    let cache = SessionOutputLayoutCache::default();
    let area = Rect::new(0, 0, 80, 24);
    let initial = cache.layout(&session, area, line_context(), None);

    // Act
    for version in 1..20 {
        let progress = format!("Step {version}");
        let updated = cache.layout(
            &session,
            area,
            SessionOutputLineContext {
                active_progress: Some(&progress),
                session_update_version: version,
                ..line_context()
            },
            None,
        );

        // Assert
        assert!(Arc::ptr_eq(&initial.lines.body, &updated.lines.body));
        assert!(!Arc::ptr_eq(&initial.lines, &updated.lines));
        assert_eq!(cache.entries.borrow().len(), 1);
        assert_eq!(cache.body_entries.borrow().len(), 1);
        assert!(
            updated
                .lines
                .tail
                .iter()
                .any(|line| line.to_string().contains(&progress))
        );
    }
}

#[test]
fn test_rendered_line_count_counts_wrapped_content() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, &"word ".repeat(40));
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
    let rendered_line_count = SessionOutput::rendered_line_count(
        &session,
        20,
        5,
        line_context(),
        Some(&markdown_render_cache),
        Some(&output_layout_cache),
    );

    // Assert
    assert!(rendered_line_count > raw_line_count);
}

#[test]
fn test_resolved_cache_reuses_scrollbar_decision_and_invalidates_viewport() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, &"line\n".repeat(40));
    let cache = SessionOutputLayoutCache::default();
    let area = Rect::new(0, 0, 80, 24);
    let first = cache.resolved_layout(&session, area, 10, line_context(), None);
    cache.entries.borrow_mut().clear();
    cache.body_entries.borrow_mut().clear();

    // Act
    let repeated = cache.resolved_layout(&session, area, 10, line_context(), None);

    // Assert
    assert!(first.show_scrollbar);
    assert!(Arc::ptr_eq(&first.layout.lines, &repeated.layout.lines));
    assert!(cache.entries.borrow().is_empty());
    assert!(cache.body_entries.borrow().is_empty());
    let taller = cache.resolved_layout(&session, area, 100, line_context(), None);
    assert!(!taller.show_scrollbar);
    assert_eq!(cache.resolved_entries.borrow().len(), 1);
}

#[test]
fn test_output_layout_cache_reuses_lines_for_matching_update_key() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "## Heading\n\ncached body");
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let context = SessionOutputLineContext {
        session_update_version: 7,
        ..line_context()
    };

    // Act
    let first_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );
    let second_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );

    // Assert
    assert_eq!(first_layout.line_count, second_layout.line_count);
    assert!(Arc::ptr_eq(&first_layout.lines, &second_layout.lines));
}

#[test]
fn test_output_layout_cache_keys_active_theme() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    set_conversation_transcript(
        &mut session,
        vec![(
            SessionMessageKind::UserPrompt,
            concat!(
                "Use **bold** and `code`.\n\n",
                "| Input | Meaning |\n",
                "| --- | --- |\n",
                "| User prompt | Markdown |",
            ),
        )],
    );
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let context = line_context();

    // Act
    let current_layout = {
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        )
    };
    let dark_horizon_layout = {
        let _theme_scope = style::scoped_active_theme(ColorTheme::DarkHorizon);
        output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        )
    };

    // Assert
    assert!(!Arc::ptr_eq(
        &current_layout.lines,
        &dark_horizon_layout.lines
    ));
    assert_eq!(
        table_header_background(&dark_horizon_layout),
        Some(Color::Rgb(33, 36, 48))
    );
}

#[test]
/// Verifies transient workflow notices invalidate layout cache entries and
/// render outside the persisted transcript text.
fn test_output_layout_cache_keys_workflow_notice() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "implemented the feature");
    session.status = Status::Review;
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let context = line_context();

    // Act
    let base_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterCompletedTurn,
        body: TransientMessageBody::Markdown("[Commit] No changes to commit.".to_string()),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::WorkflowNotice,
        turn_position: None,
    });
    let notice_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );
    let notice_text = notice_layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let transcript_text = session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default();

    // Assert
    assert!(!transcript_text.contains("[Commit] No changes to commit."));
    assert!(!Arc::ptr_eq(&base_layout.lines, &notice_layout.lines));
    assert!(notice_text.contains("[Commit] No changes to commit."));
}

#[test]
fn test_output_layout_cache_reuses_active_loader_layout_across_frames() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "active output");
    session.status = Status::InProgress;
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let first_frame_context = line_context();

    // Act
    let first_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        first_frame_context,
        Some(&markdown_render_cache),
    );
    let repeated_first_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        first_frame_context,
        Some(&markdown_render_cache),
    );
    let repeated_frame_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        first_frame_context,
        Some(&markdown_render_cache),
    );

    // Assert
    assert!(Arc::ptr_eq(
        &first_layout.lines,
        &repeated_first_layout.lines
    ));
    assert!(Arc::ptr_eq(
        &first_layout.lines,
        &repeated_frame_layout.lines
    ));
    assert!(
        first_layout
            .lines
            .iter()
            .any(|line| line.to_string().contains(Icon::TachyonLoader.as_str()))
    );
}

#[test]
fn test_output_layout_cache_keeps_tachyon_effect_state_per_session() {
    // Arrange
    let output_layout_cache = SessionOutputLayoutCache::default();
    let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);
    let mut first_buffer = Buffer::empty(area);
    let mut second_buffer = Buffer::empty(area);
    for column in 0..TACHYON_LOADER_WIDTH {
        first_buffer[(column, 0)]
            .set_symbol("▌")
            .set_fg(style::palette::text_muted());
        second_buffer[(column, 0)]
            .set_symbol("▌")
            .set_fg(style::palette::text_muted());
    }
    let first_session_id = SessionId::from("first-loader-session");
    let second_session_id = SessionId::from("second-loader-session");

    // Act
    output_layout_cache.apply_tachyon_loader_effect(&first_session_id, &mut first_buffer, area, 4);
    output_layout_cache.apply_tachyon_loader_effect(
        &second_session_id,
        &mut second_buffer,
        area,
        4,
    );

    // Assert
    assert_eq!(output_layout_cache.tachyon_loader_effects.borrow().len(), 2);
    assert!(
        (0..TACHYON_LOADER_WIDTH)
            .any(|column| second_buffer[(column, 0)].fg == style::palette::warning())
    );
}

#[test]
fn test_output_layout_cache_evicts_tachyon_effects_with_layout_lru() {
    // Arrange
    let output_layout_cache = SessionOutputLayoutCache::default();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);

    // Act
    for session_index in 0..=SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT {
        let mut session = session_fixture();
        session.id = SessionId::from(format!("loader-session-{session_index:02}"));
        set_assistant_transcript(&mut session, &format!("active output {session_index}"));
        session.status = Status::InProgress;

        output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(),
            Some(&markdown_render_cache),
        );

        let mut buffer = Buffer::empty(area);
        for column in 0..TACHYON_LOADER_WIDTH {
            buffer[(column, 0)].set_symbol("▌");
        }
        output_layout_cache.apply_tachyon_loader_effect(&session.id, &mut buffer, area, 4);
    }

    // Assert
    let tachyon_loader_effects = output_layout_cache.tachyon_loader_effects.borrow();
    assert_eq!(
        tachyon_loader_effects.len(),
        SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT
    );
    assert!(!tachyon_loader_effects.contains_key(&SessionId::from("loader-session-00")));
    assert!(tachyon_loader_effects.contains_key(&SessionId::from("loader-session-16")));
}

#[test]
fn test_indicator_area_tracks_scrolled_row() {
    // Arrange
    let output_area = Rect::new(2, 3, 80, 10);

    // Act
    let loader_area = SessionOutput::indicator_area(output_area, 19, 12, TACHYON_LOADER_WIDTH);

    // Assert
    assert_eq!(loader_area, Some(Rect::new(2, 11, TACHYON_LOADER_WIDTH, 1)));
}

#[test]
fn test_spinner_frame_uses_injected_render_time() {
    // Arrange
    let session = session_fixture();

    // Act
    let output = SessionOutput::new(&session).spinner_frame(42);

    // Assert
    assert_eq!(output.spinner_frame, 42);
}

#[test]
fn test_scrollbar_layout_reserves_padding_before_track() {
    // Arrange
    let output_area = Rect::new(2, 3, 40, 10);

    // Act
    let content_area = SessionOutput::scrollbar_layout_area(output_area);

    // Assert
    assert_eq!(content_area.x, output_area.x);
    assert_eq!(content_area.y, output_area.y);
    assert_eq!(content_area.width, 38);
    assert_eq!(content_area.height, output_area.height);
}

#[test]
fn test_segmented_layout_matches_assembled_status_rows() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "Body\n\n");
    let area = Rect::new(0, 0, 80, 24);

    // Act
    for status in [
        Status::Review,
        Status::Done,
        Status::Rebasing,
        Status::InProgress,
        Status::Queued,
    ] {
        session.status = status;
        let layout = SessionOutput::derive_layout(&session, area, line_context(), None);
        let expected = session_output_assembly::tests::output_lines(&session, 80, None, None);

        // Assert
        assert_eq!(
            layout.lines.iter().cloned().collect::<Vec<_>>(),
            expected.lines
        );
        assert_eq!(
            layout.active_loader_line_index,
            expected.active_loader_line_index
        );
        assert_eq!(
            layout.queued_line_indices.as_ref(),
            expected.queued_line_indices.as_slice()
        );
        assert_eq!(usize::from(layout.line_count), layout.lines.len());
    }
}

#[test]
fn test_output_lines_metadata_marks_status_loader_not_user_text() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(
        &mut session,
        &format!("{} pasted transcript glyph", Icon::TachyonLoader),
    );
    session.status = Status::InProgress;
    let context = line_context();

    // Act
    let output_lines =
        session_output_assembly::tests::output_lines(&session, 78, context.active_progress, None);

    // Assert
    let loader_line_index = output_lines
        .active_loader_line_index
        .expect("active loader status row should be tracked");
    let loader_line = output_lines.lines[loader_line_index].to_string();
    assert!(loader_line.contains("Working..."));
    assert!(!loader_line.contains("pasted transcript glyph"));
}

#[test]
fn test_output_lines_done_output_keeps_workflow_notice_after_answer() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::AssistantAnswer, "streamed output"),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            ),
        ],
    );
    session.status = Status::Done;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let output_index = text
        .find("streamed output")
        .expect("streamed output should be rendered");
    let commit_index = text
        .find("[Commit] No changes to commit.")
        .expect("commit footer should be rendered");

    // Assert
    assert!(output_index < commit_index);
    assert!(!text.contains("Change Summary"));
}

/// Verifies later workflow notices retain their transcript order.
#[test]
fn test_output_lines_orders_trailing_workflow_notices() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::AssistantAnswer, "streamed output"),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Sync Assist] Attempt 1/3. Resolving conflicts in:\n- \
                 crates/agentty/src/runtime/worker.rs\n",
            ),
        ],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let output_index = text
        .find("streamed output")
        .expect("streamed output should be rendered");
    let commit_index = text
        .find("[Commit] No changes to commit.")
        .expect("commit notice should be rendered");
    let sync_index = text
        .find("[Sync Assist] Attempt 1/3.")
        .expect("sync notice should be rendered");

    // Assert
    assert!(output_index < commit_index);
    assert!(commit_index < sync_index);
}

/// Verifies typed assistant answers that begin with a workflow-notice
/// prefix remain grouped with assistant output.
#[test]
fn test_output_lines_typed_assistant_notice_prefix_stays_in_answer() {
    // Arrange
    let transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "summarize merge"),
        SessionMessage::conversation(
            1,
            SessionMessageKind::AssistantAnswer,
            "Assistant output.\n[Merge] this is literal assistant text.",
        ),
    ]);
    let mut session = session_fixture();
    session.transcript = Some(transcript);
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    // Assert
    assert!(text.contains("Assistant output.\n[Merge] this is literal assistant text."));
}

/// Verifies an orchestrator renders one animated child-status loader.
#[test]
fn test_output_lines_tracks_orchestration_loader() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading(
            "Orchestrating...\n- Protocol: running\n- UI: waiting".to_string(),
        ),
        lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::Orchestration,
        turn_position: None,
    });

    // Act
    let lines = session_output_assembly::tests::output_lines(&session, 78, None, None);
    let loader_line_index = lines
        .transient_loader_line_index
        .expect("orchestration loader should be tracked");

    // Assert
    assert!(
        lines.lines[loader_line_index]
            .to_string()
            .contains("Orchestrating...")
    );
    assert!(
        lines.lines[loader_line_index + 1]
            .to_string()
            .contains("- Protocol: running")
    );
    assert!(
        lines.lines[loader_line_index + 2]
            .to_string()
            .contains("- UI: waiting")
    );
}

/// Verifies persisted message padding cannot add extra rows to the
/// canonical one-empty-line transcript gap.
#[test]
fn test_output_lines_places_one_empty_line_between_messages() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    let visible_messages = [
        "Completed turn.",
        "[Commit] No changes to commit.",
        "[Review Request] Created PR 42",
        "[Sync] Successfully synced onto main",
        "[Branch Push] Auto-pushed published branch.",
        "≡ queued › Verify the spacing.",
        "≡ queued › Keep one empty line.",
    ];
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::AssistantAnswer, visible_messages[0]),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Review Request] Created PR 42\n",
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced onto main\n",
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Branch Push] Auto-pushed published branch.\n",
            ),
        ],
    );
    session.queued_messages = vec![
        queued_message(0, "\nVerify the spacing.\n"),
        queued_message(1, " \nKeep one empty line.\n\t"),
    ];

    // Act
    let rendered_lines = output_lines(&session, Rect::new(0, 0, 120, 16), line_context(), None)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let message_rows = visible_messages
        .iter()
        .map(|message| {
            rendered_lines
                .iter()
                .position(|line| line == message)
                .expect("message should be rendered")
        })
        .collect::<Vec<_>>();

    // Assert
    assert!(
        message_rows[..6]
            .windows(2)
            .all(|rows| rows[1] == rows[0] + 2)
    );
    assert_eq!(message_rows[6], message_rows[5] + 1);
}

#[test]
fn test_output_lines_in_progress_session_places_notice_after_active_turn() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::UserPrompt, "previous turn"),
            (SessionMessageKind::AssistantAnswer, "previous answer"),
            (SessionMessageKind::UserPrompt, "current turn"),
            (SessionMessageKind::AssistantAnswer, "working"),
        ],
    );
    session.status = Status::InProgress;
    session.queued_messages = vec![queued_message(0, "queued follow-up")];
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterActiveTurn,
        body: TransientMessageBody::Markdown(
            "[Sync] Queued until the current turn finishes.".to_string(),
        ),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::WorkflowNotice,
        turn_position: session.latest_user_prompt_position(),
    });

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let previous_answer_index = text
        .find("previous answer")
        .expect("previous answer should be rendered");
    let current_turn_index = text
        .find("current turn")
        .expect("current turn should be rendered");
    let notice_index = text
        .find("[Sync] Queued until the current turn finishes.")
        .expect("queued sync notice should be rendered");
    let queued_message_index = text
        .find("queued › queued follow-up")
        .expect("queued follow-up should be rendered");

    // Assert
    assert!(previous_answer_index < current_turn_index);
    assert!(current_turn_index < notice_index);
    assert!(notice_index < queued_message_index);
}

#[test]
fn test_output_lines_preserve_literal_private_use_character() {
    // Arrange
    let prompt = "  before \u{e000} after";
    let mut session = session_fixture();
    set_conversation_transcript(&mut session, vec![(SessionMessageKind::UserPrompt, prompt)]);
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 6), line_context(), None);
    let rendered_text = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(rendered_text.contains(" ›   before \u{e000} after"));
}

#[test]
fn test_output_lines_render_markdown_tables() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(
        &mut session,
        concat!(
            "| Message kind | Storage |\n",
            "| --- | --- |\n",
            "| User prompt | Session.output |",
        ),
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Message kind"));
    assert!(text.contains("Storage"));
    assert!(text.contains("User prompt"));
    assert!(text.contains("Session.output"));
    assert!(text.contains("┌"));
    assert!(!text.contains("| --- | --- |"));
}

#[test]
fn test_output_lines_render_mermaid_diagrams() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(
        &mut session,
        concat!(
            "```mermaid\n",
            "graph TD\n",
            "    A[Start] --> B[Finish]\n",
            "```",
        ),
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains("┌"));
    assert!(text.contains("▼"));
    assert!(!text.contains("graph TD"));
}

#[test]
fn test_output_lines_render_cyclic_mermaid_flowchart() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(
        &mut session,
        concat!(
            "```mermaid\n",
            "flowchart LR\n",
            "    U[User and TUI] --> C[Orchestrator controller]\n",
            "    M[Agent model] --> P[Typed command response]\n",
            "    P --> C\n",
            "    C --> S[ag-session service]\n",
            "    S --> A[Agentty host adapter]\n",
            "    A --> W[Session workers]\n",
            "    W --> E[Session events]\n",
            "    E --> C\n",
            "    C --> M\n",
            "```",
        ),
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 48), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Orchestrator controller"));
    assert!(text.contains("Session events"));
    assert!(text.contains("Session events ───▶ Orchestrator controller"));
    assert!(text.contains("Orchestrator controller ───▶ Agent model"));
    assert!(!text.contains("flowchart LR"));
}

#[test]
fn test_output_lines_uses_transcript_for_canceled_session() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "streamed output");
    session.status = Status::Canceled;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(!text.contains("Change Summary"));
    assert!(text.contains("streamed output"));
}

#[test]
fn test_output_lines_use_generic_in_progress_loader() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "some output");
    session.status = Status::InProgress;

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Working..."));
    assert!(text.contains(Icon::TachyonLoader.as_str()));
}
