//! Application-view projection adapter for terminal frame rendering.

use ratatui::Frame;
use ratatui::widgets::TableState;

use crate::app::App;
use crate::presentation::style;
use crate::presentation::table_state::TableViewState;
use crate::ui::{RenderCacheStore, RenderContext, render};

/// Bridges renderer-neutral table state into Ratatui for one frame and writes
/// viewport changes back after rendering.
struct TableStateAdapter<'a> {
    state: TableState,
    view_state: &'a mut TableViewState,
}

impl<'a> TableStateAdapter<'a> {
    /// Creates a Ratatui table state from the app-owned presentation values.
    fn new(view_state: &'a mut TableViewState) -> Self {
        let mut state = TableState::default();
        state.select(view_state.selected());
        *state.offset_mut() = view_state.offset();

        Self { state, view_state }
    }

    /// Returns the Ratatui state used by widgets during this frame.
    fn state_mut(&mut self) -> &mut TableState {
        &mut self.state
    }
}

impl Drop for TableStateAdapter<'_> {
    fn drop(&mut self) {
        self.view_state.select(self.state.selected());
        self.view_state.set_offset(self.state.offset());
    }
}

/// Renders one complete terminal frame from the app's borrowed view projection.
pub(crate) fn draw(app: &mut App, frame: &mut Frame, render_cache_store: &RenderCacheStore) {
    let parts = app.render_parts();
    let _theme_scope = style::scoped_active_theme(parts.settings.theme);
    let mut project_table_state = TableStateAdapter::new(parts.project.table_state);
    let mut requested_review_table_state =
        TableStateAdapter::new(parts.requested_review_table_state);
    let mut session_table_state = TableStateAdapter::new(parts.session.table_state);

    render(
        frame,
        RenderContext {
            active_project_id: parts.project.active_project_id,
            available_agent_clis: &parts.available_agent_clis,
            current_tab: parts.current_tab,
            git_branch: parts.project.git_branch,
            diff_layout_cache: render_cache_store.diff_layout_cache(),
            git_upstream_ref: parts.project.git_upstream_ref,
            git_status: parts.project.git_status,
            latest_available_version: parts.latest_available_version,
            markdown_render_cache: render_cache_store.markdown_render_cache(),
            update_status: parts.update_status,
            mode: parts.mode,
            output_layout_cache: render_cache_store.session_output_layout_cache(),
            project_table_state: project_table_state.state_mut(),
            projects: parts.project.project_items,
            review_comment_cache: &parts.review_comment_cache,
            session_review_snapshot: parts.session_review_snapshot.as_ref(),
            requested_reviews: parts.requested_reviews,
            requested_review_selected_index: parts.requested_review_selected_index,
            requested_review_table_state: requested_review_table_state.state_mut(),
            active_prompt_outputs: parts.session.active_prompt_outputs,
            session_branch_names: parts.session.session_branch_names,
            session_git_statuses: parts.session.session_git_statuses,
            session_index_by_id: parts.session.session_index_by_id,
            session_progress_messages: parts.session_progress_messages,
            session_update_versions: parts.last_seen_session_update_versions,
            session_worktree_availability: parts.session.session_worktree_availability,
            settings: parts.settings,
            system_log_tail_offset: parts.system_log_tail_offset,
            system_logs: parts.system_logs,
            stats_activity: parts.session.stats_activity,
            sessions: parts.session.sessions,
            status_bar_fyi_rotation_index: parts.status_bar_fyi_rotation_index,
            table_state: session_table_state.state_mut(),
            working_dir: parts.project.working_dir,
            wall_clock_unix_seconds: parts.wall_clock_unix_seconds,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_state_adapter_round_trips_selection_and_viewport_offset() {
        // Arrange
        let mut view_state = TableViewState::default();
        view_state.select(Some(3));
        view_state.set_offset(2);

        // Act
        {
            let mut adapter = TableStateAdapter::new(&mut view_state);
            assert_eq!(adapter.state_mut().selected(), Some(3));
            assert_eq!(adapter.state_mut().offset(), 2);
            adapter.state_mut().select(Some(5));
            *adapter.state_mut().offset_mut() = 4;
        }

        // Assert
        assert_eq!(view_state.selected(), Some(5));
        assert_eq!(view_state.offset(), 4);
    }
}
