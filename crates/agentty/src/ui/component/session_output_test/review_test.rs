use std::sync::Arc;

use ratatui::layout::Rect;

use super::super::{SessionOutputLayoutCache, SessionOutputLineContext};
use super::support::{
    line_context, output_lines, session_fixture, set_assistant_transcript,
    set_conversation_transcript, set_review_transient,
};
use crate::domain::session::{SessionId, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::ui::{markdown, session_output_assembly};

#[test]
fn test_output_layout_cache_tracks_manual_branch_publish_loader() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Publishing review request...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::BranchPublish,
        turn_position: None,
    });
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();

    // Act
    let layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        line_context(),
        Some(&markdown_render_cache),
    );
    let loader_line_index = layout
        .transient_loader_line_index
        .expect("manual publish loader should be tracked through the layout cache");

    // Assert
    assert!(
        layout
            .lines
            .iter()
            .nth(loader_line_index)
            .expect("loader row")
            .to_string()
            .contains("Publishing review request...")
    );
}

#[test]
fn test_output_layout_cache_keys_stacked_draft_preview() {
    // Arrange
    let mut session = session_fixture();
    session.is_draft = true;
    session.prompt = "First staged draft".to_string();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let context = line_context();

    // Act
    let root_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 12),
        context,
        Some(&markdown_render_cache),
    );
    session.parent_session_id = Some(SessionId::from("parent-session"));
    let stacked_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 12),
        context,
        Some(&markdown_render_cache),
    );
    let stacked_text = stacked_layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(!Arc::ptr_eq(&root_layout.lines, &stacked_layout.lines));
    assert!(stacked_text.contains("start the stacked"));
    assert!(stacked_text.contains("bundle from its parent"));
    assert!(stacked_text.contains("parent"));
}

#[test]
fn test_output_layout_cache_keys_review_text() {
    // Arrange
    let mut session = session_fixture();
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let base_context = SessionOutputLineContext {
        session_update_version: 7,
        ..line_context()
    };
    // Act
    let base_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        base_context,
        Some(&markdown_render_cache),
    );
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterCompletedTurn,
        body: TransientMessageBody::Markdown("## Review\n\n- Cached finding".to_string()),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Review,
        turn_position: None,
    });
    let review_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        base_context,
        Some(&markdown_render_cache),
    );

    // Assert
    assert!(review_layout.line_count > base_layout.line_count);
    assert!(!Arc::ptr_eq(&base_layout.lines, &review_layout.lines));
}

#[test]
fn test_output_cache_distinguishes_rebuilt_review_states_with_matching_versions() {
    // Arrange
    let mut loading_session = session_fixture();
    loading_session.status = Status::AgentReview;
    loading_session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Reviewing changes".to_string()),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Review,
        turn_position: None,
    });
    let mut ready_session = session_fixture();
    ready_session.status = Status::Review;
    set_review_transient(
        &mut ready_session,
        TransientMessageBody::Markdown("## Review\n\n- Stable finding".to_string()),
    );
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let loading_then_ready_cache = SessionOutputLayoutCache::default();
    let ready_then_loading_cache = SessionOutputLayoutCache::default();
    let output_area = Rect::new(0, 0, 80, 8);

    // Act
    loading_then_ready_cache.layout(
        &loading_session,
        output_area,
        line_context(),
        Some(&markdown_render_cache),
    );
    let refreshed_ready_layout = loading_then_ready_cache.layout(
        &ready_session,
        output_area,
        line_context(),
        Some(&markdown_render_cache),
    );
    ready_then_loading_cache.layout(
        &ready_session,
        output_area,
        line_context(),
        Some(&markdown_render_cache),
    );
    let regenerated_loading_layout = ready_then_loading_cache.layout(
        &loading_session,
        output_area,
        line_context(),
        Some(&markdown_render_cache),
    );
    let refreshed_ready_text = refreshed_ready_layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let regenerated_loading_text = regenerated_loading_layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert_eq!(loading_session.id, ready_session.id);
    assert_eq!(
        loading_session.transient_messages.version(),
        ready_session.transient_messages.version()
    );
    assert!(refreshed_ready_text.contains("Stable finding"));
    assert!(!regenerated_loading_text.contains("Stable finding"));
}

#[test]
fn test_output_lines_render_staged_draft_preview_for_new_session() {
    // Arrange
    let mut session = session_fixture();
    session.is_draft = true;
    session.prompt = "First draft\n\nSecond draft".to_string();

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Draft Session"));
    assert!(text.contains("Draft messages stay local until you press s in session view"));
    assert!(text.contains("First draft"));
    assert!(text.contains("Second draft"));
}

#[test]
fn test_output_lines_render_draft_preview_with_status_lines() {
    // Arrange
    let mut session = session_fixture();
    session.is_draft = true;
    set_assistant_transcript(
        &mut session,
        "[Paste Image Error] Clipboard is unavailable.",
    );
    session.prompt = "First draft".to_string();

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Draft Session"));
    assert!(text.contains("First draft"));
    assert!(text.contains("Paste Image Error"));
    assert!(text.contains("Clipboard is unavailable"));
}

#[test]
fn test_output_lines_render_staged_draft_preview_for_stacked_session() {
    // Arrange
    let mut session = session_fixture();
    session.is_draft = true;
    session.parent_session_id = Some(SessionId::from("parent-session"));
    session.prompt = "Stacked draft".to_string();

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Draft Session"));
    assert!(text.contains("start the stacked"));
    assert!(text.contains("bundle from its parent"));
    assert!(text.contains("parent"));
    assert!(text.contains("Stacked draft"));
}

