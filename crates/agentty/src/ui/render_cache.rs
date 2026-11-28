//! UI-owned store of render caches shared by drawing and scroll-metric paths.

use std::cell::Cell;

use crate::ui::{component, markdown, page};

/// UI-owned cache store shared by render and scroll-metric paths.
///
/// The store keeps the concrete markdown, session-output, and diff cache
/// lifetimes together inside the UI boundary so app orchestration only needs
/// to retain one render-cache handle instead of knowing each cache type. It
/// also carries per-frame render feedback that the runtime reads after a
/// draw.
#[derive(Default)]
pub struct RenderCacheStore {
    diff_layout: page::diff::DiffLayoutCache,
    /// Whether the last draw painted a nerpa greeting that can animate.
    /// Each draw resets it before painting.
    greeting_animated: Cell<bool>,
    markdown_render: markdown::MarkdownRenderCache,
    session_document_layout: component::session_document_overlay::SessionDocumentLayoutCache,
    session_output_layout: component::session_output::SessionOutputLayoutCache,
}

impl RenderCacheStore {
    /// Returns the cache for parsed diff content and rendered diff layouts.
    pub(crate) fn diff_layout_cache(&self) -> &page::diff::DiffLayoutCache {
        &self.diff_layout
    }

    /// Returns the flag recording whether the current draw painted a nerpa
    /// greeting that can animate.
    pub(crate) fn greeting_animated(&self) -> &Cell<bool> {
        &self.greeting_animated
    }

    /// Returns the cache for styled markdown lines.
    pub(crate) fn markdown_render_cache(&self) -> &markdown::MarkdownRenderCache {
        &self.markdown_render
    }

    /// Returns the cache for the session notes panel's wrapped entries.
    pub(crate) fn session_document_layout_cache(
        &self,
    ) -> &component::session_document_overlay::SessionDocumentLayoutCache {
        &self.session_document_layout
    }

    /// Returns the cache for fully assembled session-output layouts.
    pub(crate) fn session_output_layout_cache(
        &self,
    ) -> &component::session_output::SessionOutputLayoutCache {
        &self.session_output_layout
    }
}

#[cfg(test)]
#[path = "render_cache_test.rs"]
mod tests;
