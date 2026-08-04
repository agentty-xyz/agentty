//! Shared terminal text rendering helpers for Agentty frontends.

/// HTML normalization and terminal rendering for forge-authored Markdown.
pub mod html;
/// Markdown parsing, terminal styling, wrapping, and rendered-line caching.
pub mod markdown;
/// Bounded Mermaid parsing and terminal diagram rendering.
pub mod mermaid;
mod style;
/// Terminal-width wrapping, truncation, borrowing, and compact formatting.
pub mod text_util;

pub use style::{TextPalette, TextRenderSettings};
