use std::cell::RefCell;

use ratatui::Frame;
use ratatui::widgets::TableState;

use crate::app::AppViewSnapshot;
use crate::ui::{self, RenderCacheStore};

/// Runtime-owned state used to measure and render the terminal presentation.
///
/// Keeping render caches here prevents application orchestration from
/// depending on concrete UI cache implementations or their invalidation
/// details.
#[derive(Default)]
pub(crate) struct PresentationState {
    assigned_issue_table_state: RefCell<TableState>,
    project_table_state: RefCell<TableState>,
    render_cache_store: RenderCacheStore,
    requested_review_table_state: RefCell<TableState>,
    session_table_state: RefCell<TableState>,
}

impl PresentationState {
    /// Renders one immutable application snapshot through the single runtime
    /// presentation boundary.
    pub(crate) fn render(&self, snapshot: &AppViewSnapshot<'_>, frame: &mut Frame) {
        let mut assigned_issue_table_state = self.assigned_issue_table_state.borrow_mut();
        let mut project_table_state = self.project_table_state.borrow_mut();
        let mut requested_review_table_state = self.requested_review_table_state.borrow_mut();
        let mut session_table_state = self.session_table_state.borrow_mut();

        ui::render_app(
            snapshot,
            frame,
            &mut assigned_issue_table_state,
            &mut project_table_state,
            &self.render_cache_store,
            &mut requested_review_table_state,
            &mut session_table_state,
        );
    }

    /// Returns the UI cache collection shared by input metrics and rendering.
    pub(crate) fn render_cache_store(&self) -> &RenderCacheStore {
        &self.render_cache_store
    }
}
