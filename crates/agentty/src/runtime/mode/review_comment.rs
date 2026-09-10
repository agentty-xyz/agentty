use ag_forge::ReviewCommentSnapshot;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::presentation::app_mode::{
    AppMode, DiffReviewComments, DiffScrollCache, DiffSidebarFocus, ReviewCommentSelection,
};
use crate::presentation::review_comment;
use crate::runtime::EventResult;
use crate::ui::{RenderCacheStore, page};

/// Handles agent-resolution, selection, and detail scrolling while the
/// Comments section of the unified diff workspace is focused.
pub(crate) async fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments: Some(mut review_comments),
        restore,
        mut scroll_cache,
        session_id,
        mut scroll_offset,
        selected_diff_line_index,
    } = mode
    else {
        app.mode = mode;

        return EventResult::Continue;
    };
    let can_reply = session_allows_review_comment_reply(app, session_id.as_str());
    let item_count =
        page::review_comment::review_comment_item_count(review_comments.comment_snapshot.as_ref());
    if key.code == KeyCode::Enter
        && key.modifiers == KeyModifiers::NONE
        && !review_comments.selected_comments.is_empty()
        && let Some(snapshot) = review_comments.comment_snapshot.as_ref()
    {
        let snapshot = snapshot.clone();
        let submitted_comments = review_comments.selected_comments.clone();
        app.mode = AppMode::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments: Some(review_comments),
            restore,
            scroll_cache,
            selected_diff_line_index,
            session_id: session_id.clone(),
            scroll_offset,
        };
        let outcome = app
            .resolve_session_review_comments(&session_id, &snapshot, &submitted_comments)
            .await;
        apply_review_comment_resolution_outcome(app, outcome);

        return EventResult::Continue;
    }

    handle_review_comment_navigation(
        &ReviewCommentNavigationInput {
            can_reply,
            content_area,
            diff: &diff,
            item_count,
            render_cache_store,
        },
        key,
        &mut review_comments,
        &mut scroll_cache,
        &mut scroll_offset,
    );

    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments: Some(review_comments),
        restore,
        scroll_cache,
        selected_diff_line_index,
        session_id,
        scroll_offset,
    };

    EventResult::Continue
}

/// Immutable inputs used while navigating review comments.
struct ReviewCommentNavigationInput<'a> {
    can_reply: bool,
    content_area: Rect,
    diff: &'a str,
    item_count: usize,
    render_cache_store: &'a RenderCacheStore,
}

/// Applies comment selection, marking, focus, and detail-scroll keys.
fn handle_review_comment_navigation(
    input: &ReviewCommentNavigationInput<'_>,
    key: KeyEvent,
    review_comments: &mut DiffReviewComments,
    scroll_cache: &mut Option<DiffScrollCache>,
    scroll_offset: &mut u16,
) {
    if input.can_reply {
        toggle_selected_comment(
            &key,
            review_comments.comment_snapshot.as_ref(),
            review_comments.selected_comment_index,
            &mut review_comments.selected_comments,
        );
    }
    match key.code {
        KeyCode::Char('j') if key.modifiers == KeyModifiers::NONE => {
            let next_index =
                next_selected_index(review_comments.selected_comment_index, input.item_count);
            if next_index != review_comments.selected_comment_index {
                review_comments.selected_comment_index = next_index;
                *scroll_offset = 0;
            }
        }
        KeyCode::Char('k') if key.modifiers == KeyModifiers::NONE => {
            let previous_index =
                previous_selected_index(review_comments.selected_comment_index, input.item_count);
            if previous_index != review_comments.selected_comment_index {
                review_comments.selected_comment_index = previous_index;
                *scroll_offset = 0;
            }
        }
        KeyCode::Down => {
            let max_scroll_offset = review_comment_max_scroll_offset(
                input.render_cache_store,
                input.content_area,
                input.diff,
                review_comments.comment_snapshot.as_ref(),
                review_comments.comment_error.as_deref(),
                review_comments.is_loading_comments,
                review_comments.selected_comment_index,
            );
            *scroll_offset = increment_scroll_offset(*scroll_offset, max_scroll_offset);
        }
        KeyCode::Up => {
            *scroll_offset = scroll_offset.saturating_sub(1);
        }
        KeyCode::Esc => {
            focus_files(review_comments, scroll_cache, scroll_offset);
        }
        KeyCode::Char('f') if key.modifiers == KeyModifiers::NONE => {
            focus_files(review_comments, scroll_cache, scroll_offset);
        }
        _ => {}
    }
}

