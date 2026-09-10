use super::rendered_output_line_count_with_cache;
use crate::app::App;
use crate::ui::RenderCacheStore;

pub(crate) fn rendered_output_line_count(
    app: &App,
    session_id: &str,
    session_index: usize,
    output_width: u16,
    viewport_height: u16,
) -> u16 {
    rendered_output_line_count_with_cache(
        app,
        &RenderCacheStore::default(),
        session_id,
        session_index,
        output_width,
        viewport_height,
    )
}
