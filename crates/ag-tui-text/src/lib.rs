//! Shared terminal text rendering helpers for Agentty frontends.

pub mod markdown;
pub mod mermaid;
mod style;
pub mod text_util;

pub use style::{TextPalette, TextRenderSettings};
