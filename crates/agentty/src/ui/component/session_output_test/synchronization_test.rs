use std::sync::Arc;

use ratatui::layout::Rect;

use super::super::{
    SessionOutput, SessionOutputLayout, SessionOutputLayoutCache, SessionOutputLineContext,
    activity_summary_lines, live_activity_lines,
};
use super::support::{line_context, output_lines, session_fixture, set_conversation_transcript};
use crate::domain::session::Status;
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::ui::{markdown, session_output_assembly, style};

fn layout_text(layout: &SessionOutputLayout) -> String {
    layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

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

#[test]
fn test_task_chain_graph_updates_refresh_cached_body_and_line_count() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::UserPrompt, "Add search"),
            (SessionMessageKind::AgentGraph, "chain"),
        ],
    );
    let cache = SessionOutputLayoutCache::default();
    let area = Rect::new(0, 0, 60, 30);
    let graph = session_output_assembly::tests::task_chain_graph("chain");
    let graph_context = SessionOutputLineContext {
        task_chain_graph: Some(&graph),
        ..line_context()
    };

    // Act
    let loading = cache.layout(&session, area, line_context(), None);
    let live = cache.layout(&session, area, graph_context, None);
    let live_again = cache.layout(&session, area, graph_context, None);
    let measured =
        SessionOutput::rendered_line_count(&session, 60, 30, graph_context, None, Some(&cache));

    // Assert
    let loading_text = layout_text(&loading);
    let live_text = layout_text(&live);
    assert!(loading_text.contains("Loading status…"));
    assert!(live_text.contains("● Awaiting your approval"));
    assert!(!live_text.contains("Loading status…"));
    assert!(Arc::ptr_eq(&live.lines, &live_again.lines));
    assert_eq!(usize::from(live.line_count), live.lines.len());
    assert_eq!(measured, live.line_count);
    assert!(live.line_count > loading.line_count);
}

#[test]
fn test_live_activity_invalidates_cached_output_without_transcript_changes() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    set_conversation_transcript(
        &mut session,
        vec![(SessionMessageKind::UserPrompt, "inspect")],
    );
    let cache = SessionOutputLayoutCache::default();
    let area = Rect::new(0, 0, 80, 20);
    let mut texts = Vec::new();

    // Act
    for status in ["…", "✓", "✗", "!"] {
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterActiveTurn,
            body: TransientMessageBody::Plain(format!("{status} **Read**\n… skill review")),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Activity,
            turn_position: Some(0),
        });
        let layout = cache.layout(&session, area, line_context(), None);
        texts.push(
            layout
                .lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    session
        .transient_messages
        .retract(TransientMessageSlot::Activity);
    let cleared = cache.layout(&session, area, line_context(), None);
    let cleared_text = cleared
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    for (text, status) in texts.iter().zip(["…", "✓", "✗", "!"]) {
        assert!(text.contains(&format!("{status} **Read**")));
        assert!(text.contains("… skill review"));
        assert!(text.find("inspect") < text.find("**Read**"));
        assert_eq!(text.matches("**Read**").count(), 1);
    }
    assert!(!cleared_text.contains("**Read**"));
    assert!(cleared_text.contains("inspect"));
}

#[test]
fn compact_activity_clips_to_terminal_width_without_wrapping_or_markdown() {
    // Arrange
    let activity = "✗ shell (exit 1) · rg '**literal**' 界界界界\n  first error with many \
                    details\n  second error …";

    // Act, Assert
    for width in [0, 1, 12, 100] {
        let lines = live_activity_lines(activity, width);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.width() <= width));
        if width == 100 {
            assert_eq!(lines[0].to_string(), activity.lines().next().expect("row"));
        } else if width == 12 {
            assert!(lines[0].to_string().ends_with("..."));
        }
    }
    assert_eq!(
        live_activity_lines("", 80),
        Vec::<ratatui::text::Line<'_>>::new()
    );
}

#[test]
fn activity_outcomes_use_semantic_colors_in_live_and_saved_rows() {
    // Arrange, Act, Assert
    for (text, color) in [
        ("✗ shell", style::palette::danger()),
        ("! shell (interrupted)", style::palette::warning()),
        ("… shell", style::palette::info()),
        ("✓ shell", style::palette::success()),
        ("Tools: shell ×1", style::palette::text_muted()),
    ] {
        assert_eq!(live_activity_lines(text, 80)[0].style.fg, Some(color));
        assert_eq!(
            activity_summary_lines(text, 80)[0].spans[0].style.fg,
            Some(color)
        );
    }
}