fn focus_files(
    review_comments: &mut DiffReviewComments,
    scroll_cache: &mut Option<DiffScrollCache>,
    scroll_offset: &mut u16,
) {
    review_comments.sidebar_focus = DiffSidebarFocus::Files;
    *scroll_cache = None;
    *scroll_offset = 0;
}

fn review_comment_max_scroll_offset(
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    diff: &str,
    comment_snapshot: Option<&ReviewCommentSnapshot>,
    comment_error: Option<&str>,
    is_loading_comments: bool,
    selected_comment_index: usize,
) -> u16 {
    page::review_comment::review_comment_view_max_scroll_offset(
        comment_snapshot,
        comment_error,
        is_loading_comments,
        diff,
        page::review_comment::ReviewCommentRenderCaches {
            diff_layout: render_cache_store.diff_layout_cache(),
            markdown: render_cache_store.markdown_render_cache(),
        },
        selected_comment_index,
        content_area,
    )
}

/// Advances one detail row while clamping stale and terminal offsets.
fn increment_scroll_offset(scroll_offset: u16, max_scroll_offset: u16) -> u16 {
    scroll_offset
        .min(max_scroll_offset)
        .saturating_add(1)
        .min(max_scroll_offset)
}

/// Returns whether the review-comments page still belongs to a session that
/// may accept direct user-driven comment work.
fn session_allows_review_comment_reply(app: &App, session_id: &str) -> bool {
    app.sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .is_some_and(crate::domain::session::Session::allows_review_comment_reply)
}

/// Toggles the selected actionable thread in the next agent batch.
fn toggle_selected_comment(
    key: &KeyEvent,
    comment_snapshot: Option<&ReviewCommentSnapshot>,
    selected_comment_index: usize,
    selected_comments: &mut Vec<ReviewCommentSelection>,
) {
    if key.code != KeyCode::Char(' ') || key.modifiers != KeyModifiers::NONE {
        return;
    }
    let Some(thread_id) = comment_snapshot.and_then(|snapshot| {
        review_comment::selected_actionable_thread_id(snapshot, selected_comment_index)
    }) else {
        return;
    };

    review_comment::toggle_selection(selected_comments, thread_id);
}

/// Applies presentation navigation returned by the review-comment workflow.
fn apply_review_comment_resolution_outcome(app: &mut App, outcome: ReviewCommentResolutionOutcome) {
    match outcome {
        ReviewCommentResolutionOutcome::KeepReviewComments => {}
        ReviewCommentResolutionOutcome::ShowSession { session_id } => {
            app.mode = AppMode::View {
                session_id,
                scroll_offset: None,
            };
        }
    }
}

/// Returns the next wrapped selection index.
fn next_selected_index(selected_index: usize, item_count: usize) -> usize {
    if item_count == 0 {
        return selected_index;
    }

    (selected_index.min(item_count - 1) + 1) % item_count
}

/// Returns the previous wrapped selection index.
fn previous_selected_index(selected_index: usize, item_count: usize) -> usize {
    if item_count == 0 {
        return selected_index;
    }
    let selected_index = selected_index.min(item_count - 1);
    if selected_index == 0 {
        return item_count - 1;
    }

    selected_index - 1
}

#[cfg(test)]
#[path = "review_comment_test.rs"]
mod tests;
