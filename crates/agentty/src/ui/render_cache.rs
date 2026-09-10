use crate::ui::{component, markdown, page};

/// UI-owned cache store shared by render and scroll-metric paths.
///
/// The store keeps the concrete markdown, session-output, and diff cache
/// lifetimes together inside the UI boundary so app orchestration only needs
/// to retain one render-cache handle instead of knowing each cache type.
#[derive(Default)]
pub struct RenderCacheStore {
    diff_layout: page::diff::DiffLayoutCache,
    markdown_render: markdown::MarkdownRenderCache,
    session_output_layout: component::session_output::SessionOutputLayoutCache,
}

impl RenderCacheStore {
    /// Returns the cache for parsed diff content and rendered diff layouts.
    pub(crate) fn diff_layout_cache(&self) -> &page::diff::DiffLayoutCache {
        &self.diff_layout
    }

    /// Returns the cache for styled markdown lines.
    pub(crate) fn markdown_render_cache(&self) -> &markdown::MarkdownRenderCache {
        &self.markdown_render
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
