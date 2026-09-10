use std::sync::Arc;

use ratatui::layout::Rect;

use super::super::{SessionOutputLayoutCache, SessionOutputLineContext};
use super::support::{line_context, output_lines, session_fixture, set_conversation_transcript};
use crate::domain::session::Status;
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::ui::markdown;

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
