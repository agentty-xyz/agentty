//! Application snapshot projection at the UI boundary.

use ratatui::Frame;
use ratatui::widgets::TableState;

use crate::app::AppViewSnapshot;
use crate::ui::{RenderCacheStore, RenderContext, SessionReviewSnapshot, style};

/// Projects application data into one terminal frame.
pub(crate) fn render_app(
    snapshot: &AppViewSnapshot<'_>,
    frame: &mut Frame,
    assigned_issue_table_state: &mut TableState,
    project_table_state: &mut TableState,
    render_cache_store: &RenderCacheStore,
    requested_review_table_state: &mut TableState,
    session_table_state: &mut TableState,
) {
    project_table_state.select(snapshot.project_selected_index);
    session_table_state.select(snapshot.session_selected_index);
    let session_review_snapshot =
        snapshot
            .session_review
            .as_ref()
            .map(|review| SessionReviewSnapshot {
                session_id: review.session_id,
                text: review.text,
            });
    let _theme_scope = style::scoped_active_theme(snapshot.settings.theme);

    super::render(
        frame,
        RenderContext {
            assigned_issue_selected_index: snapshot.assigned_issue_selected_index,
            assigned_issue_table_state,
            assigned_issues: snapshot.assigned_issues,
            active_project_id: snapshot.active_project_id,
            available_agent_clis: &snapshot.available_agent_clis,
            current_tab: snapshot.current_tab,
            git_branch: snapshot.git_branch,
            diff_layout_cache: render_cache_store.diff_layout_cache(),
            git_upstream_ref: snapshot.git_upstream_ref,
            git_status: snapshot.git_status,
            latest_available_version: snapshot.latest_available_version,
            markdown_render_cache: render_cache_store.markdown_render_cache(),
            update_status: snapshot.update_status,
            mode: snapshot.mode,
            mru_project_order: snapshot.mru_project_order,
            output_layout_cache: render_cache_store.session_output_layout_cache(),
            project_table_state,
            projects: snapshot.projects,
            review_comment_cache: &snapshot.review_comment_cache,
            session_review_snapshot: session_review_snapshot.as_ref(),
            requested_reviews: snapshot.requested_reviews,
            requested_review_selected_index: snapshot.requested_review_selected_index,
            requested_review_table_state,
            active_prompt_outputs: snapshot.active_prompt_outputs,
            session_branch_names: snapshot.session_branch_names,
            session_git_statuses: snapshot.session_git_statuses,
            session_index_by_id: snapshot.session_index_by_id,
            session_progress_messages: snapshot.session_progress_messages,
            session_update_versions: snapshot.session_update_versions,
            session_worktree_availability: snapshot.session_worktree_availability,
            settings: snapshot.settings,
            stats_activity: snapshot.stats_activity,
            sessions: snapshot.sessions,
            status_bar_fyi_rotation_index: snapshot.status_bar_fyi_rotation_index,
            table_state: session_table_state,
            working_dir: snapshot.working_dir,
            wall_clock_unix_seconds: snapshot.wall_clock_unix_seconds,
        },
    );
}
