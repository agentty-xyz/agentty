pub mod activity_heatmap;
pub mod component;
pub mod diff_util;
pub mod icon;
pub mod input_layout;
pub mod layout;
pub mod markdown;
pub mod overlay;
pub mod page;
pub mod prompt_format;
pub mod question_format;
mod render;
pub mod router;
pub mod session_format;
pub mod state;
/// Shared theme-aware UI styling helpers and status-display helpers.
pub mod style;
pub(crate) mod task_roadmap;
pub mod text_util;

/// A trait for UI components that enforces a standard rendering interface.
pub use render::Component;
/// A trait for UI pages that enforces a standard rendering interface.
pub use render::Page;
/// Immutable data required to draw a single UI frame.
pub use render::RenderContext;
/// Renders a complete frame including status bar, content area, and footer.
pub use render::render;
