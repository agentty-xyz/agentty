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
mod tests {
    use super::*;

    #[test]
    fn test_render_cache_store_reuses_shared_cache_instances() {
        // Arrange
        let store = RenderCacheStore::default();

        // Act
        let markdown_render_cache = store.markdown_render_cache();
        let repeated_markdown_render_cache = store.markdown_render_cache();
        let diff_layout_cache = store.diff_layout_cache();
        let repeated_diff_layout_cache = store.diff_layout_cache();
        let session_output_layout_cache = store.session_output_layout_cache();
        let repeated_session_output_layout_cache = store.session_output_layout_cache();

        // Assert
        assert!(std::ptr::eq(
            markdown_render_cache,
            repeated_markdown_render_cache
        ));
        assert!(std::ptr::eq(diff_layout_cache, repeated_diff_layout_cache));
        assert!(std::ptr::eq(
            session_output_layout_cache,
            repeated_session_output_layout_cache
        ));
    }
}
