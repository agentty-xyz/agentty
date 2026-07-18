/// Calendar-style recent session activity renderer.
pub mod activity_heatmap;
mod app_render;
pub mod component;
/// Unified-diff parsing, layout, and scroll helpers.
pub mod diff_util;
pub(crate) mod help_format;
/// Semantic symbols used throughout terminal rendering.
pub mod icon;
pub mod input_layout;
/// Shared page and popup layout calculations.
pub mod layout;
/// Markdown parsing, wrapping, styling, and caching.
pub mod markdown;
/// Mermaid diagram parsing and terminal rendering.
pub mod mermaid;
/// Overlay geometry and compositing helpers.
pub mod overlay;
pub mod page;
pub mod prompt_block;
pub mod prompt_format;
pub mod question_format;
mod render;
mod render_cache;
/// Forge review-comment formatting helpers.
pub mod review_comment_format;
/// Page router and mode-to-render projection.
pub mod router;
pub mod session_format;
pub(crate) mod session_output_assembly;
/// Shared theme-aware UI styling helpers and status-display helpers.
pub mod style;

pub(crate) use app_render::render_app;
/// A trait for UI components that enforces a standard rendering interface.
pub use render::Component;
/// A trait for UI pages that enforces a standard rendering interface.
pub use render::Page;
/// Immutable data required to draw a single UI frame.
pub use render::RenderContext;
/// Focused-review display state for the visible session.
pub use render::SessionReviewSnapshot;
/// Renders a complete frame including status bar, content area, and footer.
pub use render::render;
/// UI-owned cache store shared by render and scroll-metric paths.
pub use render_cache::RenderCacheStore;