#[test]
fn test_output_lines_render_empty_draft_preview_for_new_session() {
    // Arrange
    let mut session = session_fixture();
    session.is_draft = true;

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Draft Session"));
    assert!(text.contains("No draft messages staged yet."));
    assert!(text.contains("Use Enter to stage the first draft locally"));
}

#[test]
fn test_output_lines_render_empty_draft_preview_for_stacked_session() {
    // Arrange
    let mut session = session_fixture();
    session.is_draft = true;
    session.parent_session_id = Some(SessionId::from("parent-session"));

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Draft Session"));
    assert!(text.contains("No draft messages staged yet."));
    assert!(text.contains("start action appears after the parent is review-ready"));
}

/// Verifies merge failures render after focused review content.
#[test]
fn test_output_lines_places_review_before_trailing_workflow_notices() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::AssistantAnswer, "implemented fix"),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Merge Error] Cannot merge branch\n",
            ),
        ],
    );
    session.status = Status::Review;
    let review_text = "## Review\n\n### Project Impact\n\n- Documentation-only change.\n\n### \
                       Suggestions\n\n- None.";
    set_review_transient(
        &mut session,
        TransientMessageBody::Markdown(review_text.to_string()),
    );
    session.reconcile_transient_messages();

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let output_index = text
        .find("implemented fix")
        .expect("completed output should be rendered");
    let review_index = text.find("Review").expect("review should be rendered");
    let merge_error_index = text
        .find("[Merge Error] Cannot merge branch")
        .expect("merge error should be rendered");

    // Assert
    assert!(output_index < review_index);
    assert!(review_index < merge_error_index);
    assert!(text.contains("Project Impact\n- Documentation-only change."));
    assert!(text.contains("Suggestions\n- None."));
    assert!(!text.contains("type \"/apply\" to verify and apply"));
}

/// Verifies focused-review failures remain visible after the transient
/// loading status returns to `Review`.
#[test]
fn test_output_lines_review_session_shows_review_status_message_when_text_missing() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    let review_status_message = "Review assist unavailable: empty provider response";
    set_review_transient(
        &mut session,
        TransientMessageBody::Plain(review_status_message.to_string()),
    );

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains(review_status_message));
}

/// Verifies a manual review-request publish renders as an animated
/// session-chat row instead of requiring a modal loading popup.
#[test]
fn test_output_lines_tracks_manual_branch_publish_loader() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Publishing review request...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::BranchPublish,
        turn_position: None,
    });

    // Act
    let lines = session_output_assembly::tests::output_lines(&session, 78, None, None);
    let loader_line_index = lines
        .transient_loader_line_index
        .expect("manual publish loader should be tracked");

    // Assert
    assert!(
        lines.lines[loader_line_index]
            .to_string()
            .contains("Publishing review request...")
    );
}

/// Verifies persisted review-request creation renders as one logical
/// transcript line.
#[test]
fn test_output_lines_renders_persisted_review_request_on_one_line() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    let created_message =
        "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42";
    set_conversation_transcript(
        &mut session,
        vec![
            (
                SessionMessageKind::AssistantAnswer,
                "Published the changes.",
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42\n",
            ),
            (SessionMessageKind::UserPrompt, "continue the session"),
        ],
    );

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 120, 8), line_context(), None);

    // Assert
    let created_message_index = lines
        .iter()
        .position(|line| line.to_string() == created_message)
        .expect("review request notice should be rendered");
    let later_prompt_index = lines
        .iter()
        .position(|line| line.to_string().contains("continue the session"))
        .expect("later user prompt should be rendered");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.to_string() == created_message)
            .count(),
        1
    );
    assert!(created_message_index < later_prompt_index);
}

/// Verifies completed published-branch pushes render through transcript
/// notices instead of appending a sticky synthetic status row.
#[test]
fn test_output_lines_uses_transcript_for_completed_published_branch_push() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![(
            SessionMessageKind::WorkflowNotice,
            "\n[Branch Push] Auto-pushed published branch after completed turn.\n",
        )],
    );
    session.published_upstream_ref = Some("origin/wt/session-id".to_string());
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = session_output_assembly::tests::output_lines(&session, 78, None, None);
    let text = lines
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert_eq!(lines.transient_loader_line_index, None);
    assert!(text.contains("[Branch Push]"));
    assert_eq!(
        text.matches("Auto-pushed published branch after completed turn.")
            .count(),
        1
    );
}

/// Verifies focused-review fallback text is rendered literally instead of
/// being interpreted as markdown.
#[test]
fn test_output_lines_review_status_message_preserves_markdown_characters() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    let review_status_message = "# Review *failed* for `tool`";
    set_review_transient(
        &mut session,
        TransientMessageBody::Plain(review_status_message.to_string()),
    );

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains(review_status_message));
}

#[test]
fn test_output_lines_review_session_keeps_transcript_only() {
    // Arrange
    let mut session = session_fixture();
    set_assistant_transcript(&mut session, "implemented the feature");
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("implemented the feature"));
    assert!(!text.contains("No changes"));
    assert!(!text.contains("Current Turn"));
    assert!(!text.contains("Session Changes"));
}

#[test]
fn test_output_lines_agent_review_mode_shows_assisted_text() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::AgentReview;
    let assisted_text = "## Review\n\n- Focused finding";
    set_review_transient(
        &mut session,
        TransientMessageBody::Markdown(assisted_text.to_string()),
    );

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Focused finding"));
    assert!(!text.contains("Review is not available."));
}
