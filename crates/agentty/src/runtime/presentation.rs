use std::cell::{RefCell, RefMut};

use ratatui::widgets::TableState;

use crate::ui::RenderCacheStore;

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
    /// Borrows the assigned-issue table viewport for one render pass.
    pub(crate) fn assigned_issue_table_state(&self) -> RefMut<'_, TableState> {
        self.assigned_issue_table_state.borrow_mut()
    }

    /// Borrows the project-list table viewport for one render pass.
    pub(crate) fn project_table_state(&self) -> RefMut<'_, TableState> {
        self.project_table_state.borrow_mut()
    }

    /// Returns the UI cache collection shared by input metrics and rendering.
    pub(crate) fn render_cache_store(&self) -> &RenderCacheStore {
        &self.render_cache_store
    }

    /// Borrows the requested-review table viewport for one render pass.
    pub(crate) fn requested_review_table_state(&self) -> RefMut<'_, TableState> {
        self.requested_review_table_state.borrow_mut()
    }

    /// Borrows the session-list table viewport for one render pass.
    pub(crate) fn session_table_state(&self) -> RefMut<'_, TableState> {
        self.session_table_state.borrow_mut()
    }
}
