use super::RenderCacheStore;

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
