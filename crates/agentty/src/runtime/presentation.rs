use std::cell::{Cell, RefCell};

use ratatui::Frame;
use ratatui::widgets::TableState;

use crate::app::AppViewSnapshot;
use crate::ui::style::ColorSupport;
use crate::ui::{self, RenderCacheStore};

/// Runtime-owned state used to measure and render the terminal presentation.
///
/// Keeping render caches here prevents application orchestration from
/// depending on concrete UI cache implementations or their invalidation
/// details.
#[derive(Default)]
pub(crate) struct PresentationState {
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
    /// presentation boundary.
    ///
    /// `color_support` tells the UI whether the terminal renders colors, so
    /// art that relies on a second tone can adapt its glyphs.
    pub(crate) fn render(
        &self,
        snapshot: &AppViewSnapshot<'_>,
        frame: &mut Frame,
        color_support: ColorSupport,
    ) {
        let mut project_table_state = self.project_table_state.borrow_mut();
        let mut session_table_state = self.session_table_state.borrow_mut();
        self.render_cache_store.greeting_animated().set(false);

        ui::render_app(
            snapshot,
            frame,
            color_support,
            &mut project_table_state,
            &self.render_cache_store,
            &mut session_table_state,
        );
    }

    /// Returns whether the last successful draw painted a nerpa greeting that
    /// can animate, meaning it fit a blank transcript and colors are enabled.
    pub(crate) fn greeting_animated(&self) -> bool {
        self.render_cache_store.greeting_animated().get()
    }

    /// Returns the UI cache collection shared by input metrics and rendering.
    pub(crate) fn render_cache_store(&self) -> &RenderCacheStore {
        &self.render_cache_store
    }
}
