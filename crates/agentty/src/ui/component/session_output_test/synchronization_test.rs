//! Session output rebase caching, sync-status ordering, live-timeline, and
//! usage-footer rendering unit tests.

use std::sync::Arc;

use ag_contracts::{OutputEvent, OutputKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use super::super::{
    SessionOutput, SessionOutputLayoutCache, SessionOutputLineContext, activity_summary_lines,
};
use super::support::{line_context, output_lines, session_fixture, set_conversation_transcript};
use crate::domain::session::Status;
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::domain::turn_timeline::TurnTimeline;
use crate::test_support::activity_event;
use crate::ui::component::turn_timeline;
use crate::ui::{Component, markdown, style};

/// Verifies workflow-only status changes reuse the stable transcript body
/// while a rebase adds progress output.
#[test]
fn test_output_layout_cache_reuses_completed_body_during_rebase() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::UserPrompt, "implement cache reuse"),
            (
                SessionMessageKind::AssistantAnswer,
                "Completed answer stays stable.",
            ),
        ],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let review_context = SessionOutputLineContext {
        session_update_version: 7,
        ..line_context()
    };
    let review_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        review_context,
        Some(&markdown_render_cache),
    );

    // Act
    session.status = Status::Rebasing;
    let rebase_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        SessionOutputLineContext {
            active_progress: Some("Rebasing branch"),
            session_update_version: 8,
            ..line_context()
        },
        Some(&markdown_render_cache),
    );
    let rebase_text = rebase_layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    // Assert
    assert!(!Arc::ptr_eq(&review_layout.lines, &rebase_layout.lines));
    assert_eq!(output_layout_cache.body_entries.borrow().len(), 1);
    assert_eq!(output_layout_cache.entries.borrow().len(), 1);
    assert!(Arc::ptr_eq(
        &review_layout.lines.body,
        &rebase_layout.lines.body
    ));
    assert!(rebase_text.contains("Completed answer stays stable."));
    assert!(rebase_text.contains("Rebasing..."));
}

/// Verifies post-sync work remains below the durable sync result that
/// caused it, even while focused review starts in parallel.
#[test]
fn test_output_lines_orders_post_sync_statuses_chronologically() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (
                SessionMessageKind::AssistantAnswer,
                "implemented the change",
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced wt/session onto origin/main\n",
            ),
        ],
    );
    session.status = Status::Review;
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterCompletedTurn,
        body: TransientMessageBody::Markdown("[Commit] No changes to commit.".to_string()),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::WorkflowNotice,
        turn_position: session.latest_user_prompt_position(),
    });
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading(
            "Auto-pushing published branch after completed turn...".to_string(),
        ),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::PublishedBranchSync,
        turn_position: session.latest_user_prompt_position(),
    });
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Markdown(
            "## Review\n\nChronological review result.".to_string(),
        ),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Review,
        turn_position: session.latest_user_prompt_position(),
    });

    // Act
    let text = output_lines(&session, Rect::new(0, 0, 120, 12), line_context(), None)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let commit_index = text
        .find("[Commit] No changes to commit.")
        .expect("commit notice should render");
    let sync_index = text
        .find("[Sync] Successfully synced")
        .expect("sync result should render");
    let auto_push_index = text
        .find("Auto-pushing published branch")
        .expect("auto-push status should render");
    let review_index = text
        .find("Chronological review result.")
        .expect("focused-review status should render");

    // Assert
    assert!(commit_index < sync_index);
    assert!(sync_index < auto_push_index);
    assert!(auto_push_index < review_index);
}

/// Shows `timeline` as the session's live timeline.
fn show_timeline(session: &mut crate::domain::session::Session, timeline: &mut TurnTimeline) {
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterActiveTurn,
        body: TransientMessageBody::Timeline(Arc::new(timeline.snapshot())),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Turn,
        turn_position: Some(0),
    });
}

