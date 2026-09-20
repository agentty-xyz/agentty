use std::cell::{Cell, RefCell};

use ratatui::Frame;
use ratatui::widgets::TableState;

use crate::app::AppViewSnapshot;
use crate::presentation::viewport::{LayoutSnapshot, ScrollbarDrag};
use crate::ui::{self, RenderCacheStore};

/// Runtime-owned state used to measure and render the terminal presentation.
///
/// Keeping render caches here prevents application orchestration from
/// depending on concrete UI cache implementations or their invalidation
/// details. The layout snapshot and scrollbar drag state live here too because
/// they describe the painted frame and pointer interaction with it, not
/// application workflow state.
#[derive(Default)]
pub(crate) struct PresentationState {
    /// Scrollable-panel geometry recorded by the last drawn frame.
    layout_snapshot: Cell<LayoutSnapshot>,
    /// Scrollbar drag in progress, if any.
    mouse_drag: Cell<Option<ScrollbarDrag>>,
    project_table_state: RefCell<TableState>,
    render_cache_store: RenderCacheStore,
    /// Base page from the last successful draw. It is compared before each
    /// draw to invalidate the terminal buffer when routing selects another
    /// page.
    rendered_surface: Cell<Option<ui::router::SurfaceKind>>,
    session_table_state: RefCell<TableState>,
}

impl PresentationState {
    /// Returns whether the terminal must be cleared before painting `snapshot`.
    pub(crate) fn terminal_clear_needed(&self, snapshot: &AppViewSnapshot<'_>) -> bool {
        let current_surface = ui::router::surface_kind_for_mode(snapshot.mode);

        self.rendered_surface
            .get()
            .is_some_and(|rendered_surface| rendered_surface != current_surface)
    }

    /// Records the base page painted by a successful terminal draw.
    pub(crate) fn record_rendered_surface(&self, snapshot: &AppViewSnapshot<'_>) {
        let rendered_surface = ui::router::surface_kind_for_mode(snapshot.mode);

        self.rendered_surface.set(Some(rendered_surface));
    }

    /// Renders one immutable application snapshot through the single runtime
    /// presentation boundary and records the painted panel geometry for
    /// pointer hit-testing.
    pub(crate) fn render(&self, snapshot: &AppViewSnapshot<'_>, frame: &mut Frame) {
        let mut project_table_state = self.project_table_state.borrow_mut();
        let mut session_table_state = self.session_table_state.borrow_mut();

        let layout_snapshot = ui::render_app(
            snapshot,
            frame,
            &mut project_table_state,
            &self.render_cache_store,
            &mut session_table_state,
        );
        self.set_layout_snapshot(layout_snapshot);
    }

    /// Returns the scrollable-panel geometry recorded by the last frame.
    pub(crate) fn layout_snapshot(&self) -> LayoutSnapshot {
        self.layout_snapshot.get()
    }

    /// Stores the scrollable-panel geometry recorded by a freshly drawn frame.
    pub(crate) fn set_layout_snapshot(&self, layout_snapshot: LayoutSnapshot) {
        self.layout_snapshot.set(layout_snapshot);
    }

    /// Returns the scrollbar drag in progress, if any.
    pub(crate) fn mouse_drag(&self) -> Option<ScrollbarDrag> {
        self.mouse_drag.get()
    }

    /// Starts or clears the scrollbar drag in progress.
    pub(crate) fn set_mouse_drag(&self, mouse_drag: Option<ScrollbarDrag>) {
        self.mouse_drag.set(mouse_drag);
    }

    /// Returns the UI cache collection shared by input metrics and rendering.
    pub(crate) fn render_cache_store(&self) -> &RenderCacheStore {
        &self.render_cache_store
    }
}
