use crate::app::App;
use crate::ui::RenderCacheStore;
use crate::ui::component::session_output::SessionOutputLineContext;
use crate::ui::page::session_chat::SessionChatPage;

/// Returns rendered session-output line count for runtime scroll metrics,
/// resolving conditional scrollbar width against `viewport_height`.
pub(crate) fn rendered_output_line_count_with_cache(
    app: &App,
    render_cache_store: &RenderCacheStore,
    session_id: &str,
    session_index: usize,
    output_width: u16,
    viewport_height: u16,
) -> u16 {
    let active_progress = app.session_progress_message(session_id);
    let active_prompt_output = app
        .sessions
        .active_prompt_outputs()
        .get(session_id)
        .map(std::string::String::as_str);

    app.sessions.session_at(session_index).map_or(0, |session| {
        SessionChatPage::rendered_output_line_count(
            session,
            output_width,
            viewport_height,
            SessionOutputLineContext {
                active_prompt_output,
                active_progress,
                session_update_version: app.session_update_version(session_id),
            },
            render_cache_store.markdown_render_cache(),
            render_cache_store.session_output_layout_cache(),
        )
    })
}

#[cfg(test)]
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