/// Joins rendered layout rows into one string.
fn layout_text(layout: &super::super::SessionOutputLayout) -> String {
    layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_live_timeline_replaces_the_loader_and_reuses_unchanged_steps() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    set_conversation_transcript(
        &mut session,
        vec![(SessionMessageKind::UserPrompt, "inspect")],
    );
    let cache = SessionOutputLayoutCache::default();
    let area = Rect::new(0, 0, 80, 20);
    let mut timeline = TurnTimeline::default();
    timeline.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Message,
        },
        0,
    );
    timeline.observe_output(
        OutputEvent::Completed {
            id: 0,
            text: "Checking the tests.".into(),
        },
        0,
    );
    timeline.observe_activity(activity_event("call"), 0);
    let before = layout_text(&cache.layout(&session, area, line_context(), None));

    // Act
    show_timeline(&mut session, &mut timeline);
    let running = cache.layout(&session, area, line_context(), None);
    let running_text = layout_text(&running);
    timeline.observe_output(
        OutputEvent::Started {
            id: 1,
            kind: OutputKind::Message,
        },
        0,
    );
    timeline.observe_output(
        OutputEvent::Completed {
            id: 1,
            text: "Next step.".into(),
        },
        0,
    );
    show_timeline(&mut session, &mut timeline);
    let grown_text = layout_text(&cache.layout(&session, area, line_context(), None));
    session
        .transient_messages
        .retract(TransientMessageSlot::Turn);
    let cleared_text = layout_text(&cache.layout(&session, area, line_context(), None));

    // Assert
    assert!(before.contains("Working..."));
    assert!(running_text.contains("• Checking the tests."));
    assert!(running_text.contains("Running"));
    assert!(!running_text.contains("Working..."));
    assert_eq!(running.spinner_rows.len(), 1);
    assert!(running_text.find("inspect") < running_text.find("Checking"));
    assert!(grown_text.contains("• Next step."));
    assert!(cleared_text.contains("Working..."));
    assert!(!cleared_text.contains("Checking"));
}

#[test]
fn test_running_rows_paint_the_spinner_frame() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    let mut timeline = TurnTimeline::default();
    timeline.observe_activity(activity_event("call"), 100_000);
    show_timeline(&mut session, &mut timeline);
    let mut terminal = Terminal::new(TestBackend::new(40, 6)).expect("terminal");

    // Act
    terminal
        .draw(|frame| {
            SessionOutput::new(&session)
                .spinner_frame(3)
                .now_millis(112_400)
                .render(frame, frame.area());
        })
        .expect("draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let row = (0..buffer.area.height)
        .find(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, *row)].symbol())
                .collect::<String>()
                .contains("Running")
        })
        .expect("running row");
    let row_text = (0..buffer.area.width)
        .map(|column| buffer[(column, row)].symbol())
        .collect::<String>();
    assert_eq!(
        buffer[(turn_timeline::ROW_GLYPH_COLUMN, row)].symbol(),
        turn_timeline::spinner_glyph(3)
    );
    assert!(row_text.ends_with(" 12s"), "{row_text}");
    assert_eq!(
        buffer[(buffer.area.width - 1, row)].fg,
        style::palette::warning()
    );
}

#[test]
fn test_live_timeline_counts_hidden_steps_and_shows_nothing_for_an_empty_message() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    let area = Rect::new(0, 0, 80, 20);
    let mut empty = TurnTimeline::default();
    empty.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Message,
        },
        0,
    );
    let mut long = TurnTimeline::default();
    for id in 0..401 {
        long.observe_output(
            OutputEvent::Started {
                id,
                kind: OutputKind::Message,
            },
            0,
        );
        long.observe_output(
            OutputEvent::Completed {
                id,
                text: format!("Step {id}."),
            },
            0,
        );
    }

    // Act
    show_timeline(&mut session, &mut empty);
    let empty_text = output_lines(&session, area, line_context(), None)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    show_timeline(&mut session, &mut long);
    let long_text = output_lines(&session, area, line_context(), None)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(!empty_text.contains('•'));
    assert!(!empty_text.contains("Working..."));
    assert!(long_text.contains("+1 earlier step"));
    assert!(!long_text.contains("• Step 0."));
    assert!(long_text.contains("• Step 400."));
}

#[test]
fn test_spinner_rows_scrolled_out_of_view_stay_unpainted() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    let mut timeline = TurnTimeline::default();
    for id in 0..8 {
        timeline.observe_output(
            OutputEvent::Started {
                id,
                kind: OutputKind::Message,
            },
            0,
        );
        timeline.observe_output(
            OutputEvent::Completed {
                id,
                text: format!("Step {id}."),
            },
            0,
        );
    }
    timeline.observe_activity(activity_event("call"), 0);
    show_timeline(&mut session, &mut timeline);
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).expect("terminal");

    // Act
    terminal
        .draw(|frame| {
            SessionOutput::new(&session)
                .scroll_offset(0)
                .spinner_frame(3)
                .render(frame, frame.area());
        })
        .expect("draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let text = (0..buffer.area.height)
        .flat_map(|row| (0..buffer.area.width).map(move |column| (column, row)))
        .map(|position| buffer[position].symbol())
        .collect::<String>();
    assert!(!text.contains("Running"));
    assert!(!text.contains(turn_timeline::spinner_glyph(3)));
}

#[test]
fn activity_summary_rows_are_muted_and_clipped() {
    // Arrange, Act
    let lines = activity_summary_lines("Tools: shell ×1, a-very-long-tool-name ×2", 12);

    // Assert
    assert!(lines.iter().all(|line| line.width() <= 12));
    assert_eq!(
        lines[0].spans[0].style.fg,
        Some(style::palette::text_muted())
    );
}
